//! Optional Apple Music worker and Rust-owned, explicitly started download queue.
pub mod promotion;
use promotion::{PromotionIntent, PromotionService, StagedTrack};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{sync_channel, Receiver},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, Manager};
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    },
};

const LIMIT: usize = 8 * 1024 * 1024;
const MAX_JOBS: usize = 4096;
const EVENT: &str = "musicplayer://downloads";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DownloaderConfig {
    version: u32,
    deployment_root: PathBuf,
}

fn configured_deployment(config_path: &Path) -> Result<Option<PathBuf>, String> {
    let config = crate::state::load::<DownloaderConfig>(config_path, 16 * 1024)?;
    match config {
        Some(config) if config.version == 1 && config.deployment_root.is_absolute() => {
            Ok(Some(config.deployment_root))
        }
        Some(_) => Err("Optional AAC configuration is invalid; rerun Setup-AppleMusic.ps1.".into()),
        None => Ok(None),
    }
}

fn deployment_root() -> Result<PathBuf, String> {
    if let Some(root) = std::env::var_os("MUSICPLAYER_AAC_ROOT") {
        let root = PathBuf::from(root);
        return root
            .is_absolute()
            .then_some(root)
            .ok_or_else(|| "MUSICPLAYER_AAC_ROOT must be an absolute directory.".into());
    }
    let state = crate::state::directory()?;
    if let Some(root) = configured_deployment(&state.join("downloader-config.json"))? {
        return Ok(root);
    }
    Ok(state
        .parent()
        .ok_or("Application directory unavailable")?
        .join("optional/apple-music"))
}

fn worker_script() -> Result<PathBuf, String> {
    let executable = std::env::current_exe().map_err(|_| message("WORKER_UNAVAILABLE"))?;
    let installed = executable
        .parent()
        .ok_or_else(|| message("WORKER_UNAVAILABLE"))?
        .join("resources/worker/run_apple_music_worker.ps1");
    if installed.is_file() {
        return Ok(installed);
    }
    #[cfg(debug_assertions)]
    {
        let development = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("scripts/run_apple_music_worker.ps1");
        if development.is_file() {
            return Ok(development);
        }
    }
    Err(message("WORKER_UNAVAILABLE"))
}
fn identifier(prefix: &str) -> String {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{prefix}-{}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        COUNT.fetch_add(1, Ordering::Relaxed)
    )
}
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 32 && id.bytes().all(|c| c.is_ascii_digit())
}
fn valid_job(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
}
fn active(state: &str) -> bool {
    matches!(
        state,
        "resolving"
            | "downloading"
            | "decrypting"
            | "remuxing"
            | "tagging"
            | "verifying"
            | "organizing"
    )
}
fn message(code: &str) -> String {
    match code {
        "AUTH_MISSING" => "Apple Music credentials are missing. Run resources/worker/Setup-AppleMusic.ps1 -ConfigureCredentials, then Check account.",
        "AUTH_INVALID" => "Apple Music credentials are invalid. Run Setup-AppleMusic.ps1 -ConfigureCredentials for this Windows user, then Check account.",
        "SUBSCRIPTION_INACTIVE" => "An active Apple Music subscription is required.",
        "APPLE_UNAVAILABLE" => "This item is unavailable in your account's storefront.",
        "TOOLS_MISSING" | "WORKER_UNAVAILABLE" => "Optional AAC tools are unavailable. Run resources/worker/Setup-AppleMusic.ps1 -DeploymentRoot <installed AAC folder>, then restart MusicPlayer. Local playback remains available.",
        "INVALID_URL" => "Enter an HTTPS music.apple.com album or playlist URL.",
        "CATALOG_LIMIT" => "The catalog result exceeds 4,096 tracks; no partial import was accepted.",
        "STAGING_LIMIT" => "The download exceeded its staging limit.",
        "DOWNLOAD_FAILED" => "The audio download failed. Check connectivity and retry.",
        "PROCESSING_FAILED" => "AAC processing failed; the file was not added to the library.",
        "TAGGING_FAILED" => "Audio tagging failed; the staged file was not added to the library.",
        "DISK_FULL" => "The disk is full. Free space and retry; existing music was preserved.",
        "CANCELLED" => "Download cancelled.",
        "TIMEOUT" | "TOOL_TIMEOUT" => "The Apple Music operation timed out. Retry when connectivity is available.",
        _ => "The Apple Music operation failed. Check your account, connection, and worker installation.",
    }.into()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TrackInput {
    pub catalog_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: Option<String>,
    pub album_id: Option<String>,
    pub duration_ms: Option<u64>,
    pub artwork_url: Option<String>,
    pub release_date: Option<String>,
    pub disc: Option<u32>,
    pub disc_total: Option<u32>,
    pub track: Option<u32>,
    pub track_total: Option<u32>,
    pub track_count: Option<u32>,
    pub available: bool,
    pub handle: Option<String>,
    pub existing: bool,
    pub queued: bool,
    pub duplicate: bool,
}
impl TrackInput {
    fn valid(&self, require_id: bool) -> bool {
        (!require_id || valid_id(&self.catalog_id))
            && self.catalog_id.len() <= 64
            && [&self.title, &self.artist, &self.album]
                .iter()
                .all(|s| s.chars().count() <= 256 && !s.chars().any(char::is_control))
            && [
                &self.album_artist,
                &self.album_id,
                &self.release_date,
                &self.handle,
            ]
            .iter()
            .all(|s| s.as_ref().is_none_or(|s| s.chars().count() <= 256))
            && self.artwork_url.as_ref().is_none_or(|s| {
                s.len() <= 2048
                    && s.starts_with("https://")
                    && s.split('/')
                        .nth(2)
                        .is_some_and(|host| host.ends_with(".mzstatic.com"))
            })
    }
}
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchInput {
    pub title: String,
    pub artist: String,
    pub album_id: Option<String>,
}
#[derive(Deserialize)]
pub struct EnqueueRequest {
    pub tracks: Vec<TrackInput>,
    pub batch: Option<BatchInput>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Job {
    id: String,
    batch_id: Option<String>,
    batch_title: Option<String>,
    track: TrackInput,
    state: String,
    bytes_done: Option<u64>,
    bytes_total: Option<u64>,
    destination: Option<String>,
    error: Option<String>,
    retryable: bool,
    created_at: String,
    promotion_intent: Option<PromotionIntent>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Store {
    version: u32,
    jobs: Vec<Job>,
}
impl Default for Store {
    fn default() -> Self {
        Self {
            version: 1,
            jobs: vec![],
        }
    }
}
impl Store {
    fn validate(&self) -> bool {
        let mut ids = HashSet::new();
        self.version == 1
            && self.jobs.len() <= MAX_JOBS
            && self.jobs.iter().all(|j| {
                valid_job(&j.id)
                    && ids.insert(j.id.clone())
                    && j.track.valid(true)
                    && (active(&j.state)
                        || matches!(
                            j.state.as_str(),
                            "queued"
                                | "completed"
                                | "failed"
                                | "interrupted"
                                | "cancelled"
                                | "skipped"
                        ))
                    && j.error.as_ref().is_none_or(|s| s.len() <= 1024)
                    && j.destination.as_ref().is_none_or(|s| s.len() <= 1024)
                    && j.batch_id.as_ref().is_none_or(|s| valid_job(s))
                    && j.batch_title
                        .as_ref()
                        .is_none_or(|s| s.chars().count() <= 256)
                    && j.created_at.len() <= 32
                    && j.created_at.bytes().all(|b| b.is_ascii_digit())
                    && j.promotion_intent.as_ref().is_none_or(|p| {
                        p.job_id == j.id
                            && p.catalog_id == j.track.catalog_id
                            && p.sidecars.len() <= 2
                    })
            })
    }
}
struct Inner {
    store: Store,
    running: bool,
    stop_after_current: bool,
    current: Option<String>,
    error: Option<String>,
    revision: u64,
}
impl Inner {
    fn snapshot(&self, available: bool) -> Value {
        json!({"running":self.running,"stopAfterCurrent":self.stop_after_current,"queued":self.store.jobs.iter().filter(|j|j.state=="queued").count(),"runningJobId":self.current,"error":self.error,"revision":self.revision,"available":available})
    }
}
struct Shared {
    inner: Mutex<Inner>,
    worker: Mutex<Option<Worker>>,
    process: Mutex<Option<Arc<JobObject>>>,
    closing: AtomicBool,
    state_path: PathBuf,
    service: PromotionService,
    app: Option<AppHandle>,
    owner: Option<File>,
}
#[derive(Clone)]
pub struct DownloadManager {
    shared: Arc<Shared>,
}

struct JobObject(HANDLE);
unsafe impl Send for JobObject {}
unsafe impl Sync for JobObject {}
impl JobObject {
    fn new() -> Result<Self, String> {
        unsafe {
            let handle =
                CreateJobObjectW(None, None).map_err(|_| "Cannot create worker supervisor")?;
            let job = Self(handle);
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of_val(&info) as u32,
            )
            .map_err(|_| "Cannot configure worker supervisor")?;
            Ok(job)
        }
    }
    fn terminate(&self) {
        unsafe {
            let _ = TerminateJobObject(self.0, 1);
        }
    }
}
impl Drop for JobObject {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
struct Worker {
    child: Child,
    input: ChildStdin,
    output: Receiver<Result<Value, String>>,
    job: Arc<JobObject>,
}
fn read_line(reader: &mut impl BufRead) -> Result<Option<Vec<u8>>, String> {
    let mut line = Vec::new();
    loop {
        let chunk = reader.fill_buf().map_err(|_| "Worker output failed")?;
        if chunk.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Err("Worker output was truncated".into())
            };
        }
        let take = chunk
            .iter()
            .position(|b| *b == b'\n')
            .map(|n| n + 1)
            .unwrap_or(chunk.len());
        if line.len() + take > 65536 {
            return Err("Worker output exceeded its limit".into());
        }
        line.extend_from_slice(&chunk[..take]);
        reader.consume(take);
        if line.last() == Some(&b'\n') {
            return Ok(Some(line));
        }
    }
}
impl Worker {
    fn spawn() -> Result<Self, String> {
        let script = worker_script()?;
        let deployment = deployment_root()?;
        if !deployment.join(".venv/Scripts/python.exe").is_file() {
            return Err(message("WORKER_UNAVAILABLE"));
        }
        let job = Arc::new(JobObject::new()?);
        let mut child = Command::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(script)
            .arg("-DeploymentRoot")
            .arg(deployment)
            .arg("-WaitForSupervisor")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(0x08000000)
            .spawn()
            .map_err(|_| message("WORKER_UNAVAILABLE"))?;
        if unsafe { AssignProcessToJobObject(job.0, HANDLE(child.as_raw_handle())) }.is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Cannot contain worker process tree".into());
        }
        let mut input = child.stdin.take().ok_or("Worker input unavailable")?;
        let stdout = child.stdout.take().ok_or("Worker output unavailable")?;
        let (sender, output) = sync_channel(64);
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let line = match read_line(&mut reader) {
                    Ok(Some(v)) => v,
                    Ok(None) => break,
                    Err(e) => {
                        let _ = sender.send(Err(e));
                        break;
                    }
                };
                let value =
                    serde_json::from_slice(&line).map_err(|_| "Invalid worker protocol".into());
                if sender.send(value).is_err() {
                    break;
                }
            }
        });
        input
            .write_all(b"launch\n")
            .and_then(|_| input.flush())
            .map_err(|_| "Worker startup failed")?;
        let worker = Self {
            child,
            input,
            output,
            job,
        };
        let ready = worker
            .output
            .recv_timeout(Duration::from_secs(20))
            .map_err(|_| message("WORKER_UNAVAILABLE"))??;
        if ready["event"] != "ready" || ready["protocol"] != 1 {
            worker.job.terminate();
            return Err(message("WORKER_UNAVAILABLE"));
        }
        Ok(worker)
    }
    fn request(
        &mut self,
        mut request: Value,
        mut progress: impl FnMut(&Value),
        timeout: Duration,
    ) -> Result<Value, String> {
        let id = identifier("request");
        request["id"] = json!(id);
        let mut bytes = serde_json::to_vec(&request).map_err(|_| "Invalid worker request")?;
        if bytes.len() >= 65535 {
            return Err("Worker request exceeds limit".into());
        }
        bytes.push(b'\n');
        self.input
            .write_all(&bytes)
            .and_then(|_| self.input.flush())
            .map_err(|_| "Worker input closed")?;
        let deadline = Instant::now() + timeout;
        let mut items = vec![];
        loop {
            let value = self
                .output
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|_| message("TIMEOUT"))??;
            if value["request_id"] != id {
                return Err("Worker request identity mismatch".into());
            }
            match value["event"].as_str() {
                Some("item") => {
                    if items.len() >= MAX_JOBS {
                        return Err(message("CATALOG_LIMIT"));
                    }
                    let item: TrackInput = serde_json::from_value(value["data"].clone())
                        .map_err(|_| "Invalid catalog metadata")?;
                    if !item.valid(item.available) {
                        return Err("Invalid catalog metadata".into());
                    }
                    items.push(item);
                }
                Some("stage" | "progress") => progress(&value),
                Some("finished") => {
                    let mut data = value["data"].clone();
                    if let Some(collection) = data["collection"].as_str().map(str::to_string) {
                        if collection != "results" && collection != "tracks" {
                            return Err("Invalid worker collection".into());
                        }
                        data[&collection] = serde_json::to_value(items).unwrap();
                    }
                    return Ok(data);
                }
                Some("complete") => return Ok(value["data"].clone()),
                Some("error") => return Err(message(value["code"].as_str().unwrap_or(""))),
                _ => return Err("Invalid worker event".into()),
            }
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.job.terminate();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        // Dropping the final Job Object handle remains a kill-on-close fallback.
        // Never hang the player indefinitely if Windows cannot reap the process.
    }
}

impl DownloadManager {
    pub fn new(app: Option<AppHandle>) -> Self {
        let dir = crate::state::directory().unwrap_or_default();
        Self::at(app, dir)
    }
    fn at(app: Option<AppHandle>, dir: PathBuf) -> Self {
        let created = dir.is_absolute() && fs::create_dir_all(&dir).is_ok();
        let owner = if created {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .share_mode(0)
                .open(dir.join(".download-manager.lock"))
                .ok()
        } else {
            None
        };
        let path = dir.join("download-queue.json");
        let loaded = if created {
            crate::state::load::<Store>(&path, LIMIT)
        } else {
            Err("State directory unavailable".into())
        };
        let (mut store, mut error) = match loaded {
            Ok(Some(store)) if store.validate() => (store, None),
            Ok(None) => (Store::default(), None),
            _ => (
                Store::default(),
                Some(
                    "Download queue could not be restored; local playback remains available."
                        .into(),
                ),
            ),
        };
        for job in &mut store.jobs {
            if active(&job.state) {
                job.state = "interrupted".into();
                job.retryable = true;
                job.error = Some("Interrupted by application shutdown; retry explicitly.".into());
            }
        }
        if owner.is_none() {
            error = Some("Downloads are unavailable because another player instance owns the queue or state storage is inaccessible.".into());
        }
        let shared = Shared {
            inner: Mutex::new(Inner {
                store,
                running: false,
                stop_after_current: false,
                current: None,
                error,
                revision: 0,
            }),
            worker: Mutex::new(None),
            process: Mutex::new(None),
            closing: AtomicBool::new(false),
            state_path: path,
            service: PromotionService::new(
                PathBuf::from(r"D:\Music"),
                deployment_root()
                    .unwrap_or_else(|_| dir.join("unavailable-aac"))
                    .join("tools/ffmpeg/ffmpeg-9.0.1-essentials_build/bin/ffprobe.exe"),
            ),
            app,
            owner,
        };
        Self {
            shared: Arc::new(shared),
        }
    }
    pub fn snapshot(&self) -> Value {
        self.shared.snapshot()
    }
    pub fn page(&self, offset: usize) -> Value {
        let inner = self.shared.inner.lock().unwrap();
        let jobs: Vec<_> = inner.store.jobs.iter().skip(offset).take(100).map(|job| json!({"id":job.id,"batchId":job.batch_id,"batchTitle":job.batch_title,"catalogId":job.track.catalog_id,"title":job.track.title,"artist":job.track.artist,"album":job.track.album,"artworkUrl":job.track.artwork_url,"quality":"AAC 256","state":job.state,"bytesDone":job.bytes_done,"bytesTotal":job.bytes_total,"destination":job.destination,"error":job.error,"retryable":job.retryable,"createdAt":job.created_at})).collect();
        let mut batches = std::collections::BTreeMap::<String, Value>::new();
        for job in &inner.store.jobs {
            if let Some(id) = &job.batch_id {
                let batch = batches.entry(id.clone()).or_insert_with(|| json!({"id":id,"title":job.batch_title.as_ref().unwrap_or(&job.track.album),"artist":job.track.artist,"total":0,"completed":0,"failed":0,"active":0,"pending":0}));
                let field = if matches!(job.state.as_str(), "completed" | "skipped") {
                    "completed"
                } else if matches!(job.state.as_str(), "failed" | "interrupted" | "cancelled") {
                    "failed"
                } else if active(&job.state) {
                    "active"
                } else {
                    "pending"
                };
                batch["total"] = json!(batch["total"].as_u64().unwrap() + 1);
                batch[field] = json!(batch[field].as_u64().unwrap() + 1);
            }
        }
        let total = inner.store.jobs.len();
        let snapshot = inner.snapshot(self.shared.owner.is_some());
        drop(inner);
        json!({"jobs":jobs,"offset":offset,"total":total,"snapshot":snapshot,"batches":batches.into_values().collect::<Vec<_>>()})
    }
    pub fn catalog(&self, request: Value) -> Result<Value, String> {
        self.shared.available()?;
        let mut worker = self
            .shared
            .worker
            .try_lock()
            .map_err(|_| "Apple Music is busy; try again after the current operation.")?;
        if self.shared.inner.lock().unwrap().running {
            return Err(
                "Downloads are running. Stop after current before browsing Apple Music.".into(),
            );
        }
        let mut result =
            self.shared
                .request(&mut worker, request, |_| {}, Duration::from_secs(180))?;
        let key = if result.get("tracks").is_some() {
            "tracks"
        } else {
            "results"
        };
        if let Some(items) = result.get_mut(key).and_then(Value::as_array_mut) {
            let existing = self.shared.service.existing_catalog_ids()?;
            let inner = self.shared.inner.lock().unwrap();
            for item in items {
                let id = item["catalogId"].as_str().unwrap_or("").to_string();
                item["existing"] = json!(existing.contains(&id));
                item["queued"] =
                    json!(inner
                        .store
                        .jobs
                        .iter()
                        .any(|j| j.track.catalog_id == id
                            && (j.state == "queued" || active(&j.state))));
            }
        }
        // Only safe account/preview fields are allowed through the frontend boundary.
        let allowed = [
            "configured",
            "authenticated",
            "subscription",
            "storefront",
            "message",
            "collection",
            "kind",
            "catalogId",
            "title",
            "artist",
            "releaseDate",
            "artworkUrl",
            "trackCount",
            "availableCount",
            "unavailableCount",
            "total",
            "tracks",
            "results",
        ];
        if let Some(object) = result.as_object_mut() {
            object.retain(|k, v| {
                allowed.contains(&k.as_str())
                    && (!v.is_string() || v.as_str().unwrap().len() <= 2048)
            });
        } else {
            return Err("Invalid worker result".into());
        }
        Ok(result)
    }
    pub fn enqueue(&self, request: EnqueueRequest) -> Result<Value, String> {
        self.shared.available()?;
        if request.tracks.is_empty()
            || request.tracks.len() > MAX_JOBS
            || request.tracks.iter().any(|t| !t.valid(true))
        {
            return Err("Invalid track selection or selection exceeds 4,096 tracks".into());
        }
        if request.batch.as_ref().is_some_and(|b| {
            b.title.chars().count() > 256
                || b.artist.chars().count() > 256
                || b.album_id.as_ref().is_some_and(|id| id.len() > 64)
        }) {
            return Err("Invalid album selection".into());
        }
        let existing = self.shared.service.existing_catalog_ids()?;
        let mut inner = self.shared.inner.lock().unwrap();
        let mut next = inner.store.clone();
        let batch_id = request.batch.as_ref().map(|_| identifier("batch"));
        let mut accepted = 0;
        let mut skipped = 0;
        for mut track in request.tracks {
            if !track.available
                || track.duplicate
                || existing.contains(&track.catalog_id)
                || next.jobs.iter().any(|j| {
                    j.track.catalog_id == track.catalog_id
                        && (j.state == "queued" || active(&j.state))
                })
            {
                skipped += 1;
                continue;
            }
            track.handle = None;
            track.existing = false;
            track.queued = false;
            next.jobs.push(Job {
                id: identifier("job"),
                batch_id: batch_id.clone(),
                batch_title: request.batch.as_ref().map(|b| b.title.clone()),
                track,
                state: "queued".into(),
                bytes_done: None,
                bytes_total: None,
                destination: None,
                error: None,
                retryable: false,
                created_at: format!(
                    "{}",
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                ),
                promotion_intent: None,
            });
            accepted += 1;
        }
        if next.jobs.len() > MAX_JOBS {
            return Err("Download queue is full; clear finished jobs first.".into());
        }
        self.shared.save(&next)?;
        inner.store = next;
        inner.revision += 1;
        drop(inner);
        self.shared.publish();
        Ok(json!({"accepted":accepted,"skipped":skipped}))
    }
    pub fn start(&self) -> Result<(), String> {
        self.shared.available()?;
        // Do not start a scheduler while a catalog request owns the worker.
        let worker = self
            .shared
            .worker
            .try_lock()
            .map_err(|_| "Apple Music is busy; try Start again shortly.")?;
        let mut inner = self.shared.inner.lock().unwrap();
        if inner.running {
            return Ok(());
        }
        if !inner.store.jobs.iter().any(|j| j.state == "queued") {
            return Err("No queued downloads".into());
        }
        inner.running = true;
        inner.stop_after_current = false;
        inner.error = None;
        inner.revision += 1;
        drop(inner);
        drop(worker);
        let shared = self.shared.clone();
        self.shared.publish();
        thread::spawn(move || shared.run());
        Ok(())
    }
    pub fn stop_after_current(&self) {
        {
            let mut inner = self.shared.inner.lock().unwrap();
            inner.stop_after_current = true;
            inner.revision += 1;
        }
        self.shared.publish();
    }
    pub fn cancel(&self, id: &str) -> Result<(), String> {
        self.shared.available()?;
        let mut inner = self.shared.inner.lock().unwrap();
        let mut next = inner.store.clone();
        let job = next
            .jobs
            .iter_mut()
            .find(|j| j.id == id)
            .ok_or("Download job not found")?;
        if matches!(job.state.as_str(), "verifying" | "organizing") {
            return Err(
                "File verification/placement is finishing; it cannot be cancelled safely now."
                    .into(),
            );
        }
        if !active(&job.state) && job.state != "queued" {
            return Err("This job is not pending".into());
        }
        let was_active = active(&job.state);
        job.state = "cancelled".into();
        job.error = None;
        job.retryable = true;
        self.shared.save(&next)?;
        inner.store = next;
        inner.revision += 1;
        drop(inner);
        if was_active {
            if let Some(job) = self.shared.process.lock().unwrap().as_ref() {
                job.terminate();
            }
        } else if !self
            .shared
            .inner
            .lock()
            .unwrap()
            .store
            .jobs
            .iter()
            .any(|j| j.id == id && j.promotion_intent.is_some())
        {
            cleanup_staging(id)?;
        }
        self.shared.publish();
        Ok(())
    }
    pub fn retry(&self, id: &str) -> Result<(), String> {
        self.shared.available()?;
        let mut inner = self.shared.inner.lock().unwrap();
        let mut next = inner.store.clone();
        if inner.current.as_deref() == Some(id) {
            return Err("Cancellation is still finishing; retry shortly.".into());
        }
        let job = next
            .jobs
            .iter_mut()
            .find(|j| j.id == id)
            .ok_or("Download job not found")?;
        if !matches!(job.state.as_str(), "failed" | "interrupted" | "cancelled") {
            return Err("This job cannot be retried".into());
        }
        job.state = "queued".into();
        job.error = None;
        job.retryable = false;
        job.bytes_done = None;
        job.bytes_total = None;
        self.shared.save(&next)?;
        inner.store = next;
        inner.revision += 1;
        drop(inner);
        self.shared.publish();
        Ok(())
    }
    pub fn clear_finished(&self) -> Result<(), String> {
        self.shared.available()?;
        let mut inner = self.shared.inner.lock().unwrap();
        let mut next = inner.store.clone();
        next.jobs.retain(|j| {
            inner.current.as_deref() == Some(&j.id)
                || j.promotion_intent.is_some()
                || !matches!(j.state.as_str(), "completed" | "skipped" | "cancelled")
        });
        self.shared.save(&next)?;
        inner.store = next;
        inner.revision += 1;
        drop(inner);
        self.shared.publish();
        Ok(())
    }
    pub fn shutdown(&self) {
        self.shared.closing.store(true, Ordering::Release);
        let mut inner = self.shared.inner.lock().unwrap();
        inner.running = false;
        for job in &mut inner.store.jobs {
            if active(&job.state) {
                job.state = "interrupted".into();
                job.retryable = true;
            }
        }
        let _ = self.shared.save(&inner.store);
        drop(inner);
        if let Some(job) = self.shared.process.lock().unwrap().as_ref() {
            job.terminate();
        }
    }
}
impl Shared {
    fn available(&self) -> Result<(), String> {
        if self.owner.is_none() || self.closing.load(Ordering::Acquire) {
            Err("Download manager is unavailable; local playback still works.".into())
        } else {
            Ok(())
        }
    }
    fn save(&self, store: &Store) -> Result<(), String> {
        if self.owner.is_none() {
            return Err("This player instance does not own the download queue.".into());
        }
        crate::state::replace(&self.state_path, &crate::state::encode(store, LIMIT)?)
    }
    fn snapshot(&self) -> Value {
        let inner = self.inner.lock().unwrap();
        inner.snapshot(self.owner.is_some())
    }
    fn publish(&self) {
        if let Some(app) = &self.app {
            let _ = app.emit(EVENT, self.snapshot());
        }
    }
    fn request(
        &self,
        worker: &mut Option<Worker>,
        request: Value,
        progress: impl FnMut(&Value),
        timeout: Duration,
    ) -> Result<Value, String> {
        if worker.is_none() {
            *worker = Some(Worker::spawn()?);
        }
        *self.process.lock().unwrap() = Some(worker.as_ref().unwrap().job.clone());
        let cancelled = request["job_id"].as_str().is_some_and(|id| {
            self.inner
                .lock()
                .unwrap()
                .store
                .jobs
                .iter()
                .any(|j| j.id == id && j.state == "cancelled")
        });
        if self.closing.load(Ordering::Acquire) || cancelled {
            *worker = None;
            *self.process.lock().unwrap() = None;
            return Err(message("CANCELLED"));
        }
        let result = worker.as_mut().unwrap().request(request, progress, timeout);
        if result.is_err() {
            *worker = None;
            *self.process.lock().unwrap() = None;
        }
        result
    }
    fn update_job(
        &self,
        id: &str,
        update: impl FnOnce(&mut Job),
        persist: bool,
    ) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap();
        if self.closing.load(Ordering::Acquire) {
            return Err("Application is closing".into());
        }
        let mut next = inner.store.clone();
        let job = next
            .jobs
            .iter_mut()
            .find(|j| j.id == id)
            .ok_or("Job unavailable")?;
        if job.state == "cancelled" {
            return Err(message("CANCELLED"));
        }
        update(job);
        if persist {
            self.save(&next)?;
        }
        inner.store = next;
        inner.revision += 1;
        drop(inner);
        self.publish();
        Ok(())
    }
    fn run(self: Arc<Self>) {
        loop {
            let selected = {
                let mut inner = self.inner.lock().unwrap();
                if self.closing.load(Ordering::Acquire) || inner.stop_after_current {
                    None
                } else {
                    let selected = inner
                        .store
                        .jobs
                        .iter()
                        .find(|j| j.state == "queued")
                        .cloned();
                    inner.current = selected.as_ref().map(|j| j.id.clone());
                    selected
                }
            };
            let Some(job) = selected else {
                break;
            };
            let result = self.perform(&job);
            if let Err(error) = result {
                if [
                    "AUTH_MISSING",
                    "AUTH_INVALID",
                    "SUBSCRIPTION_INACTIVE",
                    "TOOLS_MISSING",
                    "WORKER_UNAVAILABLE",
                    "DISK_FULL",
                ]
                .iter()
                .any(|code| error == message(code))
                {
                    self.inner.lock().unwrap().stop_after_current = true;
                }
                let cancelled = self
                    .inner
                    .lock()
                    .unwrap()
                    .store
                    .jobs
                    .iter()
                    .any(|j| j.id == job.id && j.state == "cancelled");
                if cancelled {
                    *self.worker.lock().unwrap() = None;
                    *self.process.lock().unwrap() = None;
                    let _ = cleanup_staging(&job.id);
                } else if self
                    .update_job(
                        &job.id,
                        |j| {
                            j.state = "failed".into();
                            j.retryable = true;
                            j.error = Some(error.clone());
                        },
                        true,
                    )
                    .is_err()
                {
                    let mut inner = self.inner.lock().unwrap();
                    if let Some(failed) = inner.store.jobs.iter_mut().find(|j| j.id == job.id) {
                        failed.state = "interrupted".into();
                        failed.retryable = true;
                        failed.error = Some(error);
                    }
                    inner.error = Some("Downloads stopped because queue state could not be saved. Fix state storage and retry.".into());
                    inner.stop_after_current = true;
                }
            }
            if self.closing.load(Ordering::Acquire) {
                break;
            }
        }
        {
            let mut inner = self.inner.lock().unwrap();
            inner.running = false;
            inner.current = None;
            inner.stop_after_current = false;
            inner.revision += 1;
        }
        self.publish();
        if let Some(app) = &self.app {
            // Coalesce a final refresh after any scan triggered by an earlier track.
            let app = app.clone();
            let shared = self.clone();
            thread::spawn(move || {
                for _ in 0..300 {
                    if shared.closing.load(Ordering::Acquire) {
                        return;
                    }
                    let library = app.state::<crate::library::LibraryManager>();
                    if !library.status().scanning {
                        let _ = library.start(app.clone());
                        return;
                    }
                    thread::sleep(Duration::from_millis(200));
                }
                shared.inner.lock().unwrap().error =
                    Some("Downloads finished; rescan the library to refresh new files.".into());
                shared.publish();
            });
        }
    }
    fn perform(&self, job: &Job) -> Result<(), String> {
        self.update_job(
            &job.id,
            |j| {
                j.state = "resolving".into();
                j.error = None;
            },
            true,
        )?;
        let intent = if let Some(intent) = job.promotion_intent.clone() {
            intent
        } else {
            cleanup_staging(&job.id)?;
            let directory = create_staging(&job.id)?;
            let mut worker = self.worker.lock().unwrap();
            let value = self.request(&mut worker, json!({"command":"download","catalog_id":job.track.catalog_id,"job_id":job.id,"staging_dir":directory}), |event| {
                if event["job_id"] != job.id { return; }
                let _ = self.update_job(&job.id, |j| {
                    if event["event"] == "stage" { if let Some(stage) = event["value"].as_str().filter(|s| active(s)) { j.state = stage.into(); } }
                    else { j.bytes_done = event["bytes_done"].as_u64().filter(|n| *n <= 4 * 1024 * 1024 * 1024); j.bytes_total = event["bytes_total"].as_u64().filter(|n| *n <= 4 * 1024 * 1024 * 1024); }
                }, false);
            }, Duration::from_secs(1900))?;
            drop(worker);
            let staged: StagedTrack =
                serde_json::from_value(value).map_err(|_| "Invalid staged audio response")?;
            if staged.catalog_id != job.track.catalog_id {
                return Err("Downloaded catalog identity mismatch".into());
            }
            self.update_job(&job.id, |j| j.state = "verifying".into(), true)?;
            let intent = self.service.prepare(&job.id, &staged)?;
            self.update_job(
                &job.id,
                |j| {
                    j.state = "organizing".into();
                    j.promotion_intent = Some(intent.clone());
                },
                true,
            )?;
            intent
        };
        let outcome = self.service.recover(&intent)?;
        self.update_job(
            &job.id,
            |j| {
                j.state = if outcome.duplicate {
                    "skipped"
                } else {
                    "completed"
                }
                .into();
                j.destination = Some(outcome.path);
                j.retryable = false;
                j.error =
                    (outcome.warnings > 0).then(|| "Existing sidecars were preserved.".into());
                j.promotion_intent = None;
            },
            true,
        )?;
        if cleanup_staging(&job.id).is_err() {
            let _ = self.update_job(
                &job.id,
                |j| {
                    j.error = Some(
                        "Audio was saved successfully; temporary files could not be cleaned up."
                            .into(),
                    )
                },
                true,
            );
        }
        if let Some(app) = &self.app {
            let _ = app
                .state::<crate::library::LibraryManager>()
                .start(app.clone());
        }
        Ok(())
    }
}
fn staging_path(id: &str) -> Result<PathBuf, String> {
    if !valid_job(id) {
        return Err("Invalid staging identifier".into());
    }
    let path = PathBuf::from(r"D:\Music\.downloads").join(id);
    for parent in path.ancestors() {
        if let Ok(meta) = fs::symlink_metadata(parent) {
            if crate::library::reparse(&meta) {
                return Err("Staging links are not allowed".into());
            }
        }
    }
    Ok(path)
}
fn create_staging(id: &str) -> Result<PathBuf, String> {
    let path = staging_path(id)?;
    fs::create_dir_all(&path).map_err(|_| "Cannot create staging directory")?;
    fs::write(path.join(".musicplayer-job"), id).map_err(|_| "Cannot mark staging ownership")?;
    Ok(path)
}
fn cleanup_staging(id: &str) -> Result<(), String> {
    let path = staging_path(id)?;
    if !path.exists() {
        return Ok(());
    }
    if fs::read_to_string(path.join(".musicplayer-job"))
        .ok()
        .as_deref()
        != Some(id)
    {
        return Err("Staging ownership could not be verified; files were preserved.".into());
    }
    // Resolve the exact job before recursive removal; never operate on the library root.
    let resolved = path
        .canonicalize()
        .map_err(|_| "Cannot resolve staging directory")?;
    let expected = format!(r"\\?\D:\Music\.downloads\{id}");
    if !resolved.to_string_lossy().eq_ignore_ascii_case(&expected) {
        return Err("Staging ownership path mismatch".into());
    }
    fs::remove_dir_all(resolved).map_err(|_| "Staging cleanup failed; files were preserved.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deployment_configuration_roundtrips_unicode_and_rejects_invalid_paths() {
        let directory = std::env::temp_dir().join(identifier("aac-config"));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("downloader-config.json");
        let root = r"C:\Users\Example User\音乐 AAC";
        crate::state::replace(
            &path,
            &crate::state::encode(&json!({"version":1,"deployment_root":root}), 16384).unwrap(),
        )
        .unwrap();
        assert_eq!(
            configured_deployment(&path).unwrap(),
            Some(PathBuf::from(root))
        );
        for value in [
            json!({"version":2,"deployment_root":root}),
            json!({"version":1,"deployment_root":"relative/root"}),
            json!({"version":1,"deployment_root":root,"token":"not-a-token"}),
        ] {
            crate::state::replace(&path, &crate::state::encode(&value, 16384).unwrap()).unwrap();
            assert!(configured_deployment(&path).is_err());
        }
        fs::write(&path, vec![b' '; 16385]).unwrap();
        assert!(configured_deployment(&path).is_err());
        fs::remove_file(&path).unwrap();
        assert!(configured_deployment(&path).unwrap().is_none());
        fs::remove_file(directory.join(".downloader-config.json.lock")).unwrap();
        fs::remove_dir(&directory).unwrap();
    }
    #[test]
    fn worker_job_object_terminates_descendants() {
        use windows::Win32::System::JobObjects::{
            JobObjectBasicAccountingInformation, QueryInformationJobObject,
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
        };
        let job = JobObject::new().unwrap();
        let script = "[Console]::ReadLine() | Out-Null; Start-Process -FilePath (Join-Path $PSHOME 'powershell.exe') -ArgumentList '-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 60' -WindowStyle Hidden; [Console]::WriteLine('ready'); Start-Sleep -Seconds 60";
        let mut process = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(0x08000000)
            .spawn()
            .unwrap();
        unsafe {
            AssignProcessToJobObject(job.0, HANDLE(process.as_raw_handle())).unwrap();
        }
        process
            .stdin
            .take()
            .unwrap()
            .write_all(b"launch\n")
            .unwrap();
        let stdout = process.stdout.take().unwrap();
        let (send, receive) = sync_channel(1);
        thread::spawn(move || {
            let mut line = String::new();
            let result = BufReader::new(stdout).read_line(&mut line);
            let _ = send.send((result.is_ok(), line));
        });
        assert_eq!(
            receive.recv_timeout(Duration::from_secs(15)).unwrap(),
            (true, "ready\r\n".into())
        );
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        unsafe {
            QueryInformationJobObject(
                Some(job.0),
                JobObjectBasicAccountingInformation,
                &mut accounting as *mut _ as *mut _,
                std::mem::size_of_val(&accounting) as u32,
                None,
            )
            .unwrap();
        }
        assert!(accounting.ActiveProcesses >= 2);
        job.terminate();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            unsafe {
                QueryInformationJobObject(
                    Some(job.0),
                    JobObjectBasicAccountingInformation,
                    &mut accounting as *mut _ as *mut _,
                    std::mem::size_of_val(&accounting) as u32,
                    None,
                )
                .unwrap();
            }
            if accounting.ActiveProcesses == 0 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Worker descendants remained alive after cancellation"
            );
            thread::sleep(Duration::from_millis(25));
        }
        process.wait().unwrap();
    }
    fn sample_job(state: &str) -> Job {
        Job {
            id: identifier("job"),
            batch_id: None,
            batch_title: None,
            track: TrackInput {
                catalog_id: "123456".into(),
                title: "Fixture".into(),
                available: true,
                ..TrackInput::default()
            },
            state: state.into(),
            bytes_done: None,
            bytes_total: None,
            destination: None,
            error: None,
            retryable: true,
            created_at: "1".into(),
            promotion_intent: None,
        }
    }
    #[test]
    fn interrupted_jobs_restore_stopped_and_retry_is_persisted() {
        let dir = std::env::temp_dir().join(identifier("musicplayer-restore-test"));
        let job = sample_job("downloading");
        let id = job.id.clone();
        fs::create_dir_all(&dir).unwrap();
        crate::state::replace(
            &dir.join("download-queue.json"),
            &crate::state::encode(
                &Store {
                    version: 1,
                    jobs: vec![job],
                },
                LIMIT,
            )
            .unwrap(),
        )
        .unwrap();
        let manager = DownloadManager::at(None, dir.clone());
        assert_eq!(manager.page(0)["jobs"][0]["state"], "interrupted");
        assert_eq!(manager.snapshot()["running"], false);
        assert!(manager.shared.worker.lock().unwrap().is_none());
        manager.retry(&id).unwrap();
        let persisted: Store = crate::state::load(&dir.join("download-queue.json"), LIMIT)
            .unwrap()
            .unwrap();
        assert_eq!(persisted.jobs[0].state, "queued");
        manager.cancel(&id).unwrap();
        assert_eq!(manager.page(0)["jobs"][0]["state"], "cancelled");
        manager.clear_finished().unwrap();
        assert_eq!(manager.page(0)["total"], 0);
        drop(manager);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn persistence_failure_preserves_prior_queue() {
        let dir = std::env::temp_dir().join(identifier("musicplayer-state-test"));
        let manager = DownloadManager::at(None, dir.clone());
        let job = sample_job("failed");
        let id = job.id.clone();
        manager.shared.inner.lock().unwrap().store.jobs.push(job);
        // A directory occupying the snapshot filename prevents atomic replacement.
        fs::create_dir(dir.join("download-queue.json")).unwrap();
        assert!(manager.retry(&id).is_err());
        assert_eq!(manager.page(0)["jobs"][0]["state"], "failed");
        drop(manager);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn cancellation_prevents_late_progress_and_promotion() {
        let dir = std::env::temp_dir().join(identifier("musicplayer-cancel-test"));
        let manager = DownloadManager::at(None, dir.clone());
        let job = sample_job("downloading");
        let id = job.id.clone();
        manager.shared.inner.lock().unwrap().store.jobs.push(job);
        manager.cancel(&id).unwrap();
        assert!(manager
            .shared
            .update_job(&id, |j| j.state = "verifying".into(), true)
            .is_err());
        assert_eq!(manager.page(0)["jobs"][0]["state"], "cancelled");
        drop(manager);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn scheduler_save_failure_leaves_a_retryable_job() {
        let dir = std::env::temp_dir().join(identifier("musicplayer-scheduler-test"));
        let manager = DownloadManager::at(None, dir.clone());
        let job = sample_job("queued");
        let id = job.id.clone();
        manager.shared.inner.lock().unwrap().store.jobs.push(job);
        fs::create_dir(dir.join("download-queue.json")).unwrap();
        manager.shared.clone().run();
        assert_eq!(manager.snapshot()["running"], false);
        assert_eq!(manager.page(0)["jobs"][0]["state"], "interrupted");
        assert_eq!(manager.page(0)["jobs"][0]["retryable"], true);
        assert!(manager.shared.worker.lock().unwrap().is_none());
        fs::remove_dir(dir.join("download-queue.json")).unwrap();
        manager.retry(&id).unwrap();
        assert_eq!(manager.page(0)["jobs"][0]["state"], "queued");
        drop(manager);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn scheduler_recovers_an_ordered_album_batch_without_launching_python() {
        use sha2::{Digest, Sha256};
        let dir = std::env::temp_dir().join(identifier("musicplayer-batch-test"));
        let mut manager = DownloadManager::at(None, dir.clone());
        let library = dir.join("fixture-library");
        Arc::get_mut(&mut manager.shared).unwrap().service =
            PromotionService::new(library.clone(), PathBuf::new());
        let mut jobs = vec![];
        for index in 1..=2 {
            let mut job = sample_job("queued");
            job.track.catalog_id = format!("123{index}");
            job.batch_id = Some("batch-fixture".into());
            let source = library.join(".downloads").join(&job.id).join("track.m4a");
            let target = library.join(format!("Album/{index}.m4a"));
            fs::create_dir_all(source.parent().unwrap()).unwrap();
            fs::write(&source, b"verified fixture").unwrap();
            job.promotion_intent = Some(PromotionIntent {
                job_id: job.id.clone(),
                catalog_id: job.track.catalog_id.clone(),
                audio: promotion::FileMove {
                    source: source.to_string_lossy().into_owned(),
                    target: target.to_string_lossy().into_owned(),
                    size: 16,
                    sha256: format!("{:x}", Sha256::digest(b"verified fixture")),
                },
                sidecars: vec![],
                duplicate: false,
            });
            jobs.push(job);
        }
        manager.shared.inner.lock().unwrap().store.jobs = jobs;
        manager.shared.clone().run();
        let page = manager.page(0);
        assert_eq!(page["jobs"][0]["state"], "completed");
        assert_eq!(page["jobs"][1]["state"], "completed");
        assert_eq!(page["batches"][0]["completed"], 2);
        assert!(manager.shared.worker.lock().unwrap().is_none());
        drop(manager);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn protocol_lines_are_bounded_and_complete() {
        assert!(read_line(&mut &b"{\"event\":\"ready\"}\n"[..])
            .unwrap()
            .is_some());
        assert!(read_line(&mut &b"partial"[..]).is_err());
        assert!(read_line(&mut vec![b'x'; 65537].as_slice()).is_err());
    }
    #[test]
    fn queue_recovery_and_exclusive_owner() {
        let dir = std::env::temp_dir().join(identifier("musicplayer-download-test"));
        let manager = DownloadManager::at(None, dir.clone());
        assert_eq!(manager.snapshot()["running"], false);
        let second = DownloadManager::at(None, dir.clone());
        assert_eq!(second.snapshot()["available"], false);
        assert!(manager.shared.worker.lock().unwrap().is_none());
        drop(second);
        drop(manager);
        crate::state::replace(&dir.join("download-queue.json"), b"truncated").unwrap();
        let recovered = DownloadManager::at(None, dir.clone());
        assert!(recovered.snapshot()["error"].is_string());
        assert_eq!(recovered.page(0)["total"], 0);
        drop(recovered);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn rejects_invalid_track_metadata_and_paths() {
        let track = TrackInput {
            catalog_id: "123".into(),
            title: "Track".into(),
            available: true,
            ..TrackInput::default()
        };
        assert!(track.valid(true));
        let mut bad = track;
        bad.artwork_url = Some("https://example.com/private".into());
        assert!(!bad.valid(true));
        assert!(staging_path("../escape").is_err());
        assert!(staging_path("").is_err());
        assert!(!Store {
            version: 2,
            jobs: vec![]
        }
        .validate());
    }
}
