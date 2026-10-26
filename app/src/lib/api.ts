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

/** `audioId` comes from `loadReel`. The save fails if the reel's audio differs. */
export const saveMarkers = (root: string, index: number, markers: number[], audioId: string) =>
  invoke<{ slot: Slot; audioId: string }>("save_markers", { root, index, markers, audioId });
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
