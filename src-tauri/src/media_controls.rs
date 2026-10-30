//! Windows SMTC bridge. No audio device, global hotkey hook, or second player.
use crate::playback::PlaybackSnapshot;
use serde::Serialize;
use std::{
    path::Path,
    sync::{mpsc, Arc, Condvar, Mutex},
    thread,
    time::Duration,
};
use windows::{
    core::{factory, HSTRING},
    Foundation::TypedEventHandler,
    Media::{
        MediaPlaybackStatus, MediaPlaybackType, SystemMediaTransportControls,
        SystemMediaTransportControlsButton, SystemMediaTransportControlsButtonPressedEventArgs,
    },
    Win32::{
        Foundation::HWND,
        System::WinRT::{
            ISystemMediaTransportControlsInterop, RoInitialize, RoUninitialize,
            RO_INIT_MULTITHREADED,
        },
    },
};

pub const STATUS_EVENT: &str = "musicplayer://media-controls-state";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MediaButton {
    Play,
    Pause,
    Next,
    Previous,
    Stop,
}
pub(crate) type Dispatch = Arc<dyn Fn(MediaButton) -> Result<(), String> + Send + Sync>;
type Notify = Arc<dyn Fn(MediaControlsStatus) + Send + Sync>;

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaControlsStatus {
    pub available: bool,
    pub error: Option<String>,
    pub received_buttons: u64,
    pub last_button: Option<MediaButton>,
}

#[derive(Default)]
struct Pending {
    latest: Option<PlaybackSnapshot>,
    closing: bool,
}

pub struct MediaControls {
    pending: Arc<(Mutex<Pending>, Condvar)>,
    status: Arc<Mutex<MediaControlsStatus>>,
    done: Mutex<Option<mpsc::Receiver<()>>>,
}

impl MediaControls {
    pub(crate) fn start(hwnd: isize, dispatch: Dispatch, notify: Notify) -> Self {
        let pending = Arc::new((Mutex::new(Pending::default()), Condvar::new()));
        let status = Arc::new(Mutex::new(MediaControlsStatus::default()));
        let (done_tx, done_rx) = mpsc::channel();
        let worker_pending = pending.clone();
        let worker_status = status.clone();
        let worker_notify = notify.clone();
        let started = thread::Builder::new()
            .name("musicplayer-windows-media".into())
            .spawn(move || {
                let result = run(
                    hwnd,
                    dispatch,
                    &worker_pending,
                    &worker_status,
                    &worker_notify,
                );
                if let Err(error) = result {
                    publish(&worker_status, &worker_notify, |s| {
                        s.available = false;
                        s.error = Some(format!(
                            "Windows media controls unavailable ({:#010X})",
                            error.code().0
                        ));
                    });
                }
                let _ = done_tx.send(());
            });
        if started.is_err() {
            publish(&status, &notify, |s| {
                s.error = Some("Windows media controls worker could not start".into())
            });
        }
        Self {
            pending,
            status,
            done: Mutex::new(Some(done_rx)),
        }
    }

    pub fn update(&self, snapshot: PlaybackSnapshot) {
        let mut pending = self.pending.0.lock().unwrap();
        if !pending.closing {
            pending.latest = Some(snapshot);
            self.pending.1.notify_one();
        }
    }

    pub fn status(&self) -> MediaControlsStatus {
        self.status.lock().unwrap().clone()
    }

    pub fn shutdown(&self) {
        {
            let mut pending = self.pending.0.lock().unwrap();
            pending.closing = true;
            pending.latest = None;
            self.pending.1.notify_one();
        }
        if let Some(done) = self.done.lock().unwrap().take() {
            // A stalled Windows service must not prevent application shutdown.
            let _ = done.recv_timeout(Duration::from_secs(1));
        }
    }
}

impl Drop for MediaControls {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn publish(
    status: &Mutex<MediaControlsStatus>,
    notify: &Notify,
    update: impl FnOnce(&mut MediaControlsStatus),
) {
    let value = {
        let mut status = status.lock().unwrap();
        update(&mut status);
        status.clone()
    };
    notify(value);
}

struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            RoUninitialize();
        }
    }
}

struct Registration {
    controls: SystemMediaTransportControls,
    token: i64,
}
impl Drop for Registration {
    fn drop(&mut self) {
        let _ = self.controls.RemoveButtonPressed(self.token);
        let _ = self.controls.SetIsEnabled(false);
        let _ = self.controls.SetPlaybackStatus(MediaPlaybackStatus::Closed);
    }
}

fn run(
    hwnd: isize,
    dispatch: Dispatch,
    pending: &Arc<(Mutex<Pending>, Condvar)>,
    status: &Arc<Mutex<MediaControlsStatus>>,
    notify: &Notify,
) -> windows::core::Result<()> {
    unsafe {
        RoInitialize(RO_INIT_MULTITHREADED)?;
    }
    let _apartment = Apartment;
    let interop: ISystemMediaTransportControlsInterop =
        factory::<SystemMediaTransportControls, _>()?;
    // Tauri owns this top-level window; shutdown unregisters before app exit.
    let controls: SystemMediaTransportControls =
        unsafe { interop.GetForWindow(HWND(hwnd as *mut _))? };
    controls.SetIsEnabled(false)?;
    let updater = controls.DisplayUpdater()?;
    updater.SetType(MediaPlaybackType::Music)?;
    updater
        .MusicProperties()?
        .SetTitle(&HSTRING::from("MusicPlayer"))?;
    updater.Update()?;
    let callback_status = status.clone();
    let callback_notify = notify.clone();
    let callback_pending = pending.clone();
    let token = controls.ButtonPressed(&TypedEventHandler::<
        SystemMediaTransportControls,
        SystemMediaTransportControlsButtonPressedEventArgs,
    >::new(move |_, args| {
        if callback_pending.0.lock().unwrap().closing {
            return Ok(());
        }
        let button = match args.ok()?.Button()? {
            SystemMediaTransportControlsButton::Play => MediaButton::Play,
            SystemMediaTransportControlsButton::Pause => MediaButton::Pause,
            SystemMediaTransportControlsButton::Next => MediaButton::Next,
            SystemMediaTransportControlsButton::Previous => MediaButton::Previous,
            SystemMediaTransportControlsButton::Stop => MediaButton::Stop,
            _ => return Ok(()),
        };
        // Never wait for the engine in a Windows event callback.
        let result = dispatch(button);
        publish(&callback_status, &callback_notify, |s| {
            s.received_buttons = s.received_buttons.saturating_add(1);
            s.last_button = Some(button);
            s.error = result.err();
        });
        Ok(())
    }))?;
    let _registration = Registration {
        controls: controls.clone(),
        token,
    };
    publish(status, notify, |s| {
        s.available = true;
        s.error = None;
    });
    let mut previous = None;
    loop {
        let snapshot = {
            let mut slot = pending.0.lock().unwrap();
            while slot.latest.is_none() && !slot.closing {
                slot = pending.1.wait(slot).unwrap();
            }
            if slot.closing {
                break;
            }
            slot.latest.take().unwrap()
        };
        // Position events are frequent; only publish changed transport state.
        let enabled = snapshot.available && snapshot.queue_len > 0;
        let playback = match snapshot.state.as_str() {
            "playing" => MediaPlaybackStatus::Playing,
            "paused" => MediaPlaybackStatus::Paused,
            "loading" => MediaPlaybackStatus::Changing,
            _ => MediaPlaybackStatus::Stopped,
        };
        let next = enabled
            && (snapshot.repeat_all
                || snapshot
                    .current_index
                    .is_some_and(|i| i + 1 < snapshot.queue_len));
        let value = (enabled, playback.0, next, snapshot.current_path.clone());
        if previous.as_ref() == Some(&value) {
            continue;
        }
        controls.SetIsPlayEnabled(enabled)?;
        controls.SetIsPauseEnabled(enabled)?;
        controls.SetIsStopEnabled(enabled)?;
        controls.SetIsNextEnabled(next)?;
        controls.SetIsPreviousEnabled(enabled)?;
        controls.SetPlaybackStatus(playback)?;
        let title = snapshot
            .current_path
            .as_deref()
            .and_then(|p| Path::new(p).file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "MusicPlayer".into());
        updater.MusicProperties()?.SetTitle(&HSTRING::from(title))?;
        updater.Update()?;
        controls.SetIsEnabled(enabled)?;
        previous = Some(value);
    }
    Ok(())
}
