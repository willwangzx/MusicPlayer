<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import type { Analysis } from './playerTypes';
  import { zero, envelope, ease, curve, dots } from './spectrumShape';
  export let playing = false;
  export let showControls = true;
  let enabled = true, visible = true, mounted = false, polling = false;
  let waveStyle: 'curve' | 'dots' = 'curve';
  let responsiveness = 1, smoothness = 1, driftSpeed = 1;
  let huePhase = 0;
  let leftHue = 160, leftRange = 100, rightHue = 330, rightRange = 80;
  let saturation = 85, brightness = 60;
  let colorChannel = 'left';
  $: selectedHue = colorChannel === 'left' ? leftHue : rightHue;
  $: selectedRange = colorChannel === 'left' ? leftRange : rightRange;
  function updateHue(event: Event, range: boolean) {
    const value = Number((event.currentTarget as HTMLInputElement).value);
    if (colorChannel === 'left') { if (range) leftRange = value; else leftHue = value; }
    else { if (range) rightRange = value; else rightHue = value; }
    saveSettings();
  }
  function palette(amplitude: number, start: number, span: number, drift: number, phaseDegrees: number, sat: number, bright: number) {
    const phase = (phaseDegrees + drift) * Math.PI / 180;
    const position = .5 - .5 * Math.cos(amplitude * Math.PI + phase);
    const hue = (start + span * position) % 360;
    const light = Math.max(12, Math.min(90, bright + (amplitude - .5) * 20));
    return `hsl(${hue} ${sat}% ${light}%)`;
  }
  function saveSettings() {
    try { localStorage.setItem('musicplayer.spectrum.v1', JSON.stringify({style: waveStyle, responsiveness, smoothness, driftSpeed, leftHue, leftRange, rightHue, rightRange, saturation, brightness})); } catch { /* Session controls still work if storage is unavailable. */ }
  }
  function resetSettings() { waveStyle = 'curve'; responsiveness = smoothness = driftSpeed = 1; leftHue = 160; leftRange = 100; rightHue = 330; rightRange = 80; saturation = 85; brightness = 60; huePhase = blueDrift = redDrift = 0; saveSettings(); }
  function switchStyle() { waveStyle = waveStyle === 'dots' ? 'curve' : 'dots'; saveSettings(); }
  let channelMode = 0;
  const channelLabels = ['Stereo layers', 'Left channel', 'Right channel'];
  $: showLeft = channelMode !== 2;
  $: showRight = channelMode !== 1;
  let generation = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let animation = 0, previous = 0;
  let left = zero(), right = zero(), targetLeft = zero(), targetRight = zero();
  let upperLeft = curve(left, 1), lowerLeft = curve(left, -1);
  let upperRight = upperLeft, lowerRight = lowerLeft;
  let glowStrength = 0;
  const colorBands = [
    {amp: 0, name: 'low'}, {amp: .08, name: 'mid'},
    {amp: .25, name: 'bridge'}, {amp: .48, name: 'high'},
    {amp: .74, name: 'crest'}, {amp: 1, name: 'peak'},
  ];
  // Slightly shift the amplitude-to-color mapping without changing stop order.
  function colorOffset(amplitude: number, drift: number, sign: number) {
    return .5 + sign * .5 * Math.pow(amplitude, 1 + drift / 80);
  }
  let blueDrift = 0, redDrift = 0;
  let blueTarget = 0, redTarget = 0, colorCountdown = 0;
  function animateColor(dt: number) {
    if (reducedMotion || glowStrength === 0) {
      blueDrift = redDrift = 0; colorCountdown = 0; return;
    }
    if (driftSpeed === 0) return;
    huePhase = (huePhase + Math.min(dt, .1) * 12 * driftSpeed) % 360;
    dt *= driftSpeed;
    // Random targets are approached smoothly, never jumped to frame by frame.
    colorCountdown -= dt;
    if (colorCountdown <= 0) {
      blueTarget = (Math.random() * 2 - 1) * 10;
      redTarget = (Math.random() * 2 - 1) * 8;
      colorCountdown = 3 + Math.random() * 3;
    }
    const alpha = 1 - Math.exp(-Math.min(dt, .1) / 2.4);
    blueDrift += (blueTarget - blueDrift) * alpha;
    redDrift += (redTarget - redDrift) * alpha;
  }
  let frame: Analysis | null = null;
  let failure = '';
  let reducedMotion = false;
  function draw() {
    if (waveStyle === 'dots') {
      upperLeft = dots(left, 1); lowerLeft = dots(left, -1);
      upperRight = dots(right, 1); lowerRight = dots(right, -1);
    } else {
      upperLeft = curve(left, 1); lowerLeft = curve(left, -1);
      upperRight = curve(right, 1); lowerRight = curve(right, -1);
    }
    glowStrength = Math.min(1, Math.max(0, ...(showLeft ? left : []), ...(showRight ? right : [])) * 1.2);
  }
  function animate(now: number) {
    animation = 0;
    if (!mounted || !visible) return;
    if (now - previous >= (reducedMotion ? 100 : 30)) {
      const dt = previous ? (now - previous) / 1000 : .03; previous = now;
      left = ease(left, targetLeft, dt, reducedMotion, responsiveness); right = ease(right, targetRight, dt, reducedMotion, responsiveness); draw(); animateColor(dt);
    }
    if (polling || left.some(v => v > 0) || right.some(v => v > 0)) animation = requestAnimationFrame(animate);
  }
  function scheduleAnimation() { if (!animation && visible && mounted) { previous = performance.now(); animation = requestAnimationFrame(animate); } }
  async function poll(token: number) {
    if (!mounted || token !== generation || !polling) return;
    try {
      const result = await invoke<Analysis>('playback_analysis', {enabled: true});
      if (token !== generation || !mounted) return;
      frame = result; failure = '';
      targetLeft = result.active ? envelope(result.left, result.rmsLeft, smoothness) : zero();
      targetRight = result.active ? envelope(result.right, result.rmsRight, smoothness) : zero();
    } catch (e) {
      if (token === generation && mounted) { failure = String(e); frame = null; targetLeft = zero(); targetRight = zero(); }
    }
    if (token === generation && mounted && polling) timer = setTimeout(() => void poll(token), failure || frame?.available === false ? 500 : 80);
  }
  function synchronize(active: boolean) {
    if (active === polling) return;
    polling = active; const token = ++generation;
    clearTimeout(timer); frame = null; failure = ''; targetLeft = zero(); targetRight = zero();
    if (active) { void poll(token); scheduleAnimation(); }
    else {
      void invoke('playback_analysis', {enabled: false}).catch(() => {});
      cancelAnimationFrame(animation); animation = 0;
      left = zero(); right = zero(); draw(); animateColor(0);
    }
  }
  $: if (mounted) synchronize(playing && visible && enabled);
  // Style changes must repaint even while the idle display is not animating.
  $: if (mounted && waveStyle) draw();
  onMount(() => {
    try {
      const saved = JSON.parse(localStorage.getItem('musicplayer.spectrum.v1') || '{}');
      const valid = (v: unknown, min: number, max: number) => typeof v === 'number' && Number.isFinite(v) ? Math.max(min, Math.min(max, v)) : 1;
      waveStyle = saved?.style === 'dots' ? 'dots' : 'curve';
      responsiveness = valid(saved?.responsiveness, .25, 3);
      smoothness = valid(saved?.smoothness, .25, 3);
      driftSpeed = valid(saved?.driftSpeed, 0, 5);
      leftHue = valid(saved?.leftHue ?? 160, 0, 360);
      leftRange = valid(saved?.leftRange ?? 100, 0, 360);
      rightHue = valid(saved?.rightHue ?? 330, 0, 360);
      rightRange = valid(saved?.rightRange ?? 80, 0, 360);
      saturation = valid(saved?.saturation ?? 85, 0, 100);
      brightness = valid(saved?.brightness ?? 60, 20, 80);
    } catch { /* Ignore invalid saved preferences. */ }
    visible = !document.hidden; mounted = true;
    const motion = matchMedia('(prefers-reduced-motion: reduce)');
    const preference = () => { reducedMotion = motion.matches; }; preference();
    motion.addEventListener('change', preference);
    const visibility = () => { visible = !document.hidden; };
    document.addEventListener('visibilitychange', visibility);
    return () => { mounted = false; ++generation; clearTimeout(timer); cancelAnimationFrame(animation); motion.removeEventListener('change', preference); document.removeEventListener('visibilitychange', visibility); void invoke('playback_analysis', {enabled: false}).catch(() => {}); };
  });
  $: status = !enabled ? 'Visualization off' : !playing ? 'Play a song to see its spectrum' : failure ? 'Audio analysis unavailable' : frame?.available === false && frame?.status !== 'starting' && frame?.status !== 'inactive' ? frame?.status : '';
</script>

<div class="spectrum" class:quiet={!polling}>
  {#if showControls}<div class="spectrum-options"><button title="Switch between smooth curves and a dot matrix" aria-label={waveStyle === 'dots' ? 'Switch to curves' : 'Switch to dot matrix'} onclick={switchStyle}>{waveStyle === 'dots' ? 'Dot matrix' : 'Curve'}</button><button title="Switch between stereo, left and right channels" onclick={() => channelMode = (channelMode + 1) % channelLabels.length}>{channelLabels[channelMode]}</button><button aria-label={enabled ? 'Turn visualization off' : 'Turn visualization on'} aria-pressed={enabled} onclick={() => enabled = !enabled}>{enabled ? 'Wave on' : 'Wave off'}</button>
    <label>Responsiveness <output>{responsiveness.toFixed(2)}×</output><input aria-label="Curve responsiveness" type="range" min=".25" max="3" step=".05" bind:value={responsiveness} onchange={saveSettings}/></label>
    <label>Smoothness <output>{smoothness.toFixed(2)}×</output><input aria-label="Curve smoothness" type="range" min=".25" max="3" step=".05" bind:value={smoothness} onchange={saveSettings}/></label>
    <label>Color speed <output>{driftSpeed === 0 ? 'Paused' : driftSpeed.toFixed(2) + '×'}</output><input aria-label="Color drift speed" type="range" min="0" max="5" step=".05" bind:value={driftSpeed} onchange={saveSettings}/></label>
    <details class="color-settings"><summary>Colors</summary>
      <label>Channel<select aria-label="Color channel" bind:value={colorChannel}><option value="left">Left</option><option value="right">Right</option></select></label>
      <div class="hue-spectrum" aria-hidden="true">
        <span style:left={`${selectedHue / 3.6}%`} style:width={`${Math.min(selectedRange, 360 - selectedHue) / 3.6}%`}></span>
        {#if selectedHue + selectedRange > 360}<span style:left="0%" style:width={`${(selectedHue + selectedRange - 360) / 3.6}%`}></span>{/if}
      </div>
      <label>Starting tone <output>{selectedHue}°</output><input class="hue-slider" aria-label="Starting hue" type="range" min="0" max="360" step="1" value={selectedHue} oninput={event => updateHue(event, false)}/></label>
      <label>Color range <output>{selectedRange === 360 ? 'Full spectrum' : selectedRange + '°'}</output><input aria-label="Hue range" type="range" min="0" max="360" step="1" value={selectedRange} oninput={event => updateHue(event, true)}/></label>
      <label>Saturation <output>{saturation}%</output><input aria-label="Color saturation" type="range" min="0" max="100" step="1" bind:value={saturation} onchange={saveSettings}/></label>
      <label>Brightness <output>{brightness}%</output><input aria-label="Color brightness" type="range" min="20" max="80" step="1" bind:value={brightness} onchange={saveSettings}/></label>
      <small>Highlighted hues are used for this channel. Set range to 0 for one tone, or 360 for RGB cycling.</small>
    </details>
    <button onclick={resetSettings}>Reset settings</button>
  </div>{/if}
  <svg style:--blue-drift={`${blueDrift + huePhase}deg`} style:--red-drift={`${redDrift + huePhase}deg`} viewBox="0 0 1000 240" preserveAspectRatio="none" role="img" aria-label={waveStyle === 'dots' ? 'Stereo FFT dot matrix extending above and below the centre' : 'Smoothed stereo FFT waves extending above and below the centre'}>
    <defs>
      <filter id="aurora-halo" x="-5%" y="-25%" width="110%" height="150%" color-interpolation-filters="sRGB"><feGaussianBlur stdDeviation="5 10"/></filter>
      <mask id="aurora-fade" maskUnits="userSpaceOnUse" x="0" y="0" width="1000" height="240"><rect width="1000" height="240" fill="url(#curtain-fade)"/></mask>
      <linearGradient id="curtain-fade" gradientUnits="userSpaceOnUse" x1="0" y1="20" x2="0" y2="220"><stop offset="0" stop-color="white"/><stop offset=".25" stop-color="#bbb"/><stop offset=".4" stop-color="#777"/><stop offset=".5" stop-color="#222"/><stop offset=".6" stop-color="#777"/><stop offset=".75" stop-color="#bbb"/><stop offset="1" stop-color="white"/></linearGradient>
      <!-- Fixed scene coordinates map local height to color. Both mirrored
           halves use the same palette; colors follow the eased curve itself.
           Six color anchors spread transitions across the amplitude range;
           slowly drifting positions keep the mapping alive without abrupt changes. -->
      <linearGradient id="wave-blue" gradientUnits="userSpaceOnUse" x1="0" y1="20" x2="0" y2="220">
        {#each [...colorBands].reverse() as band}
          <stop offset={colorOffset(band.amp, blueDrift, -1)} style:stop-color={palette(band.amp, leftHue, leftRange, blueDrift, huePhase, saturation, brightness)}/>
        {/each}
        {#each colorBands.slice(1) as band}
          <stop offset={colorOffset(band.amp, blueDrift, 1)} style:stop-color={palette(band.amp, leftHue, leftRange, blueDrift, huePhase, saturation, brightness)}/>
        {/each}
      </linearGradient>
      <linearGradient id="wave-red" gradientUnits="userSpaceOnUse" x1="0" y1="20" x2="0" y2="220">
        {#each [...colorBands].reverse() as band}
          <stop offset={colorOffset(band.amp, redDrift, -1)} style:stop-color={palette(band.amp, rightHue, rightRange, redDrift, huePhase, saturation, brightness)}/>
        {/each}
        {#each colorBands.slice(1) as band}
          <stop offset={colorOffset(band.amp, redDrift, 1)} style:stop-color={palette(band.amp, rightHue, rightRange, redDrift, huePhase, saturation, brightness)}/>
        {/each}
      </linearGradient>
    </defs>
    {#if waveStyle === 'dots'}
    <g class="dot-matrix">
      {#if showRight}<path d={upperRight} class="right-line"/><path d={lowerRight} class="right-negative"/>{/if}
      {#if showLeft}<path d={upperLeft} class="left-line"/><path d={lowerLeft} class="left-negative"/>{/if}
    </g>
    {:else}
    <g class="aurora-glow" filter="url(#aurora-halo)" style:opacity={glowStrength}>
      {#if showLeft}<path d={`${upperLeft} ${lowerLeft}`} stroke="url(#wave-blue)"/>{/if}
      {#if showRight}<path d={`${upperRight} ${lowerRight}`} stroke="url(#wave-red)"/>{/if}
    </g>
    <g mask="url(#aurora-fade)">
      {#if showLeft}<path d={`${upperLeft} L1000,120 L0,120 Z ${lowerLeft} L1000,120 L0,120 Z`} fill="url(#wave-blue)" fill-opacity={.35 + .3 * glowStrength} />
      {/if}
      {#if showRight}<path d={`${upperRight} L1000,120 L0,120 Z ${lowerRight} L1000,120 L0,120 Z`} fill="url(#wave-red)" fill-opacity={.3 + .25 * glowStrength}/>{/if}
    </g>
    {#if showRight}<path d={upperRight} class="right-line"/><path d={lowerRight} class="right-negative"/>{/if}
    {#if showLeft}<path d={upperLeft} class="left-line"/><path d={lowerLeft} class="left-negative"/>{/if}
    {/if}
  </svg>
  {#if status}<span class="spectrum-status" role="status">{status}</span>{/if}
</div>
<style>
  .spectrum { position: absolute; inset: 0; z-index: 2; pointer-events: none; overflow: hidden; }
  .spectrum-options { position: absolute; right: 32px; top: 126px; max-height: calc(100vh - 280px); overflow-y: auto; display: flex; flex-wrap: wrap; gap: 8px; width: min(270px, calc(100vw - 64px)); padding: 14px; border: 1px solid var(--border); border-radius: 12px; background: var(--glass-strong); pointer-events: auto; z-index: 8; }
  button { border: 1px solid var(--border); border-radius: 20px; padding: 7px 12px; background: var(--glass); backdrop-filter: blur(16px); color: var(--muted); font-size: 11px; cursor: pointer; }
  button[aria-pressed="true"] { color: var(--text); }
  svg { position: absolute; top: 38%; left: 0; width: 100%; height: 44%; overflow: visible; opacity: .9;  }
  .left-line,.left-negative,.right-line,.right-negative { fill: none; stroke: url(#wave-blue); stroke-width: 2; vector-effect: non-scaling-stroke; stroke-linecap: round; }
  .right-line,.right-negative { stroke: url(#wave-red); stroke-width: 1.6; }
  .aurora-glow { fill: none; stroke-width: 13; }
  .dot-matrix path { stroke-width: 3; }
  .quiet svg { opacity: 0; }
  .spectrum-status { position: absolute; bottom: 136px; left: 32px; max-width: calc(100% - 64px); font-size: 11px; color: var(--muted); }
  @media (prefers-color-scheme: light) {
  }
  @media (prefers-reduced-motion: reduce) { .aurora-glow { filter: none; stroke-width: 4; } }
  @media (max-height: 700px) { svg { top: 34%; height: 45%; } }
  @media (prefers-color-scheme: light) { svg { --aurora-extra-lightness: 42%; } }
  :global(.custom-background) svg { --aurora-extra-lightness: 62%; }
  .spectrum-options label { width: 100%; display: flex; flex-wrap: wrap; gap: 7px; font-size: 11px; color: var(--text); }
  .spectrum-options output { margin-left: auto; color: var(--muted); }
  .spectrum-options input { width: 100%; accent-color: var(--accent); }
  .color-settings { width: 100%; color: var(--text); font-size: 11px; }
  .color-settings summary { cursor: pointer; padding: 8px 0; }
  .color-settings label { margin: 10px 0; }
  .color-settings select { margin-left: auto; background: var(--glass-strong); color: var(--text); border: 1px solid var(--border); border-radius: 6px; }
  .color-settings small { display: block; color: var(--muted); line-height: 1.5; }
  .hue-spectrum { height: 24px; position: relative; border-radius: 5px; overflow: hidden; background: linear-gradient(to right, red, yellow, lime, cyan, blue, magenta, red); }
  .hue-spectrum span { position: absolute; top: 0; height: 100%; border: 2px solid white; box-shadow: inset 0 0 0 1px #222; min-width: 2px; }
</style>

