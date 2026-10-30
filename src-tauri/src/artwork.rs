//! Read-only, on-demand cover extraction. Never decode artwork on the playback thread.
use lofty::{
    config::{apply_global_options, GlobalOptions, ParseOptions},
    file::TaggedFileExt,
    picture::PictureInformation,
    probe::Probe,
};
use std::{
    fs::File,
    io::Read,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};
static BUSY: AtomicBool = AtomicBool::new(false);
const LIMIT: usize = 512 * 1024;
fn valid(bytes: &[u8]) -> bool {
    if bytes.len() > LIMIT {
        return false;
    }
    let info = if bytes.starts_with(b"\x89PNG") {
        PictureInformation::from_png(bytes)
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        PictureInformation::from_jpeg(bytes)
    } else {
        return false;
    };
    info.is_ok_and(|i| i.width > 0 && i.height > 0 && i.width <= 2048 && i.height <= 2048)
}
pub fn read(path: &str) -> Result<Vec<u8>, String> {
    if BUSY.swap(true, Ordering::Acquire) {
        return Err("Artwork reader is busy".into());
    }
    struct Release;
    impl Drop for Release {
        fn drop(&mut self) {
            BUSY.store(false, Ordering::Release);
        }
    }
    let _release = Release;
    let path = crate::playback::validate_local_audio_path(path)?;
    if let Some(parent) = path.parent() {
        for name in ["cover.jpg", "cover.png", "folder.jpg"] {
            let sidecar = parent.join(name);
            if std::fs::symlink_metadata(&sidecar).is_ok_and(|m| {
                m.is_file() && !crate::library::reparse(&m) && m.len() <= LIMIT as u64
            }) {
                let mut bytes = Vec::new();
                if File::open(&sidecar)
                    .and_then(|f| f.take(LIMIT as u64 + 1).read_to_end(&mut bytes))
                    .is_ok()
                    && valid(&bytes)
                {
                    return Ok(bytes);
                }
            }
        }
    }
    embedded(&path)
}
fn embedded(path: &Path) -> Result<Vec<u8>, String> {
    apply_global_options(GlobalOptions::new().allocation_limit(2 * 1024 * 1024));
    let file = File::open(path).map_err(|_| "Artwork file unavailable")?;
    let cancel = AtomicBool::new(false);
    let reader = crate::library::BudgetReader::new(file, &cancel);
    let parsed = Probe::new(reader).guess_file_type().ok().and_then(|p| {
        p.options(
            ParseOptions::new()
                .read_cover_art(true)
                .read_properties(false),
        )
        .read()
        .ok()
    });
    if let Some(parsed) = parsed {
        for tag in parsed.tags() {
            for picture in tag.pictures() {
                if valid(picture.data()) {
                    return Ok(picture.data().to_vec());
                }
            }
        }
    }
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn artwork_rejects_unsupported_and_oversized_payloads() {
        assert!(valid(include_bytes!("../icons/128x128.png")));
        assert!(!valid(b"<svg xmlns='http://www.w3.org/2000/svg'></svg>"));
        assert!(!valid(&vec![0; LIMIT + 1]));
    }
}
