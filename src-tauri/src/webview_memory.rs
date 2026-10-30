//! Let WebView2 reclaim caches while minimized or hidden without suspending native audio.
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::Manager;
use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2_19, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW,
    COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL,
};
use windows::core::Interface;

#[derive(Default)]
pub struct MemoryPolicy(AtomicBool);

pub fn update(window: &tauri::Window) {
    let Ok(minimized) = window.is_minimized() else {
        return;
    };
    let Ok(visible) = window.is_visible() else {
        return;
    };
    let reduced = minimized || !visible;
    let policy = window.state::<MemoryPolicy>();
    if policy.0.swap(reduced, Ordering::Relaxed) == reduced {
        return;
    }
    if let Some(webview) = window.app_handle().get_webview_window(window.label()) {
        let _ = webview.with_webview(move |platform| unsafe {
            // Older runtimes may not implement this interface. Memory tuning is
            // optional and must never prevent startup, restoration, or playback.
            if let Ok(core) = platform.controller().CoreWebView2() {
                if let Ok(core) = core.cast::<ICoreWebView2_19>() {
                    let _ = core.SetMemoryUsageTargetLevel(if reduced {
                        COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW
                    } else {
                        COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL
                    });
                }
            }
        });
    }
}
