# Dependency baseline

Pinned on 2026-09-02 for the Windows x64 Phase 0/1 baseline.

## Application toolchain

| Component | Pinned version | Role |
|---|---:|---|
| Tauri Rust crate | 2.11.5 | Native desktop shell |
| Tauri CLI | 2.11.4 | Development and packaging |
| Tauri JavaScript API | 2.11.1 | Typed frontend IPC |
| Svelte | 5.57.0 | UI |
| TypeScript | 6.0.3 | Frontend type checking |
| Vite | 8.2.2 | Frontend build |
| Rust MSVC toolchain | stable x86_64 | Native core |
| Lofty | 0.24.0 | Read-only local audio metadata (MIT/Apache-2.0) |
| windows | 0.61.3 | Native Windows SMTC media-button integration (MIT/Apache-2.0) |

Phase 2 adds Lofty with an exact pin. Its Rust minimum is 1.85, reflected in
the application manifest. Scan-time artwork reads are disabled; individual tag allocations
are capped at 2 MiB and cumulative metadata reads at 16 MiB per file.
On-demand covers accept PNG/JPEG up to 512 KiB and 2048 pixels per dimension.
API reference: <https://docs.rs/lofty/0.24.0/lofty/config/index.html>.

The JavaScript versions are exact in `package.json` and `package-lock.json`.
Rust resolution is retained in `src-tauri/Cargo.lock`.

## libmpv development/runtime baseline

- Source: Zhongfly Windows build, which is one of the build providers linked
  by mpv's official Windows installation page.
- Release: `2026-09-01-174b638f29`
- mpv commit: `174b638f29`
- Archive: `mpv-dev-lgpl-x86_64-20260901-git-174b638f29.7z`
- Archive SHA-256:
  `10921fef90792a843c72cabc1aeecdc62785e383c957e930df57fe939f51d054`
- CPU profile: generic x86_64, not x86_64-v3.
- Expected client API: major 2 (the matching header reports 2.5).

Run `scripts\Install-Libmpv.ps1` to fetch and hash-check the DLL. The binary is
kept in the ignored `src-tauri\bin` directory and is loaded by absolute path
with `LoadLibraryExW`; the installer does not alter system or user `PATH`.

The downloaded LGPL package is the only acceptable development candidate.
The similarly named standard `mpv-dev-*` package is a GPL build and must not be
included in a MusicPlayer package unless the whole distribution decision is
revisited.

## License gate

- mpv is GPLv2+ by default and can be built in LGPLv2.1+ mode with
  `-Dgpl=false`.
- Zhongfly labels this artifact as an LGPLv2.1+ mpv build statically linked to
  an LGPLv3 FFmpeg build. That makes LGPLv3 the conservative distribution
  compliance baseline for the combined DLL.
- The upstream builder explicitly leaves final license auditing to
  distributors. Before any installer or portable ZIP is published, capture
  the exact build configuration and dependency list, include all required
  license/copyright notices, and provide the required source or written source
  offer/relinking path. This file is a technical record, not legal advice.
- The musicdl FFmpeg build is GPL/version-3 enabled and must never be reused as
  the core player's libmpv/FFmpeg runtime.

The pinned LGPL archive was downloaded and locally verified on 2026-09-03. Its
SHA-256 exactly matched the value above. The extracted `libmpv-2.dll` is
installed only in the ignored `src-tauri\bin` development directory and has
SHA-256
`d7326621832fac31138d800c671d6ae6e982004cfabea887d6ca5dcf5cad1bf7`.
No libmpv binary is committed at this phase.

The development archive contains the DLL, import library, and public headers,
but no license notices or build-configuration record. Therefore local runtime
verification is now complete, while the release-distribution notice/source
audit remains open. The core source and UI also remain able to start in an
actionable `libmpv unavailable` state when the optional runtime is absent.

The 2026-09-14 exact-runtime recheck, captured native configuration, upstream
build-artifact evidence, machine-readable audit gate, and remaining source
requirements are in [the release audit](release/README.md). Distribution stays
disabled until those materials are complete; run
`python scripts/check_release_audit.py` to verify the recorded gate.
