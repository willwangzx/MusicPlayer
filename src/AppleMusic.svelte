<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import {
    asRecord,
    formatDuration,
    normalizePreview,
    normalizeTrack,
    type AccountStatus,
    type ApplePreview,
    type AppleSearchResult,
    type BatchInput,
    type DownloadTrackInput,
    type EnqueueRequest,
    type EnqueueResult,
    type SearchKind,
  } from './downloadTypes';

  const PREVIEW_PAGE_SIZE = 100;
  const MAX_TRACKS = 4096;

  let account: AccountStatus = {};
  let accountLoading = true;
  let accountError = '';
  let kind: SearchKind = 'songs';
  let query = '';
  let results: AppleSearchResult[] = [];
  let resultTotal = 0;
  let searchLoading = false;
  let searchError = '';
  let selectedSearch = new Set<string>();
  let preview: ApplePreview | null = null;
  let previewLoading = false;
  let previewError = '';
  let selectedPreview = new Set<string>();
  let previewOffset = 0;
  let url = '';
  let enqueueMessage = '';
  let enqueueError = '';
  let disposed = false;
  let searchRequest = 0;
  let previewRequest = 0;
  let searchTimer: ReturnType<typeof setTimeout> | undefined;

  $: accountConnected = account.authenticated === true;
  $: availablePreviewTracks = preview?.tracks.filter(track => isSelectable(track)) ?? [];
  $: selectedPreviewCount = availablePreviewTracks.filter(track => selectedPreview.has(track.catalogId)).length;
  $: selectedSearchTracks = results.filter(track => isSelectable(track) && selectedSearch.has(track.catalogId));
  $: visiblePreviewTracks = preview?.tracks.slice(previewOffset, previewOffset + PREVIEW_PAGE_SIZE) ?? [];
  $: previewLastOffset = preview?.tracks.length ? Math.floor((preview.tracks.length - 1) / PREVIEW_PAGE_SIZE) * PREVIEW_PAGE_SIZE : 0;

  function searchResults(value: unknown): AppleSearchResult[] {
    const data = asRecord(value);
    const raw = Array.isArray(data.results) ? data.results : [];
    return raw.map(item => {
      const track = normalizeTrack(item);
      const record = asRecord(item);
      return {
        ...track,
        trackCount: typeof record.trackCount === 'number' ? record.trackCount : null,
      };
    });
  }

  function batchForPreview(): BatchInput | null {
    if (!preview) return null;
    return {
      title: preview.title ?? preview.tracks[0]?.album ?? 'Untitled album',
      artist: preview.artist ?? preview.tracks[0]?.artist ?? 'Unknown artist',
      albumId: preview.catalogId,
    };
  }

  async function loadAccount() {
    accountLoading = true;
    accountError = '';
    try {
      const next = await invoke<AccountStatus>('apple_account_status');
      if (!disposed) account = next;
    } catch (error) {
      if (!disposed) accountError = String(error);
    } finally {
      if (!disposed) accountLoading = false;
    }
  }

  async function runSearch() {
    const requestId = ++searchRequest;
    if (searchTimer) {
      clearTimeout(searchTimer);
      searchTimer = undefined;
    }
    const text = query.trim();
    if (!text) {
      results = [];
      resultTotal = 0;
      searchError = '';
      searchLoading = false;
      return;
    }
    searchLoading = true;
    searchError = '';
    try {
      const response = await invoke<unknown>('apple_search', { kind, query: text });
      if (disposed || requestId !== searchRequest) return;
      const data = asRecord(response);
      results = searchResults(response);
      resultTotal = typeof data.total === 'number' ? data.total : results.length;
      selectedSearch = new Set();
    } catch (error) {
      if (!disposed && requestId === searchRequest) searchError = String(error);
    } finally {
      if (!disposed && requestId === searchRequest) searchLoading = false;
    }
  }

  function scheduleSearch() {
    // Invalidate an in-flight response immediately when the query changes.
    // Waiting for the debounce callback would briefly show stale results.
    ++searchRequest;
    if (searchTimer) clearTimeout(searchTimer);
    searchTimer = setTimeout(() => void runSearch(), 250);
  }

  function selectKind(next: SearchKind) {
    kind = next;
    ++searchRequest;
    results = [];
    resultTotal = 0;
    selectedSearch = new Set();
    if (query.trim()) scheduleSearch();
  }

  async function openAlbum(catalogId: string | null, albumUrl: string | null = null) {
    const requestId = ++previewRequest;
    previewLoading = true;
    previewError = '';
    enqueueMessage = '';
    try {
      const response = await invoke<unknown>('apple_preview_album', { catalogId, url: albumUrl });
      if (disposed || requestId !== previewRequest) return;
      const next = normalizePreview(response);
      if (next.tracks.length > MAX_TRACKS) {
        previewError = 'This album exceeds the 4,096-track limit; no partial import was accepted.';
        preview = null;
        selectedPreview = new Set();
        previewOffset = 0;
        return;
      }
      preview = next;
      previewOffset = 0;
      selectedPreview = new Set(availableTrackIds(next));
    } catch (error) {
      if (!disposed && requestId === previewRequest) previewError = String(error);
    } finally {
      if (!disposed && requestId === previewRequest) previewLoading = false;
    }
  }

  async function openPlaylist(playlistUrl: string) {
    const requestId = ++previewRequest;
    previewLoading = true;
    previewError = '';
    enqueueMessage = '';
    try {
      const response = await invoke<unknown>('apple_preview_playlist', { url: playlistUrl });
      if (disposed || requestId !== previewRequest) return;
      const next = normalizePreview(response);
      if (next.tracks.length > MAX_TRACKS) {
        previewError = 'This playlist exceeds the 4,096-track limit; no partial import was accepted.';
        preview = null;
        selectedPreview = new Set();
        previewOffset = 0;
        return;
      }
      preview = next;
      previewOffset = 0;
      selectedPreview = new Set(availableTrackIds(next));
    } catch (error) {
      if (!disposed && requestId === previewRequest) previewError = String(error);
    } finally {
      if (!disposed && requestId === previewRequest) previewLoading = false;
    }
  }

  function availableTrackIds(value: ApplePreview): string[] {
    return value.tracks.filter(track => isSelectable(track)).map(track => track.catalogId).filter(Boolean);
  }

  function isSelectable(track: DownloadTrackInput): boolean {
    return track.available && !track.existing && !track.queued && !track.duplicate && !!track.catalogId;
  }

  function availabilityLabel(track: DownloadTrackInput): string {
    if (track.existing) return 'Already in library';
    if (track.queued) return 'Already queued';
    if (track.duplicate) return 'Duplicate in collection';
    return 'Unavailable';
  }

  function previewUrl() {
    const target = url.trim();
    if (!target) {
      previewError = 'Paste an Apple Music album or playlist URL first.';
      return;
    }
    const pathKind = target.match(/\/(album|playlist)\//i)?.[1]?.toLowerCase();
    if (pathKind === 'playlist') void openPlaylist(target);
    else void openAlbum(null, target);
  }

  function toggleSearch(track: AppleSearchResult) {
    const next = new Set(selectedSearch);
    if (next.has(track.catalogId)) next.delete(track.catalogId);
    else if (isSelectable(track)) next.add(track.catalogId);
    selectedSearch = next;
  }

  function togglePreview(track: DownloadTrackInput) {
    if (!isSelectable(track)) return;
    const next = new Set(selectedPreview);
    if (next.has(track.catalogId)) next.delete(track.catalogId);
    else next.add(track.catalogId);
    selectedPreview = next;
  }

  function selectAllPreview() {
    selectedPreview = new Set(availableTrackIds(preview as ApplePreview));
  }

  function clearPreviewSelection() {
    selectedPreview = new Set();
  }

  async function enqueue(tracks: DownloadTrackInput[], batch: BatchInput | null = null) {
    const eligible = tracks.filter(track => isSelectable(track));
    if (!eligible.length) {
      enqueueError = 'There are no available tracks to add.';
      enqueueMessage = '';
      return;
    }
    enqueueError = '';
    enqueueMessage = '';
    const request: EnqueueRequest = {tracks: eligible, batch};
    try {
      const response = await invoke<EnqueueResult>('enqueue_downloads', {request});
      const accepted = response.accepted;
      const skipped = response.skipped;
      const unavailable = tracks.length - eligible.length;
      const details = [
        `${accepted} track${accepted === 1 ? '' : 's'} added to Downloads`,
        skipped ? `${skipped} already queued or present` : '',
        unavailable ? `${unavailable} unavailable` : '',
      ].filter(Boolean).join(' · ');
      enqueueMessage = details || 'No new tracks were added.';
      selectedSearch = new Set();
      if (preview) {
        const next = new Set(selectedPreview);
        eligible.forEach(track => next.delete(track.catalogId));
        selectedPreview = next;
      }
    } catch (error) {
      enqueueError = String(error);
    }
  }

  function queueSelectedSearch() {
    void enqueue(selectedSearchTracks);
  }

  function queueAllAlbumTracks() {
    void enqueue(availablePreviewTracks, batchForPreview());
  }

  function queueSelectedAlbumTracks() {
    void enqueue(availablePreviewTracks.filter(track => selectedPreview.has(track.catalogId)), batchForPreview());
  }

  function closePreview() {
    preview = null;
    selectedPreview = new Set();
    previewOffset = 0;
    previewError = '';
    ++previewRequest;
  }

  function displayDate(value: string | null | undefined): string {
    if (!value) return '';
    return value.length >= 4 ? value.slice(0, 4) : value;
  }

  function initials(title: string): string {
    return title.trim().split(/\s+/).slice(0, 2).map(word => word[0] ?? '').join('').toUpperCase() || '♫';
  }

  onMount(() => {
    void loadAccount();
    return () => {
      disposed = true;
      ++searchRequest;
      ++previewRequest;
      if (searchTimer) clearTimeout(searchTimer);
    };
  });
</script>

<section class="apple-page" aria-label="Apple Music downloads">
  <div class="section-heading">
    <div>
      <p class="eyebrow">Apple Music</p>
      <h2>Find music to download</h2>
      <p class="lede">Choose tracks or an album, review the selection, then add it to Downloads. Nothing starts until you press Start there.</p>
    </div>
    <div class:connected={accountConnected} class="account-badge" role="status">
      <span class="account-dot" aria-hidden="true"></span>
      {#if accountLoading}Checking access…{:else if accountConnected}Apple Music connected{:else}Apple Music unavailable{/if}
    </div>
  </div>

  {#if accountError}<p class="error-message" role="alert">{accountError}</p>{/if}
  {#if !accountLoading && !accountConnected}
    <div class="setup-card" role="status">
      <h3>Connect Apple Music</h3>
      <p>{account.message ?? 'The saved Apple Music credentials are not available to the downloader.'}</p>
      <p>Set up the existing desktop credential bridge, then return here. This screen never asks for or stores tokens.</p>
      <button class="secondary" onclick={() => void loadAccount()}>Check again</button>
    </div>
  {/if}

  <div class="source-card" aria-busy={searchLoading || previewLoading}>
    <div class="source-tabs" role="tablist" aria-label="Apple Music search type">
      <button class:active={kind === 'songs'} role="tab" aria-selected={kind === 'songs'} onclick={() => selectKind('songs')}>Songs</button>
      <button class:active={kind === 'albums'} role="tab" aria-selected={kind === 'albums'} onclick={() => selectKind('albums')}>Albums</button>
    </div>
    <form class="search-form" onsubmit={(event) => { event.preventDefault(); void runSearch(); }}>
      <label class="sr-only" for="apple-search">Search Apple Music</label>
      <input id="apple-search" bind:value={query} oninput={scheduleSearch} maxlength="512" placeholder={kind === 'songs' ? 'Search songs, artists, or albums' : 'Search albums or artists'} autocomplete="off" />
      <button class="primary" type="submit" disabled={!query.trim() || searchLoading || !accountConnected}>{searchLoading ? 'Searching…' : 'Search'}</button>
    </form>
    <form class="url-form" onsubmit={(event) => { event.preventDefault(); previewUrl(); }}>
      <label class="sr-only" for="apple-url">Apple Music album or playlist URL</label>
      <input id="apple-url" bind:value={url} maxlength="2048" placeholder="Or paste an Apple Music album or playlist URL" autocomplete="url" />
      <button class="secondary" type="submit" disabled={!url.trim() || previewLoading || !accountConnected}>{previewLoading ? 'Opening…' : 'Preview URL'}</button>
    </form>
  </div>

  {#if searchError}<p class="error-message" role="alert">{searchError}</p>{/if}
  {#if enqueueError}<p class="error-message" role="alert">{enqueueError}</p>{/if}
  {#if enqueueMessage}<p class="success-message" role="status">{enqueueMessage}</p>{/if}

  {#if results.length}
    <section class="results-card" aria-label="Apple Music search results">
      <div class="card-heading"><div><p class="eyebrow">Search results</p><h3>{resultTotal} {kind}</h3></div>{#if kind === 'songs'}<button class="primary" disabled={!selectedSearchTracks.length} onclick={queueSelectedSearch}>Queue selected ({selectedSearchTracks.length})</button>{/if}</div>
      <div class="results-list" role="list">
        {#each results as result, index (`${result.catalogId || 'result'}-${index}`)}
          <div class="result-row" role="listitem" class:album-result={kind === 'albums'} class:unavailable={!isSelectable(result)}>
            {#if kind === 'songs'}
              <input type="checkbox" aria-label={`Select ${result.title} by ${result.artist}`} checked={selectedSearch.has(result.catalogId)} disabled={!isSelectable(result)} onchange={() => toggleSearch(result)} />
            {/if}
            {#if result.artworkUrl}<img class="thumb" src={result.artworkUrl} alt="" loading="lazy" />{:else}<div class="thumb fallback" aria-hidden="true">{initials(result.album || result.title)}</div>{/if}
            <div class="result-main"><strong>{result.title}</strong><span>{result.artist}{result.album ? ` · ${result.album}` : ''}</span></div>
            <span class="result-meta">{#if kind === 'albums'}{result.trackCount ?? '—'} tracks{result.releaseDate ? ` · ${displayDate(result.releaseDate)}` : ''}{:else}{formatDuration(result.durationMs)}{/if}</span>
            {#if !isSelectable(result)}<span class="availability">{availabilityLabel(result)}</span>{/if}
            {#if kind === 'albums'}<button class="secondary" disabled={!result.catalogId} onclick={() => void openAlbum(result.catalogId, null)}>Preview album</button>{/if}
          </div>
        {/each}
      </div>
    </section>
  {:else if searchLoading}
    <p class="empty-state" role="status">Searching Apple Music…</p>
  {:else if query.trim() && !searchError}
    <p class="empty-state">No {kind} found. Try another search.</p>
  {/if}

  {#if previewError}<p class="error-message" role="alert">{previewError}</p>{/if}
  {#if preview}
    <section class="preview-card" aria-label="Apple Music preview" aria-busy={previewLoading}>
      <button class="back-link" onclick={closePreview}>← Back to search results</button>
      <div class="preview-heading">
        {#if preview.artworkUrl}<img class="cover" src={preview.artworkUrl} alt="" />{:else}<div class="cover fallback" aria-hidden="true">{initials(preview.title ?? '')}</div>{/if}
        <div class="preview-copy">
          <p class="eyebrow">{preview.kind === 'playlist' ? 'Playlist' : preview.tracks.some(track => track.disc && track.disc > 1) ? 'Album / collection' : 'Album'}</p>
          <h3>{preview.title}</h3>
          <p>{preview.artist}{preview.releaseDate ? ` · ${displayDate(preview.releaseDate)}` : ''}</p>
          <p class="preview-count">{preview.tracks.length} tracks · {availablePreviewTracks.length} available · {preview.tracks.length - availablePreviewTracks.length} unavailable or already handled</p>
        </div>
      </div>
      <div class="selection-tools">
        <span>{selectedPreviewCount} selected</span>
        <button class="text-button" onclick={selectAllPreview} disabled={!availablePreviewTracks.length}>Select all available</button>
        <button class="text-button" onclick={clearPreviewSelection} disabled={!selectedPreviewCount}>Clear selection</button>
        <button class="secondary" onclick={queueAllAlbumTracks} disabled={!availablePreviewTracks.length}>Queue {preview.kind === 'playlist' ? 'playlist' : 'album'} ({availablePreviewTracks.length})</button>
        <button class="primary" onclick={queueSelectedAlbumTracks} disabled={!selectedPreviewCount}>Queue selected ({selectedPreviewCount})</button>
      </div>
      <div class="preview-list" role="list" aria-label="Album tracks">
        {#each visiblePreviewTracks as track, index (`${track.catalogId || 'track'}-${previewOffset + index}`)}
          {@const discLabel = track.disc && track.disc > 1 ? `D${track.disc} · ` : ''}
          <label class="preview-row" class:unavailable={!isSelectable(track)}>
            <input type="checkbox" checked={selectedPreview.has(track.catalogId)} disabled={!isSelectable(track)} onchange={() => togglePreview(track)} aria-label={`Select ${track.title}`} />
            <span class="track-number">{discLabel}{track.track ?? previewOffset + index + 1}</span>
            <span class="track-copy"><strong>{track.title}</strong><small>{track.artist}</small></span>
            <span class="track-duration">{formatDuration(track.durationMs)}</span>
            {#if !isSelectable(track)}<span class="availability">{availabilityLabel(track)}</span>{/if}
          </label>
        {:else}<p class="empty-state">This collection has no tracks to display.</p>{/each}
      </div>
      <div class="preview-pagination"><button class="secondary" disabled={previewOffset === 0} onclick={() => previewOffset = Math.max(0, previewOffset - PREVIEW_PAGE_SIZE)}>Previous page</button><span>{preview.tracks.length ? `${previewOffset + 1}–${Math.min(previewOffset + PREVIEW_PAGE_SIZE, preview.tracks.length)} of ${preview.tracks.length}` : '0 tracks'}</span><button class="secondary" disabled={previewOffset >= previewLastOffset} onclick={() => previewOffset = Math.min(previewLastOffset, previewOffset + PREVIEW_PAGE_SIZE)}>Next page</button></div>
    </section>
  {/if}
</section>

<style>
  .apple-page { display: grid; gap: 17px; margin-top: 38px; }
  .section-heading, .card-heading, .preview-heading, .selection-tools { display: flex; align-items: center; justify-content: space-between; gap: 18px; }
  .section-heading { align-items: flex-start; }
  h2, h3, p { margin-top: 0; }
  h2 { margin-bottom: 10px; color: #fbfbff; font-size: clamp(23px, 3vw, 34px); letter-spacing: -0.045em; }
  h3 { margin-bottom: 5px; color: #e9eaf2; font-size: 20px; }
  .lede { max-width: 650px; margin: 0; color: #aeb1bf; font-size: 14px; line-height: 1.6; }
  .account-badge { display: flex; flex: 0 0 auto; align-items: center; gap: 8px; padding: 9px 12px; border: 1px solid rgba(237, 171, 171, .2); border-radius: 999px; color: #e7b4b4; background: rgba(212, 89, 89, .09); font-size: 12px; }
  .account-badge.connected { border-color: rgba(124, 199, 155, .28); color: #a8ddb9; background: rgba(124, 199, 155, .08); }
  .account-dot { width: 7px; height: 7px; border-radius: 50%; background: currentColor; box-shadow: 0 0 0 4px color-mix(in srgb, currentColor 12%, transparent); }
  .setup-card, .source-card, .results-card, .preview-card { padding: 22px; border: 1px solid rgba(255, 255, 255, .09); border-radius: 14px; background: rgba(255, 255, 255, .025); }
  .setup-card { border-color: rgba(229, 174, 174, .22); background: rgba(212, 89, 89, .06); }
  .setup-card p { max-width: 700px; margin-bottom: 10px; color: #b9bbc8; line-height: 1.55; }
  .source-card { display: grid; gap: 14px; }
  .source-tabs { display: flex; gap: 5px; }
  .source-tabs button, .text-button, .back-link { border: 0; color: #9699aa; background: transparent; cursor: pointer; }
  .source-tabs button { padding: 8px 12px; border-radius: 7px; }
  .source-tabs button.active { color: #f4f4ff; background: rgba(143, 147, 255, .16); font-weight: 650; }
  .search-form, .url-form { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 9px; }
  input:not([type="checkbox"]) { width: 100%; border: 1px solid rgba(255, 255, 255, .1); border-radius: 8px; padding: 11px 12px; color: #e0e2ed; background: #12141b; font: inherit; }
  input:not([type="checkbox"]):focus-visible, button:focus-visible { outline: 2px solid #aeb1ff; outline-offset: 2px; }
  button.primary, button.secondary { border-radius: 8px; padding: 9px 13px; cursor: pointer; font-weight: 650; }
  button.primary { border: 0; color: #161720; background: #caccff; }
  button.secondary { border: 1px solid #363a4b; color: #dadde8; background: #202431; }
  button.primary:disabled, button.secondary:disabled, .text-button:disabled { cursor: not-allowed; opacity: .45; }
  .error-message, .success-message { margin: 0; font: 12px ui-monospace, SFMono-Regular, Consolas, monospace; line-height: 1.5; }
  .error-message { color: #efabab; }
  .success-message { color: #a8ddb9; }
  .card-heading { align-items: flex-start; }
  .card-heading .eyebrow { margin-bottom: 6px; }
  .results-list, .preview-list { display: grid; margin-top: 16px; }
  .result-row, .preview-row { display: grid; align-items: center; gap: 12px; min-width: 0; border-bottom: 1px solid #292d39; }
  .result-row { grid-template-columns: auto 40px minmax(0, 1fr) auto auto auto; padding: 10px 0; }
  .result-row.album-result { grid-template-columns: 40px minmax(0, 1fr) auto auto; }
  .result-row.unavailable, .preview-row.unavailable { opacity: .55; }
  .thumb { width: 40px; height: 40px; border-radius: 6px; object-fit: cover; }
  .fallback { display: grid; place-items: center; color: #b9bbff; background: linear-gradient(145deg, #353962, #1d2034); font-size: 12px; font-weight: 700; }
  .result-main, .track-copy { display: grid; min-width: 0; gap: 4px; }
  .result-main strong, .track-copy strong { overflow: hidden; color: #e3e5ef; text-overflow: ellipsis; white-space: nowrap; }
  .result-main span, .track-copy small, .result-meta, .track-duration { overflow: hidden; color: #9699aa; font-size: 12px; text-overflow: ellipsis; white-space: nowrap; }
  .availability { color: #e5aaaa; font-size: 11px; white-space: nowrap; }
  .preview-card { margin-top: 0; }
  .back-link { justify-self: start; padding: 0; font-size: 12px; }
  .preview-heading { justify-content: flex-start; margin: 18px 0 24px; }
  .cover { width: 132px; height: 132px; flex: 0 0 auto; border-radius: 9px; object-fit: cover; }
  .preview-copy h3 { margin-bottom: 8px; color: #f7f7fb; font-size: 28px; letter-spacing: -.04em; }
  .preview-copy p:not(.eyebrow) { margin-bottom: 8px; color: #969bae; }
  .preview-count { font-size: 12px; }
  .selection-tools { flex-wrap: wrap; justify-content: flex-end; padding: 13px 0; border-top: 1px solid #292d39; border-bottom: 1px solid #292d39; color: #aeb1bf; font-size: 12px; }
  .selection-tools > span { margin-right: auto; }
  .text-button { padding: 4px; font-size: 12px; }
  .preview-row { grid-template-columns: auto 62px minmax(0, 1fr) auto auto; padding: 11px 0; }
  .track-number { color: #9699aa; font: 11px ui-monospace, SFMono-Regular, Consolas, monospace; }
  .preview-pagination { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding-top: 16px; color: #969bae; font-size: 12px; }
  .empty-state { margin: 0; padding: 22px 0; color: #969bae; }
  .sr-only { position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px; overflow: hidden; clip: rect(0, 0, 0, 0); white-space: nowrap; border: 0; }
  @media (max-width: 760px) {
    .section-heading { display: grid; }
    .result-row { grid-template-columns: auto 40px minmax(0, 1fr) auto; }
    .result-row.album-result { grid-template-columns: 40px minmax(0, 1fr) auto; }
    .result-meta { display: none; }
    .result-row .secondary { grid-column: 3 / -1; justify-self: start; }
    .preview-row { grid-template-columns: auto 48px minmax(0, 1fr) auto; }
    .preview-row .availability { grid-column: 3 / -1; }
  }
</style>
