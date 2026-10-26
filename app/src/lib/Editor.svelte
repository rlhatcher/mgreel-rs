<script lang="ts">
  import { onDestroy, onMount, untrack } from "svelte";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { ask, open as openDialog } from "@tauri-apps/plugin-dialog";
  import Thumb from "./Thumb.svelte";
  import Waveform from "./Waveform.svelte";
  import {
    MAX_FRAMES,
    MAX_MARKERS,
    SAMPLE_RATE,
    applySpans,
    autoSlice,
    copySplices,
    editReel,
    errorText,
    exportSplices,
    insertFiles,
    formatTime,
    loadReel,
    pasteSplices,
    putFile,
    saveReel,
    seekHistory,
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
  let dragFrom: number | null = null;
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
   */
  async function edit(
    call: () => Promise<EditResult>,
    after: (added: number) => { first: number; last: number } | null,
    focus: number,
    keepOrder = false,
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
      locked = shiftLocks(r, next, keepOrder);
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

  function dropOn(target: number) {
    if (dragFrom === null) return;
    const from = dragFrom;
    dragFrom = null;
    const sel = from >= selection.first && from <= selection.last ? selection : { first: from, last: from };
    const to = target < sel.first ? target : target > sel.last ? target + 1 : null;
    if (to === null) return;
    picked = sel;
    runEdit({ kind: "moveTo", to });
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

<svelte:window onkeydown={onKey} />

<section class:dropping>
  <header>
    <h2 class="num">{slot.fileName}</h2>
    <span class="muted num">
      {formatTime(frames)} · {markers.length + 1} splices
      {#if dirty}· <strong class="unsaved">unsaved</strong>{/if}
      {#if busy}· working…{/if}
    </span>
    <div class="actions">
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
        Reverse
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

    <p class="hint muted">
      Space plays from the cursor. Double-click or press M to add a marker, drag one to move it,
      Delete removes it, K locks it. ← → step through splices, S shifts to the next splice, L
      switches between looping splices and playing through. In the list, click a splice to pick it,
      Shift-click to pick a run, drag to move them. Cmd-C, Cmd-X, Cmd-V and Cmd-D copy, cut, paste
      and duplicate splices; Shift-Delete deletes them. Cmd-Z undoes, Cmd-S saves.
    </p>

    <div class="lower">
      <div class="list">
        <table>
          <thead>
            <tr><th>#</th><th></th><th>Start</th><th>Length</th><th>Organize</th><th></th></tr>
          </thead>
          <tbody>
            {#each splices as s, i}
              <tr
                class:current={i === current}
                class:picked={i >= selection.first && i <= selection.last}
                class:playing={i === playingSplice}
                draggable="true"
                onclick={(e) => pick(i, e.shiftKey)}
                ondragstart={(e) => {
                  dragFrom = i;
                  e.dataTransfer?.setData("text/plain", String(i + 1));
                }}
                ondragover={(e) => e.preventDefault()}
                ondrop={(e) => {
                  e.preventDefault();
                  dropOn(i);
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
        <h3>
          Splice{selection.last > selection.first ? "s" : ""}
          {selection.first + 1}{selection.last > selection.first ? `–${selection.last + 1}` : ""}
        </h3>
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
        </div>
        <div class="row">
          <input type="number" step="0.5" bind:value={gainDb} /> dB
          <button onclick={() => runEdit({ kind: "gain", db: gainDb })} disabled={busy}>Gain</button>
        </div>
        <div class="row">
          <input type="number" step="0.5" max="0" bind:value={normalizeDb} /> dBFS
          <button onclick={() => runEdit({ kind: "normalize", db: normalizeDb })} disabled={busy}>
            Normalize
          </button>
        </div>
        <div class="row">
          <button
            onclick={() => runEdit({ kind: "matchLevels" })}
            disabled={busy || selection.first === selection.last}
            title="Bring each picked splice to their average loudness, so Organize doesn't jump in level"
          >
            Match levels
          </button>
        </div>
        <div class="row">
          <input type="number" min="1" step="1" bind:value={fadeMs} /> ms
          <button onclick={() => runEdit({ kind: "fadeIn", ms: fadeMs })} disabled={busy}>Fade in</button>
          <button onclick={() => runEdit({ kind: "fadeOut", ms: fadeMs })} disabled={busy}>Fade out</button>
        </div>
        <div class="row">
          <input type="number" min="2" step="5" bind:value={loopMs} /> ms
          <button
            onclick={() => runEdit({ kind: "smoothLoop", ms: loopMs })}
            disabled={busy}
            title="Crossfade each splice's end into its start so it loops without a jump. Shortens it by this much."
          >
            Smooth loop
          </button>
        </div>
        <div class="row">
          <button onclick={() => runEdit({ kind: "removeDc" })} disabled={busy}>Remove DC</button>
          <input type="number" min="5" max="2000" step="5" bind:value={highPassHz} /> Hz
          <button onclick={() => runEdit({ kind: "highPass", hz: highPassHz })} disabled={busy}>
            High-pass
          </button>
        </div>

        <div class="row">
          <input type="number" min="-36" max="36" step="1" bind:value={speedSt} /> st
          <button
            onclick={() => runEdit({ kind: "speed", semitones: speedSt })}
            disabled={busy || speedSt === 0}
            title="Like tape: pitch and length change together. +12 is an octave up and half as long."
          >
            Change speed
          </button>
        </div>

        <h3>Add audio</h3>
        <div class="row">
          <button onclick={() => addFiles()} disabled={busy} title="Each file becomes a splice after the picked ones. You can also drop WAV files here.">
            Insert files…
          </button>
          <button onclick={() => fileIntoSplice(false)} disabled={busy || selection.first !== selection.last}>
            Replace with file…
          </button>
        </div>
        <div class="row">
          <label title="Like the module's S.O.S. control: 0% keeps only the file, 100% only what was there">
            S.O.S.
            <input type="number" min="0" max="100" step="5" bind:value={sos} /> %
          </label>
          <button onclick={() => fileIntoSplice(true)} disabled={busy || selection.first !== selection.last}>
            Mix in file…
          </button>
        </div>
        <div class="row">
          <button onclick={exportPicked} disabled={busy}>Export as WAV files…</button>
        </div>

        <h3>Auto-slice</h3>
        <select value={sliceMode} onchange={(e) => setSliceMode(e.currentTarget.value as SliceMode)}>
          <option value="phrases">Phrases: where the sound changes character</option>
          <option value="gestures">Gestures: each distinct sound event</option>
        </select>
        <label>
          Shortest splice
          <input type="number" min="0.05" step="0.1" bind:value={sliceMin} /> s
        </label>
        <label title="Longer splices get cut at their quietest point. Leave empty for no limit.">
          Longest splice
          <input type="number" min="0.1" step="0.5" placeholder="none" bind:value={sliceLongest} /> s
        </label>
        {#if sliceLongest && sliceLongest < 2 * sliceMin}
          <p class="muted small">Raised to {2 * sliceMin} s: a splice can't be cut into two pieces shorter than the shortest.</p>
        {/if}
        <label>
          At most
          <input type="number" min="1" max={MAX_MARKERS + 1} bind:value={sliceMax} /> splices
        </label>
        <button onclick={runAutoSlice} disabled={slicing}>
          {slicing ? "Listening…" : "Find splices"}
        </button>
        <p class="muted small">
          Works on any source; it listens for changes in level and spectrum, not pitch or beat.
          Cuts go in the quiet moments between sounds. Replaces the markers that aren't locked.
        </p>

        <h3>Markers</h3>
        <label><input type="checkbox" bind:checked={snap} /> Snap to zero crossings</label>
        <label>
          Split into
          <input type="number" min="1" max={MAX_MARKERS + 1} bind:value={splitCount} />
          equal splices
        </label>
        <button onclick={splitEvenly}>Replace markers</button>
        <button class="danger" onclick={removeUnlocked} disabled={markers.every((m) => locked.has(m))}>
          {locked.size ? "Remove unlocked markers" : "Remove all markers"}
        </button>
      </div>
    </div>
  {/if}
</section>

<style>
  header {
    display: flex;
    align-items: baseline;
    gap: 12px;
    margin-bottom: 12px;
  }
  h2 {
    margin: 0;
    font-size: 18px;
  }
  .unsaved {
    color: var(--accent);
  }
  .actions {
    margin-left: auto;
    display: flex;
    gap: 8px;
  }
  .transport {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 10px;
  }
  .transport label {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .spacer {
    flex: 1;
  }
  .hint {
    font-size: 12px;
    margin: 8px 0 14px;
  }
  .lower {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 260px;
    gap: 16px;
  }
  .list {
    max-height: 260px;
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
    font-size: 12px;
  }
  th,
  td {
    padding: 4px 10px;
  }
  tbody tr {
    cursor: pointer;
  }
  .tools {
    display: flex;
    flex-direction: column;
    gap: 8px;
    align-items: flex-start;
  }
  .tools h3 {
    margin: 0;
    font-size: 13px;
  }
  .tools h3:not(:first-child) {
    margin-top: 10px;
  }
  .tools select {
    width: 100%;
  }
  .small {
    font-size: 12px;
    margin: 0;
  }
  .tools input {
    width: 64px;
  }
  .error {
    margin: 0 0 10px;
  }
  .module {
    flex-wrap: wrap;
    font-size: 13px;
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
  .row {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
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
