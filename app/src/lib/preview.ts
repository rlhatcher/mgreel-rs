// Playback that behaves like the module. In splice mode the selected splice
// loops, and a new selection (Organize or Shift) starts when the current
// splice reaches its end. Vari-Speed plays faster or slower, either way.

import { SAMPLE_RATE } from "./api";

export type Range = { start: number; end: number };
export type PreviewMode = "reel" | "splices";

/** The manual gives Vari-Speed about 12 semitones up and 26 down. */
export const SPEED_UP = 12;
export const SPEED_DOWN = -26;

/** How far ahead the next splice is queued, in seconds. */
const LOOKAHEAD = 0.2;
const TICK_MS = 25;

type Voice = {
  source: AudioBufferSourceNode;
  /** Context time the voice starts and stops. */
  when: number;
  until: number;
  /** Reel frame at `when`, and the direction it moves in. */
  from: number;
  dir: 1 | -1;
  rate: number;
  splice: number | null;
};

export class Preview {
  private ctx: AudioContext | null = null;
  private gain: GainNode | null = null;
  private forward: AudioBuffer | null = null;
  private backward: AudioBuffer | null = null;
  private voices: Voice[] = [];
  private timer: ReturnType<typeof setInterval> | undefined;
  private mode: PreviewMode = "reel";
  private volume = 1;

  /** Semitones from 1/1, and whether playback runs backwards. */
  semitones = 0;
  reverse = false;

  constructor(
    private left: Float32Array,
    private right: Float32Array,
    /** Read each time a splice is queued, so marker edits apply while playing. */
    private splices: () => Range[],
    /** Called each time a splice ends, to choose the next one. */
    private next: (ended: number) => number,
    private onended: () => void,
  ) {}

  get frames() {
    return this.left.length;
  }

  get playing() {
    return this.voices.length > 0;
  }

  setVolume(gain: number) {
    this.volume = gain;
    if (this.gain) this.gain.gain.value = gain;
  }

  /** Play from `from`. In splice mode, play `splice` from there, then loop or move on. */
  play(mode: PreviewMode, from: number, splice: number) {
    this.stop();
    this.setup();
    this.mode = mode;
    const now = this.ctx!.currentTime + 0.02;
    if (mode === "reel") {
      this.voice(from, this.reverse ? 0 : this.frames, null, now);
    } else {
      const r = this.splices()[splice];
      if (!r) return;
      const inside = from >= r.start && from < r.end;
      const start = inside ? from : this.reverse ? r.end : r.start;
      this.voice(start, this.reverse ? r.start : r.end, splice, now);
      this.timer = setInterval(() => this.tick(), TICK_MS);
    }
  }

  stop() {
    clearInterval(this.timer);
    this.timer = undefined;
    for (const v of this.voices) {
      v.source.onended = null;
      try {
        v.source.stop();
      } catch {
        // Not started yet or already stopped.
      }
      v.source.disconnect();
    }
    this.voices = [];
  }

  /** Where playback is now, or null when stopped. */
  position(): { frame: number; splice: number | null } | null {
    if (!this.ctx || !this.voices.length) return null;
    const now = this.ctx.currentTime;
    const v = this.voices.findLast((v) => v.when <= now) ?? this.voices[0];
    const elapsed = Math.max(0, Math.min(now, v.until) - v.when);
    const frame = v.from + v.dir * elapsed * SAMPLE_RATE * v.rate;
    return { frame: Math.max(0, Math.min(this.frames, Math.round(frame))), splice: v.splice };
  }

  close() {
    this.stop();
    this.ctx?.close();
    this.ctx = null;
  }

  private setup() {
    if (!this.ctx) {
      this.ctx = new AudioContext({ sampleRate: SAMPLE_RATE });
      this.gain = this.ctx.createGain();
      this.gain.gain.value = this.volume;
      this.gain.connect(this.ctx.destination);
    }
    if (!this.forward) this.forward = this.buffer(this.left, this.right);
    if (this.reverse && !this.backward) {
      this.backward = this.buffer(this.left.slice().reverse(), this.right.slice().reverse());
    }
  }

  private buffer(left: Float32Array, right: Float32Array) {
    const buf = this.ctx!.createBuffer(2, Math.max(1, this.frames), SAMPLE_RATE);
    buf.copyToChannel(left as Float32Array<ArrayBuffer>, 0);
    buf.copyToChannel(right as Float32Array<ArrayBuffer>, 1);
    return buf;
  }

  /** Queue playback from frame `a` towards frame `b` at context time `when`. */
  private voice(a: number, b: number, splice: number | null, when: number) {
    const ctx = this.ctx!;
    const dir = this.reverse ? -1 : 1;
    const rate = 2 ** (this.semitones / 12);
    // Reverse plays the mirrored buffer, where frame f sits at frames - f.
    const offset = (this.reverse ? this.frames - a : a) / SAMPLE_RATE;
    // A zero-length splice still takes one frame, so the queue moves on.
    const length = Math.max(1, Math.abs(b - a)) / SAMPLE_RATE;
    const source = ctx.createBufferSource();
    source.buffer = this.reverse ? this.backward : this.forward;
    source.playbackRate.value = rate;
    source.connect(this.gain!);
    source.start(when, offset, length);
    const v: Voice = { source, when, until: when + length / rate, from: a, dir, rate, splice };
    source.onended = () => {
      this.voices = this.voices.filter((x) => x !== v);
      source.disconnect();
      if (this.mode === "reel" && !this.voices.length) {
        this.stop();
        this.onended();
      }
    };
    this.voices.push(v);
  }

  private tick() {
    const splices = this.splices();
    const last = this.voices.at(-1);
    if (!this.ctx || !last || last.splice === null) return;
    if (last.until - this.ctx.currentTime > LOOKAHEAD) return;
    if (!splices.length) return;
    const i = Math.max(0, Math.min(splices.length - 1, this.next(last.splice)));
    const r = splices[i];
    if (this.reverse) this.voice(r.end, r.start, i, last.until);
    else this.voice(r.start, r.end, i, last.until);
  }
}
