use music_player_lib::playback::diagnostic;
use std::{env, path::PathBuf, process::ExitCode};

fn main() -> ExitCode {
    match run() {
        Ok(report) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&report).expect("serialize diagnostic")
            );
            if report.completed {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(error) => {
            eprintln!("queue probe failed: {error}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<diagnostic::QueueProbeReport, String> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.len() < 5 {
        return Err("usage: queue_probe <absolute-libmpv.dll> <wasapi|null> <track-count> <timeout-seconds> <local-input> [local-input ...]".into());
    }
    diagnostic::run(
        &PathBuf::from(&args[0]),
        &args[1],
        args[2].parse().map_err(|_| "invalid track count")?,
        args[3].parse().map_err(|_| "invalid timeout")?,
        &args[4..],
    )
}
