<script lang="ts">
  import { readBackground, saveBackground, resetBackground } from './playerBackground';
  import { onMount } from 'svelte';
  import Cover from './Cover.svelte';
  import Spectrum from './Spectrum.svelte';
  import type { AudioQuality, CurrentTrack } from './playerTypes';
  export let path: string | null = null;
  export let state = 'stopped';
  let optionsOpen = false;
  export let info: CurrentTrack = {track: null, albumId: null};
  export let quality: AudioQuality | null = null;
  let background = '';
  let busy = true;
  let error = '';
  let disposed = false;
  let picker: HTMLInputElement;
  const rate = (value: number | null | undefined) => value ? `${Number((value / 1000).toFixed(1))} kHz` : 'Unknown';
  const channels = (value: number | null | undefined) => value === 1 ? 'Mono' : value === 2 ? 'Stereo' : value ? `${value} channels` : 'Unknown';
  function setBackground(blob: Blob | null) {
    if (background) URL.revokeObjectURL(background);
    background = blob ? URL.createObjectURL(blob) : '';
  }
  onMount(() => {
    void readBackground().then(raw => {
      const bytes = raw instanceof ArrayBuffer ? raw : new Uint8Array(raw);
      if (!disposed && bytes.byteLength) setBackground(new Blob([bytes], {type: 'image/jpeg'}));
    }).catch(e => { if (!disposed) error = String(e); }).finally(() => { if (!disposed) busy = false; });
    return () => { disposed = true; setBackground(null); };
  });
  async function chooseBackground(event: Event) {
    const input = event.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    input.value = '';
    if (!file || busy) return;
    busy = true; error = '';
    try {
      if (file.size > 20 * 1024 * 1024 || !['image/jpeg', 'image/png', 'image/webp'].includes(file.type)) throw new Error('Choose a JPEG, PNG or WebP image up to 20 MB.');
      const bitmap = await createImageBitmap(file);
      let blob: Blob;
      try {
        const scale = Math.min(1, 1920 / Math.max(bitmap.width, bitmap.height));
        const canvas = new OffscreenCanvas(Math.max(1, Math.round(bitmap.width * scale)), Math.max(1, Math.round(bitmap.height * scale)));
        const context = canvas.getContext('2d');
        if (!context) throw new Error('Image processing is unavailable.');
        context.fillStyle = '#171b2c'; context.fillRect(0, 0, canvas.width, canvas.height);
        context.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
        blob = await canvas.convertToBlob({type: 'image/jpeg', quality: .8});
      } finally { bitmap.close(); }
      if (blob.size > 1024 * 1024) throw new Error('This image is too detailed. Choose a smaller image.');
      if (disposed) return;
      await saveBackground(Array.from(new Uint8Array(await blob.arrayBuffer())));
      if (!disposed) setBackground(blob);
    } catch (e) { if (!disposed) error = String(e); }
    finally { if (!disposed) busy = false; }
  }
  async function clearBackground() {
    if (busy) return;
    busy = true; error = '';
    try { await resetBackground(); if (!disposed) setBackground(null); }
    catch (e) { if (!disposed) error = String(e); }
    finally { if (!disposed) busy = false; }
  }
  $: title = info.track?.title ?? path?.split(/[\\/]/).pop() ?? 'Make room for music';
</script>
<section class="listening-room" class:custom-background={!!background} aria-label="Now playing">
  {#if background}<img class="room-background" src={background} alt="" />{/if}
  <div class="room-shade" aria-hidden="true"></div>
  <button class="options-toggle" aria-label="Player options" aria-expanded={optionsOpen} onclick={() => optionsOpen = !optionsOpen}>···</button>
  {#if optionsOpen}<header class="room-heading">
    <div class="background-actions"><input bind:this={picker} type="file" accept="image/jpeg,image/png,image/webp" aria-label="Choose player background image" onchange={chooseBackground} disabled={busy} /><button disabled={busy} onclick={() => picker.click()}>{busy ? 'Saving…' : 'Change background'}</button>{#if background}<button disabled={busy} onclick={clearBackground}>Reset</button>{/if}</div>
  </header>{/if}
  {#if error}<p role="alert" class="background-error">{error}</p>{/if}
  <div class="record-layout">
    <div class="record-art">{#if info.albumId}<Cover id={info.albumId} />{:else}<div class="art-placeholder" aria-hidden="true"><span>♫</span><i></i></div>{/if}</div>
    <div class="record-detail"><h2>{title}</h2><p class="artist">{info.track?.artist ?? (path ? 'Local audio' : 'Pick a song from your library to begin.')}</p>

    </div>
  </div>
  <div class="spectrum-layer"><Spectrum showControls={optionsOpen} playing={state === 'playing'} /></div>
  {#if optionsOpen}<section class="signal-details" aria-label="Audio quality">
    <details>
      <summary><span>Audio quality</span><span class="details-hint">Source &amp; output <span aria-hidden="true">↗</span></span></summary>
      <div class="signal-panel">
      {#if path}<div class="quality-tags" aria-label="Source audio quality"><span>{quality?.codec || 'Codec unknown'}</span><span>{rate(quality?.sampleRate)}</span>{#if quality?.bitrate}<span>{Math.round(quality.bitrate / 1000)} kbps</span>{/if}</div>{/if}
        <dl><div><dt>Source codec</dt><dd>{path ? quality?.codec || 'Unknown' : '—'}</dd></div><div><dt>Source sample rate</dt><dd>{path ? rate(quality?.sampleRate) : '—'}</dd></div><div><dt>Source channels</dt><dd>{path ? channels(quality?.channels) : '—'}</dd></div><div><dt>Decoder format</dt><dd>{path ? quality?.sampleFormat || 'Unknown' : '—'}</dd></div><div><dt>Output sample rate</dt><dd>{path ? rate(quality?.outputSampleRate) : '—'}</dd></div><div><dt>Output format</dt><dd>{path ? [quality?.outputFormat || 'Unknown', channels(quality?.outputChannels)].join(' · ') : '—'}</dd></div></dl>
        <p class="signal-note">Source and output formats may differ. Decoder format does not indicate the original recording’s bit depth.</p>
      </div>
    </details>
  </section>{/if}
</section>

<style>
  .listening-room {
    position: fixed;
    z-index: 20;
    isolation: isolate;
    inset: 0;
    overflow: hidden;
    color: var(--text);
    background:
      radial-gradient(ellipse at 22% 22%, rgba(112, 152, 206, .3), transparent 55%),
      radial-gradient(ellipse at 81% 48%, rgba(139, 101, 193, .26), transparent 58%),
      linear-gradient(140deg, #172840, #171b32 55%, #0f192b);
  }
  .room-background, .room-shade { position: absolute; inset: 0; width: 100%; height: 100%; object-fit: cover; }
  .room-background { z-index: 0; }
  .room-shade { z-index: 0; background: linear-gradient(180deg, rgba(6, 13, 29, .08), transparent 45%, rgba(7, 14, 31, .24)); }
  .custom-background { --text: #f7f9ff; --muted: #d3d9e9; --accent: #d1d9ff; --border: rgba(255, 255, 255, .21); --glass-strong: rgba(18, 25, 43, .84); }
  .custom-background .room-shade { background: linear-gradient(180deg, rgba(5, 12, 26, .5), rgba(5, 12, 26, .35) 45%, rgba(5, 12, 26, .65)); }
  .room-heading { position: absolute; inset: 76px 32px auto; z-index: 8; display: flex; align-items: center; justify-content: space-between; gap: 16px; }
  .background-actions { display: flex; align-items: center; gap: 20px; }
  .background-actions { gap: 8px; }
  .background-actions button { border: 1px solid var(--border); border-radius: 10px; background: var(--glass-strong); color: var(--text); padding: 9px 14px; cursor: pointer; }
  .background-actions input { display: none; }
  .background-actions button:disabled { opacity: .5; cursor: wait; }
  .record-layout { position: absolute; top: clamp(126px, 18vh, 182px); left: 50%; transform: translateX(-50%); width: min(950px, calc(100% - 120px)); display: grid; grid-template-columns: clamp(190px, 29vh, 295px) minmax(0, 1fr); align-items: center; gap: clamp(34px, 5vw, 74px); }
  .record-art { position: relative; z-index: 1; aspect-ratio: 1; border-radius: 14px; box-shadow: 0 28px 74px rgba(0, 7, 25, .3), 0 1px 0 rgba(255, 255, 255, .34); overflow: hidden; transform: none; }
  .record-art :global(.cover) { margin: 0; border-radius: 14px; width: 100%; height: 100%; }
  .art-placeholder { height: 100%; position: relative; display: grid; place-items: center; background: radial-gradient(circle at 35% 30%, #8491bd, #404976 40%, #1b2547 75%); color: #d8def7; font-size: 78px; }
  .art-placeholder i { position: absolute; width: 76%; height: 76%; border: 1px solid #ffffff20; border-radius: 50%; box-shadow: 0 0 0 15px #ffffff05, 0 0 0 30px #ffffff04; }
  .record-detail { position: relative; z-index: 3; min-width: 0; }
  .record-detail h2 { color: var(--text); font-size: clamp(29px, 4.2vw, 59px); font-weight: 600; line-height: 1.1; letter-spacing: -.055em; margin: 12px 0 17px; display: -webkit-box; line-clamp: 3; -webkit-line-clamp: 3; -webkit-box-orient: vertical; overflow: hidden; overflow-wrap: anywhere; }
  .artist { font-size: 17px; color: var(--text); margin-bottom: 8px; }
  .quality-tags { display: flex; flex-wrap: wrap; gap: 14px; margin-top: 21px; }
  .quality-tags span { font-size: 10px; padding: 3px 0; color: var(--muted); }
  .spectrum-layer { position: absolute; z-index: 2; inset: 0; pointer-events: none; }
  .signal-details { position: absolute; z-index: 8; left: 32px; bottom: calc(var(--player-height, 106px) + var(--page-gap, 20px) * 2 + 17px); width: min(370px, calc(100% - 64px)); }
  details { border: 1px solid var(--border); border-radius: 17px; background: color-mix(in srgb, var(--glass-strong) 75%, transparent); backdrop-filter: blur(24px) saturate(135%); box-shadow: 0 10px 28px rgba(0, 7, 25, .07); }
  summary { display: flex; align-items: center; justify-content: space-between; gap: 15px; padding: 12px 16px; color: var(--text); font-size: 11px; cursor: pointer; list-style: none; }
  summary::-webkit-details-marker { display: none; }
  summary:focus-visible { outline: 2px solid var(--accent); outline-offset: 4px; border-radius: 17px; }
  .details-hint { color: var(--muted); font-size: 10px; }
  .details-hint > span { margin-left: 5px; }
  .signal-panel { padding: 0 16px 15px; max-height: min(350px, calc(100vh - 280px)); overflow-y: auto; }
  dl { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 16px; margin: 7px 0 16px; }
  dt { color: var(--muted); font-size: 10px; margin-bottom: 6px; }
  dd { color: var(--text); margin: 0; font-size: 12px; overflow-wrap: anywhere; }
  .signal-note { color: var(--muted); font-size: 10px; margin: 0; line-height: 1.6; }
  .background-error { position: absolute; z-index: 9; top: 111px; right: 32px; max-width: min(450px, calc(100% - 64px)); color: var(--danger); background: var(--glass-strong); border-radius: 10px; padding: 10px 14px; font-size: 12px; }
  @media (prefers-color-scheme: light) {
    .listening-room { background: radial-gradient(ellipse at 20% 18%, rgba(157, 194, 238, .43), transparent 60%), radial-gradient(ellipse at 81% 45%, rgba(191, 169, 232, .36), transparent 60%), linear-gradient(135deg, #dae6f4, #ececf8 56%, #dfeaf2); }
    .room-shade { background: linear-gradient(180deg, rgba(255, 255, 255, .04), transparent 50%, rgba(235, 242, 251, .28)); }
    .custom-background .room-shade { background: linear-gradient(180deg, rgba(5, 12, 26, .5), rgba(5, 12, 26, .35) 45%, rgba(5, 12, 26, .65)); }
    .record-art { box-shadow: 0 28px 64px rgba(63, 81, 119, .24), 0 1px 0 rgba(255, 255, 255, .7); }
  }
  @media (max-height: 700px) {
    .record-layout { top: 130px; grid-template-columns: clamp(170px, 31vh, 216px) minmax(0, 1fr); width: min(800px, calc(100% - 120px)); gap: 40px; }
    .record-detail h2 { font-size: clamp(29px, 4vw, 43px); line-clamp: 2; -webkit-line-clamp: 2; margin-block: 9px 12px; }
    .artist { font-size: 15px; }
    .quality-tags { margin-top: 16px; }
    .signal-details { bottom: calc(var(--player-height, 106px) + var(--page-gap, 20px) * 2 + 13px); }
  }
  @media (max-width: 680px) {
    .room-heading { inset: 18px 20px auto; }
    .background-actions button { padding: 8px 10px; font-size: 10px; }
    .record-layout { top: 125px; width: calc(100% - 52px); grid-template-columns: minmax(130px, .75fr) minmax(0, 1fr); gap: 24px; }
    .record-detail h2 { font-size: clamp(24px, 5vw, 34px); }
    .artist { font-size: 13px; }
    .quality-tags { gap: 5px; margin-top: 12px; }
    .quality-tags span { padding: 5px 6px; font-size: 9px; }
    .signal-details { left: 20px; width: min(330px, calc(100% - 40px)); }
  }
  @media (max-width: 420px) {
    .record-layout { top: 123px; gap: 18px; grid-template-columns: 116px minmax(0, 1fr); }
    .record-detail h2 { font-size: 24px; }
    .artist { font-size: 12px; }
  }
  .options-toggle { position: absolute; top: 22px; right: 28px; z-index: 10; border: 0; background: transparent; color: var(--muted); font-size: 26px; padding: 6px 12px; cursor: pointer; }
  .room-heading { justify-content: flex-end; }
  .record-art { box-shadow: none; }
  .record-detail h2 { font-size: clamp(28px, 3.6vw, 48px); font-weight: 500; }
  @media (max-width: 680px) { .room-heading { top: 74px; } }
</style>
