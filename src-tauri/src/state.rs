//! Bounded, versioned JSON with same-directory Windows atomic replacement.
use serde::{de::DeserializeOwned, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    thread,
};

pub fn directory() -> Result<PathBuf, String> {
    std::env::var_os("LOCALAPPDATA")
        .map(|p| PathBuf::from(p).join("MusicPlayer/state"))
        .ok_or_else(|| "Local application data folder is unavailable".into())
}
pub fn load<T: DeserializeOwned>(path: &Path, limit: usize) -> Result<Option<T>, String> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Saved state cannot be read".into()),
    };
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Saved state cannot be read")?;
    if bytes.len() > limit {
        return Err("Saved state exceeds its size limit".into());
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| "Saved state is damaged or incompatible".into())
}
struct Limited {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for Limited {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if data.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("State size limit reached"));
        }
        self.bytes.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub fn encode<T: Serialize>(value: &T, limit: usize) -> Result<Vec<u8>, String> {
    let mut writer = Limited {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut writer, value)
        .map_err(|_| "State exceeds its size limit or cannot be encoded")?;
    Ok(writer.bytes)
}
pub fn replace(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("Invalid state directory")?;
    fs::create_dir_all(parent).map_err(|_| "Cannot create state directory")?;
    // A persistent, empty lock file bounds crash leftovers and serializes
    // replacement across app instances. Windows releases the handle on crash.
    use std::os::windows::fs::OpenOptionsExt;
    let name = path
        .file_name()
        .ok_or("Invalid state filename")?
        .to_string_lossy();
    let _lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(parent.join(format!(".{name}.lock")))
        .map_err(|_| "State is in use by another writer")?;
    let temp = parent.join(format!(".{name}.tmp"));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp)
            .map_err(|_| "Cannot create state snapshot")?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| "Cannot flush state snapshot")?;
        drop(file);
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        extern "system" {
            fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
        }
        let from: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0x1 | 0x8) } == 0 {
            return Err("Cannot replace state snapshot".into());
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
#[derive(Default)]
struct Pending {
    bytes: Option<Vec<u8>>,
    stopping: bool,
    error: Option<String>,
}
/// One coalescing writer: at most one pending snapshot and one in-flight write.
pub struct Writer {
    shared: Arc<(Mutex<Pending>, Condvar)>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Writer {
    pub fn new(path: PathBuf) -> Self {
        let shared = Arc::new((Mutex::new(Pending::default()), Condvar::new()));
        let cloned = shared.clone();
        let worker = thread::spawn(move || loop {
            let (lock, ready) = &*cloned;
            let mut state = lock.lock().unwrap();
            while state.bytes.is_none() && !state.stopping {
                state = ready.wait(state).unwrap();
            }
            let Some(bytes) = state.bytes.take() else {
                break;
            };
            drop(state);
            let error = replace(&path, &bytes).err();
            lock.lock().unwrap().error = error;
        });
        Self {
            shared,
            worker: Some(worker),
        }
    }
    pub fn submit<T: Serialize>(&self, value: &T, limit: usize) -> Result<(), String> {
        let bytes = encode(value, limit)?;
        self.shared.0.lock().unwrap().bytes = Some(bytes);
        self.shared.1.notify_one();
        Ok(())
    }
    pub fn error(&self) -> Option<String> {
        self.shared.0.lock().unwrap().error.clone()
    }
}
impl Drop for Writer {
    fn drop(&mut self) {
        self.shared.0.lock().unwrap().stopping = true;
        self.shared.1.notify_one();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn writer_flushes_latest_snapshot_on_shutdown() {
        let root = std::env::temp_dir().join(format!(
            "musicplayer-writer-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = root.join("session.json");
        let writer = Writer::new(path.clone());
        for value in 0..50 {
            writer.submit(&value, 100).unwrap();
        }
        drop(writer);
        assert_eq!(load::<u32>(&path, 100).unwrap(), Some(49));
        assert!(!root.join(".session.json.tmp").exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn atomic_replace_and_recovery_limits() {
        let root = std::env::temp_dir().join(format!(
            "musicplayer-state-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = root.join("state.json");
        replace(&path, b"[1]").unwrap();
        // An interrupted sibling write does not damage the committed snapshot.
        fs::write(root.join("interrupted.tmp"), b"[").unwrap();
        assert_eq!(load::<Vec<u8>>(&path, 20).unwrap(), Some(vec![1]));
        replace(&path, b"[2,3]").unwrap();
        assert_eq!(load::<Vec<u8>>(&path, 20).unwrap(), Some(vec![2, 3]));
        assert!(load::<Vec<u8>>(&path, 2).is_err());
        assert!(encode(&vec![1, 2, 3], 2).is_err());
        fs::write(&path, b"{").unwrap();
        assert!(load::<Vec<u8>>(&path, 20).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
