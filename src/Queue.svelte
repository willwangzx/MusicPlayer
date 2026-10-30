<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { listen, type UnlistenFn } from '@tauri-apps/api/event';
  import { onMount } from 'svelte';
  import RecycleTrack from './RecycleTrack.svelte';
  interface Page { paths: string[]; offset: number; total: number; current: number; revision: number; repeatAll: boolean; randomLibrary: boolean }
  let page: Page = {paths: [], offset: 0, total: 0, current: 0, revision: 0, repeatAll: false, randomLibrary: false};
  let changingRandom = false;
  let error = '';
  let offset = 0;
  let request = 0;
  let disposed = false;
  const PAGE_SIZE = 100;
  function name(path: string) { return path.split(/[\\/]/).pop() || path; }
  async function refresh() {
    const id = ++request;
    const requestedOffset = offset;
    try {
      const result = await invoke<Page>('playback_queue', {offset: requestedOffset});
      if (disposed || id !== request) return;
      const lastOffset = result.total ? Math.floor((result.total - 1) / PAGE_SIZE) * PAGE_SIZE : 0;
      const nextOffset = Math.min(requestedOffset, lastOffset);
      if (nextOffset !== requestedOffset) { offset = nextOffset; void refresh(); return; }
      page = result;
    } catch (e) { if (!disposed) error = String(e); }
  }
  async function edit(action: string, index = 0) {
    try { await invoke('edit_queue', {action, index, revision: page.revision, current: page.current}); error = ''; if (action === 'clear') offset = 0; } catch (e) { error = String(e); }
    if (!disposed) await refresh();
  }
  async function toggleRandom() {
    if (changingRandom) return;
    changingRandom = true;
    try {
      await invoke('set_random_library', {enabled: !page.randomLibrary, revision: page.revision, current: page.current});
      error = '';
    } catch (e) { error = String(e); }
    if (!disposed) await refresh();
    changingRandom = false;
  }
  onMount(() => {
    let stop: UnlistenFn | undefined;
    void (async () => {
      try {
        stop = await listen<{queueRevision: number; currentIndex: number | null}>('musicplayer://playback-state', event => { if (event.payload.queueRevision !== page.revision || event.payload.currentIndex !== (page.total ? page.current : null)) void refresh(); });
        if (disposed) { stop(); return; } await refresh();
      } catch (e) { if (!disposed) error = String(e); }
    })();
    return () => { disposed = true; ++request; stop?.(); };
  });
</script>
<section class="prototype">
  <div class="prototype-heading"><div><p class="eyebrow">Up next</p><h2>Queue</h2></div><div class="queue-controls"><button onclick={toggleRandom} disabled={changingRandom} aria-pressed={page.randomLibrary} aria-label={page.randomLibrary ? 'Turn random library playback off' : 'Turn random library playback on'}>{changingRandom ? 'Updating…' : page.randomLibrary ? 'Random library: On' : 'Random library: Off'}</button><button onclick={() => edit('shuffle')} disabled={page.total - page.current <= 2} aria-label="Shuffle upcoming tracks">Shuffle upcoming</button><button onclick={() => edit('repeat', page.repeatAll ? 0 : 1)} aria-pressed={page.repeatAll} aria-label={page.repeatAll ? 'Turn repeat all off' : 'Turn repeat all on'}>{page.repeatAll ? 'Repeat all: On' : 'Repeat all: Off'}</button><button onclick={() => edit('clear')} disabled={!page.total && !page.randomLibrary}>Clear queue</button></div></div>
  <p class="queue-hint">{page.randomLibrary ? 'Random library playback is on. Songs are added automatically after your queued tracks.' : 'Turn on Random library to keep playing songs picked from your library, or start from an empty queue.'}</p>
  {#if error}<p class="error-message" role="alert">{error}</p>{/if}
  <div class="queue-list" role="list" aria-label="Playback queue">
    {#each page.paths as path, i}
      <div class="queue-row" role="listitem" aria-current={page.offset + i === page.current ? 'true' : undefined} class:current={page.offset + i === page.current}><span>{page.offset + i + 1}</span><button class="queue-select" onclick={() => edit('select', page.offset + i)} aria-label={`Play ${name(path)}`}><span class="name" title={path}>{name(path)}</span><span>{page.offset + i === page.current ? 'Selected' : 'Play'}</span></button><button aria-label={`Remove ${name(path)}`} onclick={() => edit('remove', page.offset + i)}>Remove</button><RecycleTrack {path} title={name(path)} onrecycled={() => void refresh()} /></div>
    {:else}<p>Add tracks from your library to build a queue.</p>{/each}
  </div>
  <div class="prototype-actions"><button disabled={offset === 0} onclick={() => { offset = Math.max(0, offset - 100); void refresh(); }}>Previous page</button><span>{page.total ? `${offset + 1}–${Math.min(offset + 100, page.total)} of ${page.total}` : '0 tracks'}</span><button disabled={offset + 100 >= page.total} onclick={() => { offset += 100; void refresh(); }}>Next page</button></div>
</section>
<style>
  .queue-controls { display: flex; flex-wrap: wrap; justify-content: flex-end; gap: 8px; }
  .queue-hint { color: #a3a9bc; font-size: .9rem; }
  button[aria-pressed="true"] { border-color: #abb0ff; color: #c6c9ff; background: #343650; }
  .queue-row { display: grid; grid-template-columns: 30px minmax(0, 1fr) 80px auto; align-items: center; gap: 12px; padding: 12px 4px; border-bottom: 1px solid #292d39; }
  .current { color: #abb0ff; } .name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  button { background: #272b3d; color: #dadde8; border: 1px solid #363a4b; border-radius: 7px; padding: 8px; cursor: pointer; } button:disabled { opacity: .4; cursor: default; }
  .queue-select { display: grid; grid-template-columns: minmax(0, 1fr) 70px; gap: 12px; min-width: 0; border: 0; padding: 8px 4px; color: inherit; background: transparent; text-align: left; }
  .queue-select:hover, .queue-select:focus-visible { background: #272b3d; }
</style>
