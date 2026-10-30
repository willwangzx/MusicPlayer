<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { listen, type UnlistenFn } from '@tauri-apps/api/event';
  import { onMount } from 'svelte';
  import {
    emptyDownloadSnapshot,
    formatBytes,
    normalizeDownloadPage,
    type DownloadBatchSummary,
    type DownloadJob,
    type DownloadPage,
    type DownloadSnapshot,
  } from './downloadTypes';

  const PAGE_SIZE = 100;
  const ACTIVE_STATES = new Set(['resolving', 'downloading', 'decrypting', 'remuxing', 'tagging', 'verifying', 'organizing']);

  let page: DownloadPage = {jobs: [], offset: 0, total: 0, snapshot: {...emptyDownloadSnapshot}, batches: []};
  let snapshot: DownloadSnapshot = {...emptyDownloadSnapshot};
  let offset = 0;
  let loading = true;
  let error = '';
  let actionError = '';
  let action: string | null = null;
  let disposed = false;
  let request = 0;
  let snapshotRevision = -1;
  let backgroundRefreshRunning = false;
  let backgroundRefreshPending = false;

  $: batchSummaries = page.batches.length
    ? [...page.batches, ...summarize(page.jobs.filter(job => !job.batchId))]
    : summarize(page.jobs);
  $: canStart = snapshot.available !== false && !snapshot.running && snapshot.queued > 0 && !action;
  $: canStop = snapshot.available !== false && snapshot.running && !snapshot.stopAfterCurrent && !action;

  function applySnapshot(next: DownloadSnapshot) {
    const revision = next.revision ?? 0;
    if (revision >= snapshotRevision) {
      snapshotRevision = revision;
      snapshot = next;
    }
  }

  function summarize(jobs: DownloadJob[]): DownloadBatchSummary[] {
    const groups = new Map<string, DownloadBatchSummary>();
    for (const job of jobs) {
      const id = job.batchId ?? `job:${job.id}`;
      let group = groups.get(id);
      if (!group) {
        group = {id, title: job.batchId ? (job.batchTitle ?? job.album) : job.title, artist: job.artist, total: 0, completed: 0, failed: 0, active: 0, pending: 0};
        groups.set(id, group);
      }
      group.total += 1;
      if (job.state === 'completed' || job.state === 'skipped') group.completed += 1;
      else if (job.state === 'failed' || job.state === 'interrupted') group.failed += 1;
      else if (ACTIVE_STATES.has(job.state)) group.active += 1;
      else group.pending += 1;
    }
    return [...groups.values()];
  }

  async function refresh(background = false) {
    const requestId = ++request;
    const requestedOffset = offset;
    // Progress events update the existing page quietly. Toggling the loading
    // row and disabled buttons on every event shifts the layout and flickers.
    if (!background) loading = true;
    try {
      const response = await invoke<unknown>('download_queue', {offset: requestedOffset});
      if (disposed || requestId !== request) return;
      const next = normalizeDownloadPage(response, requestedOffset);
      const lastOffset = next.total ? Math.floor((next.total - 1) / PAGE_SIZE) * PAGE_SIZE : 0;
      if (requestedOffset > lastOffset) {
        offset = lastOffset;
        void refresh(background);
        return;
      }
      page = {...next, offset: requestedOffset};
      applySnapshot(next.snapshot);
      error = '';
    } catch (reason) {
      if (!disposed && requestId === request) error = String(reason);
    } finally {
      if (!disposed && requestId === request) loading = false;
    }
  }

  async function refreshInBackground() {
    backgroundRefreshPending = true;
    if (backgroundRefreshRunning) return;
    backgroundRefreshRunning = true;
    try {
      do {
        backgroundRefreshPending = false;
        await refresh(true);
      } while (backgroundRefreshPending && !disposed);
    } finally {
      backgroundRefreshRunning = false;
    }
  }

  async function runAction(name: string, args: Record<string, unknown> = {}) {
    if (action) return;
    action = name;
    actionError = '';
    try {
      await invoke(name, args);
    } catch (reason) {
      if (!disposed) actionError = String(reason);
    } finally {
      if (!disposed) action = null;
    }
    if (!disposed) await refresh();
  }

  function start() {
    void runAction('start_downloads');
  }

  function stopAfterCurrent() {
    void runAction('stop_downloads_after_current');
  }

  function cancel(job: DownloadJob) {
    void runAction('cancel_download', {jobId: job.id});
  }

  function retry(job: DownloadJob) {
    void runAction('retry_download', {jobId: job.id});
  }

  function clearFinished() {
    void runAction('clear_finished_downloads');
  }

  function stateLabel(state: DownloadJob['state']): string {
    const labels: Record<DownloadJob['state'], string> = {
      queued: 'Queued', resolving: 'Resolving', downloading: 'Downloading', decrypting: 'Decrypting', remuxing: 'Remuxing', tagging: 'Tagging', verifying: 'Verifying', organizing: 'Organizing', completed: 'Completed', failed: 'Failed', interrupted: 'Interrupted', cancelled: 'Cancelled', skipped: 'Already present',
    };
    return labels[state];
  }

  function progress(job: DownloadJob): string {
    if (job.state === 'downloading' && job.bytesDone !== null && job.bytesDone !== undefined && job.bytesTotal !== null && job.bytesTotal !== undefined && job.bytesTotal > 0) {
      return `${Math.min(100, Math.round((job.bytesDone / job.bytesTotal) * 100))}% · ${formatBytes(job.bytesDone)} / ${formatBytes(job.bytesTotal)}`;
    }
    // The AAC worker reports bytes present in staging, not a known network
    // content length. Keep those useful measurements visible without inventing
    // a percentage, transfer speed, or ETA from temporary processing files.
    if (job.bytesDone !== null && job.bytesDone !== undefined) {
      if (job.state === 'downloading') return `${formatBytes(job.bytesDone)} staged · total size unknown`;
    }
    const details: Partial<Record<DownloadJob['state'], string>> = {
      queued: 'Waiting for its turn',
      resolving: 'Resolving account and audio source',
      downloading: 'Waiting for audio data · total size unknown',
      decrypting: 'Processing encrypted AAC audio',
      remuxing: 'Writing the M4A audio container',
      tagging: 'Writing track metadata, cover and lyrics',
      verifying: 'Checking audio format and metadata',
      organizing: 'Adding verified audio to the library',
      completed: 'Added to library',
      skipped: 'Already in library',
    };
    if (details[job.state]) return details[job.state]!;
    return '';
  }

  function displayDate(value: string): string {
    if (!value) return '';
    const numeric = Number(value);
    const parsed = /^\d{9,13}$/.test(value) && Number.isFinite(numeric)
      ? new Date(value.length <= 10 ? numeric * 1000 : numeric)
      : new Date(value);
    return Number.isNaN(parsed.valueOf()) ? value : parsed.toLocaleString();
  }

  function dateTime(value: string): string {
    const numeric = Number(value);
    if (/^\d{9,13}$/.test(value) && Number.isFinite(numeric)) {
      return new Date(value.length <= 10 ? numeric * 1000 : numeric).toISOString();
    }
    return value;
  }

  function initials(job: DownloadJob): string {
    return `${job.album || job.title}`.trim().split(/\s+/).slice(0, 2).map(word => word[0] ?? '').join('').toUpperCase() || '♫';
  }

  onMount(() => {
    let stop: UnlistenFn | undefined;
    void (async () => {
      try {
        // Register the listener before the first queue query so a fast worker
        // update cannot be lost between initial render and status retrieval.
        stop = await listen<DownloadSnapshot>('musicplayer://downloads', event => {
          if (disposed) return;
          applySnapshot({...emptyDownloadSnapshot, ...event.payload});
          void refreshInBackground();
        });
        if (disposed) { stop(); return; }
        await refresh();
      } catch (reason) {
        if (!disposed) error = String(reason);
      }
    })();
    return () => { disposed = true; ++request; stop?.(); };
  });
</script>

<section class="downloads-page" aria-label="Downloads" aria-busy={loading || !!action}>
  <div class="section-heading">
    <div>
      <p class="eyebrow">Downloads</p>
      <h2>Download queue</h2>
      <p class="lede">Review queued AAC downloads here. Starting the queue is always explicit; local playback remains available while it runs.</p>
    </div>
    <div class="queue-status" class:running={snapshot.running} role="status">
      <span class="status-dot" aria-hidden="true"></span>
      {#if snapshot.available === false}Unavailable{:else if snapshot.running}Downloading{:else if snapshot.queued}Ready · {snapshot.queued} queued{:else}Idle{/if}
    </div>
  </div>

  {#if error}<p class="error-message" role="alert">{error}</p>{/if}
  {#if actionError}<p class="error-message" role="alert">{actionError}</p>{/if}
  {#if snapshot.error}<p class="error-message" role="status">{snapshot.error}</p>{/if}
  <div class="toolbar">
    <div class="toolbar-copy">
      <strong>{page.total} job{page.total === 1 ? '' : 's'}</strong>
      {#if snapshot.runningJobId}<span>Current job is shown below when it is on this page.</span>{:else}<span>AAC 256 · D:\Music</span>{/if}
    </div>
    <div class="toolbar-actions">
      <button class="primary" disabled={!canStart} onclick={start}>{action === 'start_downloads' ? 'Starting…' : 'Start'}</button>
      <button class="secondary" disabled={!canStop} onclick={stopAfterCurrent}>{snapshot.stopAfterCurrent ? 'Stopping after current' : action === 'stop_downloads_after_current' ? 'Stopping…' : 'Stop after current'}</button>
      <button class="secondary" disabled={!page.total || loading || !!action} onclick={clearFinished}>Clear finished</button>
      <button class="secondary" disabled={loading || !!action} onclick={() => void refresh()}>Refresh</button>
    </div>
  </div>

  {#if batchSummaries.length}
    <section class="summary-card" aria-label="Batch summaries">
      <div class="card-heading"><div><p class="eyebrow">Queue overview</p><h3>Batch summaries</h3></div><span class="muted">All queue jobs</span></div>
      <div class="summary-grid">
        {#each batchSummaries as batch (batch.id)}
          <div class="summary-row">
            <div class="summary-main"><strong>{batch.title}</strong><span>{batch.artist} · {batch.total} track{batch.total === 1 ? '' : 's'}</span></div>
            <div class="summary-counts"><span class="complete">{batch.completed} complete</span><span class:has-failed={batch.failed > 0}>{batch.failed} failed</span><span>{batch.active} active</span><span>{batch.pending} pending</span></div>
          </div>
        {/each}
      </div>
    </section>
  {/if}

  <section class="jobs-card" aria-label="Download jobs">
    <div class="jobs-list" role="list">
      {#each page.jobs as job (job.id)}
        <article class="job-row" class:current={job.id === snapshot.runningJobId} role="listitem">
          {#if job.artworkUrl}<img class="thumb" src={job.artworkUrl} alt="" loading="lazy" />{:else}<div class="thumb fallback" aria-hidden="true">{initials(job)}</div>{/if}
          <div class="job-main"><strong>{job.title}</strong><span>{job.artist} · {job.album}</span><small>{job.quality}{job.destination ? ` · ${job.destination}` : ''}</small></div>
          <div class="job-progress"><span class={`state state-${job.state}`}>{stateLabel(job.state)}</span>{#if progress(job)}<small title={progress(job)}>{progress(job)}</small>{/if}{#if job.error}<small class="job-error" title={job.error}>{job.error}</small>{/if}</div>
          <div class="job-actions">
            {#if job.retryable}<button class="secondary" disabled={!!action} onclick={() => retry(job)}>Retry</button>{/if}
            {#if !['completed', 'cancelled', 'skipped'].includes(job.state)}<button class="text-button" disabled={!!action} onclick={() => cancel(job)}>Cancel</button>{/if}
          </div>
          <time class="created" datetime={dateTime(job.createdAt)}>{displayDate(job.createdAt)}</time>
        </article>
      {:else}
        <div class="empty-state"><h3>No downloads queued</h3><p>Open Apple Music to search for a song or preview an album.</p></div>
      {/each}
    </div>
    {#if loading}<p class="loading-state" role="status">Loading queue…</p>{/if}
    <div class="pagination"><button class="secondary" disabled={offset === 0 || loading} onclick={() => { offset = Math.max(0, offset - PAGE_SIZE); void refresh(); }}>Previous page</button><span>{page.total ? `${offset + 1}–${Math.min(offset + PAGE_SIZE, page.total)} of ${page.total}` : '0 jobs'}</span><button class="secondary" disabled={offset + PAGE_SIZE >= page.total || loading} onclick={() => { offset += PAGE_SIZE; void refresh(); }}>Next page</button></div>
  </section>
</section>

<style>
  .downloads-page { display: grid; gap: 17px; margin-top: 38px; }
  .section-heading, .toolbar, .card-heading { display: flex; align-items: center; justify-content: space-between; gap: 18px; }
  .section-heading { align-items: flex-start; }
  h2, h3, p { margin-top: 0; }
  h2 { margin-bottom: 10px; color: #fbfbff; font-size: clamp(23px, 3vw, 34px); letter-spacing: -0.045em; }
  h3 { margin-bottom: 5px; color: #e9eaf2; font-size: 20px; }
  .lede { max-width: 650px; margin: 0; color: #aeb1bf; font-size: 14px; line-height: 1.6; }
  .queue-status { display: flex; flex: 0 0 auto; align-items: center; gap: 8px; padding: 9px 12px; border: 1px solid rgba(255, 255, 255, .1); border-radius: 999px; color: #aeb1bf; background: rgba(255, 255, 255, .035); font-size: 12px; }
  .queue-status.running { border-color: rgba(124, 199, 155, .28); color: #a8ddb9; }
  .status-dot { width: 7px; height: 7px; border-radius: 50%; background: currentColor; box-shadow: 0 0 0 4px color-mix(in srgb, currentColor 12%, transparent); }
  .toolbar, .summary-card, .jobs-card { padding: 18px 22px; border: 1px solid rgba(255, 255, 255, .09); border-radius: 14px; background: rgba(255, 255, 255, .025); }
  .toolbar { flex-wrap: wrap; }
  .toolbar-copy { display: grid; gap: 5px; color: #e2e4ee; }
  .toolbar-copy span, .muted { color: #9699aa; font-size: 12px; }
  .toolbar-actions { display: flex; flex-wrap: wrap; gap: 8px; }
  button.primary, button.secondary { border-radius: 8px; padding: 9px 13px; cursor: pointer; font-weight: 650; }
  button.primary { border: 0; color: #161720; background: #caccff; }
  button.secondary { border: 1px solid #363a4b; color: #dadde8; background: #202431; }
  button.primary:disabled, button.secondary:disabled, .text-button:disabled { cursor: not-allowed; opacity: .45; }
  .error-message { margin: 0; color: #efabab; font: 12px ui-monospace, SFMono-Regular, Consolas, monospace; line-height: 1.5; }
  .summary-card { display: grid; gap: 15px; }
  .summary-card .eyebrow { margin-bottom: 6px; }
  .summary-grid, .jobs-list { display: grid; }
  .summary-row { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 11px 0; border-top: 1px solid #292d39; }
  .summary-main { display: grid; min-width: 0; gap: 4px; }
  .summary-main strong { overflow: hidden; color: #e3e5ef; text-overflow: ellipsis; white-space: nowrap; }
  .summary-main span, .summary-counts { color: #9699aa; font-size: 12px; }
  .summary-counts { display: flex; flex-wrap: wrap; justify-content: flex-end; gap: 9px; }
  .summary-counts .complete { color: #a8ddb9; }
  .summary-counts .has-failed { color: #efabab; }
  .job-row { display: grid; grid-template-columns: 42px minmax(0, 1fr) minmax(160px, .75fr) auto minmax(125px, auto); align-items: center; gap: 12px; padding: 12px 0; border-bottom: 1px solid #292d39; }
  .job-row.current { margin-inline: -10px; padding-inline: 10px; border-radius: 8px; background: rgba(143, 147, 255, .09); }
  .thumb { width: 42px; height: 42px; border-radius: 6px; object-fit: cover; }
  .fallback { display: grid; place-items: center; color: #b9bbff; background: linear-gradient(145deg, #353962, #1d2034); font-size: 12px; font-weight: 700; }
  .job-main, .job-progress { display: grid; min-width: 0; gap: 4px; }
  .job-main strong, .job-main span, .job-main small, .job-progress small { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .job-main strong { color: #e3e5ef; }
  .job-main span, .job-main small, .job-progress small, .created { color: #9699aa; font-size: 12px; }
  .state { width: max-content; padding: 4px 7px; border-radius: 999px; color: #bfc2d5; background: rgba(255, 255, 255, .07); font-size: 11px; }
  .state-completed, .state-skipped { color: #a8ddb9; background: rgba(124, 199, 155, .1); }
  .state-failed, .state-interrupted { color: #efabab; background: rgba(212, 89, 89, .11); }
  .state-downloading, .state-resolving, .state-decrypting, .state-remuxing, .state-tagging, .state-verifying, .state-organizing { color: #caccff; background: rgba(143, 147, 255, .14); }
  .job-error { color: #efabab !important; }
  .job-actions { display: flex; align-items: center; gap: 6px; }
  .text-button { border: 0; padding: 4px; color: #aeb1bf; background: transparent; cursor: pointer; font-size: 12px; }
  .created { text-align: right; }
  .empty-state { padding: 24px 0; color: #969bae; text-align: center; }
  .empty-state p { margin: 0; }
  .loading-state { margin: 14px 0 0; color: #969bae; font-size: 12px; }
  .pagination { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding-top: 16px; color: #969bae; font-size: 12px; }
  @media (max-width: 900px) {
    .section-heading { display: grid; }
    .job-row { grid-template-columns: 42px minmax(0, 1fr) auto; }
    .job-progress { grid-column: 2 / -1; }
    .job-actions { grid-column: 2 / -1; justify-content: flex-start; }
    .created { display: none; }
  }
  @media (max-width: 600px) {
    .summary-row { display: grid; }
    .summary-counts { justify-content: flex-start; }
    .toolbar-actions { width: 100%; }
  }
</style>
