import '../src/app.css';
import { mount } from 'svelte';
import { emit } from '@tauri-apps/api/event';
import { mockIPC } from '@tauri-apps/api/mocks';
import AppleMusic from '../src/AppleMusic.svelte';
import Downloads from '../src/Downloads.svelte';

const status = document.querySelector('#fixture-status');
const state = {
  startCalls: 0,
  pageOffsets: [],
  queueDelayMs: 0,
  queueCallsInFlight: 0,
  maxQueueCallsInFlight: 0,
  lastEnqueueCount: 0,
  lastEnqueueCatalogIds: [],
  snapshot: {
    running: false,
    stopAfterCurrent: false,
    queued: 105,
    runningJobId: null,
    error: 'Fixture snapshot error (intentional QA state)',
    revision: 1,
    available: true,
  },
};

function track(id, title, artist, album, options = {}) {
  return {
    catalogId: id,
    title,
    artist,
    album,
    albumArtist: artist,
    albumId: '987654',
    durationMs: 180000,
    artworkUrl: null,
    releaseDate: '2024-01-01',
    disc: 1,
    discTotal: 2,
    track: 1,
    trackTotal: 52,
    available: true,
    handle: null,
    existing: false,
    queued: false,
    duplicate: false,
    ...options,
  };
}

const orderedAlbumTracks = Array.from({length: 104}, (_, index) => {
  const disc = index < 52 ? 1 : 2;
  const number = (index % 52) + 1;
  const options = {disc, track: number};
  if (index === 7) options.existing = true;
  if (index === 42) options.duplicate = true;
  if (index === 88) options.queued = true;
  return track(`987${String(index).padStart(3, '0')}`, `Album Track ${index + 1}`, 'Fixture Artist', 'Fixture Album', options);
});

// The worker may return catalog pages in API order. The real normalizer must
// restore album disc/track order, while preserving playlist order as supplied.
const albumTracksOnWire = [...orderedAlbumTracks].reverse();
const playlistTracksOnWire = [
  track('700003', 'Playlist Third', 'Fixture Artist', 'Fixture Playlist', {disc: 1, track: 3}),
  track('700001', 'Playlist First', 'Fixture Artist', 'Fixture Playlist', {disc: 1, track: 1}),
  track('700002', 'Playlist Second', 'Fixture Artist', 'Fixture Playlist', {disc: 1, track: 2}),
];

const albumPreview = {
  collection: 'tracks',
  kind: 'album',
  catalogId: '987654',
  title: 'Fixture Album',
  artist: 'Fixture Artist',
  releaseDate: '2024-01-01',
  artworkUrl: null,
  trackCount: orderedAlbumTracks.length,
  availableCount: orderedAlbumTracks.filter(item => item.available && !item.duplicate).length,
  unavailableCount: orderedAlbumTracks.filter(item => !item.available).length,
  tracks: albumTracksOnWire,
};

const playlistPreview = {
  collection: 'tracks',
  kind: 'playlist',
  catalogId: '123456789',
  title: 'Fixture Playlist',
  artist: 'Fixture Curator',
  releaseDate: null,
  artworkUrl: null,
  trackCount: playlistTracksOnWire.length,
  availableCount: playlistTracksOnWire.length,
  unavailableCount: 0,
  tracks: playlistTracksOnWire,
};

const searchSongFixtures = [
  orderedAlbumTracks[0],
  track('987999', 'Unavailable Search Result', 'Fixture Artist', 'Fixture Singles', {available: false}),
  track(orderedAlbumTracks[0].catalogId, 'Duplicate Search Result', 'Fixture Artist', 'Fixture Album', {duplicate: true}),
];

const jobs = Array.from({length: 105}, (_, index) => ({
  id: `fixture-job-${index + 1}`,
  batchId: index < 104 ? 'fixture-batch-album' : null,
  batchTitle: index < 104 ? 'Fixture Album' : null,
  catalogId: `987${String(index).padStart(3, '0')}`,
  title: index < 104 ? `Album Track ${index + 1}` : 'Standalone Fixture Song',
  artist: 'Fixture Artist',
  album: index < 104 ? 'Fixture Album' : 'Fixture Singles',
  artworkUrl: null,
  quality: 'AAC 256',
  state: index === 0 ? 'completed' : 'queued',
  bytesDone: null,
  bytesTotal: null,
  destination: index === 0 ? 'D:\\Music\\Fixture Album\\01 - Album Track 1.m4a' : null,
  error: null,
  retryable: false,
  createdAt: '1760000000',
}));

const batches = [{
  id: 'fixture-batch-album',
  title: 'Fixture Album',
  artist: 'Fixture Artist',
  total: 104,
  completed: 1,
  failed: 0,
  active: 0,
  pending: 103,
}];

function page(offset) {
  return {
    jobs: jobs.slice(offset, offset + 100),
    offset,
    total: jobs.length,
    snapshot: state.snapshot,
    batches,
  };
}

function updateStatus() {
  if (!status) return;
  status.replaceChildren();
  const heading = document.createElement('strong');
  heading.textContent = 'Mock evidence';
  status.append(heading);
  const lines = [
    `Album eligible tracks: ${orderedAlbumTracks.filter(item => item.available && !item.existing && !item.queued && !item.duplicate).length}`,
    `Last album enqueue: ${state.lastEnqueueCount} tracks${state.lastEnqueueCatalogIds.length ? ` (${state.lastEnqueueCatalogIds[0]} … ${state.lastEnqueueCatalogIds.at(-1)})` : ''}`,
    `Start calls: ${state.startCalls}`,
    `Queue page offsets: ${state.pageOffsets.slice(-8).join(', ') || 'none'}`,
    `Queue total: ${jobs.length} · Full album batch summary: ${batches[0].total}`,
  ];
  for (const line of lines) {
    const item = document.createElement('span');
    item.textContent = line;
    status.append(item);
  }
}

async function publishSnapshot() {
  await emit('musicplayer://downloads', state.snapshot);
}

mockIPC(async (command, args = {}) => {
  switch (command) {
    case 'apple_account_status':
      return {configured: true, authenticated: true, subscription: 'active', storefront: 'fixture', message: null};
    case 'apple_search':
      if (args.kind === 'albums') {
        return {collection: 'results', total: 1, results: [{...orderedAlbumTracks[0], catalogId: '987654', title: 'Fixture Album', album: 'Fixture Album', trackCount: 104, releaseDate: '2024-01-01'}]};
      }
      return {collection: 'results', total: searchSongFixtures.length, results: searchSongFixtures};
    case 'apple_preview_album':
      return albumPreview;
    case 'apple_preview_playlist':
      return playlistPreview;
    case 'enqueue_downloads': {
      const request = args.request ?? {};
      const tracks = Array.isArray(request.tracks) ? request.tracks : [];
      state.lastEnqueueCount = tracks.length;
      state.lastEnqueueCatalogIds = tracks.map(item => item.catalogId);
      updateStatus();
      return {accepted: tracks.length, skipped: 0};
    }
    case 'download_queue': {
      state.pageOffsets.push(args.offset ?? 0);
      // Freeze at request start to model IPC returning a stale progress revision.
      const response = structuredClone(page(args.offset ?? 0));
      state.queueCallsInFlight += 1;
      state.maxQueueCallsInFlight = Math.max(state.maxQueueCallsInFlight, state.queueCallsInFlight);
      if (state.queueDelayMs) await new Promise(resolve => setTimeout(resolve, state.queueDelayMs));
      state.queueCallsInFlight -= 1;
      updateStatus();
      return response;
    }
    case 'start_downloads':
      state.startCalls += 1;
      state.snapshot = {...state.snapshot, running: true, queued: 104, runningJobId: 'fixture-job-2', revision: state.snapshot.revision + 1};
      updateStatus();
      void publishSnapshot();
      return null;
    case 'stop_downloads_after_current':
      state.snapshot = {...state.snapshot, stopAfterCurrent: true, revision: state.snapshot.revision + 1};
      void publishSnapshot();
      return null;
    case 'clear_finished_downloads':
    case 'cancel_download':
    case 'retry_download':
      return null;
    default:
      throw new Error(`Fixture has no handler for ${command}`);
  }
}, {shouldMockEvents: true});

mount(AppleMusic, {target: document.querySelector('#apple-root')});
mount(Downloads, {target: document.querySelector('#downloads-root')});
updateStatus();

// Browser-only deterministic progress driver; no production IPC or downloads.
window.downloadFixture = {
  setDelay(milliseconds) {
    state.queueDelayMs = milliseconds;
    state.maxQueueCallsInFlight = state.queueCallsInFlight;
  },
  async progress(bytesDone, bytesTotal = 1000000, stage = 'downloading') {
    jobs[101].state = stage;
    jobs[101].bytesDone = bytesDone;
    jobs[101].bytesTotal = bytesTotal;
    state.snapshot = {...state.snapshot, revision: state.snapshot.revision + 1};
    await publishSnapshot();
  },
  stats() {
    return {inFlight: state.queueCallsInFlight, maxInFlight: state.maxQueueCallsInFlight};
  },
};
