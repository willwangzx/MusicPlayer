# Project constraints

This file is the short, repository-local contract for MusicPlayer. The full
downloader handoff remains in `DOWNLOADER_HANDOFF.md`.

## Paths and ownership

- Default library root: `D:\Music`.
- Downloader staging: `D:\Music\.downloads\<job-id>`.
- The scanner must always exclude the `.downloads` directory.
- Only files created by a successful new download may later be organized.
  Existing user files are not batch-renamed, moved or rewritten.
- Automatic cleanup may target only an exact application-owned job staging path.
- User-confirmed `Delete file` (authorized 2026-09-14) may move one indexed
  audio file to Windows Recycle Bin and remove its playback queue references.
  This does not authorize permanent deletion, album-folder cleanup, or recycling
  artwork/lyrics/other sidecar files. Queue `Remove` changes only the queue.

## Runtime boundaries

- Local playback is Rust-native and remains usable when Python or musicdl is
  unavailable.
- Python starts only for an explicit Apple Music downloader action.
- Apple Music integration is AAC Legacy only. Do not call WSL, ALAC wrappers,
  `amdecrypt` or lossless scripts.
- Do not expose tokens, cookies, playback URLs, PSSH values, decryption keys,
  raw API responses or complete musicdl objects to the UI, logs or state files.

## State and dependencies

- No SQLite, `rusqlite`, Drift or any other database.
- Active state is held in Rust memory and persisted using bounded JSON/JSONL
  snapshots and logs with atomic replacement.
- Dependencies are pinned to exact versions in `package.json` and
  `src-tauri/Cargo.toml`; `package-lock.json` and `Cargo.lock` are committed
  when generated.
- libmpv is dynamically loaded from an explicit application path. Only the
  pinned generic-x64 LGPL build in `docs/dependencies.md` is eligible for a
  future package; the standard GPL build is test-only and must not ship.
- Playback uses one persistent libmpv instance and keeps only the current
  track plus one lookahead entry in mpv; Rust owns the full logical queue.
- Phase 1 was accepted after user manual testing on 2026-09-07. Library and
  full player UI implementation may proceed; see `docs/next-phase-plan.md`.

## Change discipline

- Do not clone or rewrite the deployed `musicdl` working tree as part of this
  repository.
- Do not parse the human-oriented musicdl CLI output. The future adapter must
  use a bounded JSONL machine protocol on stdout and diagnostics on stderr.
- Keep build artifacts (`node_modules`, `dist`, `src-tauri/target`) out of Git.
