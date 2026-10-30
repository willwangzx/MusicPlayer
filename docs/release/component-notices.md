MusicPlayer Apple Music component - third-party notices and source references
Private-share build. Not for public distribution.

This component is installed separately by the user and registers its own folder
with MusicPlayer. It contains:

1. Python 3.13.9 runtime (embeddable distribution, python.org)
   PSF License 2.0; license text included at .venv\Scripts\LICENSE.txt
   Release page: https://www.python.org/downloads/release/python-3139/
   Archive SHA-256: 91d828c2da3a029b41699e918674a0cb379c02cf20dab9c501306885f837402a

2. musicdl 2.13.7 with local patches (PolyForm Noncommercial License 1.0.0)
   https://github.com/CharlesPikachu/musicdl
   Baseline commit ddcdc020cec79f45da67cc4a5a4fb380abab43fb.
   The patched files are pinned by SHA-256 in aac-component-manifest.json.
   Noncommercial use only. The local patches are part of this package's
   musicdl\ tree.

3. FFmpeg 9.0.1 essentials build (GPL v3)
   https://www.gyan.dev/ffmpeg/builds/  (source: https://ffmpeg.org/)
   tools\ffmpeg\ffmpeg-9.0.1-essentials_build\bin\ffmpeg.exe and ffprobe.exe.
   This is the downloader's FFmpeg only; the player's local playback never
   uses it. GPL text: https://www.gnu.org/licenses/gpl-3.0.html

4. N_m3u8DL-RE (MIT)
   https://github.com/nilaoda/N_m3u8DL-RE

5. Bento4 SDK 1.6.0-641 / mp4decrypt (GPL v2; commercial license available)
   https://www.bento4.com/

6. Additional Python packages are bundled inside .venv\Lib\site-packages.
   Each package's license is recorded in its own *.dist-info directory
   (for example requests, urllib3, mutagen, pywidevine, m3u8, pycryptodome,
   cryptography and others).

Keep this folder intact: the player verifies the pinned files by SHA-256 during
setup and before every download session.
