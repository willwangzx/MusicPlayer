//! Recycle one validated library audio file through the Windows Shell.
//!
//! The extended recycle flag is deliberate: `FOF_ALLOWUNDO` alone can fall
//! back to permanent deletion when the Shell decides an item is not recyclable.
use std::{
    fs,
    path::{Component, Path, PathBuf, Prefix},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
};
use windows::{
    core::{implement, Error, Ref, HRESULT, PCWSTR},
    Win32::{
        Foundation::E_ABORT,
        UI::Shell::{
            IFileOperationProgressSink, IFileOperationProgressSink_Impl, IShellItem,
            TSF_DELETE_RECYCLE_IF_POSSIBLE,
        },
    },
};

const LIBRARY_ROOT: &str = r"D:\Music";

/// Move one regular audio file to the Windows Recycle Bin.
///
/// Validation and COM work run on a fresh STA thread. The call returns only
/// after the Shell operation finishes, so playback can serialize it with queue
/// mutation and release any libmpv handles before entering this function.
pub fn recycle_file(path: &str) -> Result<(), String> {
    if path.len() > 4096 {
        return Err("Track path exceeds the 4096-byte limit".into());
    }
    let path = path.to_owned();
    thread::Builder::new()
        .name("musicplayer-recycle".into())
        .spawn(move || recycle_on_sta(&path))
        .map_err(|_| "Unable to start the Recycle Bin operation".to_string())?
        .join()
        .map_err(|_| "The Recycle Bin operation stopped unexpectedly".to_string())?
}

fn recycle_on_sta(path: &str) -> Result<(), String> {
    let path = validate_file(path)?;
    recycle_validated_on_sta(&path)
}

fn recycle_validated_on_sta(path: &Path) -> Result<(), String> {
    use windows::{
        core::HSTRING,
        Win32::{
            System::Com::{
                CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
                COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
            },
            UI::Shell::{
                FileOperation, IFileOperation, SHCreateItemFromParsingName, FILEOPERATION_FLAGS,
                FOFX_EARLYFAILURE, FOFX_RECYCLEONDELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION,
                FOF_NOERRORUI, FOF_SILENT,
            },
        },
    };

    struct ComGuard;
    impl Drop for ComGuard {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }

    unsafe {
        CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT(
                COINIT_APARTMENTTHREADED.0 | COINIT_DISABLE_OLE1DDE.0,
            ),
        )
        .ok()
        .map_err(|_| "Unable to initialize the Windows Recycle Bin service".to_string())?;
    }
    let _com = ComGuard;

    let operation: IFileOperation = unsafe {
        CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER)
            .map_err(|_| "Unable to open the Windows Recycle Bin service".to_string())?
    };
    let flags = FILEOPERATION_FLAGS(
        FOF_ALLOWUNDO.0
            | FOFX_RECYCLEONDELETE.0
            | FOFX_EARLYFAILURE.0
            | FOF_NOCONFIRMATION.0
            | FOF_NOERRORUI.0
            | FOF_SILENT.0,
    );
    unsafe {
        operation
            .SetOperationFlags(flags)
            .map_err(|_| "Unable to require a Recycle Bin-only operation".to_string())?;
    }
    // The Shell parser expects a normal drive path even though
    // `canonicalize` returns a verbatim `\\?\D:\...` path on Windows.
    let shell_path_value = shell_compatible_path(path);
    let shell_path = HSTRING::from(shell_path_value.as_os_str());
    let item: IShellItem = unsafe {
        SHCreateItemFromParsingName(&shell_path, None)
            .map_err(|_| "The selected track is no longer available".to_string())?
    };
    let recyclable_seen = Arc::new(AtomicBool::new(false));
    let sink: IFileOperationProgressSink = RecycleOnlySink {
        recyclable_seen: Arc::clone(&recyclable_seen),
    }
    .into();
    unsafe {
        operation
            .DeleteItem(&item, &sink)
            .map_err(|_| "Unable to prepare the Recycle Bin operation".to_string())?;
        operation
            .PerformOperations()
            .map_err(|_| "Windows could not move the track to the Recycle Bin".to_string())?;
        if operation
            .GetAnyOperationsAborted()
            .map_err(|_| "Unable to verify the Recycle Bin operation".to_string())?
            .as_bool()
        {
            return Err("The Recycle Bin operation was cancelled".into());
        }
    }
    if !recyclable_seen.load(Ordering::SeqCst) {
        return Err("Windows did not confirm a recyclable delete operation".into());
    }
    if path.exists() {
        return Err("Windows did not move the track to the Recycle Bin".into());
    }
    Ok(())
}

#[implement(IFileOperationProgressSink)]
struct RecycleOnlySink {
    recyclable_seen: Arc<AtomicBool>,
}

#[allow(non_snake_case)]
impl IFileOperationProgressSink_Impl for RecycleOnlySink_Impl {
    fn StartOperations(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn FinishOperations(&self, _result: HRESULT) -> windows::core::Result<()> {
        Ok(())
    }

    fn PreRenameItem(
        &self,
        _flags: u32,
        _item: Ref<'_, IShellItem>,
        _new_name: &PCWSTR,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn PostRenameItem(
        &self,
        _flags: u32,
        _item: Ref<'_, IShellItem>,
        _new_name: &PCWSTR,
        _result: HRESULT,
        _new_item: Ref<'_, IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn PreMoveItem(
        &self,
        _flags: u32,
        _item: Ref<'_, IShellItem>,
        _destination: Ref<'_, IShellItem>,
        _new_name: &PCWSTR,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn PostMoveItem(
        &self,
        _flags: u32,
        _item: Ref<'_, IShellItem>,
        _destination: Ref<'_, IShellItem>,
        _new_name: &PCWSTR,
        _result: HRESULT,
        _new_item: Ref<'_, IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn PreCopyItem(
        &self,
        _flags: u32,
        _item: Ref<'_, IShellItem>,
        _destination: Ref<'_, IShellItem>,
        _new_name: &PCWSTR,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn PostCopyItem(
        &self,
        _flags: u32,
        _item: Ref<'_, IShellItem>,
        _destination: Ref<'_, IShellItem>,
        _new_name: &PCWSTR,
        _result: HRESULT,
        _new_item: Ref<'_, IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn PreDeleteItem(&self, flags: u32, _item: Ref<'_, IShellItem>) -> windows::core::Result<()> {
        if flags & TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32 == 0 {
            return Err(Error::from_hresult(E_ABORT));
        }
        self.recyclable_seen.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn PostDeleteItem(
        &self,
        _flags: u32,
        _item: Ref<'_, IShellItem>,
        result: HRESULT,
        _new_item: Ref<'_, IShellItem>,
    ) -> windows::core::Result<()> {
        result.ok()
    }

    fn PreNewItem(
        &self,
        _flags: u32,
        _destination: Ref<'_, IShellItem>,
        _new_name: &PCWSTR,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn PostNewItem(
        &self,
        _flags: u32,
        _destination: Ref<'_, IShellItem>,
        _new_name: &PCWSTR,
        _template_name: &PCWSTR,
        _attributes: u32,
        _result: HRESULT,
        _new_item: Ref<'_, IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn UpdateProgress(&self, _total: u32, _so_far: u32) -> windows::core::Result<()> {
        Ok(())
    }

    fn ResetTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn PauseTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn ResumeTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
}

fn validate_file(value: &str) -> Result<PathBuf, String> {
    if value.is_empty() || value.len() > 4096 || value.contains('\0') {
        return Err("Invalid track path".into());
    }
    let path = PathBuf::from(value);
    if !local_d_drive_path(&path)
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("Track must use an absolute local D: path".into());
    }
    if !supported_audio(&path) {
        return Err("Only a supported audio file can be recycled".into());
    }
    if has_downloads_component(&path) {
        return Err("Downloader staging files cannot be recycled from the library".into());
    }

    let root = Path::new(LIBRARY_ROOT);
    if !within(root, &path) {
        return Err("Track is outside D:\\Music".into());
    }
    reject_reparse_ancestors(root, &path)?;

    let canonical_root = fs::canonicalize(root)
        .map_err(|_| "The D:\\Music library folder is unavailable".to_string())?;
    let canonical = fs::canonicalize(&path)
        .map_err(|_| "The selected track is no longer available".to_string())?;
    if !within(&canonical_root, &canonical) || has_downloads_component(&canonical) {
        return Err("Track resolved outside the permitted library".into());
    }
    reject_reparse_ancestors(&canonical_root, &canonical)?;

    let metadata = fs::symlink_metadata(&canonical)
        .map_err(|_| "The selected track is no longer available".to_string())?;
    if !metadata.is_file() || crate::library::reparse(&metadata) {
        return Err("The selected track must be a regular audio file".into());
    }
    Ok(canonical)
}

fn reject_reparse_ancestors(root: &Path, path: &Path) -> Result<(), String> {
    let mut reached_root = false;
    for ancestor in path.ancestors() {
        if !within_or_equal(root, ancestor) {
            continue;
        }
        let metadata = fs::symlink_metadata(ancestor)
            .map_err(|_| "The selected track or one of its folders is unavailable".to_string())?;
        if crate::library::reparse(&metadata) {
            return Err("Tracks reached through links or reparse points cannot be recycled".into());
        }
        if equal_path(root, ancestor) {
            reached_root = true;
            break;
        }
    }
    if !reached_root {
        return Err("Track is outside D:\\Music".into());
    }
    Ok(())
}

fn local_d_drive_path(path: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    matches!(
        path.components().next(),
        Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) if drive.eq_ignore_ascii_case(&b'D'))
    )
}

fn supported_audio(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("mp3" | "flac" | "m4a" | "aac" | "wav" | "ogg" | "opus" | "aiff" | "aif" | "wma")
    )
}

fn has_downloads_component(path: &Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(".downloads")
    })
}

fn normalized(path: &Path) -> String {
    let text = path.to_string_lossy().replace('/', "\\");
    text.strip_prefix(r"\\?\")
        .unwrap_or(&text)
        .trim_end_matches('\\')
        .to_string()
}

fn shell_compatible_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    text.strip_prefix(r"\\?\")
        .map(PathBuf::from)
        .unwrap_or_else(|| path.to_path_buf())
}

fn equal_path(left: &Path, right: &Path) -> bool {
    normalized(left).eq_ignore_ascii_case(&normalized(right))
}

fn within_or_equal(root: &Path, path: &Path) -> bool {
    equal_path(root, path) || within(root, path)
}

fn within(root: &Path, path: &Path) -> bool {
    let root = normalized(root);
    let path = normalized(path);
    path.len() > root.len()
        && path
            .get(..root.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(&root))
        && path.as_bytes().get(root.len()) == Some(&b'\\')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    const DISPOSABLE_ROOT: &str = r"D:\Projects\MusicPlayer\gapless-assets";

    fn recycle_disposable_fixture(path: &Path) -> Result<PathBuf, String> {
        let root = fs::canonicalize(DISPOSABLE_ROOT)
            .map_err(|_| "Disposable test root is unavailable".to_string())?;
        let canonical = fs::canonicalize(path)
            .map_err(|_| "Disposable test fixture is unavailable".to_string())?;
        let name = canonical
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("Disposable test fixture name is invalid")?;
        if !within(&root, &canonical)
            || !name.starts_with("musicplayer-recycle-smoke-")
            || !name.ends_with(".wav")
        {
            return Err("Refusing to recycle a non-disposable test fixture".into());
        }
        reject_reparse_ancestors(&root, &canonical)?;
        let metadata = fs::symlink_metadata(&canonical)
            .map_err(|_| "Disposable test fixture is unavailable".to_string())?;
        if !metadata.is_file() || crate::library::reparse(&metadata) {
            return Err("Disposable test fixture must be a regular file".into());
        }
        let worker_path = canonical.clone();
        thread::Builder::new()
            .name("musicplayer-recycle-smoke".into())
            .spawn(move || recycle_validated_on_sta(&worker_path))
            .map_err(|_| "Unable to start disposable recycle smoke".to_string())?
            .join()
            .map_err(|_| "Disposable recycle smoke stopped unexpectedly".to_string())??;
        Ok(canonical)
    }

    fn metadata_original_path(bytes: &[u8]) -> Option<String> {
        if bytes.len() < 26 {
            return None;
        }
        let version = u64::from_le_bytes(bytes.get(0..8)?.try_into().ok()?);
        let (offset, units) = if version >= 2 {
            let units = u32::from_le_bytes(bytes.get(24..28)?.try_into().ok()?) as usize;
            (28usize, units)
        } else {
            (24usize, (bytes.len() - 24) / 2)
        };
        let available = (bytes.len().saturating_sub(offset) / 2).min(units);
        let mut path = Vec::with_capacity(available);
        for chunk in bytes.get(offset..offset + available * 2)?.chunks_exact(2) {
            let unit = u16::from_le_bytes([chunk[0], chunk[1]]);
            if unit == 0 {
                break;
            }
            path.push(unit);
        }
        String::from_utf16(&path).ok()
    }

    fn recycle_metadata_contains(original: &Path) -> bool {
        let root = Path::new(r"D:\$Recycle.Bin");
        let Ok(accounts) = fs::read_dir(root) else {
            return false;
        };
        for account in accounts.flatten() {
            let Ok(entries) = fs::read_dir(account.path()) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                if !name.to_string_lossy().starts_with("$I") {
                    continue;
                }
                let Ok(file) = fs::File::open(entry.path()) else {
                    continue;
                };
                let mut bytes = Vec::new();
                if file.take(64 * 1024).read_to_end(&mut bytes).is_ok()
                    && metadata_original_path(&bytes)
                        .is_some_and(|path| equal_path(Path::new(&path), original))
                {
                    return true;
                }
            }
        }
        false
    }

    #[test]
    fn lexical_guard_rejects_non_library_and_staging_paths() {
        assert!(validate_file(r"relative\song.flac").is_err());
        assert!(validate_file(r"C:\Music\song.flac").is_err());
        assert!(validate_file(r"D:\Music\..\outside.flac").is_err());
        assert!(validate_file(r"D:\Music\.DoWnLoAdS\song.flac").is_err());
        assert!(validate_file(r"D:\Music\notes.txt").is_err());
        assert!(validate_file(r"\\server\Music\song.flac").is_err());
    }

    #[test]
    fn failed_guard_never_removes_an_outside_fixture() {
        let fixture = std::env::temp_dir().join(format!(
            "musicplayer-recycle-guard-{}-{}.flac",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&fixture, b"do not recycle").unwrap();
        assert!(recycle_file(&fixture.to_string_lossy()).is_err());
        assert_eq!(fs::read(&fixture).unwrap(), b"do not recycle");
        fs::remove_file(fixture).unwrap();
    }

    #[test]
    #[ignore = "moves only a newly generated disposable gapless-assets fixture to the real Windows Recycle Bin"]
    fn native_shell_recycles_generated_fixture_and_records_original_path() {
        let root = Path::new(DISPOSABLE_ROOT);
        assert!(root.is_dir(), "generate the gapless-assets directory first");
        let fixture = root.join(format!(
            "musicplayer-recycle-smoke-{}-{}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&fixture, b"generated disposable recycle smoke fixture").unwrap();
        let original = fs::canonicalize(&fixture).unwrap();
        let result = recycle_disposable_fixture(&fixture);
        if result.is_err() && fixture.exists() {
            fs::remove_file(&fixture).unwrap();
        }
        assert_eq!(result.unwrap(), original);
        assert!(!fixture.exists(), "Shell must remove the source path");

        let recorded = (0..20).any(|_| {
            if recycle_metadata_contains(&original) {
                true
            } else {
                thread::sleep(std::time::Duration::from_millis(50));
                false
            }
        });
        assert!(
            recorded,
            "Recycle Bin metadata must record the exact disposable original path"
        );
    }
}
