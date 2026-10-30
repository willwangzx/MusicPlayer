<script lang="ts">
  import { onMount } from 'svelte';
  import { cover } from './artwork';
  export let id: string;
  export let artworkVersion = 0;
  let element: HTMLDivElement;
  let url = '';
  let mounted = false;
  let visible = false;
  let generation = 0;
  let requestedId: string | null = null;
  let requestedVersion: number | null = null;

  function releaseUrl() {
    if (url) URL.revokeObjectURL(url);
    url = '';
  }

  function load() {
    const current = ++generation;
    const targetId = id;
    const targetVersion = artworkVersion;
    requestedId = targetId;
    requestedVersion = targetVersion;
    releaseUrl();
    void cover(targetId, () => (
      mounted && visible && current === generation && id === targetId && artworkVersion === targetVersion
    )).then(blob => {
      if (blob && mounted && visible && current === generation && id === targetId && artworkVersion === targetVersion) {
        url = URL.createObjectURL(blob);
      }
    });
  }

  function visibilityChanged(next: boolean) {
    if (next === visible) return;
    visible = next;
    if (!visible) {
      generation += 1;
      requestedId = null;
      requestedVersion = null;
      releaseUrl();
    }
  }

  $: if (mounted && visible && (requestedId !== id || requestedVersion !== artworkVersion)) load();

  onMount(() => {
    mounted = true;
    const observer = new IntersectionObserver(entries => visibilityChanged(entries[0]?.isIntersecting ?? false));
    observer.observe(element);
    return () => {
      mounted = false;
      visible = false;
      generation += 1;
      observer.disconnect();
      releaseUrl();
    };
  });
</script>
<div class="cover" bind:this={element} aria-hidden="true">{#if url}<img src={url} alt="" />{:else}<span>♫</span>{/if}</div>
<style>
.cover { display: grid; place-items: center; aspect-ratio: 1; border-radius: 12px; overflow: hidden; background: linear-gradient(135deg, #414960, #272d3b); font-size: 52px; margin-bottom: 12px; color: #aab4d4; }
img { width: 100%; height: 100%; object-fit: cover; }
</style>
