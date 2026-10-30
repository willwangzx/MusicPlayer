// Magnitudes stay nonnegative; mirrored envelopes create signed visual displacement.
export const POINTS = 1024;
// Allocate screen width to bass, musical mids, and treble explicitly.
export function bandPosition(t: number): number {
  const lowMid = Math.log(250 / 25) / Math.log(20000 / 25);
  const midHigh = Math.log(4000 / 25) / Math.log(20000 / 25);
  if (t <= .4) return t / .4 * lowMid;
  if (t <= .82) return lowMid + (t - .4) / .42 * (midHigh - lowMid);
  return midHigh + (t - .82) / .18 * (1 - midHigh);
}
export const zero = () => Array<number>(POINTS).fill(0);
export function envelope(bands: number[], rms: number, smoothness = 1): number[] {
  if (bands.length < 2 || !Number.isFinite(rms) || rms <= .00001) return zero();
  // Undo the backend's -80..0 dB encoding before shaping the spectrum.
  const clean = bands.slice(0, 512).map(v => {
    const level = Number.isFinite(v) ? Math.min(1, Math.max(0, v)) : 0;
    return level === 0 ? 0 : Math.pow(10, (level - 1) * 4);
  });
  const peak = Math.max(...clean);
  if (!peak) return zero();
  // Relative spectral shape retains peaks; actual channel RMS sets its height.
  // This fixed monotonic transfer never boosts quiet audio to full height.
  const loudness = 1 - Math.exp(-6 * Math.sqrt(Math.min(1, rms)));
  return Array.from({length: POINTS}, (_, i) => {
    // Backend bands are logarithmic; apply only the explicit musical-range allocation.
    const t = i / (POINTS - 1);
    const index = bandPosition(t) * (clean.length - 1);
    // A wider low-frequency kernel softens quantized FFT-bin steps.
    // Blend back to the narrow kernel through the bass region.
    const bass = Math.max(0, 1 - t / .4);
    const variance = (.8 + 2.2 * bass * bass) * Math.max(.25, Math.min(3, smoothness)) ** 2;
    const radius = Math.ceil(3 * Math.sqrt(variance / 2));
    let sum = 0, weight = 0;
    for (let j = Math.max(0, Math.floor(index) - radius); j <= Math.min(clean.length - 1, Math.ceil(index) + radius); j++) {
      const w = Math.exp(-((j - index) ** 2) / variance);
      sum += clean[j] * w; weight += w;
    }
    const edge = Math.min(1, t / (16 / 511), (1 - t) / (16 / 511));
    return Math.pow(sum / weight / peak, .65) * loudness * Math.sin(edge * Math.PI / 2);
  });
}
export function ease(current: number[], target: number[], seconds: number, gentle = false, responsiveness = 1): number[] {
  return current.map((v, i) => {
    const goal = target[i] ?? 0;
    const alpha = 1 - Math.exp(-Math.min(seconds, .1) * (gentle ? 1 : Math.max(.25, Math.min(3, responsiveness))) / (gentle ? (goal > v ? .38 : .85) : (goal > v ? .18 : .48)));
    const next = v + (goal - v) * alpha;
    return next < .0001 ? 0 : next;
  });
}
export function curve(values: number[], sign: 1 | -1): string {
  const points = values.map((v, i) => [i * 1000 / (values.length - 1), 120 - sign * v * 100]);
  if (points.length < 2) return 'M0,120 L1000,120';
  const f = (n: number) => n.toFixed(2);
  let d = `M${f(points[0][0])},${f(points[0][1])}`;
  for (let i = 1; i < points.length - 1; i++) {
    d += ` Q${f(points[i][0])},${f(points[i][1])} ${f((points[i][0] + points[i + 1][0]) / 2)},${f((points[i][1] + points[i + 1][1]) / 2)}`;
  }
  const last = points[points.length - 1];
  return `${d} Q${f(last[0])},${f(last[1])} ${f(last[0])},${f(last[1])}`;
}
// The dot matrix lights the same envelope on a fixed LED grid instead of
// interpolating. Each column averages its sample span; rows fill from the
// centre line outward, reaching the same 100-unit extent as the curve.
export const DOT_COLUMNS = 160, DOT_ROWS = 24;
const dotColumnX = Array.from({length: DOT_COLUMNS}, (_, c) => ((c + .5) * 1000 / DOT_COLUMNS).toFixed(1));
const dotRowOffset = Array.from({length: DOT_ROWS}, (_, r) => (r + 1) * 100 / DOT_ROWS);
const dotUpY = dotRowOffset.map(o => (120 - o).toFixed(1));
const dotDownY = dotRowOffset.map(o => (120 + o).toFixed(1));
export function dots(values: number[], sign: 1 | -1): string {
  if (values.length < 2) return '';
  const span = values.length / DOT_COLUMNS;
  const rowY = sign === 1 ? dotUpY : dotDownY;
  let d = '';
  for (let c = 0; c < DOT_COLUMNS; c++) {
    const from = Math.floor(c * span), to = Math.min(values.length, Math.max(from + 1, Math.ceil((c + 1) * span)));
    let sum = 0;
    for (let i = from; i < to; i++) sum += values[i];
    const height = sum / (to - from) * 100;
    // A tiny stroked segment with a round cap renders each cell as one dot.
    for (let r = 0; r < DOT_ROWS; r++) if (dotRowOffset[r] <= height) d += `M${dotColumnX[c]},${rowY[r]}l.01,0`;
  }
  return d;
}
