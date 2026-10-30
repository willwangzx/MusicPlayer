export interface AudioQuality {
  codec: string | null; sampleRate: number | null; channels: number | null;
  sampleFormat: string | null; bitrate: number | null;
  outputSampleRate: number | null; outputChannels: number | null; outputFormat: string | null;
}
export interface CurrentTrack {
  track: {title: string; artist: string; album: string} | null;
  albumId: string | null;
}
export interface Analysis {
  available: boolean; active: boolean; source: string; status: string; sequence: number;
  sampleRate: number; frequencies: number[]; left: number[]; right: number[];
  rmsLeft: number; rmsRight: number;
}
