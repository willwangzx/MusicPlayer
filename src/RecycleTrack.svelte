<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  export let path: string;
  export let title: string;
  export let disabled = false;
  export let onrecycled: () => void = () => {};
  let dialog: HTMLDialogElement;
  let busy = false;
  let error = '';
  let selection: {path: string; title: string} | null = null;
  async function recycle() {
    if (busy || !selection) return;
    busy = true;
    error = '';
    try {
      await invoke('recycle_library_track', {path: selection.path});
      dialog.close();
      onrecycled();
    } catch (e) { error = String(e); }
    finally { busy = false; }
  }
</script>
<button class="delete-track" {disabled} aria-label={`Move ${title} to Recycle Bin`} onclick={() => { error = ''; selection = {path, title}; dialog.showModal(); }}>Delete file</button>
<dialog bind:this={dialog} oncancel={(event) => { if (busy) event.preventDefault(); }} aria-label="Move music file to Recycle Bin">
  <h3>Move to Recycle Bin?</h3>
  <p><strong>{selection?.title}</strong></p>
  <p class="path">{selection?.path}</p>
  <p>This audio file will be removed from your library and all playback queue entries. You can restore it from the Windows Recycle Bin and scan the library again.</p>
  {#if error}<p role="alert">{error}</p>{/if}
  <div class="actions"><button disabled={busy} onclick={() => dialog.close()}>Cancel</button><button class="delete-track" disabled={busy} onclick={recycle}>{busy ? 'Moving…' : 'Move to Recycle Bin'}</button></div>
</dialog>
<style>
  button { border: 1px solid #363a4b; background: #202431; color: #dadde8; border-radius: 7px; padding: 8px; cursor: pointer; white-space: nowrap; }
  button:disabled { opacity: .4; cursor: default; }
  .delete-track { color: #ffb3b3; }
  dialog { max-width: min(520px, 85vw); padding: 24px; border: 1px solid #484b60; border-radius: 12px; color: #dadde8; background: #202431; }
  dialog::backdrop { background: #0009; }
  h3 { margin-top: 0; }
  p { line-height: 1.5; }
  .path { overflow-wrap: anywhere; color: #a8abba; font-size: 12px; }
  .actions { display: flex; justify-content: flex-end; gap: 12px; }
  [role="alert"] { color: #ffb3b3; }
</style>
