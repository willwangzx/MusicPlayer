//! Opt-in headless exercise of the same lookahead controller as the desktop.
//! Successful completion is not a sample-boundary or device-reopen measurement.

use super::{
    event, validate_local_audio_path, LookaheadQueue, Mpv, StartFileDisposition,
    LOCAL_AUDIO_OPTIONS, MAX_QUEUE_ITEMS,
};
use serde::Serialize;
use std::{
    path::Path,
    time::{Duration, Instant},
};

#[derive(Debug, Serialize)]
pub struct QueueProbeReport {
    pub schema_version: u32,
    pub completed: bool,
    pub error: Option<String>,
    pub output_backend: String,
    pub muted: bool,
    pub libmpv_path: String,
    pub expected_tracks: usize,
    pub started_tracks: usize,
    pub eof_tracks: usize,
    pub transitions: usize,
    pub maximum_mpv_entries: usize,
    pub audio_reconfig_events: usize,
    pub playback_restart_events: usize,
    pub elapsed_ms: u128,
    pub device_reopen_verified: bool,
    pub audible_gap_verified: bool,
    pub memory_samples: Vec<MemorySample>,
}

#[derive(Debug, Serialize)]
pub struct MemorySample {
    elapsed_ms: u128,
    transitions: usize,
    working_set_bytes: usize,
    private_commit_bytes: usize,
}

#[repr(C)]
#[derive(Default)]
struct ProcessMemoryCounters {
    cb: u32,
    page_fault_count: u32,
    peak_working_set: usize,
    working_set: usize,
    quota_peak_paged: usize,
    quota_paged: usize,
    quota_peak_nonpaged: usize,
    quota_nonpaged: usize,
    pagefile: usize,
    peak_pagefile: usize,
    private_usage: usize,
}

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> *mut std::ffi::c_void;
    fn K32GetProcessMemoryInfo(
        process: *mut std::ffi::c_void,
        counters: *mut ProcessMemoryCounters,
        size: u32,
    ) -> i32;
}

fn memory_sample(started: Instant, transitions: usize) -> Result<MemorySample, String> {
    let size = std::mem::size_of::<ProcessMemoryCounters>() as u32;
    let mut counters = ProcessMemoryCounters {
        cb: size,
        ..Default::default()
    };
    if unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size) } == 0 {
        return Err("Windows process memory measurement failed".into());
    }
    Ok(MemorySample {
        elapsed_ms: started.elapsed().as_millis(),
        transitions,
        working_set_bytes: counters.working_set,
        private_commit_bytes: counters.private_usage,
    })
}

pub fn run(
    dll: &Path,
    backend: &str,
    tracks: usize,
    timeout_seconds: u64,
    inputs: &[String],
) -> Result<QueueProbeReport, String> {
    if !matches!(backend, "wasapi" | "null")
        || !(2..=MAX_QUEUE_ITEMS).contains(&tracks)
        || !(1..=86_400).contains(&timeout_seconds)
        || inputs.is_empty()
        || inputs.len() > 32
    {
        return Err("expected backend wasapi|null, 2..4096 tracks, 1..86400 timeout seconds, and 1..32 local inputs".into());
    }
    if !dll.is_absolute() || !dll.is_file() {
        return Err("provide an existing absolute libmpv DLL path".into());
    }
    let source = inputs
        .iter()
        .map(|p| validate_local_audio_path(p))
        .collect::<Result<Vec<_>, _>>()?;
    let paths = source.iter().cycle().take(tracks).cloned().collect();
    let mut mpv = Mpv::create(Some(dll))?;
    // Explicit probe requests must never silently measure a different fallback DLL.
    if Path::new(mpv.loaded_from())
        .canonicalize()
        .map_err(|e| e.to_string())?
        != dll.canonicalize().map_err(|e| e.to_string())?
    {
        return Err("requested DLL did not load; refusing fallback runtime".into());
    }
    for &(name, value) in LOCAL_AUDIO_OPTIONS {
        mpv.set_option(name, value)?;
    }
    mpv.set_option("ao", backend)?;
    mpv.set_option("mute", "yes")?;
    // Start paused so the initial mapping cannot race an untimed short fixture.
    mpv.set_option("pause", "yes")?;
    if backend == "null" {
        mpv.set_option("ao-null-untimed", "yes")?;
    }
    mpv.initialize()?;
    let mut queue = LookaheadQueue::new();
    queue.replace(&mut mpv, paths, 0)?;
    let mut report = QueueProbeReport {
        schema_version: 1,
        completed: false,
        error: None,
        output_backend: backend.into(),
        muted: true,
        libmpv_path: mpv.loaded_from().into(),
        expected_tracks: tracks,
        started_tracks: 0,
        eof_tracks: 0,
        transitions: 0,
        maximum_mpv_entries: mpv.playlist_entry_ids()?.len(),
        audio_reconfig_events: 0,
        playback_restart_events: 0,
        elapsed_ms: 0,
        device_reopen_verified: false,
        audible_gap_verified: false,
        memory_samples: Vec::new(),
    };
    let started = Instant::now();
    report.memory_samples.push(memory_sample(started, 0)?);
    let mut last_sample = Instant::now();
    let mut sampled_transitions = 0;
    mpv.command(&["set", "pause", "no"])?;
    let outcome = (|| -> Result<(), String> {
        while started.elapsed() < Duration::from_secs(timeout_seconds) {
            // At most 1440 minute samples plus 63 transition samples in a run.
            if last_sample.elapsed() >= Duration::from_secs(60)
                || report.transitions.saturating_sub(sampled_transitions) >= 64
            {
                report
                    .memory_samples
                    .push(memory_sample(started, report.transitions)?);
                sampled_transitions = report.transitions;
                last_sample = Instant::now();
            }
            let summary = mpv.wait_event(0.05);
            match summary.event_id {
                event::START_FILE => {
                    let entry = summary.start_file.ok_or("missing START_FILE payload")?;
                    match queue.on_start_file(&mut mpv, entry.playlist_entry_id)? {
                        StartFileDisposition::Ignored => {
                            return Err("unexpected stale entry in diagnostic queue".into())
                        }
                        StartFileDisposition::Transitioned => report.transitions += 1,
                        StartFileDisposition::Current => (),
                    }
                    report.started_tracks += 1;
                    let count = mpv.playlist_entry_ids()?.len();
                    report.maximum_mpv_entries = report.maximum_mpv_entries.max(count);
                    if count > 2 {
                        return Err("mpv exceeded current plus one lookahead".into());
                    }
                }
                event::END_FILE => {
                    let end = summary.end_file.ok_or("missing END_FILE payload")?;
                    if end.reason != event::END_FILE_REASON_EOF || end.error < 0 {
                        return Err(format!(
                            "unexpected track end: reason {}, code {}",
                            end.reason, end.error
                        ));
                    }
                    report.eof_tracks += 1;
                    if report.eof_tracks == tracks {
                        if report.started_tracks != tracks || report.transitions != tracks - 1 {
                            return Err("incomplete queue transition accounting".into());
                        }
                        return Ok(());
                    }
                }
                event::AUDIO_RECONFIG => report.audio_reconfig_events += 1,
                event::PLAYBACK_RESTART => report.playback_restart_events += 1,
                event::QUEUE_OVERFLOW => return Err("libmpv event queue overflow".into()),
                event::SHUTDOWN => return Err("libmpv shut down early".into()),
                _ => (),
            }
        }
        Err("queue probe timed out before all tracks reached EOF".into())
    })();
    report.elapsed_ms = started.elapsed().as_millis();
    report.completed = outcome.is_ok();
    report.error = outcome.err();
    report
        .memory_samples
        .push(memory_sample(started, report.transitions)?);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_probe_options_fail_before_loading_runtime() {
        for (backend, count, seconds) in [
            ("auto", 2, 10),
            ("null", 4097, 10),
            ("null", 1, 10),
            ("null", 2, 0),
            ("null", 2, 86401),
        ] {
            assert!(run(
                Path::new("missing.dll"),
                backend,
                count,
                seconds,
                &["missing.wav".into()]
            )
            .unwrap_err()
            .starts_with("expected backend"));
        }
    }
}
