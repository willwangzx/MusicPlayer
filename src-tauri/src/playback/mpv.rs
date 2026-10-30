use std::{
    env,
    ffi::{c_char, c_ulong, c_void, CStr, CString, OsStr},
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr,
};

type HModule = *mut c_void;

const LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR: u32 = 0x0000_0100;
const LOAD_LIBRARY_SEARCH_DEFAULT_DIRS: u32 = 0x0000_1000;

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryExW(file_name: *const u16, file: *mut c_void, flags: u32) -> HModule;
    fn GetProcAddress(module: HModule, proc_name: *const u8) -> *mut c_void;
    fn FreeLibrary(module: HModule) -> i32;
    fn GetLastError() -> u32;
}

type MpvCreate = unsafe extern "C" fn() -> *mut c_void;
type MpvInitialize = unsafe extern "C" fn(*mut c_void) -> i32;
type MpvTerminateDestroy = unsafe extern "C" fn(*mut c_void);
type MpvSetOptionString = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> i32;
type MpvCommand = unsafe extern "C" fn(*mut c_void, *const *const c_char) -> i32;
type MpvGetPropertyString = unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_char;
type MpvObserveProperty = unsafe extern "C" fn(*mut c_void, u64, *const c_char, i32) -> i32;
type MpvFree = unsafe extern "C" fn(*mut c_void);
type MpvWaitEvent = unsafe extern "C" fn(*mut c_void, f64) -> *const MpvEvent;
type MpvErrorString = unsafe extern "C" fn(i32) -> *const c_char;
type MpvClientApiVersion = unsafe extern "C" fn() -> c_ulong;

// The Windows x64 ABI uses LLP64: `unsigned long` is a 32-bit value even
// though pointers are 64-bit.  libmpv declares mpv_client_api_version() as
// returning unsigned long, so keep this assertion next to the FFI alias.
const _: () = assert!(std::mem::size_of::<c_ulong>() == std::mem::size_of::<u32>());
const MAX_PLAYLIST_ENTRIES: usize = 4_096;

#[repr(C)]
pub struct MpvEvent {
    pub event_id: i32,
    pub error: i32,
    pub reply_userdata: u64,
    pub data: *mut c_void,
}

#[repr(C)]
struct MpvEventStartFile {
    playlist_entry_id: i64,
}

#[repr(C)]
struct MpvEventEndFile {
    reason: i32,
    error: i32,
    playlist_entry_id: i64,
    playlist_insert_id: i64,
    playlist_insert_num_entries: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MpvStartFileSummary {
    pub playlist_entry_id: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MpvEndFileSummary {
    pub reason: i32,
    pub error: i32,
    pub playlist_entry_id: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MpvEventSummary {
    pub event_id: i32,
    pub error: i32,
    pub reply_userdata: u64,
    pub start_file: Option<MpvStartFileSummary>,
    pub end_file: Option<MpvEndFileSummary>,
}

impl MpvEventSummary {
    fn none() -> Self {
        Self {
            event_id: event::NONE,
            error: 0,
            reply_userdata: 0,
            start_file: None,
            end_file: None,
        }
    }
}

pub mod event {
    pub const NONE: i32 = 0;
    pub const SHUTDOWN: i32 = 1;
    pub const START_FILE: i32 = 6;
    pub const END_FILE: i32 = 7;
    pub const END_FILE_REASON_EOF: i32 = 0;
    pub const END_FILE_REASON_STOP: i32 = 2;
    pub const END_FILE_REASON_QUIT: i32 = 3;
    pub const END_FILE_REASON_ERROR: i32 = 4;
    pub const END_FILE_REASON_REDIRECT: i32 = 5;
    pub const FILE_LOADED: i32 = 8;
    pub const PAUSE: i32 = 12;
    pub const UNPAUSE: i32 = 13;
    pub const AUDIO_RECONFIG: i32 = 18;
    pub const PLAYBACK_RESTART: i32 = 21;
    pub const QUEUE_OVERFLOW: i32 = 24;
    pub const PROPERTY_CHANGE: i32 = 22;
}

/// Options shared by the desktop engine and headless probes. They deliberately
/// disable user configuration, scripts, automatic sidecar discovery, video,
/// and exclusive audio so local playback behavior stays deterministic.
pub const LOCAL_AUDIO_OPTIONS: &[(&str, &str)] = &[
    ("config", "no"),
    ("load-scripts", "no"),
    ("terminal", "no"),
    ("input-default-bindings", "no"),
    ("input-vo-keyboard", "no"),
    ("osc", "no"),
    ("video", "no"),
    ("audio-display", "no"),
    ("autoload-files", "no"),
    ("access-references", "no"),
    ("gapless-audio", "yes"),
    ("audio-exclusive", "no"),
    ("audio-samplerate", "48000"),
    ("audio-format", "float"),
    ("audio-client-name", "MusicPlayer"),
    ("idle", "yes"),
];

struct MpvLibrary {
    module: HModule,
    loaded_from: String,
    create: MpvCreate,
    initialize: MpvInitialize,
    terminate_destroy: MpvTerminateDestroy,
    set_option_string: MpvSetOptionString,
    command: MpvCommand,
    get_property_string: MpvGetPropertyString,
    observe_property: MpvObserveProperty,
    free: MpvFree,
    wait_event: MpvWaitEvent,
    error_string: MpvErrorString,
    client_api_version: MpvClientApiVersion,
}

impl MpvLibrary {
    fn load(explicit_path: Option<&Path>) -> Result<Self, String> {
        let mut candidates = Vec::<PathBuf>::new();
        if let Some(path) = explicit_path {
            candidates.push(path.to_path_buf());
        }
        if let Some(path) = env::var_os("MUSICPLAYER_LIBMPV_PATH") {
            candidates.push(PathBuf::from(path));
        }
        if let Ok(executable) = env::current_exe() {
            if let Some(parent) = executable.parent() {
                candidates.push(parent.join("libmpv-2.dll"));
                candidates.push(parent.join("resources/runtime/libmpv-2.dll"));
            }
        }
        #[cfg(debug_assertions)]
        candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(r"bin\libmpv-2.dll"));

        let mut failures = Vec::new();
        for candidate in candidates {
            if !candidate.is_absolute() {
                failures.push(format!(
                    "{} (rejected: libmpv path must be absolute)",
                    candidate.display()
                ));
                continue;
            }
            if !candidate.is_file() {
                failures.push(format!("{} (not found)", candidate.display()));
                continue;
            }
            match unsafe { Self::load_path(&candidate) } {
                Ok(library) => return Ok(library),
                Err(error) => failures.push(error),
            }
        }

        Err(format!(
            "libmpv-2.dll is unavailable. Repair the MusicPlayer runtime installation or set \
             MUSICPLAYER_LIBMPV_PATH to the pinned LGPL DLL. Tried: {}",
            failures.join("; ")
        ))
    }

    unsafe fn load_path(path: &Path) -> Result<Self, String> {
        let wide = wide_null(path.as_os_str());
        let module = LoadLibraryExW(
            wide.as_ptr(),
            ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
        );
        if module.is_null() {
            return Err(format!(
                "failed to load {} (Win32 error {})",
                path.display(),
                GetLastError()
            ));
        }

        macro_rules! symbol {
            ($name:literal, $ty:ty) => {{
                let pointer = GetProcAddress(module, concat!($name, "\0").as_ptr());
                if pointer.is_null() {
                    FreeLibrary(module);
                    return Err(format!("{} does not export {}", path.display(), $name));
                }
                std::mem::transmute::<*mut c_void, $ty>(pointer)
            }};
        }

        Ok(Self {
            module,
            loaded_from: path.display().to_string(),
            create: symbol!("mpv_create", MpvCreate),
            initialize: symbol!("mpv_initialize", MpvInitialize),
            terminate_destroy: symbol!("mpv_terminate_destroy", MpvTerminateDestroy),
            set_option_string: symbol!("mpv_set_option_string", MpvSetOptionString),
            command: symbol!("mpv_command", MpvCommand),
            get_property_string: symbol!("mpv_get_property_string", MpvGetPropertyString),
            observe_property: symbol!("mpv_observe_property", MpvObserveProperty),
            free: symbol!("mpv_free", MpvFree),
            wait_event: symbol!("mpv_wait_event", MpvWaitEvent),
            error_string: symbol!("mpv_error_string", MpvErrorString),
            client_api_version: symbol!("mpv_client_api_version", MpvClientApiVersion),
        })
    }

    fn error_text(&self, code: i32) -> String {
        let pointer = unsafe { (self.error_string)(code) };
        if pointer.is_null() {
            return format!("libmpv error {code}");
        }
        unsafe { std::ffi::CStr::from_ptr(pointer) }
            .to_string_lossy()
            .into_owned()
    }
}

impl Drop for MpvLibrary {
    fn drop(&mut self) {
        if !self.module.is_null() {
            unsafe {
                FreeLibrary(self.module);
            }
        }
    }
}

pub struct Mpv {
    handle: *mut c_void,
    library: MpvLibrary,
}

impl Mpv {
    pub fn create(explicit_path: Option<&Path>) -> Result<Self, String> {
        let library = MpvLibrary::load(explicit_path)?;
        let version = unsafe { (library.client_api_version)() };
        let major = version >> 16;
        if major != 2 {
            return Err(format!(
                "unsupported libmpv client API {major}.{} from {} (expected major 2)",
                version & 0xffff,
                library.loaded_from
            ));
        }

        let handle = unsafe { (library.create)() };
        if handle.is_null() {
            return Err("mpv_create returned a null handle".to_string());
        }
        Ok(Self { handle, library })
    }

    pub fn initialize(&mut self) -> Result<(), String> {
        self.check(
            unsafe { (self.library.initialize)(self.handle) },
            "mpv_initialize",
        )
    }

    pub fn set_option(&mut self, name: &str, value: &str) -> Result<(), String> {
        let name = c_string(name, "option name")?;
        let value = c_string(value, "option value")?;
        self.check(
            unsafe { (self.library.set_option_string)(self.handle, name.as_ptr(), value.as_ptr()) },
            "mpv_set_option_string",
        )
    }

    pub fn command(&mut self, arguments: &[&str]) -> Result<(), String> {
        if arguments.is_empty() {
            return Err("an mpv command needs at least one argument".to_string());
        }
        let strings = arguments
            .iter()
            .map(|argument| c_string(argument, "command argument"))
            .collect::<Result<Vec<_>, _>>()?;
        let mut pointers = strings
            .iter()
            .map(|argument| argument.as_ptr())
            .collect::<Vec<_>>();
        pointers.push(ptr::null());
        self.check(
            unsafe { (self.library.command)(self.handle, pointers.as_ptr()) },
            arguments[0],
        )
    }

    pub fn observe(&mut self, name: &str, id: u64) -> Result<(), String> {
        let name = c_string(name, "property name")?;
        self.check(
            unsafe { (self.library.observe_property)(self.handle, id, name.as_ptr(), 0) },
            "observe property",
        )
    }
    pub fn property_string(&mut self, name: &str) -> Result<Option<String>, String> {
        let name = c_string(name, "property name")?;
        let pointer = unsafe { (self.library.get_property_string)(self.handle, name.as_ptr()) };
        if pointer.is_null() {
            return Ok(None);
        }

        let value = unsafe { CStr::from_ptr(pointer) }
            .to_string_lossy()
            .into_owned();
        unsafe { (self.library.free)(pointer.cast()) };
        Ok(Some(value))
    }

    pub fn playlist_entry_ids(&mut self) -> Result<Vec<i64>, String> {
        let count = self
            .property_string("playlist-count")?
            .ok_or_else(|| "mpv did not return playlist-count".to_string())?
            .parse::<usize>()
            .map_err(|error| format!("invalid mpv playlist-count: {error}"))?;
        if count > MAX_PLAYLIST_ENTRIES {
            return Err(format!(
                "mpv playlist exceeds the {MAX_PLAYLIST_ENTRIES}-entry limit"
            ));
        }
        let mut ids = Vec::with_capacity(count);
        for index in 0..count {
            let property = format!("playlist/{index}/id");
            let id = self
                .property_string(&property)?
                .ok_or_else(|| format!("mpv did not return {property}"))?
                .parse::<i64>()
                .map_err(|error| format!("invalid mpv playlist entry id at {index}: {error}"))?;
            ids.push(id);
        }
        Ok(ids)
    }

    pub fn current_playlist_entry_id(&mut self) -> Result<Option<i64>, String> {
        let Some(position) = self.property_string("playlist-current-pos")? else {
            return Ok(None);
        };
        let position = position
            .parse::<isize>()
            .map_err(|error| format!("invalid mpv playlist-current-pos: {error}"))?;
        if position < 0 {
            return Ok(None);
        }
        let property = format!("playlist/{position}/id");
        self.property_string(&property)?
            .map(|id| {
                id.parse::<i64>()
                    .map_err(|error| format!("invalid current mpv playlist entry id: {error}"))
            })
            .transpose()
    }

    pub fn wait_event(&mut self, timeout_seconds: f64) -> MpvEventSummary {
        let event = unsafe { (self.library.wait_event)(self.handle, timeout_seconds) };
        if event.is_null() {
            return MpvEventSummary::none();
        }

        let event = unsafe { &*event };
        let mut summary = MpvEventSummary {
            event_id: event.event_id,
            error: event.error,
            reply_userdata: event.reply_userdata,
            start_file: None,
            end_file: None,
        };

        unsafe {
            if !event.data.is_null() {
                match event.event_id {
                    event::START_FILE => {
                        let data = &*(event.data.cast::<MpvEventStartFile>());
                        summary.start_file = Some(MpvStartFileSummary {
                            playlist_entry_id: data.playlist_entry_id,
                        });
                    }
                    event::END_FILE => {
                        let data = &*(event.data.cast::<MpvEventEndFile>());
                        summary.end_file = Some(MpvEndFileSummary {
                            reason: data.reason,
                            error: data.error,
                            playlist_entry_id: data.playlist_entry_id,
                        });
                    }
                    _ => {}
                }
            }
        }
        summary
    }

    pub fn client_api_version(&self) -> String {
        let version = unsafe { (self.library.client_api_version)() };
        format!("{}.{}", version >> 16, version & 0xffff)
    }

    pub fn loaded_from(&self) -> &str {
        &self.library.loaded_from
    }

    pub fn error_description(&self, code: i32) -> String {
        self.library.error_text(code)
    }

    fn check(&self, code: i32, operation: &str) -> Result<(), String> {
        if code >= 0 {
            Ok(())
        } else {
            Err(format!(
                "{operation}: {} ({code})",
                self.library.error_text(code)
            ))
        }
    }
}

impl Drop for Mpv {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                (self.library.terminate_destroy)(self.handle);
            }
            self.handle = ptr::null_mut();
        }
    }
}

fn c_string(value: &str, label: &str) -> Result<CString, String> {
    CString::new(value).map_err(|_| format!("{label} contains an interior NUL byte"))
}

fn wide_null(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}
