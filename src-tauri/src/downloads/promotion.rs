//! Verify staged AAC files and promote only new files, with replayable intent.
use lofty::{
    config::{apply_global_options, GlobalOptions, ParseOptions},
    file::AudioFile,
    mp4::{AtomData, AtomIdent, Mp4File},
    prelude::Accessor,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::os::windows::{ffi::OsStrExt, process::CommandExt};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::AtomicBool,
    thread,
    time::{Duration, Instant},
};

const MAX_AUDIO: u64 = 2 * 1024 * 1024 * 1024;
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MediaTags {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub year: String,
    pub disc: u32,
    pub disc_total: u32,
    pub track: u32,
    pub track_total: u32,
    pub compilation: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StagedTrack {
    pub staged_path: String,
    pub catalog_id: String,
    #[serde(default)]
    pub album_id: Option<String>,
    pub tags: MediaTags,
    #[serde(default)]
    pub lyrics_path: Option<String>,
    #[serde(default)]
    pub cover_path: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileMove {
    pub source: String,
    pub target: String,
    pub size: u64,
    pub sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromotionIntent {
    pub job_id: String,
    pub catalog_id: String,
    pub audio: FileMove,
    pub sidecars: Vec<FileMove>,
    pub duplicate: bool,
}
#[derive(Clone, Debug)]
pub struct PromotionOutcome {
    pub path: String,
    pub duplicate: bool,
    pub warnings: usize,
}
pub struct PromotionService {
    root: PathBuf,
    ffprobe: PathBuf,
}

fn album_folder_name(tags: &MediaTags, album_id: Option<&str>) -> String {
    let name = safe_name(&tags.album, "Unknown Album", 55);
    if let Some(id) = album_id {
        return format!("{name} [am-{id}]");
    }
    let year: String = tags.year.chars().take(4).collect();
    if year.len() == 4 && year.bytes().all(|b| b.is_ascii_digit()) {
        format!("{name} [{year}]")
    } else {
        name
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 32 && id.bytes().all(|c| c.is_ascii_digit())
}
fn valid_job(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
}
fn clean_path(path: &Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        && path
            .to_str()
            .is_some_and(|p| !p.starts_with(r"\\") && p.encode_utf16().count() < 248)
}
fn within(path: &Path, root: &Path) -> bool {
    let p = path.to_string_lossy().replace('/', "\\").to_lowercase();
    let r = root
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase();
    p.starts_with(&(r + "\\"))
}
/// Check every ancestor, including the library root, before any read/write/move.
fn regular_ancestors(path: &Path) -> Result<(), String> {
    let mut ancestors: Vec<_> = path.ancestors().collect();
    ancestors.reverse();
    for item in ancestors {
        match fs::symlink_metadata(item) {
            Ok(m) if crate::library::reparse(&m) => {
                return Err("Filesystem links are not allowed in download paths".into())
            }
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err("Download path cannot be inspected".into()),
        }
    }
    Ok(())
}
fn hash_file(path: &Path, max: u64) -> Result<(u64, String), String> {
    regular_ancestors(path)?;
    let mut file = File::open(path).map_err(|_| "Download file cannot be read")?;
    let metadata = file
        .metadata()
        .map_err(|_| "Download file cannot be inspected")?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > max {
        return Err("Download file size is invalid".into());
    }
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    let mut total = 0;
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|_| "Download file cannot be hashed")?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > max {
            return Err("Download file exceeds its size limit".into());
        }
        hash.update(&buffer[..count]);
    }
    if total != metadata.len() {
        return Err("Download file changed during verification".into());
    }
    Ok((total, format!("{:x}", hash.finalize())))
}
fn read_tags(path: &Path) -> Result<(String, String, String, String), String> {
    apply_global_options(GlobalOptions::new().allocation_limit(2 * 1024 * 1024));
    let file = File::open(path).map_err(|_| "Audio tags cannot be read")?;
    let cancel = AtomicBool::new(false);
    let mut reader = crate::library::BudgetReader::new(file, &cancel);
    let parsed = Mp4File::read_from(
        &mut reader,
        ParseOptions::new()
            .read_cover_art(false)
            .read_properties(false),
    )
    .map_err(|_| "Audio tags are invalid")?;
    let tag = parsed.ilst().ok_or("Audio tags are missing")?;
    let id = tag
        .get(&AtomIdent::Fourcc(*b"cnID"))
        .and_then(|a| {
            a.data().find_map(|d| match d {
                AtomData::UnsignedInteger(n) => Some(n.to_string()),
                AtomData::SignedInteger(n) if *n > 0 => Some(n.to_string()),
                AtomData::Unknown { data, .. } if data.len() == 4 => {
                    Some(u32::from_be_bytes(data.as_slice().try_into().ok()?).to_string())
                }
                _ => None,
            })
        })
        .ok_or("Apple catalog tag is missing")?;
    Ok((
        id,
        tag.title().unwrap_or_default().into_owned(),
        tag.artist().unwrap_or_default().into_owned(),
        tag.album().unwrap_or_default().into_owned(),
    ))
}
fn safe_name(value: &str, fallback: &str, limit: usize) -> String {
    let mut units = 0;
    let mut result: String = value
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .take_while(|c| {
            units += c.len_utf16();
            units <= limit
        })
        .collect();
    result = result.trim().trim_end_matches(['.', ' ']).to_string();
    if result.is_empty() {
        result = fallback.into();
    }
    let stem = result.split('.').next().unwrap_or("").to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit())
    {
        result.insert(0, '_');
    }
    result
}
fn file_move(source: &Path, target: &Path, max: u64) -> Result<FileMove, String> {
    let (size, sha256) = hash_file(source, max)?;
    Ok(FileMove {
        source: source.to_string_lossy().into_owned(),
        target: target.to_string_lossy().into_owned(),
        size,
        sha256,
    })
}

impl PromotionService {
    pub fn new(root: PathBuf, ffprobe: PathBuf) -> Self {
        Self { root, ffprobe }
    }
    fn job_root(&self, id: &str) -> Result<PathBuf, String> {
        if !valid_job(id) || !clean_path(&self.root) {
            return Err("Invalid download job path".into());
        }
        let path = self.root.join(".downloads").join(id);
        regular_ancestors(&path)?;
        Ok(path)
    }
    fn validate_source(&self, path: &Path, id: &str) -> Result<(), String> {
        if !clean_path(path) || !within(path, &self.job_root(id)?) {
            return Err("Staged file is outside its job directory".into());
        }
        regular_ancestors(path)
    }
    fn validate_target(&self, path: &Path) -> Result<(), String> {
        if !clean_path(path)
            || !within(path, &self.root)
            || path.components().any(|c| {
                c.as_os_str()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(".downloads")
            })
        {
            return Err("Invalid destination path".into());
        }
        regular_ancestors(path)
    }
    fn verify_audio(&self, path: &Path, staged: &StagedTrack) -> Result<(), String> {
        if !path
            .extension()
            .is_some_and(|x| x.eq_ignore_ascii_case("m4a"))
        {
            return Err("Expected an M4A download".into());
        }
        let actual = read_tags(path)?;
        if actual.0 != staged.catalog_id
            || actual.1.is_empty()
            || actual.2.is_empty()
            || actual.3.is_empty()
            || (!staged.tags.title.is_empty() && actual.1 != staged.tags.title)
            || (!staged.tags.artist.is_empty() && actual.2 != staged.tags.artist)
            || (!staged.tags.album.is_empty() && actual.3 != staged.tags.album)
        {
            return Err("Downloaded audio tags do not match the selected song".into());
        }
        let mut child = Command::new(&self.ffprobe)
            .args([
                "-v",
                "quiet",
                "-show_entries",
                "format=format_name,duration:stream=codec_type,codec_name",
                "-of",
                "json",
            ])
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(0x08000000)
            .spawn()
            .map_err(|_| "ffprobe is unavailable")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("Verifier output is unavailable")?;
        let reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.take(65537).read_to_end(&mut bytes).map(|_| bytes)
        });
        let deadline = Instant::now() + Duration::from_secs(15);
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break Ok(s),
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err("Audio verification timed out");
                }
            }
        };
        let output = reader
            .join()
            .map_err(|_| "Audio verification failed")?
            .map_err(|_| "Audio verification failed")?;
        if !status?.success() || output.len() > 65536 {
            return Err("Audio verification failed".into());
        }
        let value: serde_json::Value =
            serde_json::from_slice(&output).map_err(|_| "Audio verification failed")?;
        let duration = value["format"]["duration"]
            .as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);
        let mp4 = value["format"]["format_name"]
            .as_str()
            .is_some_and(|s| s.split(',').any(|f| f == "mp4" || f == "m4a"));
        let audio: Vec<_> = value["streams"]
            .as_array()
            .ok_or("Audio stream is missing")?
            .iter()
            .filter(|s| s["codec_type"] == "audio")
            .collect();
        if !mp4
            || !duration.is_finite()
            || duration <= 0.0
            || audio.len() != 1
            || audio[0]["codec_name"] != "aac"
        {
            return Err("Download is not valid AAC audio".into());
        }
        Ok(())
    }
    fn catalog_paths(&self) -> Result<HashMap<String, PathBuf>, String> {
        regular_ancestors(&self.root)?;
        let mut result = HashMap::new();
        let mut stack = vec![(self.root.clone(), 0)];
        let mut visited = 0;
        while let Some((dir, depth)) = stack.pop() {
            if depth > 64 {
                return Err("Library depth limit reached while checking duplicates".into());
            }
            let entries =
                fs::read_dir(dir).map_err(|_| "Library cannot be checked for duplicates")?;
            for entry in entries {
                visited += 1;
                if visited > 500_000 {
                    return Err("Library entry limit reached while checking duplicates".into());
                }
                let entry = entry.map_err(|_| "Library entry cannot be inspected")?;
                if entry
                    .file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(".downloads")
                {
                    continue;
                }
                let path = entry.path();
                let m =
                    fs::symlink_metadata(&path).map_err(|_| "Library entry cannot be inspected")?;
                if crate::library::reparse(&m) {
                    continue;
                }
                if m.is_dir() {
                    stack.push((path, depth + 1));
                } else if m.is_file()
                    && path
                        .extension()
                        .is_some_and(|x| x.eq_ignore_ascii_case("m4a"))
                {
                    if let Ok((id, ..)) = read_tags(&path) {
                        result.entry(id).or_insert(path);
                    }
                }
            }
        }
        Ok(result)
    }
    pub fn existing_catalog_ids(&self) -> Result<HashSet<String>, String> {
        Ok(self.catalog_paths()?.into_keys().collect())
    }
    pub fn prepare(&self, job_id: &str, staged: &StagedTrack) -> Result<PromotionIntent, String> {
        if !valid_id(&staged.catalog_id)
            || staged.album_id.as_ref().is_some_and(|id| !valid_id(id))
            || [
                &staged.tags.title,
                &staged.tags.artist,
                &staged.tags.album,
                &staged.tags.album_artist,
                &staged.tags.year,
            ]
            .iter()
            .any(|s| s.chars().count() > 256)
        {
            return Err("Invalid download metadata".into());
        }
        let source = Path::new(&staged.staged_path);
        self.validate_source(source, job_id)?;
        hash_file(source, MAX_AUDIO)?;
        self.verify_audio(source, staged)?;
        let existing = self.catalog_paths()?;
        if let Some(target) = existing.get(&staged.catalog_id) {
            self.validate_target(target)?;
            return Ok(PromotionIntent {
                job_id: job_id.into(),
                catalog_id: staged.catalog_id.clone(),
                audio: file_move(source, target, MAX_AUDIO)?,
                sidecars: vec![],
                duplicate: true,
            });
        }
        let tags = &staged.tags;
        let artist = if tags.compilation {
            "Various Artists"
        } else if tags.album_artist.trim().is_empty() {
            &tags.artist
        } else {
            &tags.album_artist
        };
        let album_name = album_folder_name(tags, staged.album_id.as_deref());
        let album_dir = self
            .root
            .join(safe_name(artist, "Unknown Artist", 45))
            .join(album_name);
        let directory = if tags.disc_total > 1 {
            album_dir.join(format!("Disc {}", tags.disc.max(1)))
        } else {
            album_dir.clone()
        };
        let title = safe_name(&tags.title, "Unknown Title", 55);
        let base = format!("{:02} - {title}", tags.track);
        let mut target = directory.join(format!("{base}.m4a"));
        for index in 0..1000 {
            self.validate_target(&target)?;
            if !target.exists() {
                break;
            }
            target = directory.join(if index == 0 {
                format!("{base} [{}].m4a", staged.catalog_id)
            } else {
                format!("{base} [{}] ({index}).m4a", staged.catalog_id)
            });
        }
        if target.exists() {
            return Err("Too many destination filename collisions".into());
        }
        let mut sidecars = vec![];
        for (source, target, limit) in [
            (
                staged.lyrics_path.as_ref(),
                target.with_extension("lrc"),
                2 * 1024 * 1024,
            ),
            (
                staged.cover_path.as_ref(),
                album_dir.join("cover.jpg"),
                8 * 1024 * 1024,
            ),
        ] {
            if let Some(source) = source {
                let source = Path::new(source);
                self.validate_source(source, job_id)?;
                self.validate_target(&target)?;
                if !target.exists() {
                    sidecars.push(file_move(source, &target, limit)?);
                }
            }
        }
        Ok(PromotionIntent {
            job_id: job_id.into(),
            catalog_id: staged.catalog_id.clone(),
            audio: file_move(source, &target, MAX_AUDIO)?,
            sidecars,
            duplicate: false,
        })
    }
    fn apply_move(&self, item: &FileMove, job_id: &str, optional: bool) -> Result<bool, String> {
        let source = Path::new(&item.source);
        let target = Path::new(&item.target);
        self.validate_source(source, job_id)?;
        self.validate_target(target)?;
        if item.size == 0
            || item.size > MAX_AUDIO
            || item.sha256.len() != 64
            || !item.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("Invalid saved promotion record".into());
        }
        if target.exists() {
            if hash_file(target, MAX_AUDIO)
                .is_ok_and(|(size, hash)| size == item.size && hash == item.sha256)
            {
                return Ok(true);
            }
            if optional {
                return Ok(false);
            }
            return Err("Destination already exists; it was not overwritten".into());
        }
        if hash_file(source, MAX_AUDIO)? != (item.size, item.sha256.clone()) {
            return Err("Staged file changed after verification".into());
        }
        let parent = target.parent().ok_or("Invalid destination directory")?;
        fs::create_dir_all(parent).map_err(|_| "Destination directory cannot be created")?;
        self.validate_target(target)?;
        self.validate_source(source, job_id)?;
        // Flush only our staged output; existing library files stay read-only.
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(source)
            .and_then(|file| file.sync_all())
            .map_err(|_| "Staged file cannot be flushed")?;
        #[link(name = "kernel32")]
        extern "system" {
            fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
        }
        let from: Vec<_> = source.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<_> = target.as_os_str().encode_wide().chain(Some(0)).collect();
        // WRITE_THROUGH only: deliberately omit REPLACE_EXISTING and COPY_ALLOWED.
        if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0x8) } == 0 {
            return Err("File could not be promoted; no existing file was overwritten".into());
        }
        Ok(true)
    }
    pub fn commit(&self, intent: &PromotionIntent) -> Result<PromotionOutcome, String> {
        if !valid_id(&intent.catalog_id) || intent.sidecars.len() > 2 {
            return Err("Invalid saved promotion record".into());
        }
        self.validate_source(Path::new(&intent.audio.source), &intent.job_id)?;
        self.validate_target(Path::new(&intent.audio.target))?;
        if intent.duplicate {
            if read_tags(Path::new(&intent.audio.target))?.0 != intent.catalog_id {
                return Err("Existing duplicate changed; retry the download".into());
            }
            return Ok(PromotionOutcome {
                path: intent.audio.target.clone(),
                duplicate: true,
                warnings: 0,
            });
        }
        self.apply_move(&intent.audio, &intent.job_id, false)?;
        let mut warnings = 0;
        for sidecar in &intent.sidecars {
            if !self.apply_move(sidecar, &intent.job_id, true)? {
                warnings += 1;
            }
        }
        Ok(PromotionOutcome {
            path: intent.audio.target.clone(),
            duplicate: false,
            warnings,
        })
    }
    pub fn recover(&self, intent: &PromotionIntent) -> Result<PromotionOutcome, String> {
        self.commit(intent)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn album_identity_groups_mixed_song_years_and_separates_editions() {
        let mut tags = super::MediaTags {
            album: "21".into(),
            year: "2010-11-29".into(),
            ..Default::default()
        };
        let first = super::album_folder_name(&tags, Some("403037872"));
        tags.year = "2011-01-24".into();
        assert_eq!(first, super::album_folder_name(&tags, Some("403037872")));
        assert_eq!(first, "21 [am-403037872]");
        assert_ne!(first, super::album_folder_name(&tags, Some("999")));
        assert_eq!(super::album_folder_name(&tags, None), "21 [2011]");
    }
    use super::*;
    #[test]
    fn real_aac_verification_and_duplicate_detection() {
        let Some(ffmpeg) = std::env::var_os("MUSICPLAYER_TEST_FFMPEG") else {
            return;
        };
        use lofty::{
            config::WriteOptions,
            mp4::{Atom, Ilst},
            tag::TagExt,
        };
        let ffmpeg = PathBuf::from(ffmpeg);
        let (root, _) = fixture();
        let service =
            PromotionService::new(root.clone(), ffmpeg.parent().unwrap().join("ffprobe.exe"));
        let source = root.join(".downloads/job-1/track.m4a");
        assert!(Command::new(ffmpeg)
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=0.2",
                "-c:a",
                "aac"
            ])
            .arg(&source)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x08000000)
            .status()
            .unwrap()
            .success());
        let mut tags = Ilst::new();
        tags.set_title("Fixture".into());
        tags.set_artist("Test artist".into());
        tags.set_album("Test album".into());
        tags.insert(Atom::new(
            AtomIdent::Fourcc(*b"cnID"),
            AtomData::UnsignedInteger(123),
        ));
        tags.save_to_path(&source, WriteOptions::default()).unwrap();
        let staged = StagedTrack {
            staged_path: source.to_string_lossy().into_owned(),
            catalog_id: "123".into(),
            album_id: None,
            tags: MediaTags {
                title: "Fixture".into(),
                artist: "Test artist".into(),
                album: "Test album".into(),
                track: 1,
                ..MediaTags::default()
            },
            lyrics_path: None,
            cover_path: None,
        };
        let intent = service.prepare("job-1", &staged).unwrap();
        assert!(!service.commit(&intent).unwrap().duplicate);
        assert!(service.existing_catalog_ids().unwrap().contains("123"));
        fs::copy(&intent.audio.target, &source).unwrap();
        let duplicate = service.prepare("job-1", &staged).unwrap();
        assert!(duplicate.duplicate);
        assert!(service.commit(&duplicate).unwrap().duplicate);
        let mut mismatch = staged;
        mismatch.catalog_id = "456".into();
        assert!(service.prepare("job-1", &mismatch).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    fn fixture() -> (PathBuf, PromotionService) {
        let root = std::env::temp_dir().join(format!(
            "musicplayer-promotion-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join(".downloads/job-1")).unwrap();
        (root.clone(), PromotionService::new(root, PathBuf::new()))
    }
    #[test]
    fn windows_names_and_paths() {
        assert_eq!(safe_name("CON. ", "Unknown", 40), "_CON");
        assert_eq!(safe_name("a/b:c?", "Unknown", 40), "a_b_c_");
        let (root, service) = fixture();
        assert!(service
            .validate_source(&root.join(".downloads/job-2/a.m4a"), "job-1")
            .is_err());
        assert!(service
            .validate_target(&root.join(".downloads/x.m4a"))
            .is_err());
        assert!(service
            .validate_target(&root.join("../outside.m4a"))
            .is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn promotion_is_replayable_and_never_overwrites() {
        let (root, service) = fixture();
        let source = root.join(".downloads/job-1/track.m4a");
        let target = root.join("Artist/Album/01.m4a");
        fs::write(&source, b"verified fixture audio").unwrap();
        let intent = PromotionIntent {
            job_id: "job-1".into(),
            catalog_id: "123".into(),
            audio: file_move(&source, &target, MAX_AUDIO).unwrap(),
            sidecars: vec![],
            duplicate: false,
        };
        assert!(!service.commit(&intent).unwrap().duplicate);
        assert!(!source.exists());
        assert_eq!(
            service.recover(&intent).unwrap().path,
            target.to_string_lossy()
        );
        fs::write(&source, b"different fixture").unwrap();
        let mut conflict = intent.clone();
        conflict.audio = file_move(&source, &target, MAX_AUDIO).unwrap();
        assert!(service.commit(&conflict).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"verified fixture audio");
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn changed_source_and_sidecar_recovery() {
        let (root, service) = fixture();
        let source = root.join(".downloads/job-1/track.m4a");
        let target = root.join("Album/01.m4a");
        fs::write(&source, b"audio").unwrap();
        let audio = file_move(&source, &target, MAX_AUDIO).unwrap();
        fs::write(&source, b"changed").unwrap();
        assert!(service.apply_move(&audio, "job-1", false).is_err());
        assert!(!target.exists());
        fs::write(&source, b"audio").unwrap();
        let cover = root.join(".downloads/job-1/cover.jpg");
        fs::write(&cover, b"new cover").unwrap();
        let sidecar = file_move(&cover, &root.join("Album/cover.jpg"), MAX_AUDIO).unwrap();
        service.apply_move(&audio, "job-1", false).unwrap();
        fs::write(root.join("Album/cover.jpg"), b"existing cover").unwrap();
        let intent = PromotionIntent {
            job_id: "job-1".into(),
            catalog_id: "123".into(),
            audio,
            sidecars: vec![sidecar],
            duplicate: false,
        };
        assert_eq!(service.recover(&intent).unwrap().warnings, 1);
        assert_eq!(
            fs::read(root.join("Album/cover.jpg")).unwrap(),
            b"existing cover"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
