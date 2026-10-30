<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { listen, type UnlistenFn } from '@tauri-apps/api/event';
  import { onMount } from 'svelte';
  import Cover from './Cover.svelte';
  import RecycleTrack from './RecycleTrack.svelte';
  import RecycleSelection from './RecycleSelection.svelte';
  import { artworkCacheVersion, invalidateArtworkCache } from './artwork';
  export let mode: 'Tracks' | 'Albums' = 'Tracks';
  interface Track { id: string; path: string; title: string; artist: string; album: string; number: number; durationMs: number; metadataWarning: boolean }
  interface Album { id: string; title: string; artist: string; trackCount: number }
  interface Status { scanning: boolean; revision: number; tracks: number; warnings: number; message: string }
  interface AlbumTrackPage { tracks: Track[]; total: number; revision: number }
  let status: Status = { scanning: false, revision: 0, tracks: 0, warnings: 0, message: 'Loading library…' };
  let tracks: Track[] = [];
  let albums: Album[] = [];
  let selectedAlbum: Album | null = null;
  let albumTracks: Track[] = [];
  let albumTotal = 0;
  let albumOffset = 0;
  let albumRequest = 0;
  let albumLoading = false;
  let total = 0;
  let query = '';
  let offset = 0;
  let error = '';
  let request = 0;
  let timer: ReturnType<typeof setTimeout>;
  let scanRefreshTimer: ReturnType<typeof setTimeout> | undefined;
  let disposed = false;
  let loading = false;
  let artworkVersion = artworkCacheVersion();
  let selecting = false;
  let selectedTracks: Track[] = [];
  let batchBusy = false;
  $: visibleTracks = selectedAlbum ? albumTracks : mode === 'Tracks' ? tracks : [];
  $: selectedPaths = new Set(selectedTracks.map(track => track.path));
  $: hiddenSelection = selectedTracks.filter(track => !visibleTracks.some(visible => visible.path === track.path)).length;
  function selectTrack(track: Track, checked: boolean) {
    if (batchBusy || status.scanning) return;
    selectedTracks = checked ? [...selectedTracks.filter(selected => selected.path !== track.path), track] : selectedTracks.filter(selected => selected.path !== track.path);
  }
  function selectVisible() {
    const selection = new Map(selectedTracks.map(track => [track.path, track]));
    for (const track of visibleTracks) selection.set(track.path, track);
    selectedTracks = [...selection.values()];
  }
  function finishSelection() { selecting = false; selectedTracks = []; }
  function recycledSelection(paths: string[]) {
    const moved = new Set(paths);
    selectedTracks = selectedTracks.filter(track => !moved.has(track.path));
    recycled();
  }
  async function refresh() {
    const id = ++request;
    loading = true;
    try {
      if (mode === 'Albums') {
        const page = await invoke<{albums: Album[]; total: number}>('library_albums', {query, offset});
        if (!disposed && id === request) { albums = page.albums; total = page.total; }
      } else {
        const page = await invoke<{tracks: Track[]; total: number}>('library_tracks', {query, offset});
        if (!disposed && id === request) { tracks = page.tracks; total = page.total; }
      }
    } catch (e) { if (!disposed && id === request) error = String(e); }
    finally { if (!disposed && id === request) loading = false; }
  }
  function clearScanRefresh() {
    if (scanRefreshTimer) { clearTimeout(scanRefreshTimer); scanRefreshTimer = undefined; }
  }
  function search() { offset = 0; ++request; clearTimeout(timer); clearScanRefresh(); timer = setTimeout(() => void refresh(), 200); }
  async function command(name: string, args = {}) { error = ''; try { await invoke(name, args); } catch (e) { error = String(e); } }
  function scheduleScanRefresh() {
    if (scanRefreshTimer || disposed) return;
    scanRefreshTimer = setTimeout(() => {
      scanRefreshTimer = undefined;
      if (!disposed) void refresh();
    }, 400);
  }
  function applyStatus(next: Status) {
    const completed = !next.scanning && next.message.startsWith('Scan complete') && next.revision !== status.revision;
    status = next;
    if (completed) {
      invalidateArtworkCache();
      artworkVersion = artworkCacheVersion();
    }
  }
  async function refreshAlbum() {
    const id = ++albumRequest;
    const album = selectedAlbum;
    if (!album) return;
    albumLoading = true;
    try {
      const page = await invoke<AlbumTrackPage>('library_album_tracks', {id: album.id, offset: albumOffset});
      if (!disposed && id === albumRequest && selectedAlbum?.id === album.id) {
        albumTracks = page.tracks;
        albumTotal = page.total;
      }
    } catch (e) {
      if (!disposed && id === albumRequest && selectedAlbum?.id === album.id) {
        if (String(e) === 'Album is no longer in the library') { closeAlbum(); void refresh(); }
        else error = String(e);
      }
    } finally {
      if (!disposed && id === albumRequest && selectedAlbum?.id === album.id) albumLoading = false;
    }
  }
  function openAlbum(album: Album) {
    finishSelection();
    selectedAlbum = album;
    albumTracks = [];
    albumOffset = 0;
    albumTotal = album.trackCount;
    error = '';
    void refreshAlbum();
  }
  function closeAlbum() {
    finishSelection();
    selectedAlbum = null;
    ++albumRequest;
    albumTracks = [];
    albumOffset = 0;
    albumTotal = 0;
    error = '';
  }
  function playTrack(track: Track) {
    void command('set_playback_queue', {paths: [track.path]});
  }
  function playAlbum() {
    if (selectedAlbum) void command('play_album', {id: selectedAlbum.id});
  }
  function recycled() {
    offset = 0;
    albumOffset = 0;
    invalidateArtworkCache();
    artworkVersion = artworkCacheVersion();
    void refresh();
    if (selectedAlbum) void refreshAlbum();
  }
  async function scan() { await command('scan_library'); try { applyStatus(await invoke<Status>('library_status')); } catch (e) { error = String(e); } }
  async function add(track: Track, action: string) {
    try {
      const queue = await invoke<{revision: number; current: number}>('playback_queue', {offset: 0});
      await command('edit_queue', {action, path: track.path, revision: queue.revision, current: queue.current});
    } catch (e) { error = String(e); }
  }
  function duration(ms: number) { return ms ? `${Math.floor(ms / 60000)}:${String(Math.floor(ms / 1000) % 60).padStart(2, '0')}` : '—'; }
  onMount(() => {
    let unlisten: UnlistenFn | undefined;
    let received = false;
    void (async () => {
      try {
        unlisten = await listen<Status>('musicplayer://library-state', event => {
          received = true;
          const previous = status;
          const changed = previous.revision !== event.payload.revision;
          applyStatus(event.payload);
          if (!event.payload.scanning) {
            clearScanRefresh();
            if (changed || previous.scanning) {
              offset = 0;
              void refresh();
              if (selectedAlbum) { albumOffset = 0; void refreshAlbum(); }
            }
          } else if (changed) {
            if (!previous.scanning) {
              offset = 0;
              void refresh();
              if (selectedAlbum) { albumOffset = 0; void refreshAlbum(); }
            }
            else scheduleScanRefresh();
          }
        });
        if (disposed) { unlisten(); return; }
        const initial = await invoke<Status>('library_status');
        if (!disposed && !received) applyStatus(initial);
        if (!disposed) await refresh();
      } catch (e) { if (!disposed) error = String(e); }
    })();
    return () => { disposed = true; ++request; ++albumRequest; clearTimeout(timer); clearScanRefresh(); unlisten?.(); };
  });
</script>
<section class="prototype" aria-label="Local music library">
  <div class="prototype-heading">
    <div><p class="eyebrow">D:\Music</p>{#if selectedAlbum}<h2>{selectedAlbum.title}</h2>{/if}</div>
    <div class="prototype-actions"><button onclick={scan} disabled={status.scanning || batchBusy}>{status.scanning ? 'Scanning…' : 'Scan library'}</button>{#if status.scanning}<button onclick={() => command('cancel_library_scan')}>Cancel</button>{/if}</div>
  </div>
  <p aria-live="polite">{status.message} · {status.tracks} tracks · {status.warnings} warnings</p>
  {#if !selectedAlbum}<label class="search">Search your music<input bind:value={query} oninput={search} maxlength="128" placeholder="Tracks, artists or albums" /></label>{/if}
  {#if error}<p class="error-message" role="alert">{error}</p>{/if}
  {#if mode === 'Tracks' || selectedAlbum}
    <div class="selection-toolbar" aria-label="Track selection">
      {#if selecting}
        <div class="selection-controls">
          <span class="selection-count" role="status">{selectedTracks.length} selected{hiddenSelection ? ` · ${hiddenSelection} outside this page` : ''}</span>
          <button disabled={batchBusy || status.scanning || loading || albumLoading || !visibleTracks.length} onclick={selectVisible}>Select visible</button>
          <button disabled={batchBusy || !selectedTracks.length} onclick={() => selectedTracks = []}>Clear selection</button>
          <button disabled={batchBusy} onclick={finishSelection}>Done</button>
        </div>
        <RecycleSelection tracks={selectedTracks} disabled={status.scanning} bind:busy={batchBusy} onrecycled={recycledSelection} />
        <p class="selection-hint">Select songs using the checkboxes. Selections stay selected as you search or change pages.</p>
      {:else}<button disabled={status.scanning || !visibleTracks.length} onclick={() => selecting = true}>Select songs</button>{/if}
    </div>
  {/if}
  {#if selectedAlbum}
    <div class="album-detail">
      <button class="back-button" onclick={closeAlbum} aria-label="Back to albums">Back to albums</button>
      <div class="album-detail-header">
        <Cover id={selectedAlbum.id} artworkVersion={artworkVersion} />
        <div class="album-detail-copy">
          <p class="eyebrow">Album</p>
          <h3>{selectedAlbum.title}</h3>
          <p>{selectedAlbum.artist} · {albumTotal} tracks</p>
          <button class="play-album" onclick={playAlbum} aria-label={`Play album ${selectedAlbum.title} by ${selectedAlbum.artist}`}>Play album</button>
        </div>
      </div>
      <div class="track-list" aria-busy={albumLoading}>
        {#each albumTracks as track (track.id)}
          <div class="track-actions" class:is-selected={selectedPaths.has(track.path)}>
            {#if selecting}<input class="track-select" type="checkbox" checked={selectedPaths.has(track.path)} disabled={batchBusy || status.scanning} onchange={event => selectTrack(track, event.currentTarget.checked)} aria-label={`Select ${track.title} by ${track.artist}`} />{/if}
            <button class="track-row" onclick={() => playTrack(track)} aria-label={`Play ${track.title} by ${track.artist}`} title={track.metadataWarning ? 'Metadata unavailable; filename shown' : `Play ${track.title}`}>
              <span>{track.number || '♪'}</span><span><strong>{track.title}</strong><small>{track.artist}</small></span><span class="album">{track.album}</span><span>{duration(track.durationMs)}</span>
            </button>
            <button class="add-track" onclick={() => add(track, 'next')} aria-label={`Play ${track.title} next`}>Play next</button>
            <button class="add-track" onclick={() => add(track, 'enqueue')} aria-label={`Add ${track.title} to queue`}>Add to queue</button>
            <RecycleTrack path={track.path} title={track.title} disabled={status.scanning || batchBusy} onrecycled={() => recycledSelection([track.path])} />
          </div>
        {:else}<p class="empty">{albumLoading ? 'Loading…' : 'No tracks found for this album.'}</p>{/each}
      </div>
      <div class="prototype-actions"><button disabled={albumOffset === 0 || albumLoading} onclick={() => { albumOffset = Math.max(0, albumOffset - 100); void refreshAlbum(); }}>Previous page</button><span>{albumTotal ? albumOffset + 1 : 0}–{Math.min(albumOffset + 100, albumTotal)} of {albumTotal}</span><button disabled={albumOffset + 100 >= albumTotal || albumLoading} onclick={() => { albumOffset += 100; void refreshAlbum(); }}>Next page</button></div>
    </div>
  {:else}
    <div class="track-list" aria-busy={loading}>
    {#if mode === 'Albums'}
      <div class="album-grid">
        {#each albums as album (album.id)}
          <button class="album-card" onclick={() => openAlbum(album)} aria-label={`Open album ${album.title} by ${album.artist}, ${album.trackCount} tracks`}><Cover id={album.id} artworkVersion={artworkVersion} /><strong>{album.title}</strong><small>{album.artist} · {album.trackCount} tracks</small></button>
        {:else}<p class="empty">{loading ? 'Loading…' : 'No albums found. Scan your library or change your search.'}</p>{/each}
      </div>
    {:else}
      {#each tracks as track (track.id)}
        <div class="track-actions" class:is-selected={selectedPaths.has(track.path)}>
          {#if selecting}<input class="track-select" type="checkbox" checked={selectedPaths.has(track.path)} disabled={batchBusy || status.scanning} onchange={event => selectTrack(track, event.currentTarget.checked)} aria-label={`Select ${track.title} by ${track.artist}`} />{/if}
          <button class="track-row" onclick={() => playTrack(track)} aria-label={`Play ${track.title} by ${track.artist}`} title={track.metadataWarning ? 'Metadata unavailable; filename shown' : `Play ${track.title}`}>
            <span>{track.number || '♪'}</span><span><strong>{track.title}</strong><small>{track.artist}</small></span><span class="album">{track.album}</span><span>{duration(track.durationMs)}</span>
          </button>
          <button class="add-track" onclick={() => add(track, 'next')} aria-label={`Play ${track.title} next`}>Play next</button><button class="add-track" onclick={() => add(track, 'enqueue')} aria-label={`Add ${track.title} to queue`}>Add</button>
          <RecycleTrack path={track.path} title={track.title} disabled={status.scanning || batchBusy} onrecycled={() => recycledSelection([track.path])} />
        </div>
      {:else}<p class="empty">{loading ? 'Loading…' : status.scanning ? 'Your library will appear when the scan finishes.' : query ? 'No matching tracks.' : 'Choose Scan library to find your music.'}</p>{/each}
    {/if}
    </div>
    <div class="prototype-actions"><button disabled={offset === 0 || loading} onclick={() => { offset = Math.max(0, offset - 100); void refresh(); }}>Previous page</button><span>{total ? offset + 1 : 0}–{Math.min(offset + 100, total)} of {total}</span><button disabled={offset + 100 >= total || loading} onclick={() => { offset += 100; void refresh(); }}>Next page</button></div>
  {/if}
</section>
<style>
  .search { display: grid; gap: 8px; color: #a8abba; margin: 20px 0; }
  input { padding: 12px; border: 1px solid #353949; border-radius: 8px; color: inherit; background: #171a23; font: inherit; }
  .track-list { margin: 20px 0; }
  .track-actions { display: flex; align-items: center; gap: 8px; }
  .track-actions.is-selected { background: #caccff12; border-radius: 7px; }
  .track-select { width: 18px; height: 18px; margin: 0 4px; padding: 0; flex: 0 0 auto; accent-color: #caccff; cursor: pointer; }
  .selection-toolbar { display: flex; flex-wrap: wrap; align-items: center; gap: 12px; margin: 20px 0 12px; }
  .selection-controls { display: flex; align-items: center; flex-wrap: wrap; gap: 10px; flex: 1; }
  .selection-toolbar button { border: 1px solid #363a4b; background: #202431; color: #dadde8; border-radius: 7px; padding: 8px 12px; cursor: pointer; }
  .selection-toolbar button:disabled { opacity: .4; cursor: default; }
  .selection-count { color: #dadde8; font-size: 13px; font-weight: 600; }
  .selection-hint { flex-basis: 100%; margin: 0; color: #969bae; font-size: 12px; }
  .track-row { display: grid; grid-template-columns: 24px minmax(0, 2fr) minmax(0, 1fr) 48px; gap: 12px; align-items: center; flex: 1; min-width: 0; padding: 12px 8px; border: 0; border-bottom: 1px solid #292d39; color: #dadde8; background: transparent; text-align: left; cursor: pointer; }
  .track-row:hover, .track-row:focus-visible { background: #272b3d; }
  strong, small { display: block; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  small, .album { color: #969bae; } small { margin-top: 5px; }
  .empty { padding: 28px 0; color: #969bae; }
  .add-track { border: 1px solid #363a4b; background: #202431; color: #dadde8; border-radius: 7px; padding: 8px; cursor: pointer; white-space: nowrap; }
  .album-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(170px, 1fr)); gap: 20px; }
  .album-card { min-width: 0; text-align: left; background: transparent; border: 0; color: #e5e7f0; cursor: pointer; }
  .album-detail { margin-top: 20px; }
  .back-button, .play-album { border: 1px solid #363a4b; border-radius: 7px; padding: 8px 12px; color: #dadde8; background: #202431; cursor: pointer; }
  .album-detail-header { display: flex; align-items: center; gap: 20px; margin: 20px 0 28px; }
  .album-detail-header :global(.cover) { width: 132px; height: 132px; flex: 0 0 auto; }
  .album-detail-copy h3 { margin: 0 0 8px; color: #f7f7fb; font-size: 28px; letter-spacing: -0.04em; }
  .album-detail-copy p:not(.eyebrow) { margin: 0 0 15px; color: #969bae; }
  .play-album { color: #161720; background: #caccff; font-weight: 700; }
</style>
