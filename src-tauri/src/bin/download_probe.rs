//! Explicit, isolated Phase 3 diagnostic entrypoint. Never runs at player startup.
use music_player_lib::downloads::{BatchInput, DownloadManager, EnqueueRequest, TrackInput};
use serde_json::{json, Value};
use std::{
    path::Path,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        return Err("Usage: download_probe check | search <songs|albums> <query> | album <id> [--download-one|--download-album]".into());
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(format!("gapless-assets/download-probe-{stamp}"));
    std::fs::create_dir_all(&directory).map_err(|_| "Cannot create diagnostic folder")?;
    std::env::set_var("LOCALAPPDATA", directory.join("appdata"));
    let manager = DownloadManager::new(None);
    let mut report = json!({"initial":manager.snapshot(),"evidence":directory});
    let result: Result<(), String> = (|| {
        let response = match args[0].as_str() {
            "check" => manager.catalog(json!({"command":"check"}))?,
            "search" if args.len() >= 3 => manager
                .catalog(json!({"command":"search","kind":args[1],"query":args[2..].join(" ")}))?,
            "album" if args.len() >= 2 => {
                manager.catalog(json!({"command":"preview_album","catalog_id":args[1]}))?
            }
            _ => return Err("Invalid diagnostic arguments".into()),
        };
        report["result"] = response.clone();
        if args
            .iter()
            .any(|a| a == "--download-one" || a == "--download-album")
        {
            if args[0] != "album" {
                return Err("A selected album is required for the download probe".into());
            }
            let all = args.iter().any(|a| a == "--download-album");
            let preview = response["tracks"]
                .as_array()
                .ok_or("Invalid album preview")?;
            let tracks: Vec<TrackInput> = preview
                .iter()
                .filter(|t| {
                    t["available"] == true
                        && t["existing"] != true
                        && t["queued"] != true
                        && t["duplicate"] != true
                })
                .take(if all { 4096 } else { 1 })
                .map(|t| serde_json::from_value(t.clone()).map_err(|_| "Invalid preview track"))
                .collect::<Result<_, _>>()?;
            if tracks.is_empty() {
                return Err("No new available track in this album".into());
            }
            report["selection"] = json!({"count":tracks.len(), "orderedCatalogIds":tracks.iter().map(|t| &t.catalog_id).collect::<Vec<_>>(),
                "existing":preview.iter().filter(|t| t["existing"] == true).count(),
                "unavailable":preview.iter().filter(|t| t["available"] != true).count(),
                "duplicates":preview.iter().filter(|t| t["duplicate"] == true).count()});
            report["enqueue"] = manager.enqueue(EnqueueRequest {
                tracks,
                batch: Some(BatchInput {
                    title: response["title"].as_str().unwrap_or("Album").into(),
                    artist: response["artist"].as_str().unwrap_or("").into(),
                    album_id: Some(args[1].clone()),
                }),
            })?;
            report["beforeStart"] = manager.snapshot();
            if report["beforeStart"]["running"] != false {
                return Err("Enqueue unexpectedly started downloading".into());
            }
            manager.start()?;
            let deadline = Instant::now() + Duration::from_secs(1900);
            let mut previous = Value::Null;
            let mut observations = Vec::new();
            while manager.snapshot()["running"] == true && Instant::now() < deadline {
                let page = manager.page(0);
                let current = manager.snapshot()["runningJobId"].clone();
                let observation = page["jobs"].as_array().and_then(|jobs| jobs.iter().find(|j| j["id"] == current))
                    .map(|j| json!({"id":j["id"],"state":j["state"],"bytesDone":j["bytesDone"],"bytesTotal":j["bytesTotal"]})).unwrap_or(Value::Null);
                if observation != previous && observations.len() < 8000 {
                    observations.push(observation.clone());
                    previous = observation;
                }
                thread::sleep(Duration::from_millis(250));
            }
            report["timedOut"] = json!(manager.snapshot()["running"] == true);
            manager.shutdown();
            let mut jobs = Vec::new();
            for offset in (0..4096).step_by(100) {
                let page = manager.page(offset);
                if let Some(part) = page["jobs"].as_array() {
                    jobs.extend(part.iter().cloned());
                }
                if offset + 100 >= page["total"].as_u64().unwrap_or(0) as usize {
                    break;
                }
            }
            report["observations"] = json!(observations);
            report["jobs"] = json!(jobs);
            report["queue"] = manager.page(0);
            if report["timedOut"] == true
                || jobs.is_empty()
                || jobs
                    .iter()
                    .any(|j| j["state"] != "completed" && j["state"] != "skipped")
            {
                return Err(
                    "Download batch did not fully complete; inspect safe job states in report"
                        .into(),
                );
            }
        }
        Ok(())
    })();
    manager.shutdown();
    report["ok"] = json!(result.is_ok());
    if let Err(error) = &result {
        report["error"] = json!(error);
    }
    std::fs::write(
        directory.join("probe-report.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .map_err(|_| "Cannot save diagnostic report")?;
    println!(
        "{}",
        json!({"ok":report["ok"],"evidence":directory,"error":report["error"],"title":report["result"]["title"],"trackCount":report["result"]["trackCount"],"queue":report["queue"]})
    );
    result
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
