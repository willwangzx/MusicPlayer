MusicPlayer - third-party notices and source references
Private-share build, Windows x64. Not a public distribution.

This package is shared privately between friends. The notes below identify the
third-party components bundled with the player and where their source/license
materials can be obtained.

1. libmpv-2.dll (mpv, LGPL v2.1+ build)
   Bundled at resources\runtime\libmpv-2.dll.
   Build: Zhongfly mpv-winbuild release 2026-09-01-174b638f29
     Release page: https://github.com/zhongfly/mpv-winbuild/releases/tag/2026-09-01-174b638f29
     mpv project:  https://mpv.io/   (source: https://github.com/mpv-player/mpv)
     FFmpeg:       https://ffmpeg.org/ (statically linked into this build)
   This is an unmodified third-party build. SHA-256:
     d7326621832fac31138d800c671d6ae6e982004cfabea887d6ca5dcf5cad1bf7
   LGPL allows replacing this file with a compatible build; a replacement can be
   dropped at the same path. If you need the corresponding source or build logs
   for this exact binary, contact the person who gave you this package.

2. WebView2 (Microsoft Edge WebView2 Runtime)
   Not included. The application requires the system-installed Evergreen
   WebView2 Runtime:
   https://developer.microsoft.com/microsoft-edge/webview2/

3. Application framework and libraries
   Tauri (https://tauri.app/), Svelte (https://svelte.dev/), Rust crates and
   JavaScript packages are used under MIT / Apache-2.0 and compatible licenses.
   Exact versions are recorded in the project manifests (src-tauri/Cargo.lock,
   package-lock.json).

4. Apple Music download component (separate archive)
   Distributed separately with its own notices file. It is not part of this
   player package and is not needed for local playback.

This notice covers a private share only. A complete dependency/license audit
is required before any public distribution of this application.
