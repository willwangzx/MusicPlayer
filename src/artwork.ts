import { invoke } from '@tauri-apps/api/core';

const MAX_CACHE_BYTES = 8 * 1024 * 1024;
const MAX_CACHE_ENTRIES = 128;
const cache = new Map<string, Blob>();
interface ArtworkRequest {
  result: Promise<Blob | null>;
  consumers: Set<() => boolean>;
}
const inflight = new Map<string, ArtworkRequest>();
let bytes = 0;
let generation = 0;
let serial: Promise<void> = Promise.resolve();

export function artworkCacheVersion() { return generation; }

export function invalidateArtworkCache() {
  generation += 1;
  cache.clear();
  inflight.clear();
  bytes = 0;
}

function keyFor(id: string, cacheGeneration: number) {
  return `${cacheGeneration}\u0000${id}`;
}

export function cover(id: string, active: () => boolean): Promise<Blob | null> {
  if (!active()) return Promise.resolve(null);

  const cacheGeneration = generation;
  const key = keyFor(id, cacheGeneration);
  const found = cache.get(key);
  if (found) {
    cache.delete(key);
    cache.set(key, found);
    return Promise.resolve(found).then(blob => active() ? blob : null);
  }

  const existing = inflight.get(key);
  if (existing) {
    existing.consumers.add(active);
    return existing.result.then(blob => active() ? blob : null);
  }

  const consumers = new Set([active]);
  // A queued request may outlive its cover after scrolling or navigation.
  // Keep shared requests alive only while at least one cover still needs them.
  const needed = () => cacheGeneration === generation && [...consumers].some(consumer => consumer());

  const task = serial.then(async () => {
    if (!needed()) return null;
    const cached = cache.get(key);
    if (cached) {
      cache.delete(key);
      cache.set(key, cached);
      return cached;
    }

    const raw = await invoke<ArrayBuffer | Uint8Array | number[]>('album_artwork', {id});
    if (!needed()) return null;
    const data = raw instanceof ArrayBuffer
      ? raw
      : raw instanceof Uint8Array
        ? new Uint8Array(raw).buffer
        : new Uint8Array(raw).buffer;
    if (!data.byteLength) return null;

    const bitmap = await createImageBitmap(
      new Blob([data]),
      {resizeWidth: 256, resizeHeight: 256, resizeQuality: 'high'},
    );
    let blob: Blob;
    try {
      if (!needed()) return null;
      const canvas = new OffscreenCanvas(256, 256);
      const context = canvas.getContext('2d');
      if (!context) return null;
      context.drawImage(bitmap, 0, 0);
      blob = await canvas.convertToBlob({type: 'image/jpeg', quality: .85});
    } finally {
      bitmap.close();
    }
    if (!needed() || blob.size > MAX_CACHE_BYTES) return null;

    while (cache.size && (bytes + blob.size > MAX_CACHE_BYTES || cache.size >= MAX_CACHE_ENTRIES)) {
      const oldest = cache.keys().next().value;
      if (oldest === undefined) break;
      const old = cache.get(oldest);
      if (old) bytes -= old.size;
      cache.delete(oldest);
    }
    cache.set(key, blob);
    bytes += blob.size;
    return blob;
  }).catch(() => null);

  serial = task.then(() => undefined, () => undefined);
  const entry = {result: task, consumers};
  inflight.set(key, entry);
  void task.then(() => {
    consumers.clear();
    if (inflight.get(key) === entry) inflight.delete(key);
  });
  return task.then(blob => active() ? blob : null);
}
