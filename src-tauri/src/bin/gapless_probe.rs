use std::{
    env,
    path::{Path, PathBuf},
    process::ExitCode,
    time::Instant,
};

use music_player_lib::playback::mpv::{event, Mpv, LOCAL_AUDIO_OPTIONS};
use serde::Serialize;

#[derive(Serialize)]
struct ProbeReport {
    ok: bool,
    output: String,
    libmpv_path: String,
    client_api_version: String,
    start_file_events: u64,
    end_file_events: u64,
    audio_reconfig_events: u64,
    elapsed_ms: u128,
}

fn main() -> ExitCode {
    match run() {
        Ok(report) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&report).expect("serialize report")
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("gapless probe failed: {error}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<ProbeReport, String> {
    let mut arguments = env::args_os().skip(1);
    let libmpv = PathBuf::from(arguments.next().ok_or_else(usage)?);
    let output = validate_probe_output_path(&PathBuf::from(arguments.next().ok_or_else(usage)?))?;
    let inputs: Vec<PathBuf> = arguments.map(PathBuf::from).collect();
    if inputs.is_empty() || inputs.len() > 32 {
        return Err(usage());
    }
    for input in std::iter::once(&libmpv).chain(inputs.iter()) {
        if is_unc_path(input) {
            return Err(format!(
                "UNC and network probe inputs are not supported: {}",
                input.display()
            ));
        }
        if !input.is_file() {
            return Err(format!("file does not exist: {}", input.display()));
        }
    }
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }

    let output_text = path_text(&output)?;
    let mut mpv = Mpv::create(Some(&libmpv))?;
    for &(name, value) in LOCAL_AUDIO_OPTIONS {
        mpv.set_option(name, value)?;
    }
    for (name, value) in [
        ("audio-samplerate", "48000"),
        ("audio-format", "s16"),
        ("audio-channels", "stereo"),
        ("ao", "pcm"),
        ("ao-pcm-file", output_text),
        ("ao-pcm-waveheader", "yes"),
        ("idle", "yes"),
    ] {
        mpv.set_option(name, value)?;
    }
    mpv.initialize()?;
    let loaded_from = mpv.loaded_from().to_string();
    let api_version = mpv.client_api_version();
    for (index, input) in inputs.iter().enumerate() {
        let mode = if index == 0 { "replace" } else { "append" };
        mpv.command(&["loadfile", path_text(input)?, mode])?;
    }

    let started = Instant::now();
    let expected_events = inputs.len() as u64;
    let mut start_file_events = 0;
    let mut end_file_events = 0;
    let mut audio_reconfig_events = 0;
    while started.elapsed().as_secs() < 20 {
        let event_summary = mpv.wait_event(0.25);
        match event_summary.event_id {
            event::START_FILE => {
                start_file_events += 1;
                if event_summary.start_file.is_none() {
                    return Err("libmpv START_FILE did not include a playlist entry id".to_string());
                }
            }
            event::END_FILE => {
                let end_file = event_summary.end_file.ok_or_else(|| {
                    "libmpv END_FILE did not include playback details".to_string()
                })?;
                if end_file.reason != event::END_FILE_REASON_EOF || end_file.error < 0 {
                    return Err(format!(
                        "libmpv ended a probe input unexpectedly: {} ({})",
                        mpv.error_description(end_file.error),
                        end_file.error
                    ));
                }
                end_file_events += 1;
                if end_file_events >= expected_events {
                    break;
                }
            }
            event::AUDIO_RECONFIG => audio_reconfig_events += 1,
            event::QUEUE_OVERFLOW => return Err("libmpv event queue overflow".to_string()),
            event::SHUTDOWN => break,
            _ => {}
        }
    }
    if start_file_events != expected_events || end_file_events != expected_events {
        return Err(format!(
            "expected {expected_events} complete playlist entries, got {start_file_events} start and {end_file_events} end events"
        ));
    }

    // Destroying the one persistent probe instance closes the PCM output and
    // finalizes its WAV header before the analyzer reads it.
    drop(mpv);
    if !output.is_file() {
        return Err(format!("libmpv did not create {}", output.display()));
    }

    Ok(ProbeReport {
        ok: true,
        output: output.display().to_string(),
        libmpv_path: loaded_from,
        client_api_version: api_version,
        start_file_events,
        end_file_events,
        audio_reconfig_events,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

const DEFAULT_LIBRARY_ROOT: &str = r"D:\Music";
const DIAGNOSTIC_ROOT: &str = r"D:\Projects\MusicPlayer\gapless-assets";

fn validate_probe_output_path(path: &Path) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() {
        return Err("probe output path cannot be empty".to_string());
    }
    if path.to_string_lossy().len() > 32_767 {
        return Err("probe output path exceeds the Windows path limit".to_string());
    }
    if is_unc_path(path) {
        return Err("UNC and network probe outputs are not supported".to_string());
    }

    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .map_err(|error| format!("failed to resolve probe output directory: {error}"))?
            .join(path)
    };
    if is_within_root(&absolute, Path::new(DEFAULT_LIBRARY_ROOT)) {
        return Err(format!(
            "refusing probe output under the protected library root {DEFAULT_LIBRARY_ROOT}"
        ));
    }
    if !is_within_root(&absolute, Path::new(DIAGNOSTIC_ROOT)) {
        return Err(format!(
            "probe output must stay under the application-owned diagnostic root {DIAGNOSTIC_ROOT}"
        ));
    }
    if absolute.exists() {
        return Err(format!(
            "refusing to overwrite existing probe output: {}",
            absolute.display()
        ));
    }
    let parent = absolute
        .parent()
        .ok_or_else(|| "probe output has no parent directory".to_string())?;
    if parent.exists() {
        let canonical_parent = parent
            .canonicalize()
            .map_err(|error| format!("failed to resolve probe output directory: {error}"))?;
        let file_name = absolute
            .file_name()
            .ok_or_else(|| "probe output has no file name".to_string())?;
        let canonical_candidate = canonical_parent.join(file_name);
        if is_within_root(&canonical_candidate, Path::new(DEFAULT_LIBRARY_ROOT)) {
            return Err(format!(
                "refusing probe output resolving under the protected library root {DEFAULT_LIBRARY_ROOT}"
            ));
        }
        if !is_within_root(&canonical_candidate, Path::new(DIAGNOSTIC_ROOT)) {
            return Err(format!(
                "probe output resolves outside the diagnostic root {DIAGNOSTIC_ROOT}"
            ));
        }
    }
    Ok(absolute)
}

fn is_unc_path(path: &Path) -> bool {
    let value = path.to_string_lossy();
    value.starts_with(r"\\") || value.starts_with("//")
}

fn normalized_windows_path(path: &Path) -> String {
    let mut value = path.to_string_lossy().replace('/', "\\");
    if let Some(stripped) = value.strip_prefix("\\\\?\\") {
        value = stripped.to_string();
    }
    value.trim_end_matches('\\').to_ascii_lowercase()
}

fn is_within_root(path: &Path, root: &Path) -> bool {
    let path = normalized_windows_path(path);
    let root = normalized_windows_path(root);
    path == root || path.starts_with(&(root + "\\"))
}

fn path_text(path: &std::path::Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("path is not valid Unicode: {}", path.display()))
}

fn usage() -> String {
    "usage: gapless_probe <libmpv-2.dll> <output.wav> <input-audio> [input-audio ...]".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_output_is_confined_to_diagnostics() {
        assert!(validate_probe_output_path(Path::new(r"D:\Music\do-not-overwrite.m4a")).is_err());
        assert!(validate_probe_output_path(Path::new(r"C:\Temp\unscoped-probe.wav")).is_err());
        let allowed = Path::new(DIAGNOSTIC_ROOT).join("unit-test-new-output.wav");
        if !allowed.exists() {
            assert!(validate_probe_output_path(&allowed).is_ok());
        }
    }
}
