/**
 * Frontend-facing shapes for the Apple Music and download-manager boundary.
 *
 * The Rust commands use camelCase JSON.  The optional fields intentionally
 * mirror fields that can be absent for unavailable catalog items or for a
 * job whose destination/progress is not known yet.
 */
export type SearchKind = 'songs' | 'albums';

export type DownloadState =
  | 'queued'
  | 'resolving'
  | 'downloading'
  | 'decrypting'
  | 'remuxing'
  | 'tagging'
  | 'verifying'
  | 'organizing'
  | 'completed'
  | 'failed'
  | 'interrupted'
  | 'cancelled'
  | 'skipped';

export interface DownloadTrackInput {
  catalogId: string;
  title: string;
  artist: string;
  album: string;
  albumArtist?: string | null;
  albumId?: string | null;
  durationMs?: number | null;
  artworkUrl?: string | null;
  releaseDate?: string | null;
  disc?: number | null;
  discTotal?: number | null;
  track?: number | null;
  trackTotal?: number | null;
  available: boolean;
  handle?: string | null;
  existing?: boolean;
  queued?: boolean;
  duplicate?: boolean;
}

export interface BatchInput {
  title: string;
  artist: string;
  albumId?: string | null;
}

export interface EnqueueRequest {
  tracks: DownloadTrackInput[];
  batch?: BatchInput | null;
}

export interface EnqueueResult {
  accepted: number;
  skipped: number;
}

export interface AppleSearchResult extends DownloadTrackInput {
  trackCount?: number | null;
}

export interface ApplePreview {
  title: string;
  artist: string;
  catalogId: string | null;
  kind: 'album' | 'playlist' | string | null;
  artworkUrl?: string | null;
  releaseDate?: string | null;
  tracks: DownloadTrackInput[];
  trackCount?: number | null;
  availableCount: number;
  unavailableCount: number;
}

export interface AccountStatus {
  configured?: boolean;
  authenticated?: boolean;
  subscription?: string | null;
  storefront?: string | null;
  message?: string | null;
}

export interface DownloadJob {
  id: string;
  batchId?: string | null;
  batchTitle?: string | null;
  catalogId: string;
  title: string;
  artist: string;
  album: string;
  artworkUrl?: string | null;
  quality: string;
  state: DownloadState;
  bytesDone?: number | null;
  bytesTotal?: number | null;
  destination?: string | null;
  error?: string | null;
  retryable: boolean;
  createdAt: string;
}

export interface DownloadPage {
  jobs: DownloadJob[];
  offset: number;
  total: number;
  snapshot: DownloadSnapshot;
  batches: DownloadBatchSummary[];
}

export interface DownloadSnapshot {
  running: boolean;
  stopAfterCurrent: boolean;
  queued: number;
  runningJobId?: string | null;
  error?: string | null;
  revision?: number;
  available?: boolean;
}

export interface DownloadBatchSummary {
  id: string;
  title: string;
  artist: string;
  total: number;
  completed: number;
  failed: number;
  active: number;
  pending: number;
}

export const emptyDownloadSnapshot: DownloadSnapshot = {
  running: false,
  stopAfterCurrent: false,
  queued: 0,
  runningJobId: null,
  error: null,
  revision: 0,
  available: true,
};

export function asRecord(value: unknown): Record<string, unknown> {
  return value !== null && typeof value === 'object' ? value as Record<string, unknown> : {};
}

function stringValue(value: unknown, fallback = ''): string {
  return typeof value === 'string' ? value : fallback;
}

function optionalString(value: unknown): string | null {
  return typeof value === 'string' && value.length ? value : null;
}

function optionalNumber(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

function booleanValue(value: unknown, fallback = false): boolean {
  return typeof value === 'boolean' ? value : fallback;
}

/** Keep the UI resilient to a bounded response with an empty optional field. */
export function normalizeTrack(value: unknown): DownloadTrackInput {
  const item = asRecord(value);
  return {
    catalogId: stringValue(item.catalogId),
    title: stringValue(item.title, 'Unknown title'),
    artist: stringValue(item.artist, 'Unknown artist'),
    album: stringValue(item.album, 'Unknown album'),
    albumArtist: optionalString(item.albumArtist),
    albumId: optionalString(item.albumId),
    durationMs: optionalNumber(item.durationMs),
    artworkUrl: optionalString(item.artworkUrl),
    releaseDate: optionalString(item.releaseDate),
    disc: optionalNumber(item.disc),
    discTotal: optionalNumber(item.discTotal),
    track: optionalNumber(item.track),
    trackTotal: optionalNumber(item.trackTotal),
    available: booleanValue(item.available, true),
    handle: optionalString(item.handle),
    existing: booleanValue(item.existing),
    queued: booleanValue(item.queued),
    duplicate: booleanValue(item.duplicate),
  };
}

export function normalizePreview(value: unknown): ApplePreview {
  const data = asRecord(value);
  const rawTracks = Array.isArray(data.tracks) ? data.tracks : [];
  const kind = optionalString(data.kind);
  const tracks = rawTracks.map(normalizeTrack);
  if (kind === 'album') {
    tracks.sort((left, right) => {
      const discDelta = (left.disc ?? 1) - (right.disc ?? 1);
      return discDelta || (left.track ?? 0) - (right.track ?? 0);
    });
  }
  const title = stringValue(data.title, tracks[0]?.album ?? 'Untitled album');
  const artist = stringValue(data.artist, tracks[0]?.albumArtist ?? tracks[0]?.artist ?? 'Unknown artist');
  return {
    title,
    artist,
    catalogId: optionalString(data.catalogId),
    kind,
    artworkUrl: optionalString(data.artworkUrl ?? tracks[0]?.artworkUrl),
    releaseDate: optionalString(data.releaseDate),
    tracks,
    trackCount: optionalNumber(data.trackCount) ?? tracks.length,
    availableCount: optionalNumber(data.availableCount) ?? tracks.filter(track => track.available && !track.duplicate).length,
    unavailableCount: optionalNumber(data.unavailableCount) ?? tracks.filter(track => !track.available).length,
  };
}

export function normalizeJob(value: unknown): DownloadJob {
  const item = asRecord(value);
  const knownStates: DownloadState[] = ['queued', 'resolving', 'downloading', 'decrypting', 'remuxing', 'tagging', 'verifying', 'organizing', 'completed', 'failed', 'interrupted', 'cancelled', 'skipped'];
  const state = stringValue(item.state, 'queued');
  return {
    id: stringValue(item.id),
    batchId: optionalString(item.batchId),
    batchTitle: optionalString(item.batchTitle),
    catalogId: stringValue(item.catalogId),
    title: stringValue(item.title, 'Unknown title'),
    artist: stringValue(item.artist, 'Unknown artist'),
    album: stringValue(item.album, 'Unknown album'),
    artworkUrl: optionalString(item.artworkUrl),
    quality: stringValue(item.quality, 'AAC 256'),
    state: (knownStates.includes(state as DownloadState) ? state : 'failed') as DownloadState,
    bytesDone: optionalNumber(item.bytesDone),
    bytesTotal: optionalNumber(item.bytesTotal),
    destination: optionalString(item.destination),
    error: optionalString(item.error),
    retryable: booleanValue(item.retryable),
    createdAt: stringValue(item.createdAt),
  };
}

export function normalizeDownloadPage(value: unknown, requestedOffset: number): DownloadPage {
  const data = asRecord(value);
  const rawJobs = Array.isArray(data.jobs) ? data.jobs : [];
  return {
    jobs: rawJobs.map(normalizeJob),
    offset: typeof data.offset === 'number' ? data.offset : requestedOffset,
    total: typeof data.total === 'number' ? data.total : rawJobs.length,
    snapshot: normalizeSnapshot(data.snapshot),
    batches: Array.isArray(data.batches) ? data.batches.map(normalizeBatchSummary) : [],
  };
}

function normalizeSnapshot(value: unknown): DownloadSnapshot {
  const data = asRecord(value);
  return {
    ...emptyDownloadSnapshot,
    running: booleanValue(data.running),
    stopAfterCurrent: booleanValue(data.stopAfterCurrent),
    queued: typeof data.queued === 'number' ? Math.max(0, data.queued) : 0,
    runningJobId: optionalString(data.runningJobId),
    error: optionalString(data.error),
    revision: typeof data.revision === 'number' ? data.revision : 0,
    available: booleanValue(data.available, true),
  };
}

function normalizeBatchSummary(value: unknown): DownloadBatchSummary {
  const data = asRecord(value);
  return {
    id: stringValue(data.id),
    title: stringValue(data.title, 'Download batch'),
    artist: stringValue(data.artist, 'Unknown artist'),
    total: typeof data.total === 'number' ? data.total : 0,
    completed: typeof data.completed === 'number' ? data.completed : 0,
    failed: typeof data.failed === 'number' ? data.failed : 0,
    active: typeof data.active === 'number' ? data.active : 0,
    pending: typeof data.pending === 'number' ? data.pending : 0,
  };
}

export function formatBytes(value: number | null | undefined): string {
  if (value === null || value === undefined || !Number.isFinite(value) || value < 0) return 'Unknown size';
  if (value < 1024) return `${Math.round(value)} B`;
  const units = ['KiB', 'MiB', 'GiB'];
  let amount = value;
  let unit = 'B';
  for (const next of units) {
    amount /= 1024;
    unit = next;
    if (amount < 1024 || next === units[units.length - 1]) break;
  }
  return `${amount.toFixed(amount >= 100 ? 0 : amount >= 10 ? 1 : 2)} ${unit}`;
}

export function formatDuration(value: number | null | undefined): string {
  if (value === null || value === undefined || value <= 0) return '—';
  const seconds = Math.round(value / 1000);
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`;
}
