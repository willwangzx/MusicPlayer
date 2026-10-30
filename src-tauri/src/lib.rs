pub mod artwork;
mod audio_analysis;
mod background;
pub mod downloads;
pub mod library;
pub mod media_controls;
pub mod playback;
pub mod recycle;
pub mod state;
mod tray;
mod webview_memory;

#[tauri::command]
fn library_status(manager: tauri::State<'_, library::LibraryManager>) -> library::ScanStatus {
    manager.status()
}
#[tauri::command]
fn scan_library(
    app: tauri::AppHandle,
    manager: tauri::State<'_, library::LibraryManager>,
) -> Result<(), String> {
    manager.start(app)
}
#[tauri::command]
fn cancel_library_scan(manager: tauri::State<'_, library::LibraryManager>) {
    manager.cancel();
}
#[tauri::command]
fn library_tracks(
    manager: tauri::State<'_, library::LibraryManager>,
    query: String,
    offset: usize,
) -> Result<library::TrackPage, String> {
    manager.page(&query, offset)
}

#[tauri::command]
fn library_albums(
    manager: tauri::State<'_, library::LibraryManager>,
    query: String,
    offset: usize,
) -> Result<library::AlbumPage, String> {
    manager.albums(&query, offset)
}
#[tauri::command]
fn library_album_tracks(
    manager: tauri::State<'_, library::LibraryManager>,
    id: String,
    offset: usize,
) -> Result<library::TrackPage, String> {
    manager.album_tracks(&id, offset)
}
#[tauri::command]
fn play_album(
    library: tauri::State<'_, library::LibraryManager>,
    manager: tauri::State<'_, PlaybackManager>,
    id: String,
) -> Result<(), String> {
    manager.set_queue(library.album_paths(&id)?, 0)
}

use playback::{PlaybackManager, PlaybackSnapshot};
use tauri::{Emitter, Manager};

#[tauri::command]
async fn recycle_library_track(app: tauri::AppHandle, path: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<library::LibraryManager>().recycle_track(
            app.clone(),
            &app.state::<PlaybackManager>(),
            path,
        )
    })
    .await
    .map_err(|_| "Recycle operation stopped".to_string())?
}

async fn download_call<T: Send + 'static>(
    manager: downloads::DownloadManager,
    operation: impl FnOnce(&downloads::DownloadManager) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(move || operation(&manager))
        .await
        .map_err(|_| "Download operation stopped".to_string())?
}
#[tauri::command]
async fn apple_account_status(
    manager: tauri::State<'_, downloads::DownloadManager>,
) -> Result<serde_json::Value, String> {
    download_call(manager.inner().clone(), |m| {
        m.catalog(serde_json::json!({"command":"check"}))
    })
    .await
}
#[tauri::command]
async fn apple_search(
    manager: tauri::State<'_, downloads::DownloadManager>,
    kind: String,
    query: String,
) -> Result<serde_json::Value, String> {
    if !matches!(kind.as_str(), "songs" | "albums") || query.len() > 512 {
        return Err("Invalid Apple Music search".into());
    }
    download_call(manager.inner().clone(), move |m| {
        m.catalog(serde_json::json!({"command":"search","kind":kind,"query":query}))
    })
    .await
}
#[tauri::command]
async fn apple_preview_album(
    manager: tauri::State<'_, downloads::DownloadManager>,
    catalog_id: Option<String>,
    url: Option<String>,
) -> Result<serde_json::Value, String> {
    if url.as_ref().is_some_and(|s| s.len() > 2048)
        || catalog_id.as_ref().is_some_and(|s| s.len() > 64)
    {
        return Err("Album identifier exceeds its limit".into());
    }
    download_call(manager.inner().clone(), move |m| {
        m.catalog(serde_json::json!({"command":"preview_album","catalog_id":catalog_id,"url":url}))
    })
    .await
}
#[tauri::command]
async fn apple_preview_playlist(
    manager: tauri::State<'_, downloads::DownloadManager>,
    url: String,
) -> Result<serde_json::Value, String> {
    if url.len() > 2048 {
        return Err("Playlist URL exceeds its limit".into());
    }
    download_call(manager.inner().clone(), move |m| {
        m.catalog(serde_json::json!({"command":"preview_playlist","url":url}))
    })
    .await
}
#[tauri::command]
async fn enqueue_downloads(
    manager: tauri::State<'_, downloads::DownloadManager>,
    request: downloads::EnqueueRequest,
) -> Result<serde_json::Value, String> {
    download_call(manager.inner().clone(), move |m| m.enqueue(request)).await
}
#[tauri::command]
fn download_queue(
    manager: tauri::State<'_, downloads::DownloadManager>,
    offset: usize,
) -> serde_json::Value {
    manager.page(offset)
}
#[tauri::command]
fn start_downloads(manager: tauri::State<'_, downloads::DownloadManager>) -> Result<(), String> {
    manager.start()
}
#[tauri::command]
fn stop_downloads_after_current(manager: tauri::State<'_, downloads::DownloadManager>) {
    manager.stop_after_current();
}
#[tauri::command]
async fn cancel_download(
    manager: tauri::State<'_, downloads::DownloadManager>,
    job_id: String,
) -> Result<(), String> {
    download_call(manager.inner().clone(), move |m| m.cancel(&job_id)).await
}
#[tauri::command]
async fn retry_download(
    manager: tauri::State<'_, downloads::DownloadManager>,
    job_id: String,
) -> Result<(), String> {
    download_call(manager.inner().clone(), move |m| m.retry(&job_id)).await
}
#[tauri::command]
async fn clear_finished_downloads(
    manager: tauri::State<'_, downloads::DownloadManager>,
) -> Result<(), String> {
    download_call(manager.inner().clone(), |m| m.clear_finished()).await
}

#[tauri::command]
fn media_controls_status(
    manager: tauri::State<'_, media_controls::MediaControls>,
) -> media_controls::MediaControlsStatus {
    manager.status()
}

#[tauri::command]
fn playback_status(manager: tauri::State<'_, PlaybackManager>) -> PlaybackSnapshot {
    manager.snapshot()
}

#[tauri::command]
fn playback_analysis(
    app: tauri::AppHandle,
    analysis: tauri::State<'_, audio_analysis::AudioAnalysis>,
    player: tauri::State<'_, PlaybackManager>,
    enabled: bool,
) -> audio_analysis::AnalysisSnapshot {
    let visible = app.get_webview_window("main").is_some_and(|window| {
        window.is_visible().unwrap_or(false) && !window.is_minimized().unwrap_or(true)
    });
    analysis.poll(enabled && visible && player.snapshot().state == "playing")
}

#[tauri::command]
fn current_track_info(
    library: tauri::State<'_, library::LibraryManager>,
    path: String,
) -> library::CurrentTrackInfo {
    library.track_info(&path)
}

#[tauri::command]
fn set_playback_queue(
    manager: tauri::State<'_, PlaybackManager>,
    paths: Vec<String>,
    start_index: Option<usize>,
) -> Result<(), String> {
    manager.set_queue(paths, start_index.unwrap_or(0))
}

#[tauri::command]
fn toggle_playback(manager: tauri::State<'_, PlaybackManager>) -> Result<(), String> {
    manager.toggle_pause()
}

#[tauri::command]
fn next_track(manager: tauri::State<'_, PlaybackManager>) -> Result<(), String> {
    manager.next()
}

#[tauri::command]
fn previous_track(manager: tauri::State<'_, PlaybackManager>) -> Result<(), String> {
    manager.previous()
}

#[tauri::command]
fn stop_playback(manager: tauri::State<'_, PlaybackManager>) -> Result<(), String> {
    manager.stop()
}

#[tauri::command]
fn seek_playback(
    manager: tauri::State<'_, PlaybackManager>,
    seconds: f64,
    revision: u64,
    current: usize,
) -> Result<(), String> {
    manager.seek(seconds, revision, current)
}
#[tauri::command]
fn set_volume(manager: tauri::State<'_, PlaybackManager>, value: f64) -> Result<(), String> {
    manager.volume(value)
}
#[tauri::command]
fn playback_queue(
    manager: tauri::State<'_, PlaybackManager>,
    offset: usize,
) -> Result<playback::QueuePage, String> {
    manager.queue_page(offset)
}
#[tauri::command]
async fn set_random_library(
    app: tauri::AppHandle,
    enabled: bool,
    revision: u64,
    current: usize,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let paths = if enabled {
            app.state::<library::LibraryManager>().playback_paths()
        } else {
            Vec::new()
        };
        app.state::<PlaybackManager>()
            .set_random_library(paths, enabled, revision, current)
    })
    .await
    .map_err(|_| "Random library operation stopped".to_string())?
}
#[tauri::command]
fn edit_queue(
    manager: tauri::State<'_, PlaybackManager>,
    action: String,
    path: Option<String>,
    index: Option<usize>,
    revision: u64,
    current: usize,
) -> Result<(), String> {
    manager.edit(action, path, index.unwrap_or(0), revision, current)
}

#[tauri::command]
async fn album_artwork(
    library: tauri::State<'_, library::LibraryManager>,
    id: String,
) -> Result<tauri::ipc::Response, String> {
    let path = library
        .album_paths(&id)?
        .into_iter()
        .next()
        .ok_or("Album unavailable")?;
    tauri::async_runtime::spawn_blocking(move || {
        artwork::read(&path).map(tauri::ipc::Response::new)
    })
    .await
    .map_err(|_| "Artwork reader stopped".to_string())?
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(webview_memory::MemoryPolicy::default())
        .setup(|app| {
            tray::setup(app)?;
            app.manage(library::LibraryManager::restored());
            app.manage(downloads::DownloadManager::new(Some(app.handle().clone())));
            app.manage(PlaybackManager::start(app.handle().clone()));
            app.manage(audio_analysis::AudioAnalysis::start());
            let hwnd = app
                .get_webview_window("main")
                .and_then(|window| window.hwnd().ok())
                .map(|hwnd| hwnd.0 as isize)
                .unwrap_or(0);
            let events = app.handle().clone();
            let controls = media_controls::MediaControls::start(
                hwnd,
                app.state::<PlaybackManager>().media_dispatch(),
                std::sync::Arc::new(move |status| {
                    let _ = events.emit(media_controls::STATUS_EVENT, status);
                }),
            );
            app.manage(controls);
            app.state::<media_controls::MediaControls>()
                .update(app.state::<PlaybackManager>().snapshot());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            apple_account_status,
            apple_search,
            apple_preview_album,
            apple_preview_playlist,
            enqueue_downloads,
            download_queue,
            start_downloads,
            stop_downloads_after_current,
            cancel_download,
            retry_download,
            clear_finished_downloads,
            album_artwork,
            background::player_background,
            background::set_player_background,
            background::clear_player_background,
            playback_analysis,
            current_track_info,
            media_controls_status,
            seek_playback,
            set_volume,
            playback_queue,
            set_random_library,
            edit_queue,
            library_albums,
            library_album_tracks,
            play_album,
            library_status,
            scan_library,
            cancel_library_scan,
            library_tracks,
            recycle_library_track,
            playback_status,
            set_playback_queue,
            toggle_playback,
            next_track,
            previous_track,
            stop_playback
        ])
        .on_window_event(|window, event| {
            if matches!(
                event,
                tauri::WindowEvent::Resized(_) | tauri::WindowEvent::Focused(_)
            ) {
                webview_memory::update(window);
            }
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    if window.hide().is_ok() {
                        webview_memory::update(window);
                    }
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running MusicPlayer");
}
