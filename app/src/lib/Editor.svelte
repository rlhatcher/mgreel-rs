<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import Waveform from "./Waveform.svelte";
  import {
    MAX_MARKERS,
    SAMPLE_RATE,
    autoSlice,
    errorText,
    formatTime,
    loadReel,
    saveMarkers,
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

  const frames = $derived(reel?.left.length ?? 0);
  const splices = $derived(
    [0, ...markers].map((start, i) => ({ start, end: markers[i] ?? frames })),
  );
  const current = $derived(spliceAt(cursor));
  const tooMany = $derived(markers.length > MAX_MARKERS);

  $effect(() => {
    dirty = markers.length !== saved.length || markers.some((m, i) => m !== saved[i]);
  });

  function spliceAt(frame: number) {
    const i = splices.findIndex((s) => frame >= s.start && frame < s.end);
    return i < 0 ? splices.length - 1 : i;
  }

  /** The Organize voltage range that selects splice `i`. */
  function organizeRange(i: number) {
    const step = ORGANIZE_VOLTS / splices.length;
    return `${(i * step).toFixed(2)}–${((i + 1) * step).toFixed(2)} V`;
  }

  // Undo. Each step stores the markers and locks from before a change.
  type Snapshot = { markers: number[]; locked: number[] };
  let undoStack = $state<Snapshot[]>([]);
  let redoStack = $state<Snapshot[]>([]);

  const snapshot = (): Snapshot => ({ markers: markers.slice(), locked: [...locked] });
  const same = (a: Snapshot, b: Snapshot) =>
    a.markers.length === b.markers.length &&
    a.markers.every((m, i) => m === b.markers[i]) &&
    a.locked.length === b.locked.length &&
    a.locked.every((m) => b.locked.includes(m));

  /** Call before every change to markers or locks. */
  function record() {
    undoStack = [...undoStack.slice(1 - HISTORY), snapshot()];
    redoStack = [];
  }

  function restore(s: Snapshot) {
    markers = s.markers.slice();
    locked = new Set(s.locked);
    selectedMarker = null;
  }

  function undo() {
    const now = snapshot();
    while (undoStack.length) {
      const s = undoStack[undoStack.length - 1];
      undoStack = undoStack.slice(0, -1);
      // A drag that ended where it started leaves a step with no change.
      if (same(s, now)) continue;
      redoStack = [...redoStack, now];
      restore(s);
      return;
    }
  }

  function redo() {
    const s = redoStack[redoStack.length - 1];
    if (!s) return;
    redoStack = redoStack.slice(0, -1);
    undoStack = [...undoStack, snapshot()];
    restore(s);
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
      cursor = 0;
      selectedMarker = null;
      preview = new Preview(
        loaded.left,
        loaded.right,
        () => splices,
        () => selected,
        () => stop(),
      );
    } catch (e) {
      loadError = errorText(e);
    }
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
    playingSplice = pos.splice;
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

  function revert() {
    record();
    markers = saved.slice();
    locked = new Set([...locked].filter((m) => markers.includes(m)));
    selectedMarker = null;
  }

  function setSliceMode(next: SliceMode) {
    sliceMode = next;
    sliceMin = next === "phrases" ? 4 : 0.5;
    sliceLongest = next === "phrases" ? 15 : 5;
  }

  async function runAutoSlice() {
    slicing = true;
    try {
      const max = Math.max(1, Math.min(MAX_MARKERS + 1, Math.floor(sliceMax)));
      const min = Math.max(0.05, sliceMin);
      const longest = sliceLongest ? Math.max(sliceLongest, 2 * min) : null;
      const found = await autoSlice(root, slot.index, sliceMode, min, longest, max - 1, [...locked]);
      record();
      markers = found;
      selectedMarker = null;
      notify(`Found ${markers.length + 1} splices. Not saved yet: listen through, adjust, then save or revert.`);
    } catch (e) {
      notify(errorText(e), true);
    } finally {
      slicing = false;
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
    if (!reel || saving) return;
    saving = true;
    // Edits made while the save runs stay unsaved.
    const loaded = reel;
    const sent = markers.slice();
    try {
      const updated = await saveMarkers(root, slot.index, sent, loaded.audioId);
      loaded.audioId = updated.audioId;
      saved = sent;
      onsaved(updated.slot);
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
        if (dirty && !tooMany) save();
      } else if (key === "z") {
        e.preventDefault();
        if (e.shiftKey) redo();
        else undo();
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
        removeSelected();
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

  onMount(load);
  onDestroy(() => {
    stop();
    preview?.close();
  });
</script>

<svelte:window onkeydown={onKey} />

<section>
  <header>
    <h2 class="num">{slot.fileName}</h2>
    <span class="muted num">
      {formatTime(frames)} · {markers.length + 1} splices
      {#if dirty}· <strong class="unsaved">unsaved</strong>{/if}
    </span>
    <div class="actions">
      <button onclick={undo} disabled={!undoStack.length} title="Undo (Cmd-Z)">Undo</button>
      <button onclick={redo} disabled={!redoStack.length} title="Redo (Cmd-Shift-Z)">Redo</button>
      <button onclick={revert} disabled={!dirty || saving}>Revert</button>
      <button class="primary" onclick={save} disabled={!dirty || saving || tooMany}>
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

    <div class="transport">
      <button class="primary" onclick={() => (playing ? stop() : play())}>
        {playing ? "Stop" : "Play"}
      </button>
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
      {playhead}
      onseek={(f) => playing && play(f)}
      onadd={addMarker}
      ondragstart={record}
      ondragend={snapDragged}
    />

    <p class="hint muted">
      Space plays from the cursor. Double-click or press M to add a marker, drag one to move it,
      Delete removes it, K locks it. ← → step through splices, S shifts to the next splice, L
      switches between looping splices and playing through. Cmd-Z undoes, Cmd-S saves.
    </p>

    <div class="lower">
      <div class="list">
        <table>
          <thead><tr><th>#</th><th>Start</th><th>Length</th><th>Organize</th></tr></thead>
          <tbody>
            {#each splices as s, i}
              <tr class:current={i === current} class:playing={i === playingSplice} onclick={() => selectSplice(i)}>
                <td class="num">{i + 1}</td>
                <td class="num">{formatTime(s.start)}</td>
                <td class="num">{((s.end - s.start) / SAMPLE_RATE).toFixed(2)} s</td>
                <td class="num muted">{organizeRange(i)}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
      <div class="tools">
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
  tr.current td {
    background: var(--select);
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
    box-shadow: inset 3px 0 0 var(--accent);
  }
</style>
