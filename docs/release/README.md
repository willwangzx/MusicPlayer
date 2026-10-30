# Windows x64 release audit — 2026-09-14

**Distribution gate remains closed.** No installer or portable archive has been
produced. `src-tauri/tauri.conf.json` retains `bundle.active=false`. The current
DLL has verified identity, but its complete source and notice materials have
not been recovered. This is an evidence gap, not a request for permission.

## Verified artifact

`runtime-audit.json` records the locally rehashed 98,992,128-byte DLL and pinned
archive identity. The upstream [release](https://github.com/zhongfly/mpv-winbuild/releases/tag/2026-09-01-174b638f29)
links to [build run 33552856629](https://github.com/zhongfly/mpv-winbuild/actions/runs/33552856629).
That run publishes the same archive SHA-256. Its `64-lgpl_logs` artifact has
digest `892b5fbfdf0b9bc2b6c89a1c398ed6e5a6c8ca087525f8f7b78c47b079d9e56d`.
The public page requires sign-in to view logs; the log artifact itself was not
downloaded or inspected. A workflow success is not a dependency audit.

The local extracted archive contains only the DLL, import library, and four
public headers. It contains no license bundle, dependency lock, or source offer.
The [official installation page](https://mpv.io/installation/) identifies these
as third-party Windows builds. The [builder](https://github.com/zhongfly/mpv-winbuild)
describes LGPL mpv linked statically with LGPLv3 FFmpeg and explicitly leaves
dependency compatibility auditing to the distributor. Its general package list
includes packages used by other build variants and is not this DLL's inventory.

The actual DLL was initialized through its C API with config and scripts
disabled, null audio/video output, and no media input. It returned:

```
mpv-version: mpv v0.41.0-1020-g174b638f2
ffmpeg-version: N-126386-gc27482a18
```

Its `mpv-configuration` reported:

```
-Dprefix=/__w/mpv-winbuild/mpv-winbuild/mpv-winbuild-cmake/build64/install/x86_64-w64-mingw32 -Dlibdir=/__w/mpv-winbuild/mpv-winbuild/mpv-winbuild-cmake/build64/install/x86_64-w64-mingw32/lib -Ddefault_library=shared -Dprefer_static=true -Dgpl=false -Ddebug=true -Db_ndebug=true -Doptimization=3 -Db_lto=true -Db_lto_mode=thin -Dcplayer=false -Dlibmpv=true -Dpdf-build=disabled -Dlua=enabled -Djavascript=enabled -Dsdl2-gamepad=enabled -Dlibarchive=enabled -Dlibbluray=enabled -Ddvdnav=disabled -Duchardet=enabled -Drubberband=disabled -Dlcms2=enabled -Dopenal=disabled -Dspirv-cross=enabled -Dvulkan=enabled -Dvapoursynth=enabled -Dsubrandr=enabled -Dsixel=enabled -Dgl=enabled -Degl-angle=enabled -Dlibcurl=enabled -Dc_args=-Wno-error=int-conversion --cross-file=/__w/mpv-winbuild/mpv-winbuild/mpv-winbuild-cmake/build64/meson_cross.txt
```

FFmpeg's `avcodec_configuration`, `avcodec_license`, and
`avformat_configuration` symbols are not exported by this DLL. The mpv options
above do not establish FFmpeg's configure flags or the revisions of statically
linked libraries. [FFmpeg's distribution guidance](https://ffmpeg.org/legal.html)
requires corresponding sources and notices, including changes and configuration.

## Closing the gate

Recover the exact generic-x64 LGPL build logs, all dependency revisions, source
archives and patches. Reconcile the complete inventory with the binary and
include each applicable license/copyright notice. Provide the source and build
or relinking materials required for the combined DLL, with instructions to
replace it. A general upstream source URL or the builder's MIT license is not
a substitute for this DLL's materials. Audit Cargo/frontend release dependencies
and their notices too. Keep the optional downloader and its GPL-enabled FFmpeg
outside the core distribution.

Store reviewable material files under this directory; list their relative paths,
SHA-256 values, and kinds in `runtime-audit.json`. Required kinds are
`build_configuration`, `dependency_inventory`, `license_notices`,
`corresponding_source`, `relinking_instructions`, and `application_notices`.
Only mark `status=complete` after substantive review. The checker validates
integrity and completeness of fields; it cannot establish legal sufficiency.

Run from the repository root:

```powershell
python scripts/check_release_audit.py
python -m unittest discover -s scripts -p test_release_audit.py
```

The real audit command currently exits 1 and names missing materials. Its six
fixture tests pass, covering changed runtime/materials, missing materials,
path escape, and incomplete status. It performs no installation or copying.

## Packaging and acceptance policy

After the gate passes, enable an explicit Windows x64 bundle configuration and
include only the built player, audited libmpv, required resources and notices.
Do not use a recursive repository or `.deps` glob. Exclude user state, DPAPI
credentials, account exports, downloader environment, `.downloads`, test media,
and diagnostics. Generate a manifest of included paths, sizes and SHA-256
hashes, plus the final installer hash. Never reuse the downloader FFmpeg.

Installer/upgrade/uninstall behavior must preserve all music files. Retain
application state on upgrade and uninstall by default; any separate state reset
must identify its exact application-local directory and require an explicit user
action. Installer scripts must never recurse into `D:\Music`.

Clean Windows acceptance is still pending: no checkout or developer tools;
WebView2 prerequisite handling; install into spaces/non-ASCII path; offline first
run and local scan/playback without Python, account or network; unavailable
runtime message; state recovery across restart/upgrade; uninstall and library
preservation. Account/downloader installation is a separate test. Results on the
developer laptop do not close that gate.
