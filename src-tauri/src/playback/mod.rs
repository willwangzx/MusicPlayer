pub mod diagnostic;
pub mod mpv;

use std::{
    hash::BuildHasher,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use self::mpv::{event, Mpv, MpvEndFileSummary, LOCAL_AUDIO_OPTIONS};

type StateSink = dyn Fn(PlaybackSnapshot) + Send + Sync;

const STATE_EVENT: &str = "musicplayer://playback-state";
const ENGINE_COMMAND_CAPACITY: usize = 64;
const MAX_COMMANDS_PER_TICK: usize = 8;
const INACTIVE_EVENT_INTERVAL: Duration = Duration::from_millis(250);
const MAX_QUEUE_ITEMS: usize = 4_096;
const MAX_QUEUE_PATH_BYTES: usize = 4 * 1024 * 1024;
const MAX_RANDOM_CANDIDATES: usize = 100_000;
const RANDOM_HISTORY_LIMIT: usize = 512;
const DEFAULT_LIBRARY_ROOT: &str = r"D:\Music";
const DOWNLOADS_ROOT: &str = r"D:\Music\.downloads";
const DIAGNOSTIC_ROOT: &str = r"D:\Projects\MusicPlayer\gapless-assets";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackSnapshot {
    pub available: bool,
    pub state: String,
    pub queue_len: usize,
    pub current_index: Option<usize>,
    pub current_path: Option<String>,
    pub last_error: Option<String>,
    pub mpv_client_api_version: Option<String>,
    pub libmpv_path: Option<String>,
    pub transition_count: u64,
    pub audio_reconfig_count: u64,
    pub position: f64,
    pub duration: f64,
    pub volume: f64,
    pub queue_revision: u64,
    pub repeat_all: bool,
    pub persistence_error: Option<String>,
    pub audio_quality: AudioQuality,
}

/// Decoder/source details and negotiated playback output are kept separate.
/// Missing properties remain null; a resampled output is not a source upgrade.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioQuality {
    pub codec: Option<String>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    pub sample_format: Option<String>,
    pub bitrate: Option<u64>,
    pub output_sample_rate: Option<u32>,
    pub output_channels: Option<u32>,
    pub output_format: Option<String>,
}

impl Default for PlaybackSnapshot {
    fn default() -> Self {
        Self {
            available: false,
            state: "starting".to_string(),
            queue_len: 0,
            current_index: None,
            current_path: None,
            last_error: None,
            mpv_client_api_version: None,
            libmpv_path: None,
            transition_count: 0,
            audio_reconfig_count: 0,
            position: 0.0,
            duration: 0.0,
            volume: 100.0,
            queue_revision: 0,
            repeat_all: false,
            persistence_error: None,
            audio_quality: AudioQuality::default(),
        }
    }
}

const SESSION_LIMIT: usize = 8 * 1024 * 1024;
#[derive(Serialize, Deserialize, PartialEq)]
struct Session {
    version: u32,
    paths: Vec<String>,
    current: usize,
    position: f64,
    volume: f64,
    #[serde(default)]
    repeat_all: bool,
}
impl Session {
    fn valid(&self) -> bool {
        self.version == 1
            && self.paths.len() <= MAX_QUEUE_ITEMS
            && self.paths.iter().map(String::len).sum::<usize>() <= MAX_QUEUE_PATH_BYTES
            && (self.paths.is_empty() || self.current < self.paths.len())
            && self.position.is_finite()
            && (0.0..=604800.0).contains(&self.position)
            && self.volume.is_finite()
            && (0.0..=100.0).contains(&self.volume)
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuePage {
    paths: Vec<String>,
    offset: usize,
    total: usize,
    current: usize,
    revision: u64,
    repeat_all: bool,
    random_library: bool,
}

enum EngineCommand {
    SetQueue {
        paths: Vec<PathBuf>,
        start_index: usize,
        reply: mpsc::Sender<Result<(), String>>,
    },
    SetRandomLibrary {
        paths: Vec<PathBuf>,
        enabled: bool,
        revision: u64,
        current: usize,
        reply: mpsc::Sender<Result<(), String>>,
    },
    TogglePause(mpsc::Sender<Result<(), String>>),
    SetPaused(bool, mpsc::Sender<Result<(), String>>),
    Next(mpsc::Sender<Result<(), String>>),
    Previous(mpsc::Sender<Result<(), String>>),
    Stop(mpsc::Sender<Result<(), String>>),
    Seek {
        seconds: f64,
        revision: u64,
        current: usize,
        reply: mpsc::Sender<Result<(), String>>,
    },
    Volume(f64, mpsc::Sender<Result<(), String>>),
    Edit {
        action: String,
        path: Option<PathBuf>,
        index: usize,
        revision: u64,
        current: usize,
        reply: mpsc::Sender<Result<(), String>>,
    },
    RecyclePath {
        path: PathBuf,
        reply: mpsc::Sender<Result<(), String>>,
    },
    QueuePage(usize, mpsc::Sender<QueuePage>),
    Shutdown,
}

pub struct PlaybackManager {
    sender: mpsc::SyncSender<EngineCommand>,
    snapshot: Arc<Mutex<PlaybackSnapshot>>,
    thread: Mutex<Option<thread::JoinHandle<()>>>,
    done: Mutex<Option<mpsc::Receiver<()>>>,
    closing: AtomicBool,
    shutdown_requested: Arc<AtomicBool>,
}

impl PlaybackManager {
    pub fn start(app: AppHandle) -> Self {
        Self::start_with(
            Box::new(move |state| {
                if let Some(controls) = app.try_state::<crate::media_controls::MediaControls>() {
                    controls.update(state.clone());
                }
                let _ = app.emit(STATE_EVENT, state);
            }),
            initialize_player,
            crate::state::directory().map(|path| path.join("session.json")),
        )
    }

    fn start_with(
        sink: Box<StateSink>,
        initialize: fn() -> Result<Mpv, String>,
        session_path: Result<PathBuf, String>,
    ) -> Self {
        let (sender, receiver) = mpsc::sync_channel(ENGINE_COMMAND_CAPACITY);
        let (done_sender, done_receiver) = mpsc::channel();
        let snapshot = Arc::new(Mutex::new(PlaybackSnapshot::default()));
        let worker_snapshot = Arc::clone(&snapshot);
        let shutdown_requested = Arc::new(AtomicBool::new(false));
        let worker_shutdown = shutdown_requested.clone();
        let worker = thread::Builder::new()
            .name("musicplayer-libmpv".to_string())
            .spawn(move || {
                run_engine(
                    sink,
                    receiver,
                    worker_snapshot,
                    worker_shutdown,
                    initialize,
                    session_path,
                );
                let _ = done_sender.send(());
            })
            .expect("failed to start the libmpv event thread");

        Self {
            sender,
            snapshot,
            thread: Mutex::new(Some(worker)),
            done: Mutex::new(Some(done_receiver)),
            closing: AtomicBool::new(false),
            shutdown_requested,
        }
    }

    pub(crate) fn media_dispatch(&self) -> crate::media_controls::Dispatch {
        let sender = self.sender.clone();
        Arc::new(move |button| {
            let (reply, _receiver) = mpsc::channel();
            let command = match button {
                crate::media_controls::MediaButton::Play => EngineCommand::SetPaused(false, reply),
                crate::media_controls::MediaButton::Pause => EngineCommand::SetPaused(true, reply),
                crate::media_controls::MediaButton::Next => EngineCommand::Next(reply),
                crate::media_controls::MediaButton::Previous => EngineCommand::Previous(reply),
                crate::media_controls::MediaButton::Stop => EngineCommand::Stop(reply),
            };
            sender
                .try_send(command)
                .map_err(|_| "Playback engine unavailable or busy".to_string())
        })
    }

    pub fn snapshot(&self) -> PlaybackSnapshot {
        self.snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn set_queue(&self, paths: Vec<String>, start_index: usize) -> Result<(), String> {
        if paths.is_empty() {
            return Err("the playback queue cannot be empty".to_string());
        }
        if paths.len() > MAX_QUEUE_ITEMS {
            return Err(format!(
                "the playback queue cannot contain more than {MAX_QUEUE_ITEMS} tracks"
            ));
        }
        let total_path_bytes = paths
            .iter()
            .fold(0usize, |total, path| total.saturating_add(path.len()));
        if total_path_bytes > MAX_QUEUE_PATH_BYTES {
            return Err(format!(
                "the playback queue paths exceed the {MAX_QUEUE_PATH_BYTES}-byte limit"
            ));
        }
        if start_index >= paths.len() {
            return Err(format!(
                "start index {start_index} is outside a queue of {} tracks",
                paths.len()
            ));
        }

        let paths = paths
            .into_iter()
            .map(|path| validate_local_audio_path(&path))
            .collect::<Result<Vec<_>, _>>()?;
        self.request(|reply| EngineCommand::SetQueue {
            paths,
            start_index,
            reply,
        })
    }

    pub fn toggle_pause(&self) -> Result<(), String> {
        self.request(EngineCommand::TogglePause)
    }

    pub fn set_random_library(
        &self,
        paths: Vec<String>,
        enabled: bool,
        revision: u64,
        current: usize,
    ) -> Result<(), String> {
        if enabled && (paths.is_empty() || paths.len() > MAX_RANDOM_CANDIDATES) {
            return Err(format!(
                "Random playback needs 1 to {MAX_RANDOM_CANDIDATES} indexed tracks"
            ));
        }
        // Validate the potentially large library snapshot off the libmpv event
        // thread, so playback can continue while the disk is being scanned.
        let paths = if enabled {
            let mut seen = std::collections::HashSet::new();
            paths
                .into_iter()
                .filter_map(|path| validate_local_audio_path(&path).ok())
                .filter(|path| seen.insert(normalized_windows_path(path)))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        if enabled && paths.is_empty() {
            return Err("No indexed local audio tracks are available".into());
        }
        self.request(|reply| EngineCommand::SetRandomLibrary {
            paths,
            enabled,
            revision,
            current,
            reply,
        })
    }

    pub fn next(&self) -> Result<(), String> {
        self.request(EngineCommand::Next)
    }

    pub fn previous(&self) -> Result<(), String> {
        self.request(EngineCommand::Previous)
    }

    pub fn stop(&self) -> Result<(), String> {
        self.request(EngineCommand::Stop)
    }

    pub fn seek(&self, seconds: f64, revision: u64, current: usize) -> Result<(), String> {
        if !seconds.is_finite() || !(0.0..=604800.0).contains(&seconds) {
            return Err("Invalid seek position".into());
        }
        self.request(|reply| EngineCommand::Seek {
            seconds,
            revision,
            current,
            reply,
        })
    }
    pub fn volume(&self, value: f64) -> Result<(), String> {
        if !value.is_finite() || !(0.0..=100.0).contains(&value) {
            return Err("Volume must be between 0 and 100".into());
        }
        self.request(|reply| EngineCommand::Volume(value, reply))
    }
    pub fn queue_page(&self, offset: usize) -> Result<QueuePage, String> {
        let (reply, receiver) = mpsc::channel();
        self.sender
            .try_send(EngineCommand::QueuePage(offset, reply))
            .map_err(|_| "Playback engine unavailable or busy")?;
        receiver
            .recv()
            .map_err(|_| "Playback engine stopped".into())
    }
    pub fn edit(
        &self,
        action: String,
        path: Option<String>,
        index: usize,
        revision: u64,
        current: usize,
    ) -> Result<(), String> {
        if !matches!(
            action.as_str(),
            "enqueue" | "next" | "remove" | "clear" | "select" | "shuffle" | "repeat"
        ) {
            return Err("Unknown queue action".into());
        }
        let path = path.map(|p| validate_local_audio_path(&p)).transpose()?;
        self.request(|reply| EngineCommand::Edit {
            action,
            path,
            index,
            revision,
            current,
            reply,
        })
    }

    pub fn recycle_path(&self, path: String) -> Result<(), String> {
        let path = validate_local_audio_path(&path)?;
        let (reply, response) = mpsc::channel();
        match self.sender.try_send(EngineCommand::RecyclePath {
            path: path.clone(),
            reply,
        }) {
            Ok(()) => match response.recv() {
                Ok(result) => result,
                Err(_) => {
                    // The worker can disappear while a cold libmpv load is
                    // failing. Once its receiver and mpv value are gone, the
                    // same safe direct fallback applies.
                    crate::recycle::recycle_file(path_text(&path)?)
                }
            },
            Err(mpsc::TrySendError::Full(_)) => {
                Err("the libmpv event thread command queue is full".to_string())
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                // Initialization failure drops the worker and all mpv handles.
                // Library deletion remains available because there is then no
                // playback state that needs serialized repair.
                crate::recycle::recycle_file(path_text(&path)?)
            }
        }
    }

    fn request(
        &self,
        build: impl FnOnce(mpsc::Sender<Result<(), String>>) -> EngineCommand,
    ) -> Result<(), String> {
        let (reply, response) = mpsc::channel();
        self.sender
            .try_send(build(reply))
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => {
                    "the libmpv event thread command queue is full".to_string()
                }
                mpsc::TrySendError::Disconnected(_) => {
                    "the libmpv event thread is not running".to_string()
                }
            })?;
        response
            .recv()
            .map_err(|_| "the libmpv event thread stopped before answering".to_string())?
    }
}

impl PlaybackManager {
    pub fn begin_close(&self) -> bool {
        !self.closing.swap(true, Ordering::Relaxed)
    }
    pub fn shutdown(&self) {
        self.shutdown_requested.store(true, Ordering::Relaxed);
        // A cold libmpv DLL load can still be in progress if the user closes
        // the window immediately. Never let that initialization stall the UI
        // shutdown indefinitely: request exit, wait briefly for a clean DLL
        // teardown, then detach the exact worker so process exit can finish.
        let _ = self.sender.try_send(EngineCommand::Shutdown);
        let finished = self
            .done
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .is_some_and(|done| done.recv_timeout(Duration::from_secs(5)).is_ok());
        let worker = self
            .thread
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if finished {
            if let Some(worker) = worker {
                let _ = worker.join();
            }
        }
    }
}

impl Drop for PlaybackManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct LookaheadQueue {
    repeat_all: bool,
    random_library: Option<RandomLibrary>,
    paths: Vec<PathBuf>,
    current: usize,
    awaiting_initial_start: bool,
    generation: u64,
    current_entry_id: Option<i64>,
    lookahead_entry_id: Option<i64>,
}

struct RandomLibrary {
    candidates: Vec<PathBuf>,
    hasher: std::collections::hash_map::RandomState,
    draw: u64,
}

impl RandomLibrary {
    fn choose(&mut self, previous: Option<&Path>) -> Option<PathBuf> {
        let len = self.candidates.len();
        if len == 0 {
            return None;
        }
        self.draw = self.draw.wrapping_add(1);
        let start = (self.hasher.hash_one(self.draw) as usize) % len;
        let mut fallback = None;
        for offset in 0..len {
            let path = &self.candidates[(start + offset) % len];
            if !path.is_file() {
                continue;
            }
            if previous.is_some_and(|previous| same_windows_path(previous, path)) {
                fallback = Some(path.clone());
                continue;
            }
            return Some(path.clone());
        }
        fallback
    }
}

fn next_index(length: usize, current: usize, repeat: bool) -> Option<usize> {
    if current + 1 < length {
        Some(current + 1)
    } else if repeat && length > 0 {
        Some(0)
    } else {
        None
    }
}

#[derive(Debug, PartialEq, Eq)]
struct RemovalPlan {
    paths: Vec<PathBuf>,
    current: usize,
    removed: usize,
    selected_removed: bool,
}

fn plan_removal(
    paths: &[PathBuf],
    current: usize,
    mut remove: impl FnMut(usize, &Path) -> bool,
) -> RemovalPlan {
    let mut retained = Vec::with_capacity(paths.len());
    let mut retained_before_current = 0usize;
    let mut removed = 0usize;
    let mut selected_removed = false;
    for (index, path) in paths.iter().enumerate() {
        if remove(index, path) {
            removed += 1;
            selected_removed |= index == current;
        } else {
            retained_before_current += usize::from(index < current);
            retained.push(path.clone());
        }
    }
    let current = if retained.is_empty() {
        0
    } else {
        retained_before_current.min(retained.len() - 1)
    };
    RemovalPlan {
        paths: retained,
        current,
        removed,
        selected_removed,
    }
}

pub(crate) fn same_windows_path(left: &Path, right: &Path) -> bool {
    normalized_windows_path(left) == normalized_windows_path(right)
}

impl LookaheadQueue {
    fn new() -> Self {
        Self {
            paths: Vec::new(),
            repeat_all: false,
            random_library: None,
            current: 0,
            awaiting_initial_start: false,
            generation: 0,
            current_entry_id: None,
            lookahead_entry_id: None,
        }
    }

    fn fill_random_tail(&mut self, paths: &mut Vec<PathBuf>, current: usize) {
        if current + 1 >= paths.len() && paths.len() < MAX_QUEUE_ITEMS {
            if let Some(random) = &mut self.random_library {
                if let Some(next) = random.choose(paths.last().map(PathBuf::as_path)) {
                    let bytes = paths
                        .iter()
                        .map(|path| path.to_string_lossy().len())
                        .sum::<usize>();
                    if bytes.saturating_add(next.to_string_lossy().len()) <= MAX_QUEUE_PATH_BYTES {
                        paths.push(next);
                    }
                }
            }
        }
    }

    fn trim_random_history(&mut self) {
        if self.random_library.is_some() && self.current >= RANDOM_HISTORY_LIMIT {
            self.paths.drain(..self.current);
            self.current = 0;
        }
    }

    fn set_random_library(
        &mut self,
        mpv: &mut Mpv,
        paths: Vec<PathBuf>,
        enabled: bool,
        revision: u64,
        current: usize,
    ) -> Result<bool, String> {
        if self.generation != revision || self.current != current {
            return Err("Queue changed; please try again".into());
        }
        if !enabled {
            // An already queued random lookahead remains a normal queue item.
            self.random_library = None;
            self.generation = self.generation.wrapping_add(1);
            return Ok(false);
        }
        let mut random = RandomLibrary {
            candidates: paths,
            hasher: std::collections::hash_map::RandomState::new(),
            draw: 0,
        };
        if self.paths.is_empty() {
            let first = random
                .choose(None)
                .ok_or("No indexed local audio tracks are available")?;
            let mut initial = vec![first];
            if let Some(next) = random.choose(initial.first().map(PathBuf::as_path)) {
                initial.push(next);
            }
            let old_repeat = self.repeat_all;
            self.repeat_all = false;
            if let Err(error) = self.replace(mpv, initial, 0) {
                self.repeat_all = old_repeat;
                return Err(error);
            }
            self.random_library = Some(random);
            return Ok(true);
        }
        if self.current + 1 == self.paths.len() {
            let next = random.choose(self.paths.last().map(PathBuf::as_path));
            if let Some(next) = next {
                let path_bytes = self
                    .paths
                    .iter()
                    .map(|p| p.to_string_lossy().len())
                    .sum::<usize>();
                let must_trim = self.paths.len() >= MAX_QUEUE_ITEMS
                    || path_bytes.saturating_add(next.to_string_lossy().len())
                        > MAX_QUEUE_PATH_BYTES;
                if must_trim && self.current == 0 {
                    return Err("Queue capacity reached".into());
                }
                if self.current_entry_id.is_some() {
                    let was_paused = self.pause_and_guard_current(mpv)?;
                    if let Err(error) = mpv.command(&["playlist-clear"]) {
                        return Err(self.stop_after_error(mpv, error));
                    }
                    self.lookahead_entry_id = None;
                    if let Err(error) =
                        path_text(&next).and_then(|path| mpv.command(&["loadfile", path, "append"]))
                    {
                        return Err(self.stop_after_error(mpv, error));
                    }
                    let ids = mpv
                        .playlist_entry_ids()
                        .map_err(|error| self.stop_after_error(mpv, error))?;
                    if ids.len() != 2 {
                        return Err(self.stop_after_error(
                            mpv,
                            format!("mpv playlist has {} entries; expected 2", ids.len()),
                        ));
                    }
                    self.current_entry_id = ids.first().copied();
                    self.lookahead_entry_id = ids.get(1).copied();
                    self.restore_pause_or_stop(mpv, was_paused)?;
                }
                if must_trim {
                    self.paths.drain(..self.current);
                    self.current = 0;
                }
                self.paths.push(next);
            }
        }
        self.repeat_all = false;
        self.random_library = Some(random);
        self.generation = self.generation.wrapping_add(1);
        Ok(false)
    }

    fn replace(
        &mut self,
        mpv: &mut Mpv,
        mut paths: Vec<PathBuf>,
        mut start: usize,
    ) -> Result<(), String> {
        if paths.is_empty() || start >= paths.len() {
            return Err("cannot replace playback with an invalid queue".to_string());
        }
        if self.random_library.is_some() && start > 0 && paths.len() >= MAX_QUEUE_ITEMS {
            paths.drain(..start);
            start = 0;
        }
        self.fill_random_tail(&mut paths, start);

        let current_path = path_text(&paths[start])?;
        mpv.command(&["loadfile", current_path, "replace"])?;
        if let Some(path) =
            next_index(paths.len(), start, self.repeat_all).and_then(|i| paths.get(i))
        {
            if let Err(error) = mpv.command(&["loadfile", path_text(path)?, "append"]) {
                self.invalidate_after_partial_replace(paths, start);
                return Err(error);
            }
        }

        let ids = match mpv.playlist_entry_ids() {
            Ok(ids) => ids,
            Err(error) => {
                self.invalidate_after_partial_replace(paths, start);
                return Err(error);
            }
        };
        let expected = if next_index(paths.len(), start, self.repeat_all)
            .and_then(|i| paths.get(i))
            .is_some()
        {
            2
        } else {
            1
        };
        if ids.len() != expected {
            self.invalidate_after_partial_replace(paths, start);
            return Err(format!(
                "mpv playlist has {} entries after replace; expected {expected}",
                ids.len()
            ));
        }

        self.generation = self.generation.wrapping_add(1);
        self.paths = paths;
        self.current = start;
        self.awaiting_initial_start = true;
        self.current_entry_id = ids.first().copied();
        self.lookahead_entry_id = ids.get(1).copied();
        Ok(())
    }

    fn invalidate_after_partial_replace(&mut self, paths: Vec<PathBuf>, start: usize) {
        self.generation = self.generation.wrapping_add(1);
        self.paths = paths;
        self.current = start;
        self.awaiting_initial_start = true;
        self.current_entry_id = None;
        self.lookahead_entry_id = None;
    }

    fn invalidate_entry_mapping(&mut self) {
        self.current_entry_id = None;
        self.lookahead_entry_id = None;
        self.awaiting_initial_start = false;
    }

    fn refresh_entry_mapping(&mut self, mpv: &mut Mpv) -> Result<(), String> {
        let expected = if next_index(self.paths.len(), self.current, self.repeat_all)
            .and_then(|i| self.paths.get(i))
            .is_some()
        {
            2
        } else {
            1
        };
        let ids = mpv.playlist_entry_ids()?;
        if ids.len() != expected {
            return Err(format!(
                "mpv playlist has {} entries during generation {}; expected {expected}",
                ids.len(),
                self.generation
            ));
        }
        self.current_entry_id = ids.first().copied();
        self.lookahead_entry_id = ids.get(1).copied();
        Ok(())
    }

    fn on_start_file(
        &mut self,
        mpv: &mut Mpv,
        entry_id: i64,
    ) -> Result<StartFileDisposition, String> {
        if self.current_entry_id == Some(entry_id) {
            self.awaiting_initial_start = false;
            return Ok(StartFileDisposition::Current);
        }
        if self.lookahead_entry_id != Some(entry_id) {
            // An old START_FILE event can arrive after loadfile replace has
            // queued a new playlist. Entry IDs are unique for the life of an
            // mpv core, so an unknown ID is stale rather than a reason to
            // advance our logical queue.
            return Ok(StartFileDisposition::Ignored);
        }

        let next = next_index(self.paths.len(), self.current, self.repeat_all)
            .ok_or_else(|| "playback queue has no next entry".to_string())?;
        if next >= self.paths.len() {
            return Err(format!(
                "mpv started lookahead entry {entry_id}, but logical queue has no next entry"
            ));
        }

        if let Err(error) = mpv.command(&["playlist-remove", "0"]) {
            // The START_FILE event means that mpv may already have moved to
            // the lookahead, but a failed remove leaves the playlist mapping
            // unknowable. Stop before returning so mpv cannot run ahead while
            // the logical queue is invalidated.
            self.invalidate_entry_mapping();
            let stop_error = mpv.command(&["stop"]).err();
            return Err(match stop_error {
                Some(stop_error) => {
                    format!("{error}; failed to stop playback: {stop_error}")
                }
                None => error,
            });
        }
        self.current = next;
        self.trim_random_history();
        if self.current + 1 == self.paths.len() {
            let mut paths = std::mem::take(&mut self.paths);
            self.fill_random_tail(&mut paths, self.current);
            self.paths = paths;
        }
        if let Some(path) = next_index(self.paths.len(), self.current, self.repeat_all)
            .and_then(|i| self.paths.get(i))
        {
            let append =
                path_text(path).and_then(|path| mpv.command(&["loadfile", path, "append"]));
            if let Err(error) = append {
                self.invalidate_entry_mapping();
                let stop_error = mpv.command(&["stop"]).err();
                return Err(match stop_error {
                    Some(stop_error) => {
                        format!("{error}; failed to stop playback: {stop_error}")
                    }
                    None => error,
                });
            }
        }
        if let Err(error) = self.refresh_entry_mapping(mpv) {
            self.invalidate_entry_mapping();
            let stop_error = mpv.command(&["stop"]).err();
            return Err(match stop_error {
                Some(stop_error) => {
                    format!("{error}; failed to stop playback: {stop_error}")
                }
                None => error,
            });
        }
        self.awaiting_initial_start = false;
        self.generation = self.generation.wrapping_add(1);
        Ok(StartFileDisposition::Transitioned)
    }

    fn previous(&mut self, mpv: &mut Mpv) -> Result<(), String> {
        if self.paths.is_empty() {
            return Ok(());
        }
        let previous = self.current.saturating_sub(1);
        let paths = self.paths.clone();
        self.replace(mpv, paths, previous)
    }

    fn commit_removal(&mut self, plan: RemovalPlan) {
        self.paths = plan.paths;
        self.current = plan.current;
        self.generation = self.generation.wrapping_add(1);
    }

    fn pause_and_guard_current(&mut self, mpv: &mut Mpv) -> Result<bool, String> {
        let was_paused = mpv.property_string("pause")?.as_deref() == Some("yes");
        mpv.command(&["set", "pause", "yes"])?;
        let transition_error = if self.awaiting_initial_start {
            Some("Track is transitioning; please try again".to_string())
        } else {
            match mpv.current_playlist_entry_id() {
                Ok(id) if id == self.current_entry_id => None,
                Ok(_) => Some("Track is transitioning; please try again".to_string()),
                Err(error) => Some(error),
            }
        };
        if let Some(error) = transition_error {
            let resume_error = mpv
                .command(&["set", "pause", if was_paused { "yes" } else { "no" }])
                .err();
            return Err(match resume_error {
                Some(resume_error) => {
                    format!("{error}; failed to restore pause state: {resume_error}")
                }
                None => error,
            });
        }
        Ok(was_paused)
    }

    fn stop_after_error(&mut self, mpv: &mut Mpv, error: String) -> String {
        self.invalidate_entry_mapping();
        match mpv.command(&["stop"]) {
            Ok(()) => error,
            Err(stop_error) => format!("{error}; failed to stop playback: {stop_error}"),
        }
    }

    fn refill_lookahead(&mut self, mpv: &mut Mpv) -> Result<(), String> {
        if self.current + 1 == self.paths.len() {
            let mut paths = std::mem::take(&mut self.paths);
            self.fill_random_tail(&mut paths, self.current);
            self.paths = paths;
        }
        if let Some(path) = next_index(self.paths.len(), self.current, self.repeat_all)
            .and_then(|index| self.paths.get(index))
        {
            mpv.command(&["loadfile", path_text(path)?, "append"])?;
        }
        self.refresh_entry_mapping(mpv)
    }

    fn restore_pause_or_stop(&mut self, mpv: &mut Mpv, was_paused: bool) -> Result<(), String> {
        mpv.command(&["set", "pause", if was_paused { "yes" } else { "no" }])
            .map_err(|error| self.stop_after_error(mpv, error))
    }

    fn remove_at(&mut self, mpv: &mut Mpv, index: usize) -> Result<(), String> {
        if index >= self.paths.len() {
            return Err("Track is no longer queued".into());
        }
        let mut plan = plan_removal(&self.paths, self.current, |candidate, _| candidate == index);
        self.fill_random_tail(&mut plan.paths, plan.current);
        if self.current_entry_id.is_none() {
            self.commit_removal(plan);
            return Ok(());
        }

        if plan.selected_removed {
            let was_paused = self.pause_and_guard_current(mpv)?;
            if plan.paths.is_empty() {
                if let Err(error) = mpv.command(&["stop"]) {
                    let _ = mpv.command(&["set", "pause", if was_paused { "yes" } else { "no" }]);
                    return Err(error);
                }
                self.commit_removal(plan);
                self.invalidate_entry_mapping();
                return Ok(());
            }

            let replacement = self.replace(mpv, plan.paths, plan.current);
            if let Err(error) = replacement {
                if self.current_entry_id.is_some() {
                    let resume_error = mpv
                        .command(&["set", "pause", if was_paused { "yes" } else { "no" }])
                        .err();
                    return Err(match resume_error {
                        Some(resume_error) => {
                            format!("{error}; failed to restore pause state: {resume_error}")
                        }
                        None => error,
                    });
                }
                return Err(self.stop_after_error(mpv, error));
            }
            return self.restore_pause_or_stop(mpv, was_paused);
        }

        let old_next = next_index(self.paths.len(), self.current, self.repeat_all)
            .and_then(|next| self.paths.get(next));
        let new_next = next_index(plan.paths.len(), plan.current, self.repeat_all)
            .and_then(|next| plan.paths.get(next));
        let lookahead_unchanged = match (old_next, new_next) {
            (Some(left), Some(right)) => same_windows_path(left, right),
            (None, None) => true,
            _ => false,
        };
        if lookahead_unchanged {
            self.commit_removal(plan);
            return Ok(());
        }

        let was_paused = self.pause_and_guard_current(mpv)?;
        if let Err(error) = mpv.command(&["playlist-clear"]) {
            return Err(self.stop_after_error(mpv, error));
        }
        self.lookahead_entry_id = None;
        self.commit_removal(plan);
        if let Err(error) = self.refill_lookahead(mpv) {
            return Err(self.stop_after_error(mpv, error));
        }
        self.restore_pause_or_stop(mpv, was_paused)
    }

    fn recycle_path(&mut self, mpv: &mut Mpv, target: &Path) -> Result<Option<String>, String> {
        let plan = plan_removal(&self.paths, self.current, |_, path| {
            same_windows_path(path, target)
        });
        let target_is_current = self.current_entry_id.is_some()
            && self
                .paths
                .get(self.current)
                .is_some_and(|path| same_windows_path(path, target));
        let target_is_lookahead = self.lookahead_entry_id.is_some()
            && next_index(self.paths.len(), self.current, self.repeat_all)
                .and_then(|index| self.paths.get(index))
                .is_some_and(|path| same_windows_path(path, target));
        let unknown_mapping_with_match = self.current_entry_id.is_none() && plan.removed > 0;
        let target_text = path_text(target)?;

        if target_is_current || unknown_mapping_with_match {
            let had_active_entry = self.current_entry_id.is_some();
            let was_paused = if had_active_entry {
                self.pause_and_guard_current(mpv)?
            } else {
                true
            };
            mpv.command(&["stop"])?;
            self.invalidate_entry_mapping();
            crate::recycle::recycle_file(target_text)?;
            self.commit_removal(plan);
            if self.paths.is_empty() || !had_active_entry {
                return Ok(None);
            }

            let paths = self.paths.clone();
            let current = self.current;
            if let Err(error) = self.replace(mpv, paths, current) {
                let warning = self.stop_after_error(mpv, error);
                return Ok(Some(format!(
                    "File moved to the Recycle Bin, but playback could not continue: {warning}"
                )));
            }
            return Ok(self
                .restore_pause_or_stop(mpv, was_paused)
                .err()
                .map(|warning| {
                    format!(
                        "File moved to the Recycle Bin, but playback could not continue: {warning}"
                    )
                }));
        }

        if target_is_lookahead {
            let was_paused = self.pause_and_guard_current(mpv)?;
            if let Err(error) = mpv.command(&["playlist-clear"]) {
                return Err(self.stop_after_error(mpv, error));
            }
            self.lookahead_entry_id = None;
            if let Err(recycle_error) = crate::recycle::recycle_file(target_text) {
                let restore = self
                    .refill_lookahead(mpv)
                    .and_then(|_| self.restore_pause_or_stop(mpv, was_paused));
                return Err(match restore {
                    Ok(()) => recycle_error,
                    Err(restore_error) => self.stop_after_error(
                        mpv,
                        format!("{recycle_error}; failed to restore playback: {restore_error}"),
                    ),
                });
            }

            self.commit_removal(plan);
            if let Err(error) = self.refill_lookahead(mpv) {
                let warning = self.stop_after_error(mpv, error);
                return Ok(Some(format!(
                    "File moved to the Recycle Bin, but playback could not continue: {warning}"
                )));
            }
            return Ok(self
                .restore_pause_or_stop(mpv, was_paused)
                .err()
                .map(|warning| {
                    format!(
                        "File moved to the Recycle Bin, but playback could not continue: {warning}"
                    )
                }));
        }

        crate::recycle::recycle_file(target_text)?;
        if plan.removed > 0 {
            self.commit_removal(plan);
        }
        Ok(None)
    }

    fn edit(
        &mut self,
        mpv: &mut Mpv,
        action: &str,
        path: Option<PathBuf>,
        index: usize,
        revision: u64,
        current: usize,
    ) -> Result<(), String> {
        if self.generation != revision || self.current != current {
            return Err("Queue changed; please try again".into());
        }
        if action == "select" {
            if index >= self.paths.len() {
                return Err("Track is no longer queued".into());
            }
            self.replace(mpv, self.paths.clone(), index)?;
            return mpv.command(&["set", "pause", "no"]);
        }
        if action == "remove" {
            return self.remove_at(mpv, index);
        }
        let mut paths = self.paths.clone();
        let repeat_all = if action == "repeat" {
            index != 0
        } else {
            self.repeat_all
        };
        match action {
            "repeat" => {}
            "shuffle" => {
                let start = (self.current + 1).min(paths.len());
                let random = std::collections::hash_map::RandomState::new();
                // Fresh randomly keyed hashes permute queue entries, including duplicates.
                let mut upcoming: Vec<_> = paths.drain(start..).enumerate().collect();
                upcoming.sort_by_cached_key(|(index, _)| random.hash_one(index));
                paths.extend(upcoming.into_iter().map(|(_, path)| path));
            }
            "enqueue" | "next" => {
                let path = path.ok_or("Choose a track to add")?;
                if action == "next" && !paths.is_empty() {
                    paths.insert(self.current + 1, path);
                } else {
                    paths.push(path);
                }
            }
            "clear" => {
                mpv.command(&["stop"])?;
                paths.clear();
                self.random_library = None;
                self.current = 0;
                self.invalidate_entry_mapping();
            }
            _ => return Err("Unknown queue action".into()),
        }
        if action != "clear" && !paths.is_empty() {
            self.fill_random_tail(&mut paths, self.current);
        }
        if paths.len() > MAX_QUEUE_ITEMS
            || paths
                .iter()
                .map(|p| p.to_string_lossy().len())
                .sum::<usize>()
                > MAX_QUEUE_PATH_BYTES
        {
            return Err("Queue capacity reached".into());
        }
        if action != "clear"
            && self.current_entry_id.is_some()
            && next_index(paths.len(), self.current, repeat_all).and_then(|i| paths.get(i))
                != next_index(self.paths.len(), self.current, self.repeat_all)
                    .and_then(|i| self.paths.get(i))
        {
            let paused = mpv.property_string("pause")?.as_deref() == Some("yes");
            mpv.command(&["set", "pause", "yes"])?;

            // Check the transition guard before mutating the mpv playlist.
            // If the selection became stale, restore the original pause state
            // and leave both the logical queue and mapping untouched.
            let transition_error = if self.awaiting_initial_start {
                Some("Track is transitioning; please try again".to_string())
            } else {
                match mpv.current_playlist_entry_id() {
                    Ok(id) if id == self.current_entry_id => None,
                    Ok(_) => Some("Track is transitioning; please try again".to_string()),
                    Err(error) => Some(error),
                }
            };
            if let Some(error) = transition_error {
                let resume_error = mpv
                    .command(&["set", "pause", if paused { "yes" } else { "no" }])
                    .err();
                return Err(match resume_error {
                    Some(resume_error) => {
                        format!("{error}; failed to restore pause state: {resume_error}")
                    }
                    None => error,
                });
            }

            // playlist-clear is the first mutating operation. If it or a
            // later replacement step fails, stop and invalidate the mapping;
            // retain the logical edit once playlist-clear has succeeded so a
            // later explicit restart can rebuild the complete queue.
            if let Err(error) = mpv.command(&["playlist-clear"]) {
                self.invalidate_entry_mapping();
                let stop_error = mpv.command(&["stop"]).err();
                return Err(match stop_error {
                    Some(stop_error) => {
                        format!("{error}; failed to stop playback: {stop_error}")
                    }
                    None => error,
                });
            }
            self.lookahead_entry_id = None;
            self.paths = paths.clone();
            self.repeat_all = repeat_all;
            let replacement = next_index(paths.len(), self.current, repeat_all)
                .and_then(|i| paths.get(i))
                .map(|next| mpv.command(&["loadfile", path_text(next)?, "append"]))
                .transpose()
                .map(|_| ())
                .and_then(|_| self.refresh_entry_mapping(mpv));
            if let Err(error) = replacement {
                self.invalidate_entry_mapping();
                self.generation = self.generation.wrapping_add(1);
                let stop_error = mpv.command(&["stop"]).err();
                return Err(match stop_error {
                    Some(stop_error) => {
                        format!("{error}; failed to stop playback: {stop_error}")
                    }
                    None => error,
                });
            }
            if let Err(error) = mpv.command(&["set", "pause", if paused { "yes" } else { "no" }]) {
                self.invalidate_entry_mapping();
                self.generation = self.generation.wrapping_add(1);
                let stop_error = mpv.command(&["stop"]).err();
                return Err(match stop_error {
                    Some(stop_error) => {
                        format!("{error}; failed to stop playback: {stop_error}")
                    }
                    None => error,
                });
            }
            self.generation = self.generation.wrapping_add(1);
            if action == "repeat" && repeat_all {
                self.random_library = None;
            }
            return Ok(());
        }
        self.paths = paths;
        self.repeat_all = repeat_all;
        if action == "repeat" && repeat_all {
            self.random_library = None;
        }
        self.generation = self.generation.wrapping_add(1);
        Ok(())
    }
    fn contains_entry(&self, entry_id: i64) -> bool {
        self.current_entry_id == Some(entry_id) || self.lookahead_entry_id == Some(entry_id)
    }

    fn current_path(&self) -> Option<String> {
        self.paths
            .get(self.current)
            .map(|path| path.display().to_string())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartFileDisposition {
    Ignored,
    Current,
    Transitioned,
}

fn run_engine(
    sink: Box<StateSink>,
    receiver: mpsc::Receiver<EngineCommand>,
    snapshot: Arc<Mutex<PlaybackSnapshot>>,
    shutdown_requested: Arc<AtomicBool>,
    initialize: fn() -> Result<Mpv, String>,
    session_path: Result<PathBuf, String>,
) {
    let app = &*sink;
    let mut mpv = match initialize() {
        Ok(player) => player,
        Err(error) => {
            update_snapshot(app, &snapshot, |state| {
                state.available = false;
                state.state = "unavailable".to_string();
                state.last_error = Some(error);
            });
            return;
        }
    };

    update_snapshot(app, &snapshot, |state| {
        state.available = true;
        state.state = "idle".to_string();
        state.last_error = None;
        state.mpv_client_api_version = Some(mpv.client_api_version());
        state.libmpv_path = Some(mpv.loaded_from().to_string());
    });

    let mut queue = LookaheadQueue::new();
    let mut resume_position = None;
    let mut paused_seek_position = None;
    let mut consecutive_failures = 0usize;
    match session_path
        .as_ref()
        .map_err(Clone::clone)
        .and_then(|p| crate::state::load::<Session>(p, SESSION_LIMIT))
    {
        Ok(Some(saved)) if saved.valid() => {
            let old_current = saved.current;
            let mut current = 0;
            let mut selected_survived = false;
            let mut missing = 0;
            for (index, path) in saved.paths.iter().enumerate() {
                match validate_local_audio_path(path) {
                    Ok(path) => {
                        if index == old_current {
                            current = queue.paths.len();
                            selected_survived = true;
                        }
                        queue.paths.push(path);
                    }
                    Err(_) => missing += 1,
                }
            }
            queue.current = current;
            queue.repeat_all = saved.repeat_all;
            resume_position = selected_survived.then_some(saved.position);
            let _ = mpv.command(&["set", "volume", &saved.volume.to_string()]);
            update_queue_snapshot(app, &snapshot, &queue, "stopped");
            update_snapshot(app, &snapshot, |s| {
                s.volume = saved.volume;
                s.position = resume_position.unwrap_or(0.0);
                if missing > 0 {
                    s.last_error = Some(format!("{missing} unavailable saved tracks were omitted"));
                }
            });
        }
        Ok(None) => {}
        _ => update_snapshot(app, &snapshot, |s| {
            s.persistence_error = Some("Saved playback session could not be restored".into())
        }),
    }
    let writer = session_path.ok().map(crate::state::Writer::new);
    let mut last_save = Instant::now();
    let mut last_saved = None;
    let mut telemetry_dirty = true;
    let mut last_telemetry = Instant::now();
    let mut events_drained = false;
    'engine: loop {
        if shutdown_requested.load(Ordering::Relaxed) {
            break;
        }
        let inactive = matches!(
            snapshot.lock().unwrap().state.as_str(),
            "idle" | "stopped" | "paused" | "error"
        );
        // Wait on commands while inactive so UI and SMTC actions wake us
        // immediately, instead of waking the thread twenty times per second.
        // Drain queued mpv events first; active/gapless playback keeps its
        // original event wait. External mpv events still get bounded service.
        let mut pending_command = if inactive && events_drained {
            match receiver.recv_timeout(INACTIVE_EVENT_INTERVAL) {
                Ok(command) => Some(command),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        } else {
            None
        };
        let mut commands_processed = 0;
        while commands_processed < MAX_COMMANDS_PER_TICK {
            let command = match pending_command
                .take()
                .map(Ok)
                .unwrap_or_else(|| receiver.try_recv())
            {
                Ok(command) => command,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break 'engine,
            };
            commands_processed += 1;
            match command {
                EngineCommand::SetQueue {
                    paths,
                    start_index,
                    reply,
                } => {
                    resume_position = None;
                    paused_seek_position = None;
                    consecutive_failures = 0;
                    queue.random_library = None;
                    let result = queue
                        .replace(&mut mpv, paths, start_index)
                        .and_then(|_| mpv.command(&["set", "pause", "no"]));
                    match &result {
                        Ok(()) => {
                            update_queue_snapshot(app, &snapshot, &queue, "loading");
                            update_snapshot(app, &snapshot, |s| s.last_error = None);
                        }
                        Err(error) => {
                            update_queue_snapshot(app, &snapshot, &queue, "error");
                            record_error(app, &snapshot, error.clone());
                        }
                    }
                    let _ = reply.send(result);
                }
                EngineCommand::SetRandomLibrary {
                    paths,
                    enabled,
                    revision,
                    current,
                    reply,
                } => {
                    let result =
                        queue.set_random_library(&mut mpv, paths, enabled, revision, current);
                    let starts_playback = matches!(&result, Ok(true));
                    let result = result.and_then(|starts| {
                        if starts {
                            mpv.command(&["set", "pause", "no"])?;
                        }
                        Ok(())
                    });
                    if starts_playback && result.is_ok() {
                        resume_position = None;
                        paused_seek_position = None;
                        consecutive_failures = 0;
                    }
                    let state = if starts_playback && result.is_ok() {
                        "loading".to_string()
                    } else if result.is_err() && queue.current_entry_id.is_none() {
                        "error".to_string()
                    } else {
                        snapshot.lock().unwrap().state.clone()
                    };
                    update_queue_snapshot(app, &snapshot, &queue, &state);
                    if let Err(error) = &result {
                        record_error(app, &snapshot, error.clone());
                    }
                    let _ = reply.send(result);
                }
                EngineCommand::TogglePause(reply) => {
                    let state = snapshot.lock().unwrap().state.clone();
                    let result = if matches!(state.as_str(), "stopped" | "idle" | "error")
                        && !queue.paths.is_empty()
                    {
                        queue
                            .replace(&mut mpv, queue.paths.clone(), queue.current)
                            .and_then(|_| mpv.command(&["set", "pause", "no"]))
                    } else {
                        mpv.command(&["cycle", "pause"])
                    };
                    let _ = reply.send(result);
                }
                EngineCommand::SetPaused(paused, reply) => {
                    let state = snapshot.lock().unwrap().state.clone();
                    let inactive = matches!(state.as_str(), "stopped" | "idle" | "error");
                    let result = if queue.paths.is_empty() || (paused && inactive) {
                        Ok(())
                    } else if !paused && inactive {
                        consecutive_failures = 0;
                        queue
                            .replace(&mut mpv, queue.paths.clone(), queue.current)
                            .and_then(|_| mpv.command(&["set", "pause", "no"]))
                    } else {
                        mpv.command(&["set", "pause", if paused { "yes" } else { "no" }])
                    };
                    if let Err(error) = &result {
                        record_error(app, &snapshot, error.clone());
                    }
                    let _ = reply.send(result);
                }
                EngineCommand::Next(reply) => {
                    let result = if let Some(next) =
                        next_index(queue.paths.len(), queue.current, queue.repeat_all)
                    {
                        resume_position = None;
                        if queue.lookahead_entry_id.is_some() {
                            mpv.command(&["playlist-next", "weak"])
                        } else if queue.current_entry_id.is_none() {
                            queue
                                .replace(&mut mpv, queue.paths.clone(), next)
                                .and_then(|_| mpv.command(&["set", "pause", "no"]))
                        } else {
                            Err("the mpv lookahead entry is not available".to_string())
                        }
                    } else {
                        Ok(())
                    };
                    if let Err(error) = &result {
                        record_error(app, &snapshot, error.clone());
                    }
                    let _ = reply.send(result);
                }
                EngineCommand::Previous(reply) => {
                    resume_position = None;
                    paused_seek_position = None;
                    consecutive_failures = 0;
                    let result = queue.previous(&mut mpv);
                    match &result {
                        Ok(()) => update_queue_snapshot(app, &snapshot, &queue, "loading"),
                        Err(error) => {
                            update_queue_snapshot(app, &snapshot, &queue, "error");
                            record_error(app, &snapshot, error.clone());
                        }
                    }
                    let _ = reply.send(result);
                }
                EngineCommand::Stop(reply) => {
                    let result = mpv.command(&["stop"]);
                    if result.is_ok() {
                        queue.invalidate_entry_mapping();
                        resume_position = None;
                        paused_seek_position = None;
                        update_snapshot(app, &snapshot, |state| {
                            state.position = 0.0;
                            state.state = "stopped".to_string();
                        });
                    } else {
                        record_error(app, &snapshot, result.clone().unwrap_err());
                    }
                    let _ = reply.send(result);
                }
                EngineCommand::Seek {
                    seconds,
                    revision,
                    current,
                    reply,
                } => {
                    let duration = snapshot.lock().unwrap().duration;
                    let target = seconds.min(duration.max(0.0));
                    let result = seek_track(&mut mpv, &queue, target, duration, revision, current);
                    if let Ok(was_paused) = result {
                        // Audio-only mpv defers time-pos changes during paused seeks.
                        paused_seek_position = was_paused.then_some(target);
                        update_snapshot(app, &snapshot, |s| s.position = target);
                    }
                    let result = result.map(|_| ());
                    let _ = reply.send(result);
                }
                EngineCommand::Volume(value, reply) => {
                    let result = mpv.command(&["set", "volume", &value.to_string()]);
                    if result.is_ok() {
                        update_snapshot(app, &snapshot, |s| s.volume = value);
                    }
                    let _ = reply.send(result);
                }
                EngineCommand::RecyclePath { path, reply } => {
                    let previous_state = snapshot.lock().unwrap().state.clone();
                    let previous_current = queue.current_path();
                    let had_active_entry = queue.current_entry_id.is_some();
                    let result = queue.recycle_path(&mut mpv, &path);
                    let current_changed = previous_current != queue.current_path();
                    if current_changed || (had_active_entry && queue.current_entry_id.is_none()) {
                        resume_position = None;
                        paused_seek_position = None;
                    }
                    let state = if queue.paths.is_empty() || queue.current_entry_id.is_none() {
                        "stopped"
                    } else if queue.awaiting_initial_start {
                        "loading"
                    } else {
                        &previous_state
                    };
                    update_queue_snapshot(app, &snapshot, &queue, state);
                    if had_active_entry && queue.current_entry_id.is_none() {
                        update_snapshot(app, &snapshot, |state| {
                            state.position = 0.0;
                            state.duration = 0.0;
                        });
                    }
                    match result {
                        Ok(warning) => {
                            if let Some(warning) = warning {
                                record_error(app, &snapshot, warning);
                            }
                            let _ = reply.send(Ok(()));
                        }
                        Err(error) => {
                            record_error(app, &snapshot, error.clone());
                            let _ = reply.send(Err(error));
                        }
                    }
                }
                EngineCommand::QueuePage(offset, reply) => {
                    let _ = reply.send(QueuePage {
                        paths: queue
                            .paths
                            .iter()
                            .skip(offset)
                            .take(100)
                            .map(|p| p.to_string_lossy().into_owned())
                            .collect(),
                        offset,
                        total: queue.paths.len(),
                        current: queue.current,
                        revision: queue.generation,
                        repeat_all: queue.repeat_all,
                        random_library: queue.random_library.is_some(),
                    });
                }
                EngineCommand::Edit {
                    action,
                    path,
                    index,
                    revision,
                    current,
                    reply,
                } => {
                    let removing_current = action == "remove" && index == queue.current;
                    let result = queue.edit(&mut mpv, &action, path, index, revision, current);
                    let state = if action == "select" && result.is_ok() {
                        resume_position = None;
                        paused_seek_position = None;
                        "loading".to_string()
                    } else if action == "clear" && result.is_ok() {
                        resume_position = None;
                        paused_seek_position = None;
                        "stopped".to_string()
                    } else if removing_current && result.is_ok() {
                        resume_position = None;
                        paused_seek_position = None;
                        if queue.paths.is_empty() || queue.current_entry_id.is_none() {
                            "stopped".to_string()
                        } else {
                            "loading".to_string()
                        }
                    } else if result.is_err() && queue.current_entry_id.is_none() {
                        paused_seek_position = None;
                        "error".to_string()
                    } else {
                        snapshot.lock().unwrap().state.clone()
                    };
                    update_queue_snapshot(app, &snapshot, &queue, &state);
                    let _ = reply.send(result);
                }
                EngineCommand::Shutdown => break 'engine,
            }
        }

        let timeout = if inactive || commands_processed == MAX_COMMANDS_PER_TICK {
            0.0
        } else {
            0.05
        };
        let event_summary = mpv.wait_event(timeout);
        events_drained = event_summary.event_id == event::NONE;
        match event_summary.event_id {
            event::NONE => {}
            event::PROPERTY_CHANGE => {
                telemetry_dirty = true;
            }
            event::SHUTDOWN => {
                update_snapshot(app, &snapshot, |state| {
                    state.available = false;
                    state.state = "unavailable".to_string();
                    state.last_error = Some("libmpv shut down".to_string());
                });
                break;
            }
            event::START_FILE => match event_summary.start_file {
                Some(start_file) => {
                    match queue.on_start_file(&mut mpv, start_file.playlist_entry_id) {
                        Ok(StartFileDisposition::Ignored) => {}
                        Ok(disposition) => {
                            paused_seek_position = None;
                            update_snapshot(app, &snapshot, |state| {
                                if disposition == StartFileDisposition::Transitioned {
                                    state.transition_count += 1;
                                }
                                state.position = 0.0;
                                state.duration = 0.0;
                                state.audio_quality = AudioQuality::default();
                                state.queue_revision = queue.generation;
                                state.repeat_all = queue.repeat_all;
                                state.state = "loading".to_string();
                                state.queue_len = queue.paths.len();
                                state.current_index =
                                    (!queue.paths.is_empty()).then_some(queue.current);
                                state.current_path = queue.current_path();
                            });
                        }
                        Err(error) => {
                            paused_seek_position = None;
                            resume_position = None;
                            update_queue_snapshot(app, &snapshot, &queue, "error");
                            update_snapshot(app, &snapshot, |state| {
                                state.position = 0.0;
                                state.duration = 0.0;
                                state.last_error = Some(error);
                            });
                        }
                    }
                }
                None => record_error(
                    app,
                    &snapshot,
                    "libmpv START_FILE did not include a playlist entry id".to_string(),
                ),
            },
            event::FILE_LOADED | event::PLAYBACK_RESTART | event::UNPAUSE => {
                match mpv.current_playlist_entry_id() {
                    Ok(id) if id.is_some() && id == queue.current_entry_id => {
                        if event_summary.event_id == event::FILE_LOADED {
                            update_snapshot(app, &snapshot, |state| state.last_error = None);
                            if let Some(position) = resume_position.take() {
                                let _ =
                                    mpv.command(&["seek", &position.to_string(), "absolute+exact"]);
                            }
                        }
                        let paused =
                            mpv.property_string("pause").ok().flatten().as_deref() == Some("yes");
                        update_queue_snapshot(
                            app,
                            &snapshot,
                            &queue,
                            if paused { "paused" } else { "playing" },
                        );
                    }
                    Ok(_) => {
                        // Ignore a late state event from a queue generation
                        // that has already been replaced.
                    }
                    Err(error) => record_error(app, &snapshot, error),
                }
            }
            event::END_FILE => {
                if let Some(end_file) = event_summary.end_file {
                    if queue.contains_entry(end_file.playlist_entry_id) {
                        if end_file.reason == event::END_FILE_REASON_ERROR || end_file.error < 0 {
                            consecutive_failures = consecutive_failures.saturating_add(1);
                        } else if end_file.reason == event::END_FILE_REASON_EOF {
                            consecutive_failures = 0;
                        }
                    }
                    let failure_limit = if let Some(random) = &queue.random_library {
                        random.candidates.len().clamp(1, 32)
                    } else if queue.repeat_all {
                        queue.paths.len().max(1)
                    } else {
                        usize::MAX
                    };
                    if consecutive_failures >= failure_limit {
                        let _ = mpv.command(&["stop"]);
                        queue.invalidate_entry_mapping();
                        update_queue_snapshot(app, &snapshot, &queue, "error");
                        record_error(
                            app,
                            &snapshot,
                            "No queued tracks could be played; playback stopped".into(),
                        );
                        consecutive_failures = 0;
                    } else {
                        handle_end_file(app, &snapshot, &mpv, &mut queue, end_file);
                    }
                } else {
                    record_error(
                        app,
                        &snapshot,
                        "libmpv END_FILE did not include playback details".to_string(),
                    );
                }
            }
            event::PAUSE => update_queue_snapshot(app, &snapshot, &queue, "paused"),
            event::AUDIO_RECONFIG => update_snapshot(app, &snapshot, |state| {
                state.audio_reconfig_count += 1;
            }),
            event::QUEUE_OVERFLOW => record_error(
                app,
                &snapshot,
                "libmpv event queue overflowed; playback state may be stale".to_string(),
            ),
            _ => {}
        }
        if telemetry_dirty && last_telemetry.elapsed() >= Duration::from_millis(250) {
            telemetry_dirty = false;
            last_telemetry = Instant::now();
            let position = number_property(&mut mpv, "time-pos");
            let duration = number_property(&mut mpv, "duration");
            let volume = number_property(&mut mpv, "volume");
            let paused = mpv.property_string("pause").ok().flatten().as_deref() == Some("yes");
            let quality = audio_quality(&mut mpv);
            let position = if paused {
                paused_seek_position.or(position)
            } else {
                paused_seek_position = None;
                position
            };
            update_snapshot(app, &snapshot, |s| {
                if queue.current_entry_id.is_some() {
                    s.audio_quality = quality;
                    if let Some(v) = position {
                        s.position = v;
                    }
                    if let Some(v) = duration {
                        s.duration = v;
                    }
                }
                if let Some(v) = volume {
                    s.volume = v;
                }
                if matches!(s.state.as_str(), "playing" | "paused") {
                    s.state = if paused { "paused" } else { "playing" }.into();
                }
            });
        }
        if last_save.elapsed() >= Duration::from_secs(5) {
            let previous_error = snapshot.lock().unwrap().persistence_error.clone();
            save_session(&writer, &queue, &snapshot, &mut last_saved);
            let changed = snapshot.lock().unwrap().persistence_error != previous_error;
            if changed {
                update_snapshot(app, &snapshot, |_| {});
            }
            last_save = Instant::now();
        }
    }
    save_session(&writer, &queue, &snapshot, &mut last_saved);
}
fn seek_track(
    mpv: &mut Mpv,
    queue: &LookaheadQueue,
    target: f64,
    duration: f64,
    revision: u64,
    current: usize,
) -> Result<bool, String> {
    if revision != queue.generation || current != queue.current {
        return Err("Track changed; please seek again".into());
    }
    if queue.current_entry_id.is_none() || duration <= 0.0 {
        return Err("Wait for the track to load before seeking".into());
    }
    let was_paused = mpv.property_string("pause")?.as_deref() == Some("yes");
    mpv.command(&["set", "pause", "yes"])?;
    let result = (|| {
        if queue.awaiting_initial_start
            || mpv.current_playlist_entry_id()? != queue.current_entry_id
        {
            return Err("Track is transitioning; please seek again".into());
        }
        mpv.command(&["seek", &target.to_string(), "absolute+exact"])
    })();
    let resume = mpv.command(&["set", "pause", if was_paused { "yes" } else { "no" }]);
    result?;
    resume?;
    Ok(was_paused)
}

fn number_property(mpv: &mut Mpv, name: &str) -> Option<f64> {
    mpv.property_string(name)
        .ok()
        .flatten()?
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v >= 0.0)
}

fn audio_quality(mpv: &mut Mpv) -> AudioQuality {
    fn text(mpv: &mut Mpv, name: &str) -> Option<String> {
        mpv.property_string(name)
            .ok()
            .flatten()
            .filter(|value| !value.is_empty())
    }
    AudioQuality {
        codec: text(mpv, "current-tracks/audio/codec"),
        sample_rate: number_property(mpv, "audio-params/samplerate").map(|n| n as u32),
        channels: number_property(mpv, "audio-params/channel-count").map(|n| n as u32),
        sample_format: text(mpv, "audio-params/format"),
        bitrate: number_property(mpv, "audio-bitrate")
            .filter(|n| *n > 0.0)
            .map(|n| n as u64),
        output_sample_rate: number_property(mpv, "audio-out-params/samplerate").map(|n| n as u32),
        output_channels: number_property(mpv, "audio-out-params/channel-count").map(|n| n as u32),
        output_format: text(mpv, "audio-out-params/format"),
    }
}
fn save_session(
    writer: &Option<crate::state::Writer>,
    queue: &LookaheadQueue,
    snapshot: &Arc<Mutex<PlaybackSnapshot>>,
    last_saved: &mut Option<Session>,
) {
    let mut state = snapshot.lock().unwrap();
    if let Some(writer) = writer {
        // Compare borrowed paths before materializing a full session. An idle
        // queue can contain thousands of paths and does not need a fresh copy
        // on every persistence tick. Preserve retries after writer failures.
        if last_saved.as_ref().is_some_and(|saved| {
            saved.current == queue.current
                && saved.position == state.position
                && saved.volume == state.volume
                && saved.repeat_all == queue.repeat_all
                && saved.paths.len() == queue.paths.len()
                && saved
                    .paths
                    .iter()
                    .zip(&queue.paths)
                    .all(|(saved, path)| saved == path.to_string_lossy().as_ref())
        }) && writer.error().is_none()
        {
            state.persistence_error = None;
            return;
        }
        let session = Session {
            version: 1,
            paths: queue
                .paths
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
            current: queue.current,
            position: state.position,
            volume: state.volume,
            repeat_all: queue.repeat_all,
        };
        if last_saved.as_ref() != Some(&session) || writer.error().is_some() {
            state.persistence_error = writer
                .submit(&session, SESSION_LIMIT)
                .err()
                .or_else(|| writer.error());
            if state.persistence_error.is_none() {
                *last_saved = Some(session);
            }
        } else {
            state.persistence_error = writer.error();
        }
    } else {
        state.persistence_error = Some("Playback state cannot be saved".into());
    }
}

fn handle_end_file(
    app: &StateSink,
    snapshot: &Arc<Mutex<PlaybackSnapshot>>,
    mpv: &Mpv,
    queue: &mut LookaheadQueue,
    end_file: MpvEndFileSummary,
) {
    if !queue.contains_entry(end_file.playlist_entry_id) {
        // Events from a playlist that was replaced by a newer generation are
        // deliberately ignored. Entry IDs are unique within the mpv core.
        return;
    }

    if end_file.reason == event::END_FILE_REASON_ERROR || end_file.error < 0 {
        let error = format!(
            "track playback failed: {} ({})",
            mpv.error_description(end_file.error),
            end_file.error
        );
        let has_next = queue.lookahead_entry_id.is_some();
        update_snapshot(app, snapshot, |state| {
            state.last_error = Some(error);
            state.state = if has_next { "loading" } else { "error" }.to_string();
        });
        return;
    }

    if end_file.reason == event::END_FILE_REASON_EOF && queue.lookahead_entry_id.is_none() {
        update_queue_snapshot(app, snapshot, queue, "idle");
        queue.invalidate_entry_mapping();
    }
}

fn initialize_player() -> Result<Mpv, String> {
    initialize_player_with(&[])
}
fn initialize_player_with(options: &[(&str, &str)]) -> Result<Mpv, String> {
    let mut mpv = Mpv::create(None)?;
    for &(name, value) in LOCAL_AUDIO_OPTIONS {
        mpv.set_option(name, value)?;
    }
    for &(name, value) in options {
        mpv.set_option(name, value)?;
    }
    mpv.initialize()?;
    for (id, name) in [
        "time-pos",
        "duration",
        "pause",
        "volume",
        "audio-params",
        "audio-out-params",
        "current-tracks/audio/codec",
    ]
    .iter()
    .enumerate()
    {
        mpv.observe(name, id as u64 + 1)?;
    }
    Ok(mpv)
}

fn update_queue_snapshot(
    app: &StateSink,
    snapshot: &Arc<Mutex<PlaybackSnapshot>>,
    queue: &LookaheadQueue,
    playback_state: &str,
) {
    update_snapshot(app, snapshot, |state| {
        state.state = playback_state.to_string();
        state.queue_revision = queue.generation;
        state.repeat_all = queue.repeat_all;
        state.queue_len = queue.paths.len();
        state.current_index = (!queue.paths.is_empty()).then_some(queue.current);
        state.current_path = queue.current_path();
        if queue.paths.is_empty() || playback_state == "loading" {
            state.position = 0.0;
            state.duration = 0.0;
            state.audio_quality = AudioQuality::default();
        }
    });
}

fn record_error(app: &StateSink, snapshot: &Arc<Mutex<PlaybackSnapshot>>, error: String) {
    update_snapshot(app, snapshot, |state| {
        state.last_error = Some(error);
    });
}

fn update_snapshot(
    app: &StateSink,
    snapshot: &Arc<Mutex<PlaybackSnapshot>>,
    update: impl FnOnce(&mut PlaybackSnapshot),
) {
    let value = {
        let mut state = snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        update(&mut state);
        state.clone()
    };
    app(value);
}

pub(crate) fn validate_local_audio_path(value: &str) -> Result<PathBuf, String> {
    if value.is_empty() {
        return Err("audio path cannot be empty".to_string());
    }
    if value.len() > 32_767 {
        return Err("audio path exceeds the Windows path limit".to_string());
    }
    let path = Path::new(value);
    if is_unc_path(path) {
        return Err("UNC and network audio paths are not supported".to_string());
    }
    if !path.is_absolute() {
        return Err(format!("audio path must be absolute: {value}"));
    }
    if is_playlist_path(path) {
        return Err(format!(
            "playlist files are not valid audio tracks: {value}"
        ));
    }
    if is_within_root(path, Path::new(DOWNLOADS_ROOT)) {
        return Err("audio paths under D:\\Music\\.downloads are not allowed".to_string());
    }
    if !is_within_root(path, Path::new(DEFAULT_LIBRARY_ROOT))
        && !is_within_root(path, Path::new(DIAGNOSTIC_ROOT))
    {
        return Err(format!(
            "audio path must be under {DEFAULT_LIBRARY_ROOT} or {DIAGNOSTIC_ROOT}: {value}"
        ));
    }
    if !path.is_file() {
        return Err(format!("audio file does not exist: {value}"));
    }
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("failed to resolve {value}: {error}"))?;
    if is_unc_path(&canonical)
        || is_within_root(&canonical, Path::new(DOWNLOADS_ROOT))
        || (!is_within_root(&canonical, Path::new(DEFAULT_LIBRARY_ROOT))
            && !is_within_root(&canonical, Path::new(DIAGNOSTIC_ROOT)))
    {
        return Err(format!(
            "audio path resolves outside an allowed local root: {value}"
        ));
    }
    Ok(canonical)
}

fn is_unc_path(path: &Path) -> bool {
    // canonicalize() returns a verbatim disk prefix on Windows. It is local,
    // unlike UNC, verbatim UNC, and device-namespace paths.
    if matches!(
        path.components().next(),
        Some(std::path::Component::Prefix(prefix))
            if matches!(prefix.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_))
    ) {
        return false;
    }
    let value = path.to_string_lossy();
    value.starts_with(r"\\") || value.starts_with("//")
}

fn normalized_windows_path(path: &Path) -> String {
    let mut value = path.to_string_lossy().replace('/', "\\");
    if let Some(stripped) = value.strip_prefix("\\\\?\\") {
        value = stripped.to_string();
    }
    value.trim_end_matches('\\').to_ascii_lowercase()
}

fn is_within_root(path: &Path, root: &Path) -> bool {
    let path = normalized_windows_path(path);
    let root = normalized_windows_path(root);
    path == root || path.starts_with(&(root + "\\"))
}

fn is_playlist_path(path: &Path) -> bool {
    let extension = path
        .extension()
        .map(|value| value.to_string_lossy().to_ascii_lowercase());
    matches!(
        extension.as_deref(),
        Some("m3u") | Some("m3u8") | Some("pls") | Some("xspf") | Some("cue") | Some("asx")
    )
}

fn path_text(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("path is not valid Unicode: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        env, fs,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn random_picker_skips_missing_files_and_avoids_immediate_repeat() {
        let root = env::temp_dir().join(format!(
            "musicplayer-random-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let a = root.join("a.wav");
        let b = root.join("b.wav");
        let missing = root.join("missing.wav");
        fs::write(&a, []).unwrap();
        fs::write(&b, []).unwrap();
        let mut random = RandomLibrary {
            candidates: vec![missing, a.clone(), b.clone()],
            hasher: std::collections::hash_map::RandomState::new(),
            draw: 0,
        };
        let mut previous = random.choose(None).unwrap();
        for _ in 0..32 {
            let next = random.choose(Some(&previous)).unwrap();
            assert_ne!(next, previous);
            previous = next;
        }
        fs::remove_file(&b).unwrap();
        assert_eq!(random.choose(Some(&a)), Some(a.clone()));
        fs::remove_file(&a).unwrap();
        assert!(random.choose(None).is_none());
        fs::remove_dir(&root).unwrap();
    }

    #[test]
    fn random_history_remains_bounded_after_transitions() {
        let mut queue = LookaheadQueue::new();
        queue.random_library = Some(RandomLibrary {
            candidates: Vec::new(),
            hasher: std::collections::hash_map::RandomState::new(),
            draw: 0,
        });
        queue.paths = (0..RANDOM_HISTORY_LIMIT + 8)
            .map(|index| PathBuf::from(format!("track-{index}")))
            .collect();
        queue.current = RANDOM_HISTORY_LIMIT;
        let selected = queue.paths[queue.current].clone();
        queue.trim_random_history();
        assert_eq!(queue.current, 0);
        assert_eq!(queue.paths.first(), Some(&selected));
        assert_eq!(queue.paths.len(), 8);
    }

    #[test]
    fn rejects_relative_and_missing_paths() {
        assert!(validate_local_audio_path("relative.wav").is_err());
        assert!(validate_local_audio_path("D:\\definitely-missing-musicplayer-file.wav").is_err());
        assert!(validate_local_audio_path(r"\\server\share\track.wav").is_err());
        assert!(validate_local_audio_path(r"D:\Music\.downloads\job\track.m4a").is_err());
        assert!(validate_local_audio_path(r"C:\Windows\Media\track.wav").is_err());
        assert!(validate_local_audio_path(r"D:\Music\playlist.m3u8").is_err());
    }

    #[test]
    fn canonical_local_files_are_accepted_and_network_prefixes_rejected() {
        assert!(!is_unc_path(Path::new(r"\\?\D:\Music\track.wav")));
        for path in [
            r"\\server\share\track.wav",
            r"\\?\UNC\server\share\track.wav",
            r"\\.\PhysicalDrive0",
        ] {
            assert!(is_unc_path(Path::new(path)));
        }
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        fs::create_dir_all(DIAGNOSTIC_ROOT).unwrap();
        let fixture = Path::new(DIAGNOSTIC_ROOT).join(format!(
            "path-validation-{}-{nonce}.wav",
            std::process::id()
        ));
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&fixture)
            .unwrap();
        drop(file);
        let actual = validate_local_audio_path(fixture.to_str().unwrap());
        let expected = fixture.canonicalize().unwrap();
        fs::remove_file(&fixture).unwrap();
        assert_eq!(actual.unwrap(), expected);
    }

    #[test]
    fn root_matching_is_component_aware_and_case_insensitive() {
        assert!(is_within_root(
            Path::new(r"d:\music\Artist\track.flac"),
            Path::new(DEFAULT_LIBRARY_ROOT)
        ));
        assert!(!is_within_root(
            Path::new(r"D:\music2\track.flac"),
            Path::new(DEFAULT_LIBRARY_ROOT)
        ));
        assert!(is_within_root(
            Path::new(r"D:\Projects\MusicPlayer\gapless-assets\fixture.wav"),
            Path::new(DIAGNOSTIC_ROOT)
        ));
    }

    #[test]
    fn removal_plan_reselects_coherently_for_past_current_and_final_entries() {
        let paths = ["a", "b", "c", "d"].map(PathBuf::from).to_vec();

        let past = plan_removal(&paths, 2, |index, _| index == 0);
        assert_eq!(past.paths, ["b", "c", "d"].map(PathBuf::from));
        assert_eq!(past.current, 1);
        assert!(!past.selected_removed);

        let current = plan_removal(&paths, 2, |index, _| index == 2);
        assert_eq!(current.paths, ["a", "b", "d"].map(PathBuf::from));
        assert_eq!(current.current, 2, "the following track becomes selected");
        assert!(current.selected_removed);

        let final_entry = plan_removal(&paths, 3, |index, _| index == 3);
        assert_eq!(final_entry.paths, ["a", "b", "c"].map(PathBuf::from));
        assert_eq!(
            final_entry.current, 2,
            "the preceding track becomes selected"
        );

        let singleton = plan_removal(&paths[..1], 0, |_, _| true);
        assert!(singleton.paths.is_empty());
        assert_eq!(singleton.current, 0);
    }

    #[test]
    fn path_removal_matches_canonical_prefix_slashes_and_case_and_removes_duplicates() {
        let target = Path::new(r"D:\Music\Artist\Track.flac");
        let paths = vec![
            PathBuf::from(r"\\?\D:\MUSIC\artist\track.FLAC"),
            PathBuf::from(r"D:\Music\Artist\other.flac"),
            PathBuf::from("d:/music/artist/TRACK.flac"),
        ];
        let plan = plan_removal(&paths, 2, |_, path| same_windows_path(path, target));
        assert_eq!(
            plan.paths,
            vec![PathBuf::from(r"D:\Music\Artist\other.flac")]
        );
        assert_eq!(plan.removed, 2);
        assert_eq!(plan.current, 0);
        assert!(plan.selected_removed);
    }

    fn test_player() -> Result<Mpv, String> {
        initialize_player_with(&[("ao", "null"), ("volume", "0"), ("speed", "0.25")])
    }

    #[test]
    #[ignore = "requires local libmpv and generated WAV fixtures"]
    fn real_engine_random_library_preserves_manual_queue_refills_and_stops() {
        let a = Path::new(DIAGNOSTIC_ROOT).join("gapless-part-a.wav");
        let b = Path::new(DIAGNOSTIC_ROOT).join("gapless-part-b.wav");
        assert!(a.is_file() && b.is_file());
        let session = env::temp_dir().join(format!(
            "musicplayer-random-session-{}-{}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let manager =
            PlaybackManager::start_with(Box::new(|_| {}), test_player, Ok(session.clone()));
        wait_for_state(&manager, |state| state.available);
        manager
            .set_queue(vec![a.display().to_string(), b.display().to_string()], 0)
            .unwrap();
        wait_for_state(&manager, |state| {
            state.state == "playing" && state.current_index == Some(0)
        });
        let state = manager.snapshot();
        manager
            .set_random_library(
                vec![a.display().to_string(), b.display().to_string()],
                true,
                state.queue_revision,
                state.current_index.unwrap(),
            )
            .unwrap();
        let page = manager.queue_page(0).unwrap();
        assert!(page.random_library);
        assert_eq!(page.total, 2, "manual upcoming B stays in place");
        manager.next().unwrap();
        wait_for_state(&manager, |state| state.current_index == Some(1));
        let page = manager.queue_page(0).unwrap();
        assert_eq!(page.total, 3, "random lookahead is appended after B");
        assert!(same_windows_path(Path::new(&page.paths[2]), &a));
        manager.next().unwrap();
        wait_for_state(&manager, |state| state.current_index == Some(2));
        assert_eq!(manager.queue_page(0).unwrap().total, 4);
        manager.stop().unwrap();
        wait_for_state(&manager, |state| state.state == "stopped");
        thread::sleep(Duration::from_millis(350));
        assert_eq!(manager.snapshot().state, "stopped");
        let state = manager.snapshot();
        manager
            .set_random_library(
                Vec::new(),
                false,
                state.queue_revision,
                state.current_index.unwrap(),
            )
            .unwrap();
        assert!(!manager.queue_page(0).unwrap().random_library);
        manager.shutdown();
        let _ = fs::remove_file(session);
    }

    #[test]
    #[ignore = "requires local libmpv and generated WAV fixtures"]
    fn real_libmpv_random_library_continues_past_history_bound_on_natural_eof() {
        let a = Path::new(DIAGNOSTIC_ROOT).join("gapless-part-a.wav");
        let b = Path::new(DIAGNOSTIC_ROOT).join("gapless-part-b.wav");
        assert!(a.is_file() && b.is_file());
        let mut mpv = Mpv::create(None).unwrap();
        for &(name, value) in LOCAL_AUDIO_OPTIONS {
            mpv.set_option(name, value).unwrap();
        }
        mpv.set_option("ao", "null").unwrap();
        mpv.set_option("ao-null-untimed", "yes").unwrap();
        mpv.set_option("pause", "yes").unwrap();
        mpv.initialize().unwrap();

        let mut queue = LookaheadQueue::new();
        assert!(queue
            .set_random_library(&mut mpv, vec![a.clone(), b.clone()], true, 0, 0)
            .unwrap());
        assert!(queue.random_library.is_some());
        assert!(!queue.repeat_all);
        assert_eq!(mpv.playlist_entry_ids().unwrap().len(), 2);
        mpv.command(&["set", "pause", "no"]).unwrap();

        let deadline = Instant::now() + Duration::from_secs(90);
        let mut started = 0usize;
        let mut prior: Option<PathBuf> = None;
        while started < RANDOM_HISTORY_LIMIT + 12 {
            assert!(
                Instant::now() < deadline,
                "random playback stalled at {started} tracks"
            );
            let summary = mpv.wait_event(0.05);
            if summary.event_id == event::QUEUE_OVERFLOW {
                panic!("libmpv event queue overflow during random playback");
            }
            if let Some(start) = summary.start_file {
                let disposition = queue
                    .on_start_file(&mut mpv, start.playlist_entry_id)
                    .expect("advance random lookahead on natural EOF");
                if disposition != StartFileDisposition::Ignored {
                    let current = queue.paths[queue.current].clone();
                    if let Some(prior) = &prior {
                        assert!(!same_windows_path(prior, &current));
                    }
                    prior = Some(current);
                    started += 1;
                    assert!(queue.paths.len() <= RANDOM_HISTORY_LIMIT + 1);
                    assert_eq!(mpv.playlist_entry_ids().unwrap().len(), 2);
                }
            }
        }

        // Control changes preserve the current song and the already queued
        // lookahead; clear explicitly stops all future generation.
        let revision = queue.generation;
        let current = queue.current;
        queue
            .set_random_library(&mut mpv, Vec::new(), false, revision, current)
            .unwrap();
        assert!(queue.random_library.is_none());
        queue
            .edit(&mut mpv, "clear", None, 0, queue.generation, queue.current)
            .unwrap();
        assert!(queue.paths.is_empty());
        assert!(queue.current_entry_id.is_none());
    }

    fn wait_for_current_start(mpv: &mut Mpv, queue: &mut LookaheadQueue) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while queue.awaiting_initial_start {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for START_FILE"
            );
            let event = mpv.wait_event(0.05);
            if let Some(start) = event.start_file {
                queue
                    .on_start_file(mpv, start.playlist_entry_id)
                    .expect("map current START_FILE");
            }
        }
    }

    #[test]
    #[ignore = "requires local libmpv and generated WAV fixtures"]
    fn real_libmpv_removes_past_current_last_and_singleton_and_preserves_recycle_failure() {
        let a = Path::new(DIAGNOSTIC_ROOT).join("gapless-part-a.wav");
        let b = Path::new(DIAGNOSTIC_ROOT).join("gapless-part-b.wav");
        assert!(a.is_file() && b.is_file());
        let mut mpv = Mpv::create(None).unwrap();
        for &(name, value) in LOCAL_AUDIO_OPTIONS {
            mpv.set_option(name, value).unwrap();
        }
        mpv.set_option("ao", "null").unwrap();
        mpv.set_option("pause", "yes").unwrap();
        mpv.initialize().unwrap();

        let mut queue = LookaheadQueue::new();
        queue
            .replace(
                &mut mpv,
                vec![a.clone(), b.clone(), a.clone(), b.clone()],
                2,
            )
            .unwrap();
        wait_for_current_start(&mut mpv, &mut queue);
        let current_entry = queue.current_entry_id;

        queue.remove_at(&mut mpv, 0).unwrap();
        assert_eq!(queue.paths, vec![b.clone(), a.clone(), b.clone()]);
        assert_eq!(queue.current, 1);
        assert_eq!(queue.current_entry_id, current_entry);

        queue.remove_at(&mut mpv, 1).unwrap();
        wait_for_current_start(&mut mpv, &mut queue);
        assert_eq!(queue.paths, vec![b.clone(), b.clone()]);
        assert_eq!(queue.current, 1, "the following entry replaces current");

        queue.remove_at(&mut mpv, 1).unwrap();
        wait_for_current_start(&mut mpv, &mut queue);
        assert_eq!(queue.paths, vec![b.clone()]);
        assert_eq!(queue.current, 0, "the preceding entry replaces the last");

        queue.remove_at(&mut mpv, 0).unwrap();
        assert!(queue.paths.is_empty());
        assert_eq!(queue.current, 0);
        assert!(queue.current_entry_id.is_none());

        queue
            .replace(&mut mpv, vec![a.clone(), b.clone(), a.clone()], 0)
            .unwrap();
        wait_for_current_start(&mut mpv, &mut queue);
        let before = queue.paths.clone();
        let result = queue.recycle_path(&mut mpv, &a);
        assert!(
            result.is_err(),
            "diagnostic files are outside the library root"
        );
        assert_eq!(
            queue.paths, before,
            "failed recycle must retain every match"
        );
        assert!(
            queue.current_entry_id.is_none(),
            "failure may stop playback"
        );
        assert!(a.is_file(), "the rejected fixture must remain untouched");
    }

    fn wait_for_state(
        manager: &PlaybackManager,
        condition: impl Fn(&PlaybackSnapshot) -> bool,
    ) -> PlaybackSnapshot {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = manager.snapshot();
            if condition(&snapshot) {
                return snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "Timed out waiting for engine state: {snapshot:?}"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
    #[test]
    #[ignore = "requires local libmpv and generated WAV fixtures"]
    fn real_engine_restores_session_without_autoplay_and_flushes_on_close() {
        let fixture = Path::new(DIAGNOSTIC_ROOT).join(format!(
            "session-lifecycle-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&fixture).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(fixture.clone());
        let session_path = fixture.join("session.json");
        let paths = ["gapless-part-a.wav", "gapless-part-b.wav"].map(|name| {
            Path::new(DIAGNOSTIC_ROOT)
                .join(name)
                .to_string_lossy()
                .into_owned()
        });
        let manager =
            PlaybackManager::start_with(Box::new(|_| {}), test_player, Ok(session_path.clone()));
        wait_for_state(&manager, |s| s.available);
        manager.set_queue(paths.to_vec(), 0).unwrap();
        wait_for_state(&manager, |s| s.state == "playing" && s.duration > 0.0);
        manager.toggle_pause().unwrap();
        wait_for_state(&manager, |s| s.state == "paused");
        manager
            .seek(0.12, manager.snapshot().queue_revision, 0)
            .unwrap();
        assert!(manager
            .seek(0.2, manager.snapshot().queue_revision, 1)
            .is_err());
        wait_for_state(&manager, |s| s.state == "paused" && s.position >= 0.11);
        manager.volume(37.0).unwrap();
        manager
            .edit(
                "repeat".into(),
                None,
                1,
                manager.snapshot().queue_revision,
                0,
            )
            .unwrap();
        manager.shutdown();
        let saved = crate::state::load::<Session>(&session_path, SESSION_LIMIT)
            .unwrap()
            .unwrap();
        assert!(saved.valid());
        assert_eq!(saved.paths.len(), 2);
        assert_eq!(saved.current, 0);
        assert_eq!(saved.volume, 37.0);
        assert!(saved.repeat_all);
        assert!(saved.position >= 0.11);
        drop(manager);

        let restored =
            PlaybackManager::start_with(Box::new(|_| {}), test_player, Ok(session_path.clone()));
        let snapshot = wait_for_state(&restored, |s| s.available && s.queue_len == 2);
        assert_eq!(snapshot.state, "stopped");
        assert_eq!(snapshot.current_index, Some(0));
        assert_eq!(snapshot.volume, 37.0);
        assert_eq!(snapshot.position, saved.position);
        assert!(restored.queue_page(0).unwrap().repeat_all);
        thread::sleep(Duration::from_millis(350));
        assert_eq!(
            restored.snapshot().state,
            "stopped",
            "Restore must not start audio"
        );
        restored.toggle_pause().unwrap();
        let resumed = wait_for_state(&restored, |s| s.state == "playing" && s.duration > 0.0);
        assert!(
            resumed.position >= saved.position,
            "The first decoded telemetry must reflect the saved seek target"
        );
        restored.stop().unwrap();
        wait_for_state(&restored, |s| s.state == "stopped");
        restored.toggle_pause().unwrap();
        wait_for_state(&restored, |s| s.state == "playing");
        restored.shutdown();
    }

    #[test]
    #[ignore = "requires local libmpv and generated WAV fixtures"]
    fn real_libmpv_queue_edit_failure_stops_and_retains_logical_edit() {
        let a = Path::new(DIAGNOSTIC_ROOT).join("gapless-part-a.wav");
        let b = Path::new(DIAGNOSTIC_ROOT).join("gapless-part-b.wav");
        assert!(a.is_file() && b.is_file());
        let mut mpv = Mpv::create(None).unwrap();
        for &(name, value) in LOCAL_AUDIO_OPTIONS {
            mpv.set_option(name, value).unwrap();
        }
        mpv.set_option("ao", "null").unwrap();
        mpv.set_option("pause", "yes").unwrap();
        mpv.initialize().unwrap();
        let mut queue = LookaheadQueue::new();
        queue
            .replace(&mut mpv, vec![a.clone(), b.clone()], 0)
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut loaded = false;
        while Instant::now() < deadline && !loaded {
            let event = mpv.wait_event(0.05);
            if let Some(start) = event.start_file {
                queue
                    .on_start_file(&mut mpv, start.playlist_entry_id)
                    .unwrap();
            }
            loaded = event.event_id == event::FILE_LOADED;
        }
        assert!(loaded);

        // An interior NUL reaches mpv only after playlist-clear, making the
        // replacement step fail after the logical edit has been staged.
        let invalid = PathBuf::from("invalid\0track.wav");
        let result = queue.edit(
            &mut mpv,
            "next",
            Some(invalid.clone()),
            0,
            queue.generation,
            queue.current,
        );
        assert!(result.is_err());
        assert_eq!(queue.paths, vec![a, invalid, b]);
        assert_eq!(queue.current_entry_id, None);
        assert_eq!(queue.lookahead_entry_id, None);
        assert!(!queue.awaiting_initial_start);
    }

    #[test]
    #[ignore = "requires local libmpv and generated WAV fixtures"]
    fn real_libmpv_transition_failure_stops_and_invalidates_mapping() {
        let a = Path::new(DIAGNOSTIC_ROOT).join("gapless-part-a.wav");
        let b = Path::new(DIAGNOSTIC_ROOT).join("gapless-part-b.wav");
        let c = Path::new(DIAGNOSTIC_ROOT).join("gapless-part-a.wav");
        assert!(a.is_file() && b.is_file() && c.is_file());
        let mut mpv = Mpv::create(None).unwrap();
        for &(name, value) in LOCAL_AUDIO_OPTIONS {
            mpv.set_option(name, value).unwrap();
        }
        mpv.set_option("ao", "null").unwrap();
        mpv.set_option("pause", "yes").unwrap();
        mpv.initialize().unwrap();
        let mut queue = LookaheadQueue::new();
        queue
            .replace(&mut mpv, vec![a.clone(), b.clone(), c.clone()], 0)
            .unwrap();
        let ids = mpv.playlist_entry_ids().unwrap();
        assert_eq!(ids.len(), 2);

        // Leave only the lookahead in mpv. The transition then removes it and
        // appends C, but mapping refresh observes only one entry instead of
        // the expected current-plus-lookahead pair.
        mpv.command(&["playlist-remove", "0"]).unwrap();
        let result = queue.on_start_file(&mut mpv, ids[1]);
        assert!(result.is_err());
        assert_eq!(queue.current, 1);
        assert_eq!(queue.paths, vec![a, b, c]);
        assert_eq!(queue.current_entry_id, None);
        assert_eq!(queue.lookahead_entry_id, None);
        assert!(!queue.awaiting_initial_start);
    }

    #[test]
    #[ignore = "requires interactive Windows media services and local libmpv"]
    fn windows_media_session_routes_transport_commands_to_real_engine() {
        use windows::{
            core::w,
            Media::Control::GlobalSystemMediaTransportControlsSessionManager,
            Win32::{
                System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED},
                UI::WindowsAndMessaging::{
                    CreateWindowExW, DestroyWindow, DispatchMessageW, PeekMessageW,
                    TranslateMessage, MSG, PM_REMOVE, WINDOW_EX_STYLE, WS_OVERLAPPEDWINDOW,
                },
            },
        };
        unsafe {
            RoInitialize(RO_INIT_MULTITHREADED).unwrap();
        }
        struct Apartment;
        impl Drop for Apartment {
            fn drop(&mut self) {
                unsafe {
                    RoUninitialize();
                }
            }
        }
        let _apartment = Apartment;
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("MusicPlayer media integration test"),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                100,
                100,
                None,
                None,
                None,
                None,
            )
            .unwrap()
        };
        struct Window(windows::Win32::Foundation::HWND);
        impl Drop for Window {
            fn drop(&mut self) {
                unsafe {
                    let _ = DestroyWindow(self.0);
                }
            }
        }
        let _window = Window(hwnd);
        fn pump() {
            unsafe {
                let mut message = MSG::default();
                while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            thread::sleep(Duration::from_millis(10));
        }
        let fixture = Path::new(DIAGNOSTIC_ROOT).join(format!(
            "media-buttons-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&fixture).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(fixture.clone());
        let title = format!(
            "media-button-test-{}",
            fixture.file_name().unwrap().to_string_lossy()
        );
        let a = fixture.join(format!("{title}.wav"));
        let b = fixture.join("second.wav");
        fs::copy(Path::new(DIAGNOSTIC_ROOT).join("gapless-part-a.wav"), &a).unwrap();
        fs::copy(Path::new(DIAGNOSTIC_ROOT).join("gapless-part-b.wav"), &b).unwrap();
        let link: Arc<Mutex<Option<Arc<crate::media_controls::MediaControls>>>> =
            Arc::new(Mutex::new(None));
        let listener = link.clone();
        let manager = PlaybackManager::start_with(
            Box::new(move |snapshot| {
                if let Some(controls) = listener.lock().unwrap().as_ref() {
                    controls.update(snapshot);
                }
            }),
            test_player,
            Ok(fixture.join("session.json")),
        );
        wait_for_state(&manager, |s| s.available);
        let controls = Arc::new(crate::media_controls::MediaControls::start(
            hwnd.0 as isize,
            manager.media_dispatch(),
            Arc::new(|_| {}),
        ));
        *link.lock().unwrap() = Some(controls.clone());
        manager
            .set_queue(
                vec![
                    a.to_string_lossy().into_owned(),
                    b.to_string_lossy().into_owned(),
                ],
                0,
            )
            .unwrap();
        wait_for_state(&manager, |s| s.state == "playing");
        // Pause through the engine while the Windows service discovers the session,
        // preventing these deliberately short fixtures from finishing first.
        manager.media_dispatch()(crate::media_controls::MediaButton::Pause).unwrap();
        wait_for_state(&manager, |s| s.state == "paused");
        controls.update(manager.snapshot());
        let sessions = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
            .unwrap()
            .get()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let session = loop {
            pump();
            assert!(
                Instant::now() < deadline,
                "Test media session not published: {:?}",
                controls.status().error
            );
            let mut found = None;
            for candidate in sessions.GetSessions().unwrap() {
                if let Ok(properties) = candidate
                    .TryGetMediaPropertiesAsync()
                    .and_then(|operation| operation.get())
                {
                    if properties.Title().unwrap() == title {
                        found = Some(candidate);
                        break;
                    }
                }
            }
            if let Some(found) = found {
                break found;
            }
        };
        assert!(controls.status().available);
        // Only send commands to the session identified by this unique diagnostic title.
        assert!(session.TryPlayAsync().unwrap().get().unwrap());
        wait_for_state(&manager, |s| s.state == "playing");
        assert!(session.TryPauseAsync().unwrap().get().unwrap());
        wait_for_state(&manager, |s| s.state == "paused");
        assert!(session.TryPauseAsync().unwrap().get().unwrap());
        thread::sleep(Duration::from_millis(100));
        assert_eq!(
            manager.snapshot().state,
            "paused",
            "Pause must be idempotent"
        );
        assert!(session.TrySkipNextAsync().unwrap().get().unwrap());
        wait_for_state(&manager, |s| {
            s.current_index == Some(1) && s.state == "paused"
        });
        assert!(session.TrySkipPreviousAsync().unwrap().get().unwrap());
        wait_for_state(&manager, |s| {
            s.current_index == Some(0) && s.state == "paused"
        });
        // Windows may coalesce the duplicate Pause, so require the four distinct
        // transport actions rather than a callback for the redundant request.
        let deadline = Instant::now() + Duration::from_secs(2);
        while controls.status().last_button != Some(crate::media_controls::MediaButton::Previous) {
            assert!(Instant::now() < deadline);
            pump();
        }
        assert!(controls.status().received_buttons >= 4);
        assert_eq!(
            controls.status().last_button,
            Some(crate::media_controls::MediaButton::Previous)
        );
        controls.shutdown();
        manager.shutdown();
        *link.lock().unwrap() = None;
    }

    #[test]
    #[ignore = "requires local libmpv and generated WAV fixtures"]
    fn real_queue_select_shuffle_and_repeat_controls() {
        let mut mpv = test_player().unwrap();
        let a = Path::new(DIAGNOSTIC_ROOT).join("gapless-part-a.wav");
        let b = Path::new(DIAGNOSTIC_ROOT).join("gapless-part-b.wav");
        let mut queue = LookaheadQueue::new();
        queue
            .replace(
                &mut mpv,
                vec![a.clone(), b.clone(), a.clone(), b.clone()],
                0,
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(Instant::now() < deadline);
            let e = mpv.wait_event(0.05);
            if let Some(start) = e.start_file {
                queue
                    .on_start_file(&mut mpv, start.playlist_entry_id)
                    .unwrap();
            }
            if e.event_id == event::FILE_LOADED {
                break;
            }
        }
        mpv.command(&["set", "pause", "yes"]).unwrap();
        let original_id = queue.current_entry_id;
        let mut sorted = queue.paths.clone();
        sorted.sort();
        queue
            .edit(&mut mpv, "shuffle", None, 0, queue.generation, 0)
            .unwrap();
        assert_eq!(queue.current_entry_id, original_id);
        assert_eq!(queue.paths[0], a);
        let mut shuffled = queue.paths.clone();
        shuffled.sort();
        assert_eq!(sorted, shuffled);
        assert_eq!(mpv.playlist_entry_ids().unwrap().len(), 2);
        let old_revision = queue.generation;
        queue
            .edit(&mut mpv, "repeat", None, 1, queue.generation, 0)
            .unwrap();
        assert!(queue.repeat_all);
        assert!(queue
            .edit(&mut mpv, "select", None, 1, old_revision, 0)
            .is_err());
        queue
            .edit(&mut mpv, "select", None, 3, queue.generation, 0)
            .unwrap();
        assert_eq!(queue.current, 3);
        assert_eq!(mpv.playlist_entry_ids().unwrap().len(), 2);
        assert!(queue
            .edit(&mut mpv, "select", None, 99, queue.generation, 3)
            .is_err());

        // Both multi-track and one-track queues wrap through the same two-entry core.
        mpv.command(&["set", "speed", "4"]).unwrap();
        for paths in [vec![a.clone(), b], vec![a]] {
            queue
                .replace(&mut mpv, paths.clone(), paths.len() - 1)
                .unwrap();
            let mut selections = vec![];
            let deadline = Instant::now() + Duration::from_secs(12);
            while selections.len() < 3 {
                assert!(Instant::now() < deadline);
                let e = mpv.wait_event(0.05);
                if let Some(start) = e.start_file {
                    let disposition = queue
                        .on_start_file(&mut mpv, start.playlist_entry_id)
                        .unwrap();
                    if disposition != StartFileDisposition::Ignored {
                        selections.push(queue.current);
                        assert_eq!(mpv.playlist_entry_ids().unwrap().len(), 2);
                    }
                }
            }
            assert_eq!(
                selections,
                if paths.len() == 1 {
                    vec![0, 0, 0]
                } else {
                    vec![1, 0, 1]
                }
            );
            // Process FILE_LOADED before editing the lookahead.
            let deadline = Instant::now() + Duration::from_secs(5);
            while mpv.current_playlist_entry_id().unwrap() != queue.current_entry_id {
                assert!(Instant::now() < deadline);
                mpv.wait_event(0.01);
            }
            queue
                .edit(&mut mpv, "repeat", None, 0, queue.generation, queue.current)
                .unwrap();
            assert!(!queue.repeat_all);
            assert_eq!(mpv.playlist_entry_ids().unwrap().len(), 1);
            queue.repeat_all = true;
        }
    }

    #[test]
    fn old_sessions_default_to_repeat_off() {
        let session: Session = serde_json::from_str(
            r#"{"version":1,"paths":[],"current":0,"position":0,"volume":50}"#,
        )
        .unwrap();
        assert!(session.valid());
        assert!(!session.repeat_all);
    }

    #[test]
    fn rejects_invalid_session_schema_and_bounds() {
        let mut session = Session {
            version: 1,
            paths: vec![],
            current: 0,
            position: 0.0,
            volume: 50.0,
            repeat_all: false,
        };
        assert!(session.valid());
        session.version = 2;
        assert!(!session.valid());
        session.version = 1;
        session.volume = f64::NAN;
        assert!(!session.valid());
        session.volume = 50.0;
        session.paths.push("x".into());
        session.current = 1;
        assert!(!session.valid());
    }

    #[test]
    #[ignore = "requires local libmpv and generated WAV fixtures"]
    fn real_libmpv_queue_edits_preserve_current_and_reject_stale_selection() {
        let a = Path::new(DIAGNOSTIC_ROOT).join("gapless-part-a.wav");
        let b = Path::new(DIAGNOSTIC_ROOT).join("gapless-part-b.wav");
        assert!(a.is_file() && b.is_file());
        let mut mpv = Mpv::create(None).unwrap();
        for &(name, value) in LOCAL_AUDIO_OPTIONS {
            mpv.set_option(name, value).unwrap();
        }
        mpv.set_option("ao", "null").unwrap();
        mpv.set_option("pause", "yes").unwrap();
        mpv.initialize().unwrap();
        mpv.observe("time-pos", 1).unwrap();
        let mut queue = LookaheadQueue::new();
        queue
            .replace(&mut mpv, vec![a.clone(), b.clone()], 0)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut loaded = false;
        while Instant::now() < deadline && !loaded {
            let e = mpv.wait_event(0.05);
            if let Some(start) = e.start_file {
                queue
                    .on_start_file(&mut mpv, start.playlist_entry_id)
                    .unwrap();
            }
            loaded = e.event_id == event::FILE_LOADED;
        }
        assert!(loaded);
        let original = queue.current_entry_id;
        let revision = queue.generation;
        queue
            .edit(&mut mpv, "next", Some(a.clone()), 0, revision, 0)
            .unwrap();
        assert_eq!(queue.paths, vec![a.clone(), a.clone(), b.clone()]);
        assert_eq!(mpv.current_playlist_entry_id().unwrap(), original);
        assert_eq!(mpv.playlist_entry_ids().unwrap().len(), 2);
        assert!(queue
            .edit(&mut mpv, "remove", None, 1, revision, 0)
            .is_err());
        queue
            .edit(&mut mpv, "remove", None, 1, queue.generation, 0)
            .unwrap();
        assert_eq!(queue.paths, vec![a.clone(), b.clone()]);
        assert_eq!(mpv.current_playlist_entry_id().unwrap(), original);
        mpv.command(&["set", "volume", "37"]).unwrap();
        assert_eq!(number_property(&mut mpv, "volume"), Some(37.0));
        mpv.command(&["seek", "0.1", "absolute+exact"]).unwrap();
        // Force a transition without dispatching START_FILE: an edit must refuse
        // the old selection rather than remove the newly playing entry.
        mpv.command(&["playlist-next", "weak"]).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while mpv.current_playlist_entry_id().unwrap() == original && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_ne!(mpv.current_playlist_entry_id().unwrap(), original);
        assert!(queue
            .edit(&mut mpv, "next", Some(a), 0, queue.generation, 0)
            .is_err());
        assert_eq!(mpv.playlist_entry_ids().unwrap().len(), 2);
        let deadline = Instant::now() + Duration::from_secs(5);
        while queue.current == 0 && Instant::now() < deadline {
            let e = mpv.wait_event(0.05);
            if let Some(start) = e.start_file {
                queue
                    .on_start_file(&mut mpv, start.playlist_entry_id)
                    .unwrap();
            }
        }
        assert_eq!(queue.current, 1);
        assert_eq!(mpv.playlist_entry_ids().unwrap().len(), 1);
        queue
            .edit(&mut mpv, "clear", None, 0, queue.generation, 1)
            .unwrap();
        assert!(queue.paths.is_empty());
    }

    #[test]
    #[ignore = "requires an external libmpv DLL and generated gapless fixtures"]
    fn real_libmpv_skips_corrupt_lookahead_and_keeps_two_entries() {
        let dll = env::var_os("MUSICPLAYER_TEST_LIBMPV_PATH")
            .map(PathBuf::from)
            .expect("set MUSICPLAYER_TEST_LIBMPV_PATH to libmpv-2.dll");
        let diagnostic_root = PathBuf::from(DIAGNOSTIC_ROOT);
        let part_a = diagnostic_root.join("gapless-part-a.wav");
        let part_b = diagnostic_root.join("gapless-part-b.wav");
        assert!(
            part_a.is_file() && part_b.is_file(),
            "generate the root WAV fixtures first"
        );

        let corrupt = diagnostic_root.join(format!(
            "corrupt-lookahead-test-{}.flac",
            std::process::id()
        ));
        fs::write(&corrupt, b"this is deliberately not an audio file")
            .expect("create corrupt diagnostic fixture");
        struct RemoveFixture(PathBuf);
        impl Drop for RemoveFixture {
            fn drop(&mut self) {
                let _ = fs::remove_file(&self.0);
            }
        }
        let _cleanup = RemoveFixture(corrupt.clone());

        let mut mpv = Mpv::create(Some(&dll)).expect("load test libmpv");
        for &(name, value) in LOCAL_AUDIO_OPTIONS {
            mpv.set_option(name, value).expect("set base option");
        }
        mpv.set_option("ao", "null").expect("select null AO");
        mpv.set_option("ao-null-untimed", "yes")
            .expect("make null AO run without wall-clock delay");
        mpv.initialize().expect("initialize test libmpv");

        let mut queue = LookaheadQueue::new();
        queue
            .replace(&mut mpv, vec![part_a, corrupt, part_b], 0)
            .expect("create lookahead queue");

        let started = Instant::now();
        let mut start_events = 0;
        let mut eof_events = 0;
        let mut error_events = 0;
        while started.elapsed().as_secs() < 10 && eof_events + error_events < 3 {
            let summary = mpv.wait_event(0.1);
            match summary.event_id {
                event::START_FILE => {
                    let started_file = summary.start_file.expect("START_FILE payload");
                    queue
                        .on_start_file(&mut mpv, started_file.playlist_entry_id)
                        .expect("advance lookahead state");
                    assert!(
                        mpv.playlist_entry_ids().expect("read playlist").len() <= 2,
                        "mpv must contain only current and one lookahead entry"
                    );
                    start_events += 1;
                }
                event::END_FILE => {
                    let ended = summary.end_file.expect("END_FILE payload");
                    if ended.reason == event::END_FILE_REASON_ERROR || ended.error < 0 {
                        error_events += 1;
                    } else if ended.reason == event::END_FILE_REASON_EOF {
                        eof_events += 1;
                    }
                }
                event::QUEUE_OVERFLOW => panic!("libmpv event queue overflow"),
                _ => {}
            }
        }

        assert_eq!(
            start_events, 3,
            "good, corrupt, and recovery track must all start"
        );
        assert_eq!(eof_events, 2, "both valid WAV files must reach EOF");
        assert_eq!(
            error_events, 1,
            "the corrupt lookahead must report one error"
        );
        assert_eq!(
            queue.current, 2,
            "logical queue must recover onto the third track"
        );
    }

    #[test]
    #[ignore = "requires an external libmpv DLL and generated gapless fixtures"]
    fn real_libmpv_recovers_when_lookahead_file_is_removed_after_append() {
        let dll = env::var_os("MUSICPLAYER_TEST_LIBMPV_PATH")
            .map(PathBuf::from)
            .expect("set MUSICPLAYER_TEST_LIBMPV_PATH to libmpv-2.dll");
        let diagnostic_root = PathBuf::from(DIAGNOSTIC_ROOT);
        let source_a = diagnostic_root.join("gapless-part-a.wav");
        let source_c = diagnostic_root.join("gapless-part-b.wav");
        assert!(
            source_a.is_file() && source_c.is_file(),
            "generate the root WAV fixtures first"
        );

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after the Unix epoch")
            .as_nanos();
        let fixture_root =
            diagnostic_root.join(format!("missing-lookahead-{}-{nonce}", std::process::id()));
        fs::create_dir(&fixture_root).expect("create unique diagnostic fixture directory");
        struct RemoveFixtureDirectory(PathBuf);
        impl Drop for RemoveFixtureDirectory {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = RemoveFixtureDirectory(fixture_root.clone());

        let part_a = fixture_root.join("a.wav");
        let part_b = fixture_root.join("temporary-b.wav");
        let part_c = fixture_root.join("c.wav");
        fs::copy(&source_a, &part_a).expect("create valid A fixture");
        fs::copy(&source_a, &part_b).expect("create valid temporary B fixture");
        fs::copy(&source_c, &part_c).expect("create valid C fixture");

        let mut mpv = Mpv::create(Some(&dll)).expect("load test libmpv");
        for &(name, value) in LOCAL_AUDIO_OPTIONS {
            mpv.set_option(name, value).expect("set base option");
        }
        mpv.set_option("ao", "null").expect("select null AO");
        mpv.set_option("ao-null-untimed", "yes")
            .expect("make null AO run without wall-clock delay");
        mpv.set_option("pause", "yes")
            .expect("pause before appending the lookahead");
        mpv.initialize().expect("initialize test libmpv");

        let mut queue = LookaheadQueue::new();
        queue
            .replace(
                &mut mpv,
                vec![part_a.clone(), part_b.clone(), part_c.clone()],
                0,
            )
            .expect("create current plus lookahead queue");
        let initial_ids = mpv.playlist_entry_ids().expect("read initial playlist");
        assert_eq!(initial_ids.len(), 2, "A and B must be in libmpv");
        let a_id = initial_ids[0];
        let b_id = initial_ids[1];

        fs::remove_file(&part_b).expect("remove B after it entered the lookahead");
        assert!(
            !part_b.exists(),
            "temporary B must be absent before playback"
        );
        assert!(
            mpv.playlist_entry_ids()
                .expect("read playlist after removing B")
                .len()
                <= 2,
            "libmpv must contain at most current plus one lookahead"
        );
        mpv.command(&["set", "pause", "no"])
            .expect("resume after removing B");

        let started_at = Instant::now();
        let mut started_ids = Vec::new();
        let mut c_id = None;
        let mut a_eof = false;
        let mut c_eof = false;
        let mut b_errors = 0;
        while started_at.elapsed() < Duration::from_secs(10) && !(a_eof && c_eof && b_errors == 1) {
            let summary = mpv.wait_event(0.1);
            let playlist_len = mpv
                .playlist_entry_ids()
                .expect("read bounded playlist")
                .len();
            assert!(
                playlist_len <= 2,
                "libmpv must contain at most current plus one lookahead, got {playlist_len}"
            );
            match summary.event_id {
                event::START_FILE => {
                    let started = summary.start_file.expect("START_FILE payload");
                    started_ids.push(started.playlist_entry_id);
                    assert!(
                        started.playlist_entry_id == a_id
                            || started.playlist_entry_id == b_id
                            || c_id == Some(started.playlist_entry_id),
                        "only A, removed B, and recovery C may start"
                    );
                    queue
                        .on_start_file(&mut mpv, started.playlist_entry_id)
                        .expect("advance logical lookahead state");
                    let ids = mpv.playlist_entry_ids().expect("read post-start playlist");
                    assert!(ids.len() <= 2, "post-start playlist must stay bounded");
                    if started.playlist_entry_id == b_id {
                        c_id = ids.get(1).copied();
                        assert!(c_id.is_some(), "C must be appended while B is current");
                    }
                }
                event::END_FILE => {
                    let ended = summary.end_file.expect("END_FILE payload");
                    if ended.reason == event::END_FILE_REASON_ERROR || ended.error < 0 {
                        assert_eq!(ended.playlist_entry_id, b_id, "only B may fail");
                        b_errors += 1;
                    } else if ended.reason == event::END_FILE_REASON_EOF {
                        if ended.playlist_entry_id == a_id {
                            a_eof = true;
                        } else {
                            assert_eq!(c_id, Some(ended.playlist_entry_id), "C must reach EOF");
                            c_eof = true;
                        }
                    }
                }
                event::QUEUE_OVERFLOW => panic!("libmpv event queue overflow"),
                _ => {}
            }
        }

        assert_eq!(
            started_ids.len(),
            3,
            "A, removed B, and recovery C must start"
        );
        assert_eq!(started_ids[0], a_id);
        assert_eq!(started_ids[1], b_id);
        assert_eq!(started_ids[2], c_id.expect("capture C playlist entry id"));
        assert!(a_eof, "A must reach EOF");
        assert!(c_eof, "C must reach EOF after B fails");
        assert_eq!(b_errors, 1, "removed B must report exactly one error");
        assert_eq!(queue.current, 2, "logical queue must recover onto C");
    }
}
