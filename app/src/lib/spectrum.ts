// Spectrogram for the waveform view: level by time and frequency, with
// frequency on a log scale from 20 Hz to 20 kHz.

import { SAMPLE_RATE } from "./api";

const SIZE = 2048;
const LOW_HZ = 20;
const HIGH_HZ = 20_000;
/** Levels map onto 0 to 1 between these, in dB. */
const FLOOR_DB = -100;
const CEIL_DB = -20;

const hann = Float32Array.from({ length: SIZE }, (_, i) => 0.5 - 0.5 * Math.cos((2 * Math.PI * i) / SIZE));
const cos = Float32Array.from({ length: SIZE / 2 }, (_, i) => Math.cos((-2 * Math.PI * i) / SIZE));
const sin = Float32Array.from({ length: SIZE / 2 }, (_, i) => Math.sin((-2 * Math.PI * i) / SIZE));
const bits = Math.log2(SIZE);
const reversed = Uint32Array.from({ length: SIZE }, (_, i) => {
  let r = 0;
  for (let b = 0; b < bits; b++) r |= ((i >> b) & 1) << (bits - 1 - b);
  return r;
});

/** In-place radix-2 FFT of SIZE points. */
function fft(re: Float32Array, im: Float32Array) {
  for (let i = 0; i < SIZE; i++) {
    const j = reversed[i];
    if (j > i) {
      [re[i], re[j]] = [re[j], re[i]];
      [im[i], im[j]] = [im[j], im[i]];
    }
  }
  for (let len = 2; len <= SIZE; len *= 2) {
    const half = len / 2;
    const step = SIZE / len;
    for (let i = 0; i < SIZE; i += len) {
      for (let k = 0; k < half; k++) {
        const wr = cos[k * step];
        const wi = sin[k * step];
        const a = i + k;
        const b = a + half;
        const tr = re[b] * wr - im[b] * wi;
        const ti = re[b] * wi + im[b] * wr;
        re[b] = re[a] - tr;
        im[b] = im[a] - ti;
        re[a] += tr;
        im[a] += ti;
      }
    }
  }
}

/**
 * Levels from 0 to 1 for `columns` slices of frames `start..end`, `rows`
 * frequency bands each, row 0 the lowest. Returned column by column.
 */
export function spectrogram(
  left: Float32Array,
  right: Float32Array,
  start: number,
  end: number,
  columns: number,
  rows: number,
): Float32Array {
  const out = new Float32Array(columns * rows);
  const re = new Float32Array(SIZE);
  const im = new Float32Array(SIZE);
  const mag = new Float32Array(SIZE / 2);
  // Each row covers a band of FFT bins; take the loudest bin in it.
  const edges = Array.from({ length: rows + 1 }, (_, r) => {
    const hz = LOW_HZ * (HIGH_HZ / LOW_HZ) ** (r / rows);
    return Math.min(SIZE / 2 - 1, Math.max(1, Math.round((hz * SIZE) / SAMPLE_RATE)));
  });
  const frames = left.length;
  // The window's gain and the FFT size, so a full-scale sine reads near 0 dB.
  const scale = 4 / SIZE;
  for (let c = 0; c < columns; c++) {
    const centre = Math.round(start + ((c + 0.5) * (end - start)) / columns);
    const from = centre - SIZE / 2;
    for (let i = 0; i < SIZE; i++) {
      const f = from + i;
      re[i] = f >= 0 && f < frames ? (left[f] + right[f]) * 0.5 * hann[i] : 0;
      im[i] = 0;
    }
    fft(re, im);
    for (let k = 0; k < SIZE / 2; k++) mag[k] = Math.hypot(re[k], im[k]) * scale;
    for (let r = 0; r < rows; r++) {
      let peak = 0;
      for (let k = edges[r]; k <= Math.max(edges[r], edges[r + 1] - 1); k++) peak = Math.max(peak, mag[k]);
      const db = 20 * Math.log10(peak + 1e-12);
      out[c * rows + r] = Math.min(1, Math.max(0, (db - FLOOR_DB) / (CEIL_DB - FLOOR_DB)));
    }
  }
  return out;
}
