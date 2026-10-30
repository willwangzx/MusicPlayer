use lofty::{
    config::{apply_global_options, GlobalOptions, ParseOptions},
    file::{AudioFile, TaggedFileExt},
    prelude::{Accessor, ItemKey},
    probe::Probe,
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, Metadata},
    io::{self, Read, Seek, SeekFrom},
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter};

const ROOT: &str = r"D:\Music";
const MAX_TRACKS: usize = 100_000;
const MAX_ENTRIES: usize = 500_000;
const MAX_DEPTH: usize = 64;
const PAGE_SIZE: usize = 100;
const EVENT: &str = "musicplayer://library-state";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    pub id: String,
    pub path: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub disc: u32,
    pub number: u32,
    pub duration_ms: u64,
    pub metadata_warning: bool,
}
#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanStatus {
    pub scanning: bool,
    pub revision: u64,
    pub visited: usize,
    pub tracks: usize,
    pub warnings: usize,
    pub message: String,
}
#[derive(Serialize)]
pub struct TrackPage {
    pub tracks: Vec<Track>,
    pub total: usize,
    pub revision: u64,
}
const CACHE_LIMIT: usize = 64 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
struct Cache {
    version: u32,
    tracks: Vec<Track>,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Album {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub track_count: usize,
    pub duration_ms: u64,
}
#[derive(Serialize)]
pub struct AlbumPage {
    pub albums: Vec<Album>,
    pub total: usize,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentTrackInfo {
    pub track: Option<Track>,
    pub album_id: Option<String>,
}
fn album_id(track: &Track) -> String {
    // Unknown albums are separated by folder rather than merging unrelated files.
    serde_json::to_string(&(
        &track.album_artist,
        &track.album,
        if track.album == "Unknown Album" {
            Path::new(&track.path)
                .parent()
                .and_then(Path::to_str)
                .unwrap_or("")
        } else {
            ""
        },
    ))
    .unwrap()
}
fn sort_tracks(tracks: &mut [Track]) {
    tracks.sort_by(|a, b| {
        (
            &a.album_artist,
            &a.album,
            a.disc,
            a.number,
            &a.title,
            &a.path,
        )
            .cmp(&(
                &b.album_artist,
                &b.album,
                b.disc,
                b.number,
                &b.title,
                &b.path,
            ))
    });
}
fn valid_cache(cache: &Cache) -> bool {
    cache.version == 1
        && cache.tracks.len() <= MAX_TRACKS
        && cache.tracks.iter().all(|t| {
            t.id == t.path
                && t.path.len() <= 4096
                && Path::new(&t.path).is_absolute()
                && t.path.to_lowercase().starts_with("d:\\music\\")
                && !Path::new(&t.path).components().any(|c| {
                    matches!(c, std::path::Component::ParentDir)
                        || c.as_os_str()
                            .to_string_lossy()
                            .eq_ignore_ascii_case(".downloads")
                })
                && [&t.title, &t.artist, &t.album, &t.album_artist]
                    .iter()
                    .all(|v| v.chars().count() <= 256)
        })
        && {
            let mut ids = std::collections::HashSet::new();
            cache.tracks.iter().all(|t| ids.insert(&t.id))
        }
}
#[derive(Default)]
struct Library {
    tracks: Vec<Track>,
    preview: Option<Vec<Track>>,
    status: ScanStatus,
}
#[derive(Default)]
pub struct LibraryManager {
    library: Arc<Mutex<Library>>,
    cancel: Arc<AtomicBool>,
}
impl LibraryManager {
    pub fn track_info(&self, path: &str) -> CurrentTrackInfo {
        let state = self.library.lock().unwrap();
        let track = state
            .tracks
            .iter()
            .find(|track| {
                crate::playback::same_windows_path(Path::new(&track.path), Path::new(path))
            })
            .cloned();
        let album_id = track.as_ref().map(album_id);
        CurrentTrackInfo { track, album_id }
    }
    pub fn restored() -> Self {
        let loaded = crate::state::directory()
            .and_then(|dir| crate::state::load::<Cache>(&dir.join("library.json"), CACHE_LIMIT));
        Self::from_cache(loaded)
    }
    fn from_cache(loaded: Result<Option<Cache>, String>) -> Self {
        let manager = Self::default();
        let mut state = manager.library.lock().unwrap();
        match loaded {
            Ok(Some(mut cache)) if valid_cache(&cache) => {
                sort_tracks(&mut cache.tracks);
                state.tracks = cache.tracks;
                state.status.tracks = state.tracks.len();
                state.status.revision = 1;
                state.status.message = "Saved library loaded; rescan to refresh".into();
            }
            Ok(None) => state.status.message = "Scan your library to get started".into(),
            _ => state.status.message = "Saved library could not be restored; please rescan".into(),
        }
        drop(state);
        manager
    }
    pub fn albums(&self, query: &str, offset: usize) -> Result<AlbumPage, String> {
        if query.len() > 512 {
            return Err("Search is limited to 512 bytes".into());
        }
        let state = self.library.lock().unwrap();
        let mut albums = std::collections::BTreeMap::<String, Album>::new();
        let query = query.to_lowercase();
        for track in state.preview.as_deref().unwrap_or(&state.tracks) {
            if !query.is_empty()
                && !track.album.to_lowercase().contains(&query)
                && !track.album_artist.to_lowercase().contains(&query)
            {
                continue;
            }
            let id = album_id(track);
            let album = albums.entry(id.clone()).or_insert_with(|| Album {
                id,
                title: track.album.clone(),
                artist: track.album_artist.clone(),
                track_count: 0,
                duration_ms: 0,
            });
            album.track_count += 1;
            album.duration_ms = album.duration_ms.saturating_add(track.duration_ms);
        }
        Ok(AlbumPage {
            total: albums.len(),
            albums: albums.into_values().skip(offset).take(PAGE_SIZE).collect(),
        })
    }
    pub fn album_tracks(&self, id: &str, offset: usize) -> Result<TrackPage, String> {
        if id.len() > 8192 {
            return Err("Invalid album selection".into());
        }
        let state = self.library.lock().unwrap();
        let matches = state
            .preview
            .as_deref()
            .unwrap_or(&state.tracks)
            .iter()
            .filter(|track| album_id(track) == id);
        let total = matches.clone().count();
        if total == 0 {
            return Err("Album is no longer in the library".into());
        }
        Ok(TrackPage {
            tracks: matches.skip(offset).take(PAGE_SIZE).cloned().collect(),
            total,
            revision: state.status.revision,
        })
    }
    pub fn album_paths(&self, id: &str) -> Result<Vec<String>, String> {
        if id.len() > 8192 {
            return Err("Invalid album selection".into());
        }
        let state = self.library.lock().unwrap();
        let paths: Vec<_> = state
            .preview
            .as_deref()
            .unwrap_or(&state.tracks)
            .iter()
            .filter(|t| album_id(t) == id)
            .map(|t| t.path.clone())
            .collect();
        if paths.is_empty() {
            return Err("Album is no longer in the library".into());
        }
        if paths.len() > 4096 {
            return Err("Album exceeds the 4096-track queue limit".into());
        }
        Ok(paths)
    }
    /// Use the committed index, never a partial scan preview, for radio playback.
    pub fn playback_paths(&self) -> Vec<String> {
        self.library
            .lock()
            .unwrap()
            .tracks
            .iter()
            .map(|track| track.path.clone())
            .collect()
    }
    pub fn status(&self) -> ScanStatus {
        self.library.lock().unwrap().status.clone()
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    pub fn recycle_track(
        &self,
        app: AppHandle,
        playback: &crate::playback::PlaybackManager,
        path: String,
    ) -> Result<(), String> {
        let mut library = self.library.lock().unwrap();
        if library.status.scanning {
            return Err("Tracks cannot be recycled while a library scan is running".into());
        }
        let selection = normalized_selection_path(&path);
        let mut matches = library.tracks.iter().enumerate().filter(|(_, track)| {
            track.id == track.path && normalized_selection_path(&track.path) == selection
        });
        let index = matches
            .next()
            .map(|(index, _)| index)
            .ok_or_else(|| "The exact track is no longer in the indexed library".to_string())?;
        if matches.next().is_some() {
            return Err("The selected path is ambiguous in the indexed library".into());
        }
        let indexed_path = library.tracks[index].path.clone();

        // Keep the library lock until playback has released the selected file,
        // recycled it, and removed every matching queue entry. This also keeps a
        // new scan from racing the filesystem operation.
        playback.recycle_path(indexed_path)?;
        let removed = library.tracks.remove(index);
        library.status.revision = library.status.revision.saturating_add(1);
        library.status.tracks = library.tracks.len();
        library.status.warnings = library
            .status
            .warnings
            .saturating_sub(usize::from(removed.metadata_warning));

        let save_error = crate::state::directory()
            .and_then(|dir| {
                let bytes = crate::state::encode(
                    &Cache {
                        version: 1,
                        tracks: library.tracks.clone(),
                    },
                    CACHE_LIMIT,
                )?;
                crate::state::replace(&dir.join("library.json"), &bytes)
            })
            .err();
        library.status.message = save_error
            .map(|error| format!("Track recycled; library not saved: {error}"))
            .unwrap_or_else(|| "Track moved to the Recycle Bin".into());
        let snapshot = library.status.clone();
        drop(library);
        let _ = app.emit(EVENT, snapshot);
        Ok(())
    }
    pub fn page(&self, query: &str, offset: usize) -> Result<TrackPage, String> {
        if query.len() > 512 {
            return Err("Search is limited to 512 bytes".into());
        }
        let query = query.to_lowercase();
        let library = self.library.lock().unwrap();
        let matches = library
            .preview
            .as_deref()
            .unwrap_or(&library.tracks)
            .iter()
            .filter(|t| {
                query.is_empty()
                    || [&t.title, &t.artist, &t.album, &t.album_artist]
                        .iter()
                        .any(|s| s.to_lowercase().contains(&query))
            });
        let total = matches.clone().count();
        Ok(TrackPage {
            tracks: matches.skip(offset).take(PAGE_SIZE).cloned().collect(),
            total,
            revision: library.status.revision,
        })
    }
    pub fn start(&self, app: AppHandle) -> Result<(), String> {
        {
            let mut library = self.library.lock().unwrap();
            if library.status.scanning {
                return Err("A library scan is already running".into());
            }
            self.cancel.store(false, Ordering::Relaxed);
            library.preview = Some(Vec::new());
            library.status.scanning = true;
            library.status.visited = 0;
            library.status.warnings = 0;
            library.status.message = "Scanning D:\\Music".into();
        }
        let library = Arc::clone(&self.library);
        let cancel = Arc::clone(&self.cancel);
        let result = std::thread::Builder::new()
            .name("musicplayer-library".into())
            .spawn(move || {
                let mut last = Instant::now();
                let mut status = ScanStatus {
                    scanning: true,
                    ..Default::default()
                };
                let mut tracks = Vec::new();
                let result = scan(
                    Path::new(ROOT),
                    &cancel,
                    &mut tracks,
                    &mut status,
                    &mut |progress, found| {
                        if last.elapsed() >= Duration::from_millis(250) {
                            let snapshot = {
                                let mut state = library.lock().unwrap();
                                if let Some(preview) = &mut state.preview {
                                    let from = preview.len();
                                    preview.extend_from_slice(
                                        &found[from..found.len().min(from + 512)],
                                    );
                                }
                                state.status.revision += 1;
                                state.status.visited = progress.visited;
                                state.status.warnings = progress.warnings;
                                state.status.message =
                                    format!("Scanning: {} tracks found", progress.tracks);
                                state.status.clone()
                            };
                            let _ = app.emit(EVENT, snapshot);
                            last = Instant::now();
                        }
                    },
                );
                sort_tracks(&mut tracks);
                let save_error = if result.is_ok() {
                    crate::state::directory()
                        .and_then(|dir| {
                            let bytes = crate::state::encode(
                                &Cache {
                                    version: 1,
                                    tracks: tracks.clone(),
                                },
                                CACHE_LIMIT,
                            )?;
                            crate::state::replace(&dir.join("library.json"), &bytes)
                        })
                        .err()
                } else {
                    None
                };
                let snapshot = {
                    let mut state = library.lock().unwrap();
                    state.preview = None;
                    status.scanning = false;
                    status.revision = state.status.revision + 1;
                    status.message = match result {
                        Ok(()) => {
                            state.tracks = tracks;
                            save_error
                                .map(|e| format!("Scan complete; library not saved: {e}"))
                                .unwrap_or_else(|| "Scan complete; library saved".into())
                        }
                        Err(message) => message,
                    };
                    status.tracks = state.tracks.len();
                    state.status = status;
                    state.status.clone()
                };
                let _ = app.emit(EVENT, snapshot);
            });
        if result.is_err() {
            let mut library = self.library.lock().unwrap();
            library.preview = None;
            library.status.scanning = false;
            library.status.message = "Unable to start scan".into();
            return Err(library.status.message.clone());
        }
        Ok(())
    }
}

fn normalized_selection_path(value: &str) -> String {
    let mut value = value.replace('/', "\\");
    if let Some(stripped) = value.strip_prefix(r"\\?\") {
        value = stripped.to_string();
    }
    value.trim_end_matches('\\').to_ascii_lowercase()
}
impl Drop for LibraryManager {
    fn drop(&mut self) {
        self.cancel();
    }
}

pub(crate) fn reparse(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}
fn supported(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("mp3" | "flac" | "m4a" | "aac" | "wav" | "ogg" | "opus" | "aiff" | "aif" | "wma")
    )
}
fn scan(
    root: &Path,
    cancel: &AtomicBool,
    tracks: &mut Vec<Track>,
    status: &mut ScanStatus,
    progress: &mut impl FnMut(&ScanStatus, &[Track]),
) -> Result<(), String> {
    let metadata = fs::symlink_metadata(root)
        .map_err(|_| "Library folder is unavailable; previous library retained".to_string())?;
    if !metadata.is_dir() || reparse(&metadata) {
        return Err("Library root must be a regular local directory".into());
    }
    visit(root, 0, cancel, tracks, status, progress)?;
    if cancel.load(Ordering::Relaxed) {
        return Err("Scan cancelled; previous library retained".into());
    }
    Ok(())
}
fn visit(
    dir: &Path,
    depth: usize,
    cancel: &AtomicBool,
    tracks: &mut Vec<Track>,
    status: &mut ScanStatus,
    progress: &mut impl FnMut(&ScanStatus, &[Track]),
) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err("Directory depth limit reached; previous library retained".into());
    }
    let entries = match fs::read_dir(dir) {
        Ok(v) => v,
        Err(_) => {
            if depth == 0 {
                return Err("Library folder cannot be read; previous library retained".into());
            }
            status.warnings += 1;
            return Ok(());
        }
    };
    for entry in entries {
        if cancel.load(Ordering::Relaxed) {
            return Err("Scan cancelled; previous library retained".into());
        }
        status.visited += 1;
        if status.visited > MAX_ENTRIES {
            return Err("Scan entry limit reached; previous library retained".into());
        }
        progress(status, tracks);
        let entry = match entry {
            Ok(v) => v,
            Err(_) => {
                status.warnings += 1;
                continue;
            }
        };
        if entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case(".downloads")
        {
            continue;
        }
        let path = entry.path();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(v) => v,
            Err(_) => {
                status.warnings += 1;
                continue;
            }
        };
        if reparse(&metadata) {
            continue;
        }
        if metadata.is_dir() {
            visit(&path, depth + 1, cancel, tracks, status, progress)?;
        } else if metadata.is_file() && supported(&path) {
            if tracks.len() >= MAX_TRACKS {
                return Err("Track limit reached; previous library retained".into());
            }
            if path.to_str().is_none() || path.to_string_lossy().len() > 4096 {
                status.warnings += 1;
                continue;
            }
            let track = read_track(&path, cancel);
            status.warnings += usize::from(track.metadata_warning);
            tracks.push(track);
            status.tracks = tracks.len();
        }
    }
    Ok(())
}
// A cumulative I/O budget and deadline prevent malformed metadata from causing
// unbounded reads. Artwork is not decoded or sent to the frontend in this slice.
pub(crate) struct BudgetReader<'a> {
    file: File,
    remaining: u64,
    deadline: Instant,
    cancel: &'a AtomicBool,
}
impl<'a> BudgetReader<'a> {
    pub(crate) fn new(file: File, cancel: &'a AtomicBool) -> Self {
        Self {
            file,
            remaining: 16 * 1024 * 1024,
            deadline: Instant::now() + Duration::from_secs(2),
            cancel,
        }
    }
}
impl Read for BudgetReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0
            || Instant::now() > self.deadline
            || self.cancel.load(Ordering::Relaxed)
        {
            return Err(io::Error::other("metadata read budget exceeded"));
        }
        let len = buffer.len().min(self.remaining as usize);
        let count = self.file.read(&mut buffer[..len])?;
        self.remaining -= count as u64;
        Ok(count)
    }
}
impl Seek for BudgetReader<'_> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.file.seek(from)
    }
}
fn bounded(value: &str) -> String {
    value
        .chars()
        .take(256)
        .collect::<String>()
        .trim()
        .to_string()
}
fn read_track(path: &Path, cancel: &AtomicBool) -> Track {
    apply_global_options(GlobalOptions::new().allocation_limit(2 * 1024 * 1024));
    let path_text = path.to_string_lossy().into_owned();
    let mut track = Track {
        id: path_text.clone(),
        path: path_text,
        title: bounded(&path.file_stem().unwrap_or_default().to_string_lossy()),
        artist: "Unknown Artist".into(),
        album: "Unknown Album".into(),
        album_artist: "Unknown Artist".into(),
        disc: 0,
        number: 0,
        duration_ms: 0,
        metadata_warning: false,
    };
    let parsed = (|| {
        let file = File::open(path).ok()?;
        let reader = BudgetReader {
            file,
            remaining: 16 * 1024 * 1024,
            deadline: Instant::now() + Duration::from_secs(2),
            cancel,
        };
        Probe::new(reader)
            .guess_file_type()
            .ok()?
            .options(ParseOptions::new().read_cover_art(false))
            .read()
            .ok()
    })();
    if let Some(file) = parsed {
        track.duration_ms = file.properties().duration().as_millis() as u64;
        if let Some(tag) = file.primary_tag().or_else(|| file.first_tag()) {
            if let Some(value) = tag.title().map(|v| bounded(&v)).filter(|v| !v.is_empty()) {
                track.title = value;
            }
            if let Some(value) = tag.artist().map(|v| bounded(&v)).filter(|v| !v.is_empty()) {
                track.artist = value;
            }
            if let Some(value) = tag.album().map(|v| bounded(&v)).filter(|v| !v.is_empty()) {
                track.album = value;
            }
            track.album_artist = tag
                .get_string(ItemKey::AlbumArtist)
                .map(bounded)
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| track.artist.clone());
            track.disc = tag.disk().unwrap_or(0);
            track.number = tag.track().unwrap_or(0);
        }
    } else {
        track.metadata_warning = true;
    }
    track
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scan_excludes_staging_and_preserves_corrupt_files() {
        let root = std::env::temp_dir().join(format!(
            "musicplayer-library-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join(".DoWnLoAdS")).unwrap();
        fs::write(root.join(".DoWnLoAdS/hidden.mp3"), b"hidden").unwrap();
        fs::write(root.join("track.FLAC"), b"corrupt fixture").unwrap();
        fs::write(root.join("notes.txt"), b"not audio").unwrap();
        #[cfg(windows)]
        {
            let outcome = std::process::Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command", "New-Item -ItemType Junction -Path $env:MP_TEST_LINK -Target $env:MP_TEST_TARGET -ErrorAction Stop | Out-Null"])
                .env("MP_TEST_LINK", root.join("staging-alias"))
                .env("MP_TEST_TARGET", root.join(".DoWnLoAdS"))
                .output().unwrap();
            assert!(outcome.status.success(), "fixture junction creation failed");
        }
        let mut tracks = Vec::new();
        let mut status = ScanStatus::default();
        scan(
            &root,
            &AtomicBool::new(false),
            &mut tracks,
            &mut status,
            &mut |_, _| {},
        )
        .unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].title, "track");
        assert!(tracks[0].metadata_warning);
        assert_eq!(
            fs::read(root.join("track.FLAC")).unwrap(),
            b"corrupt fixture"
        );
        assert_eq!(
            fs::read(root.join(".DoWnLoAdS/hidden.mp3")).unwrap(),
            b"hidden"
        );
        assert!(scan(
            &root,
            &AtomicBool::new(true),
            &mut Vec::new(),
            &mut ScanStatus::default(),
            &mut |_, _| {}
        )
        .is_err());
        #[cfg(windows)]
        fs::remove_dir(root.join("staging-alias")).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    #[ignore = "read-only smoke scan of the user's actual D:\\Music library"]
    fn real_library_scan() {
        let mut tracks = Vec::new();
        let mut status = ScanStatus::default();
        scan(
            Path::new(ROOT),
            &AtomicBool::new(false),
            &mut tracks,
            &mut status,
            &mut |_, _| {},
        )
        .unwrap();
        assert!(tracks
            .iter()
            .all(|t| !Path::new(&t.path).components().any(|c| c
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(".downloads"))));
        println!(
            "Scanned {} entries, {} tracks, {} metadata/read warnings",
            status.visited,
            tracks.len(),
            status.warnings
        );
    }

    #[test]
    fn cached_library_restores_album_order_and_rejects_invalid_data() {
        let track = |number| Track {
            id: format!(r"D:\Music\Example\{number}.flac"),
            path: format!(r"D:\Music\Example\{number}.flac"),
            title: format!("Track {number}"),
            artist: "Artist".into(),
            album: "Album".into(),
            album_artist: "Artist".into(),
            disc: 1,
            number,
            duration_ms: 1000,
            metadata_warning: false,
        };
        let cache = Cache {
            version: 1,
            tracks: vec![track(2), track(1)],
        };
        let bytes = crate::state::encode(&cache, CACHE_LIMIT).unwrap();
        let decoded: Cache = serde_json::from_slice(&bytes).unwrap();
        let manager = LibraryManager::from_cache(Ok(Some(decoded)));
        assert_eq!(manager.status().tracks, 2);
        let canonical = manager.track_info(r"\\?\D:\MUSIC\example\1.FLAC");
        assert_eq!(canonical.track.unwrap().number, 1);
        assert!(canonical.album_id.is_some());
        assert!(manager
            .track_info("d:/music/Example/2.flac")
            .track
            .is_some());
        assert!(manager
            .track_info(r"D:\Projects\MusicPlayer\gapless-assets\tone.wav")
            .track
            .is_none());
        assert!(manager
            .track_info(r"D:\Music\.downloads\hidden.flac")
            .track
            .is_none());
        let albums = manager.albums("artist", 0).unwrap();
        assert_eq!(albums.total, 1);
        assert_eq!(albums.albums[0].track_count, 2);
        let paths = manager.album_paths(&albums.albums[0].id).unwrap();
        assert!(paths[0].ends_with("1.flac"));
        assert!(paths[1].ends_with("2.flac"));
        // Radio must use the committed library even during a partial scan.
        manager.library.lock().unwrap().preview = Some(vec![track(3)]);
        assert_eq!(manager.playback_paths(), paths);
        manager.library.lock().unwrap().preview = None;
        let detail = manager.album_tracks(&albums.albums[0].id, 0).unwrap();
        assert_eq!(detail.total, 2);
        assert_eq!(
            detail.tracks.iter().map(|t| t.number).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(
            manager
                .album_tracks(&albums.albums[0].id, 1)
                .unwrap()
                .tracks[0]
                .number,
            2
        );
        assert!(manager.album_tracks("missing album", 0).is_err());
        assert_eq!(manager.page("track 1", 0).unwrap().total, 1);
        let mut invalid = Cache {
            version: 2,
            tracks: vec![track(1)],
        };
        assert!(!valid_cache(&invalid));
        invalid.version = 1;
        invalid.tracks[0].path = r"D:\Music\.downloads\hidden.flac".into();
        invalid.tracks[0].id = invalid.tracks[0].path.clone();
        assert!(!valid_cache(&invalid));
        assert_eq!(
            LibraryManager::from_cache(Ok(Some(invalid)))
                .status()
                .tracks,
            0
        );
    }

    #[test]
    fn missing_root_and_bounded_pages() {
        assert!(scan(
            Path::new(r"Z:\musicplayer-nonexistent-fixture"),
            &AtomicBool::new(false),
            &mut Vec::new(),
            &mut ScanStatus::default(),
            &mut |_, _| {}
        )
        .is_err());
        let manager = LibraryManager::default();
        assert!(manager.page(&"x".repeat(513), 0).is_err());
        assert_eq!(manager.page("", usize::MAX).unwrap().total, 0);
    }

    #[test]
    fn indexed_selection_normalizes_queue_canonical_prefix_case_and_separators() {
        let indexed = r"D:\Music\Album\Song.FLAC";
        assert_eq!(
            normalized_selection_path(indexed),
            normalized_selection_path(r"\\?\d:/music/album/song.flac")
        );
        assert_ne!(
            normalized_selection_path(indexed),
            normalized_selection_path(r"D:\Music\Other\Song.FLAC")
        );
    }
}
