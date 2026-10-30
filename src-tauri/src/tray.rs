//! Closing the main window preserves playback; only the tray exit shuts down.
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager,
};

pub fn setup(app: &tauri::App) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "打开 MusicPlayer", true, None::<&str>)?;
    let exit = MenuItem::with_id(app, "exit", "彻底退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &exit])?;
    let icon = app.default_window_icon().cloned().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Missing MusicPlayer tray icon",
        )
    })?;
    // Tauri retains the tray in application state for the application's lifetime.
    TrayIconBuilder::with_id("music-player")
        .icon(icon)
        .tooltip("MusicPlayer")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => restore(app),
            "exit" => shutdown(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                restore(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

fn restore(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        crate::webview_memory::update(&window.as_ref().window());
    }
}

fn shutdown(app: &AppHandle) {
    if app
        .state::<crate::playback::PlaybackManager>()
        .begin_close()
    {
        let app = app.clone();
        std::thread::spawn(move || {
            app.state::<crate::downloads::DownloadManager>().shutdown();
            app.state::<crate::media_controls::MediaControls>()
                .shutdown();
            app.state::<crate::library::LibraryManager>().cancel();
            app.state::<crate::playback::PlaybackManager>().shutdown();
            app.state::<crate::audio_analysis::AudioAnalysis>()
                .shutdown();
            app.exit(0);
        });
    }
}
