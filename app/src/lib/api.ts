// Typed wrappers for the Rust commands in src-tauri/src/lib.rs.
import { invoke } from "@tauri-apps/api/core";

export const SAMPLE_RATE = 48_000;
/** 2.9 minutes. */
export const MAX_FRAMES = 8_352_000;
/** 299 markers make 300 splices. */
export const MAX_MARKERS = 299;

export type Card = { root: string; label: string };

export type Slot = {
  index: number;
  fileName: string;
  present: boolean;
  frames: number;
  seconds: number;
  markers: number[];
  formatOk: boolean;
  problems: string[];
};

export type LoadedReel = { markers: number[]; audioId: string; left: Float32Array; right: Float32Array };

export type Source = { path: string; name: string; frames: number; error: string | null };

export type SourceSpec = { path: string; start: number | null; end: number | null };

export type Target = { kind: "slot"; root: string; index: number } | { kind: "file"; path: string };

export type BuildResult = { frames: number; markers: number; writtenTo: string };

export type OptionEntry = {
  key: string;
  value: string;
  comment: string;
  name: string | null;
  choices: { value: string; label: string }[] | null;
  ratio: boolean;
};

export type Options = {
  exists: boolean;
  firmware: number | null;
  text: string;
  entries: OptionEntry[];
};

export const listCards = () => invoke<Card[]>("list_cards");
export const listSlots = (root: string) => invoke<Slot[]>("list_slots", { root });
export const swapSlots = (root: string, a: number, b: number) =>
  invoke<void>("swap_slots", { root, a, b });
export const moveSlot = (root: string, from: number, to: number) =>
  invoke<void>("move_slot", { root, from, to });
export const clearSlot = (root: string, index: number) =>
  invoke<void>("clear_slot", { root, index });
export const backupsFolder = (root: string) => invoke<string>("backups_folder", { root });
export const backupCard = (root: string) => invoke<string>("backup_card", { root });
export const restoreCard = (root: string, from: string) =>
  invoke<void>("restore_card", { root, from });

export async function loadReel(root: string, index: number): Promise<LoadedReel> {
  const buffer = await invoke<ArrayBuffer>("load_reel", { root, index });
  const view = new DataView(buffer);
  const count = view.getUint32(0, true);
  const audioId = view.getBigUint64(4, true).toString();
  const markers = Array.from({ length: count }, (_, i) => view.getUint32(12 + i * 4, true));
  // The header is a whole number of u32s, so the samples are 4-byte aligned.
  const offset = 12 + count * 4;
  const frames = (buffer.byteLength - offset) / 8;
  return {
    markers,
    audioId,
    left: new Float32Array(buffer, offset, frames),
    right: new Float32Array(buffer, offset + frames * 4, frames),
  };
}

/** Write the open reel and its markers to the card. */
export const saveReel = (root: string, index: number, markers: number[]) =>
  invoke<Slot>("save_reel", { root, index, markers });

/** What to do to the selected splices. */
export type EditOp =
  | { kind: "delete" }
  | { kind: "duplicate" }
  | { kind: "moveTo"; to: number }
  | { kind: "shuffle"; seed: number }
  | { kind: "reverse" }
  | { kind: "gain"; db: number }
  | { kind: "normalize"; db: number }
  | { kind: "matchLevels" }
  | { kind: "fadeIn"; ms: number }
  | { kind: "fadeOut"; ms: number }
  | { kind: "smoothLoop"; ms: number }
  | { kind: "removeDc" }
  | { kind: "highPass"; hz: number }
  | { kind: "speed"; semitones: number };

/** At `start`, drop `removed` frames and put these samples in. */
export type Span = { start: number; removed: number; left: Float32Array; right: Float32Array };

export type EditResult = {
  /** History position after the edit, and the oldest one undo can reach. */
  pos: number;
  oldest: number;
  /** Id of the audio now; never reused, unlike positions. */
  state: number;
  /** The position holding the audio on the card, or null if undo can't reach it. */
  savedPos: number | null;
  /** New markers, or null when the caller already knows them (undo, redo). */
  markers: number[] | null;
  spans: Span[];
};

export function parseEdit(buffer: ArrayBuffer): EditResult {
  const view = new DataView(buffer);
  let at = 0;
  const u32 = () => {
    const v = view.getUint32(at, true);
    at += 4;
    return v;
  };
  const pos = u32();
  const oldest = u32();
  const state = u32();
  const saved = u32();
  const hasMarkers = u32() === 1;
  const markers = Array.from({ length: u32() }, u32);
  const heads = Array.from({ length: u32() }, () => ({ start: u32(), removed: u32(), inserted: u32() }));
  const spans = heads.map((h) => {
    // Copy out of the reply, so the spans don't keep the whole buffer alive.
    const left = new Float32Array(buffer.slice(at, at + h.inserted * 4));
    at += h.inserted * 4;
    const right = new Float32Array(buffer.slice(at, at + h.inserted * 4));
    at += h.inserted * 4;
    return { start: h.start, removed: h.removed, left, right };
  });
  return { pos, oldest, state, savedPos: saved === 0xffffffff ? null : saved, markers: hasMarkers ? markers : null, spans };
}

/** Apply spans in order to copies of the channels. */
export function applySpans(left: Float32Array, right: Float32Array, spans: Span[]) {
  for (const s of spans) {
    const join = (old: Float32Array, inserted: Float32Array) => {
      const out = new Float32Array(old.length - s.removed + inserted.length);
      out.set(old.subarray(0, s.start), 0);
      out.set(inserted, s.start);
      out.set(old.subarray(s.start + s.removed), s.start + inserted.length);
      return out;
    };
    left = join(left, s.left);
    right = join(right, s.right);
  }
  return { left, right };
}

export const editReel = async (
  root: string,
  index: number,
  markers: number[],
  first: number,
  last: number,
  op: EditOp,
) => parseEdit(await invoke<ArrayBuffer>("edit_reel", { root, index, markers, first, last, op }));
export const copySplices = (root: string, index: number, markers: number[], first: number, last: number) =>
  invoke<number>("copy_splices", { root, index, markers, first, last });
export const pasteSplices = async (root: string, index: number, markers: number[], at: number) =>
  parseEdit(await invoke<ArrayBuffer>("paste_splices", { root, index, markers, at }));
export const insertFiles = async (root: string, index: number, markers: number[], at: number, paths: string[]) =>
  parseEdit(await invoke<ArrayBuffer>("insert_files", { root, index, markers, at, paths }));
/** Replace splice `splice` with a file, or mix it in when `sos` is set (0 file only, 1 original only). */
export const putFile = async (
  root: string,
  index: number,
  markers: number[],
  splice: number,
  path: string,
  sos: number | null,
) => parseEdit(await invoke<ArrayBuffer>("put_file", { root, index, markers, splice, path, sos }));
export const exportSplices = (
  root: string,
  index: number,
  markers: number[],
  first: number,
  last: number,
  dir: string,
) => invoke<string[]>("export_splices", { root, index, markers, first, last, dir });
/** Undo or redo to `pos`, which must still hold audio state `state`. */
export const seekHistory = async (root: string, index: number, pos: number, state: number) =>
  parseEdit(await invoke<ArrayBuffer>("seek_history", { root, index, pos, state }));
export type SliceMode = "phrases" | "gestures";
export const autoSlice = (
  root: string,
  index: number,
  mode: SliceMode,
  minSeconds: number,
  maxSeconds: number | null,
  maxMarkers: number,
  keep: number[],
) => invoke<number[]>("auto_slice", { root, index, mode, minSeconds, maxSeconds, maxMarkers, keep });
export const probeSources = (paths: string[]) => invoke<Source[]>("probe_sources", { paths });
export const buildReel = (
  sources: SourceSpec[],
  normalize: boolean,
  fadeMs: number,
  target: Target,
) => invoke<BuildResult>("build_reel", { sources, normalize, fadeMs, target });
export const readOptions = (root: string) => invoke<Options>("read_options", { root });
export const writeOptions = (root: string, changes: { key: string; value: string }[]) =>
  invoke<void>("write_options", { root, changes });
export const writeOptionsText = (root: string, text: string) =>
  invoke<void>("write_options_text", { root, text });

/** `m:ss.s` */
export function formatTime(frames: number): string {
  const seconds = frames / SAMPLE_RATE;
  const m = Math.floor(seconds / 60);
  const s = seconds - m * 60;
  return `${m}:${s.toFixed(1).padStart(4, "0")}`;
}

export function errorText(e: unknown): string {
  return typeof e === "string" ? e : e instanceof Error ? e.message : JSON.stringify(e);
}
