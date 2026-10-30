# MusicPlayer implementation constraints

- This repository targets native Windows x64. Do not invoke WSL.
- The default library is `D:\Music`; always exclude `D:\Music\.downloads`.
- Never move, rename, overwrite, or retag existing library files.
  Exception authorized on 2026-09-14: an explicit user-confirmed Delete file
  action may recycle the selected indexed audio file through Windows Recycle
  Bin. Never permanently delete it or include related folders/sidecar files.
- Keep local playback in Rust/libmpv and independent of Python/downloader state.
- Do not add SQLite or another database. Persist bounded JSON/JSONL state with
  atomic replacement in later phases.
- Apple Music integration is AAC Legacy only. Do not call ALAC wrappers,
  `amdecrypt`, lossless scripts, or human-oriented musicdl CLI output.
- Never expose or log tokens, cookies, playback URLs, PSSH, decryption keys,
  raw API responses, or full musicdl objects.
- Phase 1 was accepted by the user after manual testing on 2026-09-07.
  The prototype-only scope restriction is retired. See `docs/next-phase-plan.md`
  for the next implementation sequence and `docs/gapless-test-report.md` for evidence.
