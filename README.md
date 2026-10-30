# MusicPlayer

Lightweight Windows desktop music player built with Tauri 2, Rust, Svelte 5
and TypeScript. Phase 1 playback was manually accepted on 2026-09-07.
Phase 2 now includes read-only background scanning, track search, album browsing
and playback, lazy covers, queue editing, seeking, volume, and saved library/session
state. Select **Scan library** once to index `D:\Music`; scans exclude staging and
reparse points. Rescan to refresh changes on disk. A saved queue restores stopped;
press Play to resume. Application state lives under
`%LOCALAPPDATA%\MusicPlayer\state` using bounded JSON snapshots.

Closing the window (including Alt+F4) hides MusicPlayer to the Windows system
tray while playback and downloads continue. Click the tray icon or right-click
and choose **打开 MusicPlayer** to restore it. Choose **彻底退出** in the tray
menu to stop playback, shut down download workers, save state, and exit.

**Delete file** in Tracks, album track lists, or Queue asks you to confirm the
exact audio file before moving it to Windows Recycle Bin. Successful deletion
also removes every matching playback queue entry. Restore the file from Recycle
Bin and scan again to add it back. Queue **Remove** only removes that entry and
works for past, current, and upcoming tracks. See [deletion validation](docs/delete-validation.md).

Start with the [consolidated work record and handoff](docs/work-summary-and-handoff.md).
See [Phase 2 progress](docs/phase2-progress.md) and the
[next-phase plan](docs/next-phase-plan.md).

Phase 3 adds Apple Music song/album search, album and playlist previews, and
AAC Legacy downloads. Open **Apple Music**, check the existing account, select
songs or an album, then enqueue. Open **Downloads** and press **Start** explicitly;
restarting the player restores the queue stopped. Cancel, retry, and stop after
the current track are available there. Completed downloads refresh the library.
The optional deployment is selected explicitly with `scripts/Setup-AppleMusic.ps1
-DeploymentRoot <AAC folder>`. Existing same-user DPAPI credentials are preserved;
use `-ConfigureCredentials` for secure interactive setup or refresh.
See [Phase 3 verification and limitations](docs/phase3-validation.md).

The latest local release build is `src-tauri/target/release/music-player.exe`.
Build it with `npm run tauri build -- --no-bundle` so the frontend is embedded.
For development, use `npm run tauri dev`; directly opening a development
executable requires the Vite server at `localhost:1420` and is not a standalone
launch. If a release opens a localhost connection error, rebuild with the command
above and reopen the release executable.
It automatically requests lower WebView2 memory use while minimized and restores
the normal memory target on return. See [resource measurements](docs/resource-optimization.md).
Release resources resolve beside the executable; see [deployment](docs/deployment.md).
[Phase 4 validation](docs/phase4-validation.md) records final hardening work and
remaining acceptance gates. No distributable installer is available: exact runtime
source/notices, clean-machine, long-run and physical device checks remain open.

## Product boundaries

- The default local library is `D:\Music`.
- `D:\Music\.downloads` is downloader staging and must be excluded from every
  library scan.
- Existing files in `D:\Music` are never automatically moved, renamed or
  reorganized. The explicit, confirmed **Delete file** action may recycle one
  selected audio file; folders and sidecar files remain untouched.
- Local playback will run in the Rust process through libmpv; it must not
  depend on Python, a browser `<audio>` element or the downloader.
- Python is reserved for the optional Apple Music AAC worker and is started only
  on demand.
- This version does not use SQLite or another database.
- WSL, ALAC wrappers and `amdecrypt` are explicitly out of scope.
- The Apple Music integration is limited to the existing AAC Legacy path.

## Development

Requirements:

- Windows with the MSVC Rust target (`x86_64-pc-windows-msvc`)
- Node.js 24 or newer
- npm 11 or newer

Install dependencies and run the frontend:

```powershell
npm install
npm run dev
```

Install the pinned generic-x64 LGPL libmpv runtime, then run the desktop shell:

```powershell
.\scripts\Install-Libmpv.ps1
npm run tauri dev
```

The player loads `libmpv-2.dll` by absolute path without modifying `PATH`. If
the optional DLL is absent, the desktop shell still starts and reports an
actionable unavailable state.

Checks used by the baseline:

```powershell
npm run check
npm run build
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml
```

Run the deterministic analyzer self-test and the real libmpv PCM boundary
probe:

```powershell
.\scripts\Gapless-WavTests.ps1 -Mode SelfTest -OutputDir .\gapless-assets\selftest
.\scripts\Gapless-WavTests.ps1 -Mode Generate -OutputDir .\gapless-assets
.\scripts\Invoke-GaplessProbe.ps1
```

Exercise the production queue controller with muted Windows audio output:

```powershell
.\scripts\Invoke-QueueProbe.ps1 -Backend wasapi -TrackCount 16 -TimeoutSeconds 40
```

For an accelerated 4096-track queue stress test, use `-Backend null
-TrackCount 4096 -TimeoutSeconds 120`. See `scripts/README-queue-probe.md` for
the bounded memory samples, failure reports, and limits of these measurements.

The app keeps one libmpv instance for its lifetime. Rust owns the full logical
queue while mpv receives only the current track and one lookahead track. State
changes are emitted to Svelte instead of being polled.

The Tauri bundle remains disabled until release-runtime license auditing and
third-party notices are complete. See `docs\dependencies.md` and
`docs\gapless-test-report.md`. Phase 0 startup and idle-process observations
are recorded in `docs\resource-baseline.md`.

## Repository layout

- `src/`: Svelte/TypeScript UI shell.
- `src-tauri/`: Rust/Tauri shell, playback manager and dynamic libmpv adapter.
- `scripts/`: dependency bootstrap, deterministic WAV tests and measurements.
- `docs/`: dependency/license decisions and evidence-backed test reports.
- `DOWNLOADER_HANDOFF.md`: integration facts and safety constraints for the
  separate musicdl deployment used by the optional AAC worker.

See `PROJECT_CONSTRAINTS.md` for the short implementation contract and phase
boundaries.
