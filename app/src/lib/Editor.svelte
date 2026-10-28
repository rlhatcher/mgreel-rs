<script lang="ts">
  import { onDestroy, onMount, untrack } from "svelte";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { ask, open as openDialog } from "@tauri-apps/plugin-dialog";
  import Thumb from "./Thumb.svelte";
  import Waveform from "./Waveform.svelte";
  import {
    MARKER_FILE_EXTENSIONS,
    MAX_FRAMES,
    MAX_MARKERS,
    SAMPLE_RATE,
    applySpans,
    autoSlice,
    copySplices,
    deleteSplices,
    editReel,
    errorText,
    exportSplices,
    insertFiles,
    formatTime,
    loadReel,
    mergeShortSplices,
    pasteSplices,
    putFile,
    readMarkerFile,
    saveReel,
    seekHistory,
    silentSplices,
    type EditOp,
    type EditResult,
    type LoadedReel,
    type SliceMode,
    type Slot,
  } from "./api";
  import { Preview, SPEED_DOWN, SPEED_UP, type PreviewMode } from "./preview";

  type Props = {
    root: string;
    slot: Slot;
    onsaved: (slot: Slot) => void;
    notify: (text: string, error?: boolean) => void;
    dirty: boolean;
  };
  let { root, slot, onsaved, notify, dirty = $bindable() }: Props = $props();

  /** Organize spans 0 to 5 V and spreads the splices evenly over it. */
  const ORGANIZE_VOLTS = 5;
  /** How far a new marker may move to land on a zero crossing: 2 ms. */
  const SNAP_FRAMES = 96;
  const HISTORY = 200;

  let reel = $state<LoadedReel | null>(null);
  let loadError = $state("");
  let markers = $state<number[]>([]);
  let saved = $state<number[]>([]);
  /** Frames of markers that auto-slice, split and delete leave alone. */
  let locked = $state(new Set<number>());
  let cursor = $state(0);
  let playhead = $state<number | null>(null);
  let selectedMarker = $state<number | null>(null);
  let saving = $state(false);
  let snap = $state(true);
  let splitCount = $state(8);
  /** Where the reel's audio starts on a marker file's timeline, in seconds. */
  let importOffset = $state(0);
  let importReplace = $state(false);
  type ToolTab = "edit" | "markers" | "add" | "tidy";
  const TOOL_TABS: { id: ToolTab; label: string }[] = [
    { id: "edit", label: "Edit" },
    { id: "markers", label: "Markers" },
    { id: "add", label: "Add audio" },
    { id: "tidy", label: "Tidy" },
  ];
  const TOOL_KEY = "mgreel.editorTools";
  let toolTab = $state<ToolTab>(readToolTab());

  function readToolTab(): ToolTab {
    try {
      const v = localStorage.getItem(TOOL_KEY);
      if (TOOL_TABS.some((t) => t.id === v)) return v as ToolTab;
    } catch {
      // Storage can be off; the first tab will do.
    }
    return "edit";
  }

  function showTools(tab: ToolTab) {
    toolTab = tab;
    try {
      localStorage.setItem(TOOL_KEY, tab);
    } catch {
      // Only a convenience.
    }
  }

  /** Tidy: a sample below this level counts as silence. */
  let silenceDb = $state(-60);
  let shortestMs = $state(50);
  let sliceMode = $state<SliceMode>("phrases");
  let sliceMin = $state(4);
  /** Empty means no limit. */
  let sliceLongest = $state<number | null>(15);
  let sliceMax = $state(MAX_MARKERS + 1);
  let slicing = $state(false);
  let waveform = $state<Waveform | undefined>();

  // Audio edits happen in the app's Rust side. These track its history.
  let audioPos = $state(0);
  let oldestPos = $state(0);
  /** Id of the current audio, from the Rust side. Positions get reused
   *  when an edit follows an undo; state ids never do. */
  let audioState = $state(0);
  /** Id of the audio on the card. */
  let savedState = $state(0);
  /** The history position holding the card's audio, or null if undo can't reach it. */
  let savedAt = $state<number | null>(0);
  /** A command that changes audio is running. */
  let busy = $state(false);
  /** Splices picked in the list. Null means the one under the cursor. */
  let picked = $state<{ first: number; last: number } | null>(null);
  let anchor = 0;
  // Rows move with pointer events, not HTML5 drag and drop, because the
  // Tauri window takes drag events for its own file drop.
  /** A press on a splice row. It turns into a drag once the pointer moves. */
  let press: { index: number; y: number } | null = null;
  let dragFrom = $state<number | null>(null);
  let dropTarget = $state<number | null>(null);
  /** Set when a drag ends, so the click that follows doesn't pick a row. */
  let dragged = false;
  let list = $state<HTMLDivElement>();
  let gainDb = $state(-3);
  let normalizeDb = $state(-1);
  let fadeMs = $state(10);
  let loopMs = $state(20);
  let highPassHz = $state(40);
  let speedSt = $state(-12);
  let spectrum = $state(false);
  /** Sound On Sound balance for mixing a file in, in percent: 0 is only the file. */
  let sos = $state(50);
  let dropping = $state(false);

  const frames = $derived(reel?.left.length ?? 0);
  const splices = $derived(
    [0, ...markers].map((start, i) => ({ start, end: markers[i] ?? frames })),
  );
  const current = $derived(spliceAt(cursor));
  const selection = $derived(
    picked && picked.last < splices.length ? picked : { first: current, last: current },
  );
  const tooMany = $derived(markers.length > MAX_MARKERS);
  const pickedLabel = $derived(
    selection.last > selection.first
      ? `Splices ${selection.first + 1}–${selection.last + 1}`
      : `Splice ${selection.first + 1}`,
  );
  const tooLong = $derived(frames > MAX_FRAMES);
  const jumps = $derived(splices.map(loopJumps));
  const sameMarkers = (a: number[], b: number[]) => a.length === b.length && a.every((m, i) => m === b[i]);

  $effect(() => {
    dirty = audioState !== savedState || !sameMarkers(markers, saved);
  });

  $effect(() => {
    // Moving the cursor outside the picked splices drops the pick.
    const c = current;
    untrack(() => {
      if (picked && (c < picked.first || c > picked.last)) picked = null;
    });
  });

  /** True if a splice jumps hard from its last sample back to its first. */
  function loopJumps(s: { start: number; end: number }) {
    if (!reel || s.end - s.start < 64) return false;
    const { left, right } = reel;
    const n = 16;
    let local = 0;
    for (const ch of [left, right]) {
      for (let k = 1; k <= n; k++) {
        local += Math.abs(ch[s.start + k] - ch[s.start + k - 1]);
        local += Math.abs(ch[s.end - k] - ch[s.end - k - 1]);
      }
    }
    local /= 4 * n;
    const jump = Math.max(
      Math.abs(left[s.end - 1] - left[s.start]),
      Math.abs(right[s.end - 1] - right[s.start]),
    );
    // Noisy sound hides a jump, so compare it to the sample-to-sample
    // movement around the loop point.
    return jump > 0.02 && jump > 4 * local;
  }

  function spliceAt(frame: number) {
    const i = splices.findIndex((s) => frame >= s.start && frame < s.end);
    return i < 0 ? splices.length - 1 : i;
  }

  /** The Organize voltage range that selects splice `i`. */
  function organizeRange(i: number) {
    const step = ORGANIZE_VOLTS / splices.length;
    return `${(i * step).toFixed(2)}–${((i + 1) * step).toFixed(2)} V`;
  }

  // Undo. Each step stores the markers, locks and audio history position
  // from before a change.
  type Snapshot = { markers: number[]; locked: number[]; audio: number; state: number };
  let undoStack = $state<Snapshot[]>([]);
  let redoStack = $state<Snapshot[]>([]);

  const snapshot = (): Snapshot => ({
    markers: markers.slice(),
    locked: [...locked],
    audio: audioPos,
    state: audioState,
  });
  const same = (a: Snapshot, b: Snapshot) =>
    a.state === b.state &&
    sameMarkers(a.markers, b.markers) &&
    a.locked.length === b.locked.length &&
    a.locked.every((m) => b.locked.includes(m));

  function pushUndo(s: Snapshot) {
    undoStack = [...undoStack.slice(1 - HISTORY), s];
    redoStack = [];
  }

  /** Call before every change to markers or locks. */
  function record() {
    pushUndo(snapshot());
  }

  async function restore(s: Snapshot) {
    if (s.state !== audioState) {
      if (s.audio < oldestPos) {
        throw new Error("Older audio edits were dropped to save memory, so they can't be undone.");
      }
      applyAudio(await seekHistory(root, slot.index, s.audio, s.state));
    }
    markers = s.markers.slice();
    locked = new Set(s.locked);
    selectedMarker = null;
    picked = null;
  }

  async function undo() {
    if (busy || saving) return;
    const now = snapshot();
    while (undoStack.length) {
      const s = undoStack[undoStack.length - 1];
      // A drag that ended where it started leaves a step with no change.
      if (same(s, now)) {
        undoStack = undoStack.slice(0, -1);
        continue;
      }
      busy = true;
      try {
        await restore(s);
        undoStack = undoStack.slice(0, -1);
        redoStack = [...redoStack, now];
      } catch (e) {
        // A step that can't be restored never will be, so drop it rather
        // than leave Undo stuck on it.
        undoStack = undoStack.slice(0, -1);
        notify(`Can't undo that step: ${errorText(e)}`, true);
      } finally {
        busy = false;
      }
      return;
    }
  }

  async function redo() {
    const s = redoStack[redoStack.length - 1];
    if (!s || busy || saving) return;
    const now = snapshot();
    busy = true;
    try {
      await restore(s);
      redoStack = redoStack.slice(0, -1);
      undoStack = [...undoStack, now];
    } catch (e) {
      redoStack = redoStack.slice(0, -1);
      notify(`Can't redo that step: ${errorText(e)}`, true);
    } finally {
      busy = false;
    }
  }

  /** Take in the audio side of an edit, undo or redo. */
  function applyAudio(r: EditResult) {
    audioPos = r.pos;
    oldestPos = r.oldest;
    audioState = r.state;
    savedAt = r.savedPos;
    if (!r.spans.length || !reel) return;
    stop();
    const { left, right } = applySpans(reel.left, reel.right, r.spans);
    reel = { ...reel, left, right };
    cursor = Math.min(cursor, Math.max(0, left.length - 1));
    preview?.close();
    preview = makePreview(reel);
  }

  /**
   * Locks move with their audio. When an edit keeps the splices in order
   * and their count the same, each lock follows its marker by index.
   * Otherwise locks after the edit shift and those inside it go.
   */
  function shiftLocks(r: EditResult, next: number[], keepOrder: boolean) {
    if (keepOrder && next.length === markers.length) {
      return new Set(markers.flatMap((m, i) => (locked.has(m) ? [next[i]] : [])));
    }
    const s = r.spans[0];
    if (!s) return locked;
    const delta = s.left.length - s.removed;
    const moved = [...locked].flatMap((m) =>
      m <= s.start ? [m] : m >= s.start + s.removed ? [m + delta] : [],
    );
    return new Set(moved.filter((m) => next.includes(m)));
  }

  /**
   * Run a command that edits the open reel, then take in its result.
   * `after` gets how many splices were added and returns what to pick;
   * the cursor moves to the start of splice `focus` or the pick.
   * `mapLocks`, when given, says where the locks go instead of
   * `shiftLocks`.
   */
  async function edit(
    call: () => Promise<EditResult>,
    after: (added: number) => { first: number; last: number } | null,
    focus: number,
    keepOrder = false,
    mapLocks?: (next: number[]) => Set<number>,
  ) {
    if (busy || saving || !reel) return;
    busy = true;
    try {
      const before = snapshot();
      const r = await call();
      const next = r.markers ?? markers;
      if (r.state === audioState && sameMarkers(next, markers)) {
        notify("That made no change.");
        return;
      }
      const added = next.length - markers.length;
      // A new audio edit replaces the history past this position, so undo
      // steps that point there (left by Revert) can't be restored.
      if (r.state !== audioState) undoStack = undoStack.filter((u) => u.audio <= before.audio);
      pushUndo(before);
      locked = mapLocks ? mapLocks(next) : shiftLocks(r, next, keepOrder);
      applyAudio(r);
      markers = next;
      selectedMarker = null;
      picked = after(added);
      const starts = [0, ...markers];
      cursor = starts[Math.min(picked?.first ?? focus, starts.length - 1)];
    } catch (e) {
      notify(errorText(e), true);
    } finally {
      busy = false;
    }
  }

  /** Run an edit on the picked splices. */
  function runEdit(op: EditOp) {
    const { first, last } = selection;
    const n = last - first + 1;
    return edit(
      () => editReel(root, slot.index, markers, first, last, op),
      () => {
        if (op.kind === "moveTo") {
          const at = op.to < first ? op.to : op.to - n;
          return { first: at, last: at + n - 1 };
        }
        if (op.kind === "delete") return null;
        if (op.kind === "duplicate") return { first: last + 1, last: last + n };
        return { first, last };
      },
      first,
      !["moveTo", "shuffle", "delete", "duplicate"].includes(op.kind),
    );
  }

  async function copy() {
    const { first, last } = selection;
    try {
      const n = await copySplices(root, slot.index, markers, first, last);
      notify(`Copied ${n} splice${n === 1 ? "" : "s"}. Paste them into this reel or another one.`);
      return true;
    } catch (e) {
      notify(errorText(e), true);
      return false;
    }
  }

  async function cut() {
    if (await copy()) await runEdit({ kind: "delete" });
  }

  /** New splices go after the picked ones. */
  function paste() {
    const at = selection.last + 1;
    return edit(
      () => pasteSplices(root, slot.index, markers, at),
      (added) => ({ first: at, last: at + added - 1 }),
      at,
    );
  }

  const isWav = (p: string) => /\.wave?$/i.test(p);

  async function pickWavs(multiple: boolean) {
    const chosen = await openDialog({ multiple, filters: [{ name: "WAV audio", extensions: ["wav", "wave"] }] });
    if (!chosen) return [];
    return Array.isArray(chosen) ? chosen : [chosen];
  }

  async function addFiles(paths?: string[]) {
    paths ??= await pickWavs(true);
    const wavs = paths.filter(isWav);
    if (wavs.length < paths.length) notify("Skipped files that aren't .wav", true);
    if (!wavs.length) return;
    const at = selection.last + 1;
    await edit(
      () => insertFiles(root, slot.index, markers, at, wavs),
      (added) => ({ first: at, last: at + added - 1 }),
      at,
    );
  }

  /** Replace the picked splice with a file, or mix one in at the S.O.S. balance. */
  async function fileIntoSplice(mix: boolean) {
    const [path] = await pickWavs(false);
    if (!path) return;
    const i = selection.first;
    await edit(
      () => putFile(root, slot.index, markers, i, path, mix ? sos / 100 : null),
      () => ({ first: i, last: i }),
      i,
    );
  }

  async function exportPicked() {
    const dir = await openDialog({ directory: true, title: "Export splices to a folder" });
    if (typeof dir !== "string") return;
    const { first, last } = selection;
    try {
      const written = await exportSplices(root, slot.index, markers, first, last, dir);
      notify(`Exported ${written.length} splice${written.length === 1 ? "" : "s"} to ${dir}`);
    } catch (e) {
      notify(errorText(e), true);
    }
  }

  function pick(i: number, extend: boolean) {
    if (extend) {
      picked = { first: Math.min(anchor, i), last: Math.max(anchor, i) };
      return;
    }
    anchor = i;
    picked = { first: i, last: i };
    selectSplice(i);
  }

  /** The splices a drag moves: the pick if it started there, else one row. */
  const dragging = $derived(
    dragFrom === null
      ? null
      : dragFrom >= selection.first && dragFrom <= selection.last
        ? selection
        : { first: dragFrom, last: dragFrom },
  );

  function dropOn(target: number) {
    const sel = dragging;
    dragFrom = null;
    if (!sel) return;
    const to = target < sel.first ? target : target > sel.last ? target + 1 : null;
    if (to === null) return;
    picked = sel;
    runEdit({ kind: "moveTo", to });
  }

  let pointer = { x: 0, y: 0 };
  let scrollFrame = 0;

  function findTarget() {
    const row = document.elementFromPoint(pointer.x, pointer.y)?.closest<HTMLElement>("tr[data-splice]");
    dropTarget = row ? Number(row.dataset.splice) : null;
  }

  // While the pointer stays near the top or bottom edge, scroll the list a
  // step each frame to reach rows out of view.
  function edgeScroll() {
    scrollFrame = 0;
    if (dragFrom === null || !list) return;
    const box = list.getBoundingClientRect();
    const step = pointer.y < box.top + 24 ? -12 : pointer.y > box.bottom - 24 ? 12 : 0;
    if (step === 0) return;
    list.scrollTop += step;
    findTarget();
    scrollFrame = requestAnimationFrame(edgeScroll);
  }

  function onPointerMove(e: PointerEvent) {
    if (!press) return;
    // The button came up where the window couldn't see it, as outside it.
    if (e.buttons === 0) {
      cancelDrag();
      return;
    }
    if (dragFrom === null) {
      if (Math.abs(e.clientY - press.y) < 4) return;
      dragFrom = press.index;
    }
    pointer = { x: e.clientX, y: e.clientY };
    findTarget();
    if (!scrollFrame) edgeScroll();
  }

  function onPointerUp() {
    if (!press) return;
    if (dragFrom !== null) {
      dragged = true;
      if (dropTarget !== null) dropOn(dropTarget);
    }
    cancelDrag();
  }

  function cancelDrag() {
    press = null;
    dragFrom = null;
    dropTarget = null;
    cancelAnimationFrame(scrollFrame);
    scrollFrame = 0;
  }

  // Preview
  let preview: Preview | null = null;
  let playing = $state(false);
  let playingSplice = $state<number | null>(null);
  let mode = $state<PreviewMode>("splices");
  /** The splice that plays when the current one ends, as on the module. */
  let selected = $state(0);
  let organize = $state(0);
  let semitones = $state(0);
  let reverse = $state(false);
  let volumeDb = $state(0);
  let raf = 0;

  async function load() {
    stop();
    preview?.close();
    preview = null;
    reel = null;
    loadError = "";
    try {
      const loaded = await loadReel(root, slot.index);
      reel = loaded;
      markers = loaded.markers.slice();
      saved = loaded.markers.slice();
      locked = new Set();
      undoStack = [];
      redoStack = [];
      audioPos = 0;
      oldestPos = 0;
      audioState = 0;
      savedState = 0;
      savedAt = 0;
      picked = null;
      cursor = 0;
      selectedMarker = null;
      preview = makePreview(loaded);
    } catch (e) {
      loadError = errorText(e);
    }
  }

  function makePreview(r: LoadedReel) {
    return new Preview(
      r.left,
      r.right,
      () => splices,
      () => selected,
      () => stop(),
    );
  }

  function play(from = cursor) {
    if (!preview) return;
    stop();
    preview.semitones = semitones;
    preview.reverse = reverse;
    preview.setVolume(10 ** (volumeDb / 20));
    const at = spliceAt(from);
    selected = at;
    preview.play(mode, from, at);
    playing = true;
    follow();
  }

  function follow() {
    const pos = preview?.position();
    if (!pos) return;
    playhead = pos.frame;
    // Playing through the reel has no queued splice; mark the one under it.
    playingSplice = pos.splice ?? spliceAt(pos.frame);
    waveform?.keepInView(pos.frame);
    raf = requestAnimationFrame(follow);
  }

  function stop() {
    cancelAnimationFrame(raf);
    preview?.stop();
    playing = false;
    playhead = null;
    playingSplice = null;
  }

  /** Speed and direction apply from the current position. */
  function restart() {
    if (playing) play(playhead ?? cursor);
  }

  function shift() {
    if (!splices.length) return;
    selected = (selected + 1) % splices.length;
    if (!playing) selectSplice(selected);
  }

  function setOrganize(volts: number) {
    organize = volts;
    selected = Math.min(splices.length - 1, Math.floor((volts / ORGANIZE_VOLTS) * splices.length));
    if (!playing) selectSplice(selected);
  }

  // Editing
  function zeroCrossing(at: number) {
    if (!reel) return at;
    const { left, right } = reel;
    const x = (i: number) => left[i] + right[i];
    for (let d = 0; d <= SNAP_FRAMES; d++) {
      for (const i of [at - d, at + d]) {
        if (i <= 1 || i >= frames) continue;
        const a = x(i - 1);
        const b = x(i);
        if (a === 0 || b === 0 || a < 0 !== b < 0) return Math.abs(a) < Math.abs(b) ? i - 1 : i;
      }
    }
    return at;
  }

  function addMarker(at: number) {
    at = Math.round(at);
    if (snap) at = zeroCrossing(at);
    if (at <= 0 || at >= frames || markers.includes(at)) return;
    record();
    markers = [...markers, at].sort((a, b) => a - b);
    selectedMarker = markers.indexOf(at);
  }

  function snapDragged(i: number) {
    if (!snap) return;
    const at = zeroCrossing(markers[i]);
    const lo = markers[i - 1] ?? 0;
    const hi = markers[i + 1] ?? frames;
    if (at <= lo || at >= hi || at === markers[i]) return;
    const next = markers.slice();
    next[i] = at;
    markers = next;
  }

  function removeSelected() {
    if (selectedMarker === null) return;
    if (locked.has(markers[selectedMarker])) {
      notify("That marker is locked. Unlock it (K) to delete it.", true);
      return;
    }
    record();
    markers = markers.filter((_, i) => i !== selectedMarker);
    selectedMarker = null;
  }

  function toggleLock() {
    if (selectedMarker === null) return;
    const m = markers[selectedMarker];
    record();
    const next = new Set(locked);
    if (next.has(m)) next.delete(m);
    else next.add(m);
    locked = next;
  }

  /** Put the markers from a DAW, label or WAV file on the reel. */
  async function importMarkers() {
    if (!reel) return;
    const path = await openDialog({
      filters: [{ name: "Markers", extensions: MARKER_FILE_EXTENSIONS }],
      title: "Choose an Audacity label file, REAPER marker export, Ableton Live set or WAV file",
    });
    if (typeof path !== "string") return;
    // Reading runs off the main thread, so drop the result if the markers
    // changed meanwhile, as auto-slice does.
    const started = snapshot();
    try {
      const offset = Number.isFinite(importOffset) ? importOffset : 0;
      const r = await readMarkerFile(path, offset, frames);
      if (!same(started, snapshot())) {
        notify("The markers changed while the file was read, so nothing was imported. Import it again.", true);
        return;
      }
      const keep = importReplace ? markers.filter((m) => locked.has(m)) : markers;
      const next = [...new Set([...keep, ...r.markers])].sort((a, b) => a - b);
      const name = path.split("/").pop();
      const parts = [];
      if (sameMarkers(next, markers)) {
        parts.push(`The markers in ${name} are on the reel already.`);
      } else {
        record();
        markers = next;
        selectedMarker = null;
        parts.push(`Imported ${r.markers.length} marker${r.markers.length === 1 ? "" : "s"} from ${name}. Not saved yet.`);
      }
      if (r.outside) {
        parts.push(`${r.outside} fell before the audio or after its end and ${r.outside === 1 ? "was" : "were"} left out.`);
      }
      parts.push(...r.warnings);
      notify(parts.join(" "), r.warnings.length > 0);
    } catch (e) {
      notify(errorText(e), true);
    }
  }

  /** Tidy: delete every splice that stays below the silence level. */
  function dropSilent() {
    const db = silenceDb;
    const before = markers.slice();
    let gone: number[] = [];
    return edit(
      async () => {
        gone = await silentSplices(root, slot.index, before, db);
        if (!gone.length) throw new Error(`No splice stays below ${db} dBFS.`);
        if (gone.length === before.length + 1) {
          throw new Error(`Every splice stays below ${db} dBFS, and a reel needs at least one.`);
        }
        return deleteSplices(root, slot.index, before, gone);
      },
      () => {
        notify(`Deleted ${gone.length} silent splice${gone.length === 1 ? "" : "s"}. Not saved yet.`);
        return null;
      },
      0,
      false,
      (next) => {
        // The kept splices stay in order, so each lock moves to where its
        // splice now starts. A splice that becomes the first loses its lock.
        const starts = [0, ...before];
        const nextStarts = [0, ...next];
        const kept = starts.flatMap((start, i) => (gone.includes(i) ? [] : [start]));
        return new Set(kept.flatMap((start, j) => (locked.has(start) && nextStarts[j] > 0 ? [nextStarts[j]] : [])));
      },
    );
  }

  /** Tidy: cut the silence at the start of every splice. */
  function trimStarts() {
    return edit(
      () => editReel(root, slot.index, markers, 0, splices.length - 1, { kind: "trimStart", db: silenceDb }),
      () => null,
      current,
      true,
    );
  }

  /** Tidy: join splices shorter than the shortest to a neighbour. */
  async function mergeShort() {
    if (!reel) return;
    const started = snapshot();
    try {
      const next = await mergeShortSplices(markers, frames, shortestMs, [...locked]);
      if (!same(started, snapshot())) {
        notify("The markers changed meanwhile, so nothing was merged. Merge again.", true);
        return;
      }
      if (sameMarkers(next, markers)) {
        notify(`No splice is shorter than ${shortestMs} ms, apart from any between locked markers.`);
        return;
      }
      const merged = markers.length - next.length;
      record();
      markers = next;
      selectedMarker = null;
      notify(`Merged ${merged} short splice${merged === 1 ? "" : "s"} into a neighbour. Not saved yet.`);
    } catch (e) {
      notify(errorText(e), true);
    }
  }

  function removeUnlocked() {
    record();
    markers = markers.filter((m) => locked.has(m));
    selectedMarker = null;
  }

  async function revert() {
    if (busy || saving) return;
    if (savedAt === null) {
      // An edit after undo replaced the saved audio in the history, so
      // read it back from the card. That starts a fresh undo history.
      const ok = await ask("Revert reads the reel back from the card and clears undo. Continue?", {
        title: "Revert",
        kind: "warning",
      });
      if (ok) await load();
      return;
    }
    const before = snapshot();
    busy = true;
    try {
      await restore({
        markers: saved,
        locked: [...locked].filter((m) => saved.includes(m)),
        audio: savedAt,
        state: savedState,
      });
      pushUndo(before);
    } catch (e) {
      notify(errorText(e), true);
    } finally {
      busy = false;
    }
  }

  function setSliceMode(next: SliceMode) {
    sliceMode = next;
    sliceMin = next === "phrases" ? 4 : 0.5;
    sliceLongest = next === "phrases" ? 15 : 5;
  }

  async function runAutoSlice() {
    if (busy || saving) return;
    // Busy holds off audio edits and undo while slicing. Marker edits still
    // work, so the result is dropped if anything changed in the meantime.
    const started = snapshot();
    slicing = true;
    busy = true;
    try {
      const max = Math.max(1, Math.min(MAX_MARKERS + 1, Math.floor(sliceMax)));
      const min = Math.max(0.05, sliceMin);
      const longest = sliceLongest ? Math.max(sliceLongest, 2 * min) : null;
      const found = await autoSlice(root, slot.index, sliceMode, min, longest, max - 1, started.locked);
      if (!same(started, snapshot())) {
        notify("The markers changed while auto-slice ran, so its result was dropped. Run it again.", true);
        return;
      }
      record();
      markers = found;
      selectedMarker = null;
      notify(`Found ${markers.length + 1} splices. Not saved yet: listen through, adjust, then save or revert.`);
    } catch (e) {
      notify(errorText(e), true);
    } finally {
      slicing = false;
      busy = false;
    }
  }

  /** Split into about `splitCount` equal splices, keeping locked markers. */
  function splitEvenly() {
    const n = Math.max(1, Math.min(MAX_MARKERS + 1, Math.floor(splitCount)));
    const keep = markers.filter((m) => locked.has(m));
    const bounds = [0, ...keep, frames];
    const next = [...keep];
    for (let b = 0; b + 1 < bounds.length; b++) {
      const [start, end] = [bounds[b], bounds[b + 1]];
      // Each stretch between locked markers gets its share of the splices.
      const parts = Math.max(1, Math.round((n * (end - start)) / frames));
      for (let k = 1; k < parts; k++) next.push(Math.round(start + ((end - start) * k) / parts));
    }
    record();
    markers = [...new Set(next)].filter((m) => m > 0 && m < frames).sort((a, b) => a - b);
    selectedMarker = null;
  }

  function selectSplice(i: number) {
    if (i < 0 || i >= splices.length) return;
    cursor = splices[i].start;
    selectedMarker = null;
    waveform?.reveal(cursor);
    if (playing) play();
  }

  async function save() {
    // One save at a time: two would write the same temp file on the card.
    if (!reel || saving || busy) return;
    saving = true;
    const sent = markers.slice();
    const pos = audioPos;
    const state = audioState;
    try {
      const updated = await saveReel(root, slot.index, sent);
      saved = sent;
      savedState = state;
      savedAt = pos;
      onsaved(updated);
      notify(`Saved ${sent.length + 1} splices to ${slot.fileName}`);
    } catch (e) {
      notify(errorText(e), true);
    } finally {
      saving = false;
    }
  }

  function onKey(e: KeyboardEvent) {
    const target = e.target as HTMLElement;
    if (target.closest("input, textarea, select")) return;
    if (e.metaKey || e.ctrlKey) {
      const key = e.key.toLowerCase();
      if (key === "s") {
        e.preventDefault();
        if (dirty && !tooMany && !tooLong) save();
      } else if (key === "z") {
        e.preventDefault();
        if (e.shiftKey) redo();
        else undo();
      } else if (key === "c") {
        e.preventDefault();
        copy();
      } else if (key === "x") {
        e.preventDefault();
        cut();
      } else if (key === "v") {
        e.preventDefault();
        paste();
      } else if (key === "d") {
        e.preventDefault();
        runEdit({ kind: "duplicate" });
      } else if (key === "a") {
        e.preventDefault();
        picked = { first: 0, last: splices.length - 1 };
      }
      return;
    }
    switch (e.key) {
      case " ":
        e.preventDefault();
        if (playing) stop();
        else play();
        break;
      case "Escape":
        cancelDrag();
        stop();
        break;
      case "m":
        addMarker(playhead ?? cursor);
        break;
      case "k":
        toggleLock();
        break;
      case "s":
        shift();
        break;
      case "l":
        mode = mode === "splices" ? "reel" : "splices";
        restart();
        break;
      case "Backspace":
      case "Delete":
        if (e.shiftKey) runEdit({ kind: "delete" });
        else removeSelected();
        break;
      case "ArrowLeft":
        e.preventDefault();
        selectSplice(Math.max(0, current - 1));
        break;
      case "ArrowRight":
        e.preventDefault();
        selectSplice(Math.min(splices.length - 1, current + 1));
        break;
      case "=":
      case "+":
        waveform?.zoom(2);
        break;
      case "-":
        waveform?.zoom(0.5);
        break;
      case "0":
        waveform?.fit();
        break;
    }
  }

  onMount(() => {
    load();
    // WAV files dropped on the editor go in as new splices after the pick.
    const unlisten = getCurrentWebview().onDragDropEvent((event) => {
      const p = event.payload;
      // Drags inside the page, such as reordering splices, carry no paths.
      if (p.type === "enter") dropping = p.paths.length > 0;
      else if (p.type === "leave") dropping = false;
      else if (p.type === "drop") {
        dropping = false;
        if (p.paths.length) addFiles(p.paths);
      }
    });
    return () => {
      unlisten.then((f) => f());
    };
  });
  onDestroy(() => {
    stop();
    preview?.close();
  });
</script>

<svelte:window onkeydown={onKey} onpointermove={onPointerMove} onpointerup={onPointerUp} onpointercancel={cancelDrag} />

<section class="editor" class:dropping>
  <header class="page-header">
    <div class="title">
      <h2><span class="num">{slot.fileName}</span>{#if slot.name}<span class="reel-name">{slot.name}</span>{/if}</h2>
      <span class="muted num">
        {formatTime(frames)} · {markers.length + 1} splices
        {#if dirty}· <strong class="unsaved">unsaved</strong>{/if}
        {#if busy}· working…{/if}
      </span>
    </div>
    <div class="actions">
      <details class="keys">
        <summary>Keys</summary>
        <dl class="small">
          <dt>Space</dt><dd>Play or stop, from the cursor</dd>
          <dt>M, double-click</dt><dd>Add a marker</dd>
          <dt>Delete</dt><dd>Delete the picked marker</dd>
          <dt>K</dt><dd>Lock or unlock the picked marker</dd>
          <dt>← →</dt><dd>Previous or next splice</dd>
          <dt>S</dt><dd>Shift to the next splice</dd>
          <dt>L</dt><dd>Loop splices or play through</dd>
          <dt>+ − 0</dt><dd>Zoom in, out, show all</dd>
          <dt>Shift-click</dt><dd>Pick a run of splices in the list</dd>
          <dt>Cmd-A</dt><dd>Pick every splice</dd>
          <dt>Escape</dt><dd>Drop the pick</dd>
          <dt>Cmd-C X V D</dt><dd>Copy, cut, paste, duplicate splices</dd>
          <dt>Shift-Delete</dt><dd>Delete the picked splices</dd>
          <dt>Cmd-Z</dt><dd>Undo; with Shift, redo</dd>
          <dt>Cmd-S</dt><dd>Save to the card</dd>
          <dt>Drop WAV files</dt><dd>Insert them after the pick</dd>
        </dl>
      </details>
      <button onclick={undo} disabled={!undoStack.length || busy} title="Undo (Cmd-Z)">Undo</button>
      <button onclick={redo} disabled={!redoStack.length || busy} title="Redo (Cmd-Shift-Z)">Redo</button>
      <button onclick={revert} disabled={!dirty || saving || busy}>Revert</button>
      <button class="primary" onclick={save} disabled={!dirty || saving || busy || tooMany || tooLong}>
        {saving ? "Saving…" : "Save to card"}
      </button>
    </div>
  </header>

  {#if loadError}
    <p class="error">{loadError}</p>
  {:else if !reel}
    <p class="muted">Loading…</p>
  {:else}
    {#if !slot.formatOk}
      <p class="error">
        This file isn't 48 kHz 32-bit float stereo. Saving converts it so the Morphagene can load it.
      </p>
    {/if}
    {#if tooMany}
      <p class="error">{markers.length} markers; the Morphagene allows {MAX_MARKERS}. Remove some to save.</p>
    {/if}
    {#if tooLong}
      <p class="error">
        {formatTime(frames)} long; the Morphagene loads reels up to {formatTime(MAX_FRAMES)}. Delete some
        audio to save.
      </p>
    {/if}

    <div class="transport">
      <button class="primary" onclick={() => (playing ? stop() : play())}>
        {playing ? "Stop" : "Play"}
      </button>
      <span class="num readout time" class:live={playhead !== null} title="Playhead, or the cursor when stopped">
        {formatTime(playhead ?? cursor)}
      </span>
      <select
        value={mode}
        onchange={(e) => ((mode = e.currentTarget.value as PreviewMode), restart())}
        title="L switches"
      >
        <option value="splices">Loop splices, like the module</option>
        <option value="reel">Play through the reel</option>
      </select>
      <button onclick={() => addMarker(playhead ?? cursor)} title="M">Add marker</button>
      <button onclick={toggleLock} disabled={selectedMarker === null} title="K">
        {selectedMarker !== null && locked.has(markers[selectedMarker]) ? "Unlock" : "Lock"}
      </button>
      <button onclick={removeSelected} disabled={selectedMarker === null} title="Delete">Delete marker</button>
      <span class="spacer"></span>
      <label title="Show frequency over time instead of the waveform">
        <input type="checkbox" bind:checked={spectrum} /> Spectrogram
      </label>
      <button onclick={() => waveform?.zoom(0.5)} title="Zoom out (-)">−</button>
      <button onclick={() => waveform?.fit()} title="Show all (0)">Fit</button>
      <button onclick={() => waveform?.zoom(2)} title="Zoom in (+)">+</button>
    </div>

    <div class="transport module">
      <label title="About 12 semitones up and 26 down, as on the module">
        Vari-Speed
        <input
          type="range"
          min={SPEED_DOWN}
          max={SPEED_UP}
          step="1"
          value={semitones}
          oninput={(e) => (semitones = Number(e.currentTarget.value))}
          onchange={restart}
        />
        <span class="num readout">{semitones > 0 ? "+" : ""}{semitones} st</span>
      </label>
      <button onclick={() => ((semitones = 0), restart())} disabled={semitones === 0}>1/1</button>
      <label>
        <input
          type="checkbox"
          checked={reverse}
          onchange={(e) => ((reverse = e.currentTarget.checked), restart())}
        />
        Play backwards
      </label>
      <label>
        Volume
        <input
          type="range"
          min="-30"
          max="6"
          step="1"
          value={volumeDb}
          oninput={(e) => {
            volumeDb = Number(e.currentTarget.value);
            preview?.setVolume(10 ** (volumeDb / 20));
          }}
        />
        <span class="num readout">{volumeDb} dB</span>
      </label>
      {#if mode === "splices"}
        <label title="Picks the next splice by voltage. It starts when the current splice ends.">
          Organize
          <input
            type="range"
            min="0"
            max={ORGANIZE_VOLTS}
            step="0.01"
            value={organize}
            oninput={(e) => setOrganize(Number(e.currentTarget.value))}
          />
          <span class="num readout">{organize.toFixed(2)} V</span>
        </label>
        <button onclick={shift} title="Next splice, when the current one ends (S)">Shift</button>
        {#if playing && playingSplice !== null && selected !== playingSplice}
          <span class="muted small">next: {selected + 1}</span>
        {/if}
      {/if}
    </div>

    <div class="wave">
    <Waveform
      bind:this={waveform}
      left={reel.left}
      right={reel.right}
      bind:markers
      bind:cursor
      bind:selectedMarker
      {locked}
      {spectrum}
      {playhead}
      onseek={(f) => playing && play(f)}
      onadd={addMarker}
      ondragstart={record}
      ondragend={snapDragged}
    />
    </div>

    <div class="lower">
      <div class="list" bind:this={list}>
        <table class:dragging={dragFrom !== null}>
          <thead>
            <tr><th>#</th><th></th><th>Start</th><th>Length</th><th>Organize</th><th></th></tr>
          </thead>
          <tbody>
            {#each splices as s, i}
              <tr
                class:current={i === current}
                class:picked={i >= selection.first && i <= selection.last}
                class:playing={i === playingSplice}
                class:drop-before={dragging && i === dropTarget && i < dragging.first}
                class:drop-after={dragging && i === dropTarget && i > dragging.last}
                data-splice={i}
                onpointerdown={(e) => {
                  dragged = false;
                  if (e.button === 0) press = { index: i, y: e.clientY };
                }}
                onclick={(e) => {
                  if (dragged) dragged = false;
                  else pick(i, e.shiftKey);
                }}
              >
                <td class="num">{i + 1}</td>
                <td><Thumb left={reel.left} right={reel.right} start={s.start} end={s.end} /></td>
                <td class="num">{formatTime(s.start)}</td>
                <td class="num">{((s.end - s.start) / SAMPLE_RATE).toFixed(2)} s</td>
                <td class="num muted">{organizeRange(i)}</td>
                <td>
                  {#if jumps[i]}
                    <span class="jump" title="Jumps from its end back to its start, so the loop may click. Smooth loop fixes it.">
                      loop jump
                    </span>
                  {/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
      <div class="tools">
        <div class="tabs-inline" role="tablist" aria-label="Tools">
          {#each TOOL_TABS as t}
            <button role="tab" aria-selected={toolTab === t.id} class:active={toolTab === t.id} onclick={() => showTools(t.id)}>
              {t.label}
            </button>
          {/each}
        </div>
        <div class="tool-body" role="tabpanel">
          {#if toolTab === "edit"}
            <p class="scope">{pickedLabel}. These change their audio.</p>
            <div class="row">
              <button onclick={cut} disabled={busy}>Cut</button>
              <button onclick={copy} disabled={busy}>Copy</button>
              <button onclick={paste} disabled={busy} title="After the picked splices">Paste</button>
              <button onclick={() => runEdit({ kind: "duplicate" })} disabled={busy}>Duplicate</button>
              <button
                class="danger"
                onclick={() => runEdit({ kind: "delete" })}
                disabled={busy || splices.length < 2}
              >
                Delete
              </button>
            </div>
            <div class="row">
              <button onclick={() => runEdit({ kind: "reverse" })} disabled={busy}>Reverse</button>
              <button
                onclick={() => runEdit({ kind: "shuffle", seed: Math.floor(Math.random() * 2 ** 32) })}
                disabled={busy || selection.first === selection.last}
                title="Put the picked splices in a random order"
              >
                Shuffle
              </button>
              <button
                onclick={() => runEdit({ kind: "matchLevels" })}
                disabled={busy || selection.first === selection.last}
                title="Bring each picked splice to their average loudness, so Organize doesn't jump in level"
              >
                Match levels
              </button>
              <button onclick={() => runEdit({ kind: "removeDc" })} disabled={busy}>Remove DC</button>
            </div>
            <div class="row">
              <label class="field">Gain <input type="number" step="0.5" bind:value={gainDb} /> dB</label>
              <button onclick={() => runEdit({ kind: "gain", db: gainDb })} disabled={busy}>Apply</button>
            </div>
            <div class="row">
              <label class="field">Peak <input type="number" step="0.5" max="0" bind:value={normalizeDb} /> dBFS</label>
              <button onclick={() => runEdit({ kind: "normalize", db: normalizeDb })} disabled={busy}>Normalize</button>
            </div>
            <div class="row">
              <label class="field">Fade <input type="number" min="1" step="1" bind:value={fadeMs} /> ms</label>
              <button onclick={() => runEdit({ kind: "fadeIn", ms: fadeMs })} disabled={busy}>Fade in</button>
              <button onclick={() => runEdit({ kind: "fadeOut", ms: fadeMs })} disabled={busy}>Fade out</button>
            </div>
            <div class="row">
              <label class="field">Crossfade <input type="number" min="2" step="5" bind:value={loopMs} /> ms</label>
              <button
                onclick={() => runEdit({ kind: "smoothLoop", ms: loopMs })}
                disabled={busy}
                title="Crossfade each splice's end into its start so it loops without a jump. Shortens it by this much."
              >
                Smooth loop
              </button>
            </div>
            <div class="row">
              <label class="field">Cutoff <input type="number" min="5" max="2000" step="5" bind:value={highPassHz} /> Hz</label>
              <button onclick={() => runEdit({ kind: "highPass", hz: highPassHz })} disabled={busy}>High-pass</button>
            </div>
            <div class="row">
              <label class="field">Speed <input type="number" min="-36" max="36" step="1" bind:value={speedSt} /> st</label>
              <button
                onclick={() => runEdit({ kind: "speed", semitones: speedSt })}
                disabled={busy || speedSt === 0}
                title="Like tape: pitch and length change together. +12 is an octave up and half as long."
              >
                Change speed
              </button>
            </div>
          {:else if toolTab === "markers"}
            <p class="scope">The whole reel. These change markers only, not the audio.</p>
            <label><input type="checkbox" bind:checked={snap} /> Snap new markers to zero crossings</label>

            <h4>Auto-slice</h4>
            <select value={sliceMode} onchange={(e) => setSliceMode(e.currentTarget.value as SliceMode)}>
              <option value="phrases">Phrases: where the sound changes character</option>
              <option value="gestures">Gestures: each distinct sound event</option>
            </select>
            <div class="row">
              <label class="field">Shortest <input type="number" min="0.05" step="0.1" bind:value={sliceMin} /> s</label>
              <label class="field" title="Longer splices get cut at their quietest point. Leave empty for no limit.">
                Longest <input type="number" min="0.1" step="0.5" placeholder="none" bind:value={sliceLongest} /> s
              </label>
            </div>
            {#if sliceLongest && sliceLongest < 2 * sliceMin}
              <p class="muted small">Raised to {2 * sliceMin} s: a splice can't be cut into two pieces shorter than the shortest.</p>
            {/if}
            <div class="row">
              <label class="field">At most <input type="number" min="1" max={MAX_MARKERS + 1} bind:value={sliceMax} /> splices</label>
              <button
                onclick={runAutoSlice}
                disabled={slicing}
                title="Listens for changes in level and spectrum, not pitch or beat, and cuts in the quiet between sounds. Replaces the unlocked markers."
              >
                {slicing ? "Listening…" : "Find splices"}
              </button>
            </div>

            <h4>Split evenly</h4>
            <div class="row">
              <label class="field">Into <input type="number" min="1" max={MAX_MARKERS + 1} bind:value={splitCount} /> splices</label>
              <button onclick={splitEvenly} title="Replaces the unlocked markers">Split evenly</button>
            </div>

            <h4>Import</h4>
            <label class="field" title="If the audio was exported from later than the start of the file's timeline, how much later">
              Audio starts at <input type="number" step="0.1" bind:value={importOffset} /> s
            </label>
            <label><input type="checkbox" bind:checked={importReplace} /> Replace the unlocked markers</label>
            <div class="row">
              <button
                onclick={importMarkers}
                title="From an Audacity label file, a REAPER marker export (.csv), an Ableton Live set's locators, or a WAV file's cue points and region ends"
              >
                Import markers…
              </button>
            </div>

            <h4>Clear</h4>
            <div class="row">
              <button class="danger" onclick={removeUnlocked} disabled={markers.every((m) => locked.has(m))}>
                {locked.size ? "Remove unlocked markers" : "Remove all markers"}
              </button>
            </div>
          {:else if toolTab === "add"}
            <p class="scope">{pickedLabel}. New audio goes after it; replace and mix need one splice.</p>
            <div class="row">
              <button onclick={() => addFiles()} disabled={busy} title="Each file becomes a splice after the picked ones. You can also drop WAV files here.">
                Insert files…
              </button>
              <button onclick={() => fileIntoSplice(false)} disabled={busy || selection.first !== selection.last}>
                Replace with file…
              </button>
            </div>
            <div class="row">
              <label class="field" title="Like the module's S.O.S. control: 0% keeps only the file, 100% only what was there">
                S.O.S. <input type="number" min="0" max="100" step="5" bind:value={sos} /> %
              </label>
              <button onclick={() => fileIntoSplice(true)} disabled={busy || selection.first !== selection.last}>
                Mix in file…
              </button>
            </div>
            <div class="row">
              <button onclick={exportPicked} disabled={busy}>Export as WAV files…</button>
            </div>
          {:else}
            <p class="scope">The whole reel, for reels recorded or spliced on the module. Each step undoes on its own.</p>
            <label class="field">Silence below <input type="number" max="0" step="6" bind:value={silenceDb} /> dBFS</label>
            <div class="row">
              <button onclick={dropSilent} disabled={busy || splices.length < 2}>Delete silent splices</button>
              <button
                onclick={trimStarts}
                disabled={busy}
                title="Cut each splice's silent start, leaving 5 ms before the sound so the attack stays"
              >
                Trim silent starts
              </button>
            </div>
            <div class="row">
              <label class="field">Merge splices shorter than <input type="number" min="1" step="10" bind:value={shortestMs} /> ms</label>
              <button
                onclick={mergeShort}
                disabled={splices.length < 2}
                title="Join each shorter splice to its shorter neighbour. Locked markers stay."
              >
                Merge
              </button>
            </div>
          {/if}
        </div>
      </div>
    </div>
  {/if}
</section>

<style>
  /* The editor fills the window: the waveform and the lower split share the
     height, and the splice list and the tools scroll on their own. */
  .editor {
    flex: 1;
    min-height: 560px;
    display: flex;
    flex-direction: column;
  }
  .title {
    display: flex;
    align-items: baseline;
    gap: var(--s3);
    min-width: 0;
  }
  h2 {
    margin: 0;
    font-size: var(--t-title);
    white-space: nowrap;
  }
  .reel-name {
    margin-left: 10px;
    font-weight: 400;
  }
  .unsaved {
    color: var(--accent);
  }
  .keys {
    position: relative;
  }
  .keys summary {
    list-style: none;
    cursor: pointer;
    padding: 5px 12px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--panel-2);
  }
  .keys summary::-webkit-details-marker {
    display: none;
  }
  .keys dl {
    position: absolute;
    right: 0;
    top: calc(100% + 4px);
    z-index: 10;
    width: 340px;
    margin: 0;
    padding: var(--s2) var(--s3);
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 2px var(--s3);
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 8px;
    box-shadow: 0 6px 20px rgba(0, 0, 0, 0.15);
  }
  .keys dt {
    font-family: var(--mono);
    white-space: nowrap;
  }
  .keys dd {
    margin: 0;
  }
  .transport {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--s2);
    margin-bottom: var(--s2);
  }
  .transport label {
    display: flex;
    align-items: center;
    gap: var(--s1);
  }
  .spacer {
    flex: 1;
  }
  .wave {
    flex: 3 1 0;
    min-height: 160px;
    display: flex;
    flex-direction: column;
    margin-bottom: var(--s3);
  }
  .lower {
    flex: 2 1 0;
    min-height: 200px;
    display: grid;
    grid-template-columns: minmax(0, 1fr) clamp(260px, 30%, 320px);
    gap: var(--s3);
  }
  .list {
    min-height: 0;
    overflow: auto;
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--panel);
  }
  table {
    width: 100%;
    border-collapse: collapse;
  }
  th {
    position: sticky;
    top: 0;
    background: var(--panel-2);
    text-align: left;
    font-weight: 600;
    font-size: var(--t-small);
  }
  th,
  td {
    padding: 4px 10px;
    white-space: nowrap;
  }
  tbody tr {
    cursor: pointer;
    user-select: none;
    -webkit-user-select: none;
  }
  table.dragging tbody tr {
    cursor: grabbing;
  }
  tr.drop-before td {
    box-shadow: inset 0 2px 0 var(--accent);
  }
  tr.drop-after td {
    box-shadow: inset 0 -2px 0 var(--accent);
  }
  .tools {
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: var(--s2);
    padding: var(--s2);
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--panel);
  }
  .tool-body {
    flex: 1;
    min-height: 0;
    overflow: auto;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: var(--s2);
    padding: 0 var(--s1) var(--s1);
  }
  .tool-body h4 {
    margin: var(--s2) 0 0;
    font-size: var(--t-label);
  }
  .tool-body select {
    width: 100%;
  }
  .scope {
    margin: 0;
    font-size: var(--t-small);
    color: var(--muted);
  }
  .row {
    display: flex;
    align-items: center;
    gap: var(--s2);
    flex-wrap: wrap;
  }
  .small {
    margin: 0;
  }
  .error {
    margin: 0 0 var(--s2);
  }
  .module {
    font-size: var(--t-label);
  }
  .module input[type="range"] {
    width: 110px;
  }
  .readout {
    display: inline-block;
    min-width: 48px;
  }
  tr.playing td:first-child {
    box-shadow: inset 3px 0 0 var(--playhead);
  }
  tr.picked td {
    background: var(--select);
  }
  tr.current td:first-child {
    font-weight: 700;
  }
  .jump {
    font-size: 11px;
    color: var(--danger);
    white-space: nowrap;
  }
  section.dropping {
    outline: 2px dashed var(--accent);
    outline-offset: 4px;
    border-radius: 8px;
  }
  .time {
    min-width: 72px;
    color: var(--muted);
  }
  .time.live {
    color: var(--playhead);
    font-weight: 600;
  }
</style>
