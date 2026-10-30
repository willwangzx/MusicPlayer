<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  export let tracks: {path: string; title: string}[] = [];
  export let disabled = false;
  export let busy = false;
  export let onrecycled: (paths: string[]) => void = () => {};
  let dialog: HTMLDialogElement;
  let targets: {path: string; title: string}[] = [];
  let failures: {path: string; title: string; message: string}[] = [];
  let completed = 0;
  let attempted = 0;
  let message = '';
  function open() {
    if (disabled || busy || !tracks.length) return;
    // Freeze exact paths at confirmation time; refreshes must never retarget a deletion.
    targets = tracks.map(({path, title}) => ({path, title}));
    failures = [];
    completed = 0;
    attempted = 0;
    message = '';
    dialog.showModal();
  }
  async function recycle() {
    if (busy || disabled || !targets.length) return;
    busy = true;
    failures = [];
    attempted = 0;
    const moved: string[] = [];
    const remaining: typeof targets = [];
    for (const track of targets) {
      try {
        // The existing command revalidates each indexed audio file and uses Recycle Bin only.
        await invoke('recycle_library_track', {path: track.path});
        moved.push(track.path);
        completed += 1;
      } catch (e) {
        remaining.push(track);
        failures = [...failures, {...track, message: String(e)}];
      }
      attempted += 1;
    }
    targets = remaining;
    message = `${completed} ${completed === 1 ? 'file moved' : 'files moved'} to Recycle Bin.${remaining.length ? ` ${remaining.length} could not be moved; they remain selected.` : ''}`;
    busy = false;
    if (!remaining.length) dialog.close();
    if (moved.length) onrecycled(moved);
  }
</script>
<button class="delete-track" disabled={disabled || busy || !tracks.length} onclick={open}>Delete selected files{tracks.length ? ` (${tracks.length})` : ''}</button>
{#if message}<p class="recycle-result" role="status">{message}</p>{/if}
<dialog bind:this={dialog} oncancel={(event) => { if (busy) event.preventDefault(); }} aria-label="Move selected music files to Recycle Bin">
  <h3>Move {targets.length} {targets.length === 1 ? 'file' : 'files'} to Recycle Bin?</h3>
  <p>These audio files will be removed from your library and all playback queue entries. Folders, artwork and lyrics stay in place. Restore audio files from Windows Recycle Bin and scan the library again.</p>
  <ul class="selected-files" aria-label="Selected audio files">
    {#each targets as track (track.path)}<li><strong>{track.title}</strong><span class="path">{track.path}</span></li>{/each}
  </ul>
  {#if busy}<p role="status">Moving files… {attempted} of {targets.length} processed.</p>{/if}
  {#if failures.length}
    <div role="alert"><p>{completed} moved to Recycle Bin. {failures.length} could not be moved.</p><ul>{#each failures as failure (failure.path)}<li><strong>{failure.title}</strong>: {failure.message}</li>{/each}</ul></div>
  {/if}
  {#if disabled}<p role="status">Wait for the library scan to finish before moving files.</p>{/if}
  <div class="actions"><button disabled={busy} onclick={() => dialog.close()}>{failures.length ? 'Close' : 'Cancel'}</button><button class="delete-track" disabled={busy || disabled} onclick={recycle}>{busy ? 'Moving…' : failures.length ? 'Retry remaining files' : 'Move selected files to Recycle Bin'}</button></div>
</dialog>
<style>
  button { border: 1px solid #363a4b; background: #202431; color: #dadde8; border-radius: 7px; padding: 8px 12px; cursor: pointer; white-space: nowrap; }
  button:disabled { opacity: .4; cursor: default; }
  .delete-track { color: #ffb3b3; }
  dialog { width: min(600px, 85vw); max-height: 80vh; padding: 24px; border: 1px solid #484b60; border-radius: 12px; color: #dadde8; background: #202431; }
  dialog::backdrop { background: #0009; }
  h3 { margin-top: 0; }
  p { line-height: 1.5; }
  .selected-files { max-height: 220px; overflow-y: auto; padding-left: 22px; }
  li { padding: 5px 0; overflow-wrap: anywhere; }
  .path { display: block; overflow-wrap: anywhere; color: #a8abba; font-size: 12px; margin-top: 3px; }
  .actions { display: flex; justify-content: flex-end; gap: 12px; margin-top: 20px; }
  .recycle-result { flex-basis: 100%; margin: 0; color: #a8abba; font-size: 12px; }
  [role="alert"] { color: #ffb3b3; }
</style>
