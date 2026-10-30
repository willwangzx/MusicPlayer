<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { listen, type UnlistenFn } from '@tauri-apps/api/event';
  import { onMount } from 'svelte';
  import AppleMusic from './AppleMusic.svelte';
  import Downloads from './Downloads.svelte';
  import Library from './Library.svelte';
  import Queue from './Queue.svelte';
  import Cover from './Cover.svelte';
  import NowPlaying from './NowPlaying.svelte';
  import type { AudioQuality, CurrentTrack } from './playerTypes';
  interface Playback {
    available: boolean; state: string; queueLen: number; currentIndex: number | null; currentPath: string | null;
    lastError: string | null; mpvClientApiVersion: string | null; libmpvPath: string | null; transitionCount: number;
    audioReconfigCount: number; position: number; duration: number; volume: number; queueRevision: number; persistenceError: string | null;
    audioQuality?: AudioQuality;
  }
  let playback: Playback = {available: false, state: 'starting', queueLen: 0, currentIndex: null, currentPath: null, lastError: null, mpvClientApiVersion: null, libmpvPath: null, transitionCount: 0, audioReconfigCount: 0, position: 0, duration: 0, volume: 100, queueRevision: 0, persistenceError: null};
  let view: 'Now Playing' | 'Tracks' | 'Albums' | 'Apple Music' | 'Downloads' | 'Queue' | 'Diagnostics' = 'Tracks';
  const navigation = ['Now Playing', 'Tracks', 'Albums', 'Queue', 'Apple Music', 'Downloads', 'Diagnostics'] as const;
  const icons = ['◉', '♫', '▦', '≡', '⌕', '↓', '⚙'];
  let trackInfo: CurrentTrack = {track: null, albumId: null};
  let infoRequest = 0;
  let requestedPath: string | null = null;
  let appDisposed = false;
  async function refreshTrack(path: string | null) {
    requestedPath = path;
    const request = ++infoRequest;
    trackInfo = {track: null, albumId: null};
    if (!path) return;
    try {
      const result = await invoke<CurrentTrack>('current_track_info', {path});
      if (!appDisposed && request === infoRequest) trackInfo = result;
    } catch { /* Files played outside the library still display their filename. */ }
  }
  $: if (playback.currentPath !== requestedPath) void refreshTrack(playback.currentPath);
  interface MediaControls { available: boolean; error: string | null; receivedButtons: number; lastButton: string | null }
  let mediaControls: MediaControls = {available: false, error: null, receivedButtons: 0, lastButton: null};
  let error = '';
  let partA = 'D:\\Projects\\MusicPlayer\\gapless-assets\\gapless-part-a.wav';
  let partB = 'D:\\Projects\\MusicPlayer\\gapless-assets\\gapless-part-b.wav';
  let seekValue: number | null = null;
  let seekSession: {revision: number; current: number; seconds: number} | null = null;
  async function command(name: string, args = {}) { error = ''; try { await invoke(name, args); } catch (e) { error = String(e); } }
  function time(seconds: number) { return `${Math.floor(seconds / 60)}:${String(Math.floor(seconds) % 60).padStart(2, '0')}`; }
  function playbackState(next: Playback) {
    if (seekSession && (next.queueRevision !== seekSession.revision || next.currentIndex !== seekSession.current)) seekValue = null;
    playback = next;
  }
  function seekBegin() {
    seekSession = {revision: playback.queueRevision, current: playback.currentIndex ?? 0, seconds: playback.position};
  }
  function seekCancel() { seekSession = null; seekValue = null; }
  function seekInput(event: Event) {
    const value = Number((event.currentTarget as HTMLInputElement).value);
    if (seekSession && (seekSession.revision !== playback.queueRevision || seekSession.current !== playback.currentIndex)) return;
    if (!seekSession) {
      seekSession = {revision: playback.queueRevision, current: playback.currentIndex ?? 0, seconds: value};
    } else {
      seekSession.seconds = value;
    }
    seekValue = value;
  }
  function seekCommit() {
    const session = seekSession;
    seekSession = null;
    seekValue = null;
    if (session && session.revision === playback.queueRevision && session.current === playback.currentIndex) void command('seek_playback', {seconds: session.seconds, revision: session.revision, current: session.current});
  }
  function keyboard(event: KeyboardEvent) {
    if (event.key === 'Escape' && view === 'Now Playing') { view = 'Tracks'; return; }
    if (event.target instanceof HTMLElement && (event.target.closest('input, button, textarea, select, dialog') || event.target.isContentEditable)) return;
    if (event.code === 'Space' && playback.available && playback.queueLen) { event.preventDefault(); void command('toggle_playback'); }
  }
  onMount(() => {
    let stop: UnlistenFn | undefined;
    let stopMedia: UnlistenFn | undefined;
    let stopLibrary: UnlistenFn | undefined;
    let disposed = false;
    let received = false;
    let mediaReceived = false;
    void (async () => {
      try {
        stop = await listen<Playback>('musicplayer://playback-state', event => { received = true; playbackState(event.payload); });
        if (disposed) { stop(); return; }
        const initial = await invoke<Playback>('playback_status');
        if (!disposed && !received) playbackState(initial);
      } catch (e) { if (!disposed) error = String(e); }
    })();
    void listen<{scanning: boolean}>('musicplayer://library-state', event => { if (!event.payload.scanning && !disposed) void refreshTrack(playback.currentPath); }).then(unlisten => { if (disposed) unlisten(); else stopLibrary = unlisten; }).catch(() => {});
    void (async () => {
      try {
        stopMedia = await listen<MediaControls>('musicplayer://media-controls-state', event => { mediaReceived = true; if (!disposed) mediaControls = event.payload; });
        if (disposed) { stopMedia(); return; }
        const initial = await invoke<MediaControls>('media_controls_status');
        if (!disposed && !mediaReceived) mediaControls = initial;
      } catch (e) { if (!disposed) mediaControls = {...mediaControls, error: String(e)}; }
    })();
    return () => { disposed = true; appDisposed = true; ++infoRequest; stop?.(); stopMedia?.(); stopLibrary?.(); };
  });
</script>
<svelte:head><title>MusicPlayer</title><meta name="description" content="Your local music library and gapless player." /></svelte:head>
<svelte:window onkeydown={keyboard} />
<div class="shell" class:immersive={view === 'Now Playing'}>
  {#if view !== 'Now Playing'}
  <aside class="sidebar" aria-label="Primary navigation">
    <div class="brand"><span class="brand-mark" aria-hidden="true">♫</span><div class="brand-copy"><span>MusicPlayer</span></div></div>
    <nav>{#each navigation as item, i}<button class:active={view === item} aria-label={item} aria-current={view === item ? 'page' : undefined} onclick={() => view = item}><span class="nav-icon" aria-hidden="true">{icons[i]}</span><span class="nav-label">{item}</span>{#if view === item}<span class="nav-chevron" aria-hidden="true">›</span>{/if}</button>{/each}</nav>
  </aside>
  {/if}
  <main class="content">
    <p class="view-announcement" role="status" aria-live="polite" aria-atomic="true">{view} view</p>
    {#if view !== 'Now Playing'}<header class="topbar"><h1>{view === 'Queue' ? 'Up next' : view}</h1></header>{/if}
    {#if error || playback.lastError}<p class="error-message" role="alert">{error || playback.lastError}</p>{/if}
    {#if playback.persistenceError}<p class="error-message" role="status">{playback.persistenceError}</p>{/if}
    {#if view === 'Now Playing'}
      <NowPlaying path={playback.currentPath} state={playback.state} info={trackInfo} quality={playback.audioQuality ?? null} />
    {:else if view === 'Tracks' || view === 'Albums'}
      {#key view}<Library mode={view} />{/key}
    {:else if view === 'Queue'}<Queue />
    {:else if view === 'Apple Music'}<AppleMusic />
    {:else if view === 'Downloads'}<Downloads />
    {:else}
      <section class="prototype">
        <div class="prototype-heading"><h2>Playback diagnostics</h2><span class="engine-status" class:ready={playback.available}>{playback.available ? `libmpv API ${playback.mpvClientApiVersion}` : playback.state}</span></div>
        <p role="status">Windows media buttons: {mediaControls.available ? 'Ready' : mediaControls.error ? 'Unavailable' : 'Starting?'} ? {mediaControls.receivedButtons} commands received{mediaControls.lastButton ? ` ? Last: ${mediaControls.lastButton}` : ''}</p>
        {#if mediaControls.error}<p class="error-message">{mediaControls.error}</p>{/if}
        <div class="path-grid"><label>First audio file<input bind:value={partA} spellcheck="false" /></label><label>Second audio file (optional)<input bind:value={partB} spellcheck="false" /></label></div>
        <div class="prototype-actions"><button disabled={!playback.available} onclick={() => command('set_playback_queue', {paths: [partA, partB].map(p => p.trim()).filter(Boolean), startIndex: 0})}>Play entered files</button><span>{playback.transitionCount} transitions · {playback.audioReconfigCount} audio reconfigs</span></div>
        <p class="runtime-path">{playback.libmpvPath ?? 'Run scripts\\Install-Libmpv.ps1 to install the playback runtime.'}</p>
      </section>
    {/if}
  </main>
  <footer class="player-bar" aria-label="Playback controls">
    <button class="now-playing open-player" aria-label="Open now playing" onclick={() => view = 'Now Playing'}><div class="mini-cover" aria-hidden="true">{#if trackInfo.albumId}<Cover id={trackInfo.albumId} />{:else}♫{/if}</div><div><p class="now-title" title={playback.currentPath ?? ''}>{trackInfo.track?.title ?? playback.currentPath?.split(/[\\/]/).pop() ?? 'Nothing selected'}</p><p class="now-subtitle">{trackInfo.track?.artist ?? playback.state} · {playback.currentIndex === null ? 0 : playback.currentIndex + 1}/{playback.queueLen}</p></div></button>
    <div class="center-controls">
      <div class="transport"><button aria-label="Previous track" disabled={!playback.available || !playback.queueLen} onclick={() => command('previous_track')}>◀</button><button class="play" aria-label={playback.state === 'playing' ? 'Pause' : 'Play'} disabled={!playback.available || !playback.queueLen} onclick={() => command('toggle_playback')}>{playback.state === 'playing' ? 'Ⅱ' : '▶'}</button><button aria-label="Next track" disabled={!playback.available || !playback.queueLen} onclick={() => command('next_track')}>▶</button><button aria-label="Stop playback" disabled={!playback.available || !playback.queueLen} onclick={() => command('stop_playback')}>■</button></div>
      <div class="seek"><span>{time(seekValue ?? playback.position)}</span><input aria-label="Seek position" type="range" min="0" max={Math.max(playback.duration, 1)} step="0.1" value={seekValue ?? playback.position} disabled={!playback.available || !['playing', 'paused'].includes(playback.state) || !playback.duration} onpointerdown={seekBegin} onpointercancel={seekCancel} oninput={seekInput} onchange={seekCommit} /><span>{time(playback.duration)}</span></div>
    </div>
    <label class="volume">Volume<input aria-label="Volume" type="range" min="0" max="100" step="1" value={playback.volume} disabled={!playback.available} onchange={event => command('set_volume', {value: Number(event.currentTarget.value)})} /></label>
  </footer>
</div>
<style>
  .open-player { text-align: left; border: 0; padding: 0; background: transparent; cursor: pointer; min-width: 0; }
  .open-player > div:last-child { min-width: 0; }
  .mini-cover { flex-shrink: 0; overflow: hidden; }
  .mini-cover :global(.cover) { height: 100%; width: 100%; margin: 0; border-radius: 0; font-size: 20px; }
  .view-announcement { position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px; overflow: hidden; clip: rect(0, 0, 0, 0); white-space: nowrap; border: 0; }
</style>
