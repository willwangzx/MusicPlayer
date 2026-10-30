"""AAC-only private worker. stdout is a bounded, allowlisted JSONL protocol."""
from __future__ import annotations
import argparse
import contextlib
import errno
import json
import logging
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import time
from urllib.parse import urlparse

MAX_LINE = 65536
MAX_ITEMS = 4096
MAX_ARTIFACT_BYTES = 2 * 1024**3
OUT = sys.stdout
logging.disable(logging.CRITICAL)

class WorkerError(Exception):
    def __init__(self, code):
        self.code = code

def text(value, limit=256):
    if value is None:
        return ''
    if not isinstance(value, (str, int, float, bool)):
        isoformat = getattr(value, 'isoformat', None)
        if not callable(isoformat):
            return ''
        value = isoformat()
    value = str(value).encode('utf-8', 'replace').decode('utf-8')
    return ''.join(c for c in value if c >= ' ' and c != '\x7f')[:limit].strip()

def number(value, default=0):
    try:
        return max(0, min(int(value), 2**32 - 1))
    except (TypeError, ValueError, OverflowError):
        return default

def catalog(value):
    value = str(value or '')
    if not re.fullmatch(r'[0-9]{1,32}', value):
        raise WorkerError('INVALID_CATALOG_ID')
    return value

def emit(event, request_id=None, **fields):
    value = {'event': event, **fields}
    if request_id is not None:
        value['request_id'] = request_id
    line = json.dumps(value, ensure_ascii=False, separators=(',', ':'))
    if len(line.encode('utf-8')) + 1 > MAX_LINE:
        raise WorkerError('PROTOCOL_LIMIT')
    OUT.write(line + '\n')
    OUT.flush()

def apple_url(value, kind):
    if not isinstance(value, str):
        raise WorkerError('INVALID_URL')
    if len(value) > 2048:
        raise WorkerError('INVALID_URL')
    parsed = urlparse(value)
    try:
        port = parsed.port
    except ValueError:
        raise WorkerError('INVALID_URL') from None
    if parsed.scheme != 'https' or parsed.hostname != 'music.apple.com' or parsed.username or parsed.password or port not in (None, 443):
        raise WorkerError('INVALID_URL')
    parts = parsed.path.strip('/').split('/')
    token = 'album' if kind == 'album' else 'playlist'
    if token not in parts or not parts[-1] or not re.fullmatch(r'[a-zA-Z0-9.\-]+', parts[-1]):
        raise WorkerError('INVALID_URL')
    return parts[-1], 'library' in parts

def artwork(value):
    url = str(value or '').replace('{w}', '256').replace('{h}', '256')
    parsed = urlparse(url)
    try:
        port = parsed.port
    except ValueError:
        return None
    return url if len(url) <= 2048 and parsed.scheme == 'https' and not parsed.username and not parsed.password and port in (None, 443) and (parsed.hostname or '').endswith('.mzstatic.com') else None

def stage_directory(value, job_id):
    if not re.fullmatch(r'[a-zA-Z0-9-]{1,64}', job_id):
        raise WorkerError('INVALID_STAGING')
    if not isinstance(value, str):
        raise WorkerError('INVALID_STAGING')
    path = Path(value)
    expected = Path('D:/Music/.downloads') / job_id
    if not path.is_absolute() or '..' in path.parts or os.path.normcase(str(path)) != os.path.normcase(str(expected)):
        raise WorkerError('INVALID_STAGING')
    for item in [*reversed(path.parents), path]:
        if item.exists() and (item.is_symlink() or getattr(item.stat(follow_symlinks=False), 'st_file_attributes', 0) & stat.FILE_ATTRIBUTE_REPARSE_POINT):
            raise WorkerError('INVALID_STAGING')
    if not path.is_dir():
        raise WorkerError('INVALID_STAGING')
    return path

def flush_staged_file(path):
    """Flush an owned staged file without truncating or rewriting its contents."""
    try:
        with Path(path).open('r+b', buffering=0) as handle:
            os.fsync(handle.fileno())
    except OSError as error:
        if error.errno == errno.ENOSPC or getattr(error, 'winerror', None) == 112:
            raise WorkerError('DISK_FULL') from None
        raise WorkerError('TAGGING_FAILED') from None

def is_disk_full(error):
    return getattr(error, 'errno', None) == errno.ENOSPC or getattr(error, 'winerror', None) == 112

def error_code(error, command=None, last_stage=None):
    """Map failures to protocol codes without exposing exception contents."""
    if isinstance(error, WorkerError):
        return error.code
    if is_disk_full(error):
        return 'DISK_FULL'
    status = getattr(getattr(error, 'response', None), 'status_code', None)
    if status in (401, 403):
        return 'AUTH_INVALID'
    if status == 404:
        return 'APPLE_UNAVAILABLE'
    if command == 'download' and last_stage == 'tagging':
        return 'TAGGING_FAILED'
    return 'APPLE_REQUEST_FAILED'

def validate_request(request):
    if not isinstance(request, dict):
        raise WorkerError('INVALID_REQUEST')
    request_id = request.get('id')
    command = request.get('command')
    if not isinstance(request_id, str) or not re.fullmatch(r'[a-zA-Z0-9-]{1,64}', request_id):
        raise WorkerError('INVALID_REQUEST')
    allowed = {
        'check': {'id', 'command'},
        'account_status': {'id', 'command'},
        'search': {'id', 'command', 'kind', 'query', 'limit'},
        'preview_album': {'id', 'command', 'catalog_id', 'url'},
        'preview_playlist': {'id', 'command', 'url'},
        'resolve_catalog': {'id', 'command', 'catalog_id'},
        'download': {'id', 'command', 'catalog_id', 'handle', 'job_id', 'staging_dir'},
        'shutdown': {'id', 'command'},
    }
    if command not in allowed or not set(request).issubset(allowed[command]):
        raise WorkerError('INVALID_REQUEST')
    return request_id, command

class Worker:
    def __init__(self, deployment):
        self.deployment = Path(deployment)
        self.token = os.environ.pop('MUSICDL_APPLE_TOKEN', '')
        self.credential_error = os.environ.pop('MUSICPLAYER_CREDENTIAL_ERROR', '')
        self.api = None
        self.utils = None
        self.handles = {}
        self.handle_generation = 0
        self.active_subscription = False
        self.last_stage = None

    def reset_handles(self):
        self.handles.clear()
        self.handle_generation += 1

    def initialize(self):
        if self.api is not None:
            return
        if self.credential_error:
            raise WorkerError('AUTH_INVALID')
        if not self.token:
            raise WorkerError('AUTH_MISSING')
        sys.path.insert(0, str(self.deployment))
        # Dependencies occasionally print on import. They never own protocol stdout.
        from musicdl.modules.utils.appleutils import AppleMusicClientAPIUtils, AppleMusicClientDownloadSongUtils, SongCodec
        self.utils = AppleMusicClientDownloadSongUtils
        self.codec = SongCodec.AAC_LEGACY
        api = AppleMusicClientAPIUtils.createfromnetscapecookies({'media-user-token': self.token}, request_overrides={'timeout': (10, 30)})
        self.api = api
        self.active_subscription = bool(api.active_subscription)
        self.token = ''

    def fetch(self, path, params=None):
        if not path.startswith('/v1/') or path.startswith('//') or '..' in path:
            raise WorkerError('INVALID_CATALOG_RESPONSE')
        response = self.api.client.get('https://amp-api.music.apple.com' + path, params=params, timeout=(10, 30), stream=True, allow_redirects=False)
        with response:
            response.raise_for_status()
            data = bytearray()
            for chunk in response.iter_content(65536):
                data.extend(chunk)
                if len(data) > 8 * 1024 * 1024:
                    raise WorkerError('CATALOG_LIMIT')
            value = json.loads(data)
        if not isinstance(value, dict):
            raise WorkerError('INVALID_CATALOG_RESPONSE')
        return value

    def base(self):
        return '/v1/catalog/' + self.api.storefront

    def track(self, raw):
        if not isinstance(raw, dict):
            raise WorkerError('INVALID_CATALOG_RESPONSE')
        attrs = raw.get('attributes', {})
        if not isinstance(attrs, dict):
            raise WorkerError('INVALID_CATALOG_RESPONSE')
        play_params = attrs.get('playParams') if isinstance(attrs.get('playParams'), dict) else {}
        identifier = str(play_params.get('catalogId') or raw.get('id', ''))
        available = raw.get('type') in ('songs', 'library-songs') and bool(re.fullmatch('[0-9]{1,32}', identifier)) and bool(play_params)
        handle = f'track-{self.handle_generation}-{len(self.handles)}'
        if len(self.handles) < MAX_ITEMS:
            self.handles[handle] = identifier
        else:
            raise WorkerError('CATALOG_LIMIT')
        album_resource = raw.get('relationships', {}).get('albums', {}).get('data', [])
        album_resource = album_resource[0] if isinstance(album_resource, list) and album_resource and isinstance(album_resource[0], dict) else {}
        album_attrs = album_resource.get('attributes', {}) if isinstance(album_resource, dict) else {}
        album_id = str(album_resource.get('id') or '')
        if not re.fullmatch(r'[0-9]{1,32}', album_id):
            album_id = ''
        return {'catalogId': identifier if available else '', 'title': text(attrs.get('name')) or 'Unknown Title', 'artist': text(attrs.get('artistName')) or 'Unknown Artist', 'album': text(attrs.get('albumName')) or 'Unknown Album', 'albumArtist': text(album_attrs.get('artistName') or attrs.get('artistName')), 'albumId': album_id or None, 'durationMs': number(attrs.get('durationInMillis')), 'disc': number(attrs.get('discNumber'), 1), 'track': number(attrs.get('trackNumber')), 'trackTotal': number(attrs.get('trackCount')), 'releaseDate': text(attrs.get('releaseDate')), 'artworkUrl': artwork(attrs.get('artwork', {}).get('url')), 'available': available, 'handle': handle}

    def search(self, request):
        kind = request.get('kind')
        query = request.get('query', '')
        if kind not in ('songs', 'albums') or not isinstance(query, str) or not query.strip() or len(query.encode('utf-8')) > 512:
            raise WorkerError('INVALID_REQUEST')
        limit = number(request.get('limit'), 25)
        if limit < 1 or limit > 50:
            raise WorkerError('INVALID_REQUEST')
        self.reset_handles()
        result = self.fetch(self.base() + '/search', {'term': query, 'types': kind, 'limit': limit})
        items = result.get('results', {}).get(kind, {}).get('data', [])
        if not isinstance(items, list) or len(items) > limit:
            raise WorkerError('INVALID_CATALOG_RESPONSE')
        for raw in items:
            if kind == 'songs':
                item = self.track(raw)
            else:
                attrs = raw.get('attributes', {})
                item = {'catalogId': catalog(raw.get('id')), 'title': text(attrs.get('name')), 'artist': text(attrs.get('artistName')), 'album': text(attrs.get('name')), 'trackCount': number(attrs.get('trackCount')), 'releaseDate': text(attrs.get('releaseDate')), 'artworkUrl': artwork(attrs.get('artwork', {}).get('url')), 'available': True}
            emit('item', request['id'], data=item)
        return {'collection': 'results', 'total': len(items)}

    def resolve_catalog(self, request):
        identifier = catalog(request.get('catalog_id'))
        raw = self.api.getsong(identifier, request_overrides={'timeout': (10, 30)}).get('data', [])
        if len(raw) != 1:
            raise WorkerError('APPLE_UNAVAILABLE')
        self.reset_handles()
        emit('item', request['id'], data=self.track(raw[0]))
        return {'collection': 'results', 'total': 1}

    def preview(self, request, kind):
        identifier = request.get('catalog_id')
        library = False
        if request.get('url'):
            identifier, library = apple_url(request['url'], kind)
        elif kind == 'album':
            identifier = catalog(identifier)
        else:
            raise WorkerError('INVALID_URL')
        plural = 'albums' if kind == 'album' else 'playlists'
        path = (f'/v1/me/library/{plural}/' if library else f'{self.base()}/{plural}/') + identifier
        raw = self.fetch(path, {'include': 'tracks', 'limit[tracks]': 100}).get('data', [])
        if len(raw) != 1:
            raise WorkerError('APPLE_UNAVAILABLE')
        parent = raw[0]
        attrs = parent.get('attributes', {})
        relationship = parent.get('relationships', {}).get('tracks', {})
        tracks = list(relationship.get('data', []))
        next_path = relationship.get('next')
        seen_pages = set()
        while next_path:
            if next_path in seen_pages or len(tracks) >= MAX_ITEMS:
                raise WorkerError('CATALOG_LIMIT')
            seen_pages.add(next_path)
            page = self.fetch(next_path)
            tracks.extend(page.get('data', []))
            next_path = page.get('next')
        if len(tracks) > MAX_ITEMS:
            raise WorkerError('CATALOG_LIMIT')
        self.reset_handles()
        safe = [self.track(item) for item in tracks]
        if kind == 'album':
            safe.sort(key=lambda t: (t['disc'], t['track']))
            disc_total = max((t['disc'] for t in safe), default=1)
            for item in safe:
                item['albumId'] = identifier
                item['discTotal'] = disc_total
        seen = set()
        for item in safe:
            item['duplicate'] = bool(item['catalogId'] and item['catalogId'] in seen)
            if item['catalogId']:
                seen.add(item['catalogId'])
            emit('item', request['id'], data=item)
        return {'collection': 'tracks', 'kind': kind, 'catalogId': identifier, 'title': text(attrs.get('name')), 'artist': text(attrs.get('artistName') or attrs.get('curatorName')), 'releaseDate': text(attrs.get('releaseDate')), 'artworkUrl': artwork(attrs.get('artwork', {}).get('url')), 'trackCount': len(safe), 'availableCount': sum(t['available'] and not t['duplicate'] for t in safe), 'unavailableCount': sum(not t['available'] for t in safe)}

    def tool(self, args, cwd, request_id, job_id, stage):
        self.last_stage = stage
        emit('stage', request_id, job_id=job_id, value=stage)
        process = subprocess.Popen(args, cwd=cwd, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, creationflags=subprocess.CREATE_NO_WINDOW)
        deadline = time.monotonic() + 1800
        last = -1
        while process.poll() is None:
            if time.monotonic() > deadline:
                process.kill()
                process.wait()
                raise WorkerError('TOOL_TIMEOUT')
            size = 0
            count = 0
            for current, dirs, files in os.walk(cwd, followlinks=False):
                dirs[:] = [d for d in dirs if not (getattr((Path(current) / d).stat(follow_symlinks=False), 'st_file_attributes', 0) & stat.FILE_ATTRIBUTE_REPARSE_POINT)]
                for filename in files:
                    count += 1
                    if count > 10000:
                        process.kill(); process.wait(); raise WorkerError('STAGING_LIMIT')
                    try:
                        size += (Path(current) / filename).stat().st_size
                    except OSError:
                        pass
            if size > MAX_ARTIFACT_BYTES:
                process.kill(); process.wait(); raise WorkerError('STAGING_LIMIT')
            if size != last:
                emit('progress', request_id, job_id=job_id, bytes_done=size, bytes_total=None)
                last = size
            time.sleep(0.25)
        if process.returncode:
            raise WorkerError('DOWNLOAD_FAILED' if stage == 'downloading' else 'PROCESSING_FAILED')

    def download_album(self, raw, identifier):
        """Resolve album identity from catalog relationships, never a song release year."""
        albums = raw.get('relationships', {}).get('albums', {}).get('data', [])
        if not albums:
            songs = self.fetch(f'{self.base()}/songs/{identifier}', {'include': 'albums'}).get('data', [])
            if len(songs) != 1 or str(songs[0].get('id')) != identifier:
                raise WorkerError('INVALID_CATALOG_RESPONSE')
            albums = songs[0].get('relationships', {}).get('albums', {}).get('data', [])
        if len(albums) != 1 or not isinstance(albums[0], dict):
            raise WorkerError('INVALID_CATALOG_RESPONSE')
        album_id = catalog(albums[0].get('id'))
        resources = self.fetch(f'{self.base()}/albums/{album_id}').get('data', [])
        if len(resources) != 1 or str(resources[0].get('id')) != album_id or resources[0].get('type') != 'albums':
            raise WorkerError('INVALID_CATALOG_RESPONSE')
        attrs = resources[0].get('attributes', {})
        if not isinstance(attrs, dict) or not text(attrs.get('name')) or not text(attrs.get('artistName')):
            raise WorkerError('INVALID_CATALOG_RESPONSE')
        return album_id, attrs

    def download(self, request):
        if not self.active_subscription:
            raise WorkerError('SUBSCRIPTION_INACTIVE')
        identifier = catalog(request.get('catalog_id') or self.handles.get(request.get('handle')))
        job_id = request.get('job_id', '')
        directory = stage_directory(request.get('staging_dir', ''), job_id)
        self.last_stage = 'resolving'
        emit('stage', request['id'], job_id=job_id, value='resolving')
        raw = self.api.getsong(identifier, request_overrides={'timeout': (10, 30)})['data'][0]
        album_id, album_attrs = self.download_album(raw, identifier)
        item = self.utils.getdownloaditem(raw, None, codec=self.codec, apple_music_api=self.api, use_wrapper=False, request_overrides={'timeout': (10, 30)})
        if not item.stream_info or not item.decryption_key:
            raise WorkerError('APPLE_UNAVAILABLE')
        ffmpeg = self.deployment / 'tools/ffmpeg/ffmpeg-9.0.1-essentials_build/bin/ffmpeg.exe'
        nm3u8 = self.deployment / 'tools/n_m3u8/N_m3u8DL-RE.exe'
        decrypt = self.deployment / 'tools/bento4/win-x64/Bento4-SDK-1-6-0-641.x86_64-microsoft-win32/bin/mp4decrypt.exe'
        if not all(p.is_file() for p in (ffmpeg, nm3u8, decrypt)):
            raise WorkerError('TOOLS_MISSING')
        encrypted, decrypted, output = [directory / name for name in ('encrypted.m4a', 'decrypted.m4a', 'track.m4a')]
        if any(path.exists() for path in (encrypted, decrypted, output)):
            raise WorkerError('STAGING_NOT_EMPTY')
        self.tool([str(nm3u8), item.stream_info.audio_track.stream_url, '--binary-merge', '--auto-select', '--no-log', '--write-meta-json', 'false', '--disable-update-check', '--log-level', 'OFF', '--save-name', 'encrypted', '--save-dir', str(directory), '--tmp-dir', str(directory), '--ffmpeg-binary-path', str(ffmpeg)], directory, request['id'], job_id, 'downloading')
        self.tool([str(decrypt), '--key', '1:' + item.decryption_key.audio_track.key, str(encrypted), str(decrypted)], directory, request['id'], job_id, 'decrypting')
        self.tool([str(ffmpeg), '-v', 'quiet', '-nostdin', '-n', '-i', str(decrypted), '-map', '0:a:0', '-c:a', 'copy', '-movflags', '+faststart', str(output)], directory, request['id'], job_id, 'remuxing')
        self.last_stage = 'tagging'
        emit('stage', request['id'], job_id=job_id, value='tagging')
        from mutagen.mp4 import MP4, MP4Cover
        media = item.media_tags
        media.title = text(media.title) or 'Unknown Title'
        media.artist = text(media.artist) or 'Unknown Artist'
        media.album = text(album_attrs['name'])
        media.album_artist = text(album_attrs['artistName'])
        media.date = text(album_attrs.get('releaseDate')) or media.date
        media.compilation = bool(album_attrs.get('isCompilation', media.compilation))
        media.title_id = int(identifier)
        audio = MP4(output)
        if audio.tags is None:
            audio.add_tags()
        audio.tags.update(media.asmp4tags())
        cover_path = None
        cover_url = artwork(item.cover_url)
        if cover_url:
            try:
                import requests
                with requests.get(cover_url, timeout=(10, 15), stream=True) as response:
                    response.raise_for_status()
                    cover = bytearray()
                    for chunk in response.iter_content(65536):
                        cover.extend(chunk)
                        if len(cover) > 512 * 1024:
                            raise WorkerError('COVER_LIMIT')
                    if cover.startswith(b'\xff\xd8\xff'):
                        cover_path = directory / 'cover.jpg'
                        cover_path.write_bytes(cover)
                        audio.tags['covr'] = [MP4Cover(bytes(cover), imageformat=MP4Cover.FORMAT_JPEG)]
            except Exception:
                cover_path = None
        audio.save()
        lyrics_path = None
        synced = getattr(item.lyrics, 'synced', None)
        if isinstance(synced, str) and len(synced.encode('utf-8')) <= 2 * 1024 * 1024:
            lyrics_path = directory / 'track.lrc'
            lyrics_path.write_text(synced, encoding='utf-8')
        flush_staged_file(output)
        tags = {'title': media.title, 'artist': media.artist, 'album': media.album, 'album_artist': media.album_artist, 'year': text(media.date), 'disc': number(media.disc), 'disc_total': number(media.disc_total), 'track': number(media.track), 'track_total': number(media.track_total), 'compilation': bool(media.compilation)}
        return {'staged_path': str(output), 'catalog_id': identifier, 'album_id': album_id, 'tags': tags, 'lyrics_path': str(lyrics_path) if lyrics_path else None, 'cover_path': str(cover_path) if cover_path else None}

    def dispatch(self, request):
        command = request.get('command')
        if command in ('check', 'account_status') and not self.token and self.api is None and not self.credential_error:
            return {'configured': False, 'authenticated': False, 'subscription': 'unknown', 'message': 'Run the existing configure_apple_music.ps1 credential setup.'}
        self.initialize()
        if command in ('check', 'account_status'):
            return {'configured': True, 'authenticated': True, 'subscription': 'active' if self.active_subscription else 'inactive', 'storefront': text(self.api.storefront)}
        if command == 'search':
            return self.search(request)
        if command in ('preview_album', 'preview_playlist'):
            return self.preview(request, command.removeprefix('preview_'))
        if command == 'resolve_catalog':
            return self.resolve_catalog(request)
        if command == 'download':
            return self.download(request)
        raise WorkerError('INVALID_REQUEST')

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--deployment', required=True)
    args = parser.parse_args()
    worker = Worker(args.deployment)
    emit('ready', protocol=1)
    with open(os.devnull, 'w') as quiet, contextlib.redirect_stdout(quiet), contextlib.redirect_stderr(quiet):
        while True:
            line = sys.stdin.buffer.readline(MAX_LINE + 2)
            if not line:
                return
            if len(line) > MAX_LINE or not line.endswith(b'\n'):
                emit('fatal', code='PROTOCOL_LIMIT')
                return
            request_id = None
            command = None
            try:
                request = json.loads(line)
                request_id, command = validate_request(request)
                if command == 'shutdown':
                    emit('finished', request_id, data={})
                    return
                data = worker.dispatch(request)
                emit('complete' if command == 'download' else 'finished', request_id, data=data)
            except Exception as error:
                emit('error', request_id, code=error_code(error, command, worker.last_stage))

if __name__ == '__main__':
    main()
