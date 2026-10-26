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

  type Props = {
    root: string;
    slot: Slot;
    onsaved: (slot: Slot) => void;
    notify: (text: string, error?: boolean) => void;
    dirty: boolean;
  };
  let { root, slot, onsaved, notify, dirty = $bindable() }: Props = $props();

  let reel = $state<LoadedReel | null>(null);
  let loadError = $state("");
  let markers = $state<number[]>([]);
  let saved = $state<number[]>([]);
  let cursor = $state(0);
  let playhead = $state<number | null>(null);
  let selectedMarker = $state<number | null>(null);
  let loop = $state(false);
  let saving = $state(false);
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
  const current = $derived(splices.findIndex((s) => cursor >= s.start && cursor < s.end));
  const tooMany = $derived(markers.length > MAX_MARKERS);

  $effect(() => {
    dirty = markers.length !== saved.length || markers.some((m, i) => m !== saved[i]);
  });

  // Audio
  let audio: AudioContext | null = null;
  let buffer: AudioBuffer | null = null;
  let source: AudioBufferSourceNode | null = null;
  let startedAt = 0;
  let startFrame = 0;
  let loopRange: { start: number; end: number } | null = null;
  let raf = 0;

  async function load() {
    stop();
    reel = null;
    loadError = "";
    try {
      const loaded = await loadReel(root, slot.index);
      reel = loaded;
      markers = loaded.markers.slice();
      saved = loaded.markers.slice();
      cursor = 0;
      selectedMarker = null;
      buffer = null;
    } catch (e) {
      loadError = errorText(e);
    }
  }

  function ensureBuffer() {
    if (!reel) return null;
    audio ??= new AudioContext({ sampleRate: SAMPLE_RATE });
    if (!buffer) {
      buffer = audio.createBuffer(2, Math.max(1, frames), SAMPLE_RATE);
      buffer.copyToChannel(reel.left as Float32Array<ArrayBuffer>, 0);
      buffer.copyToChannel(reel.right as Float32Array<ArrayBuffer>, 1);
    }
    return buffer;
  }

  function play(from = cursor) {
    const buf = ensureBuffer();
    if (!buf || !audio) return;
    stop();
    source = audio.createBufferSource();
    source.buffer = buf;
    source.connect(audio.destination);
    if (loop && current >= 0) {
      loopRange = splices[current];
      source.loop = true;
      source.loopStart = loopRange.start / SAMPLE_RATE;
      source.loopEnd = loopRange.end / SAMPLE_RATE;
      if (from < loopRange.start || from >= loopRange.end) from = loopRange.start;
    } else {
      loopRange = null;
    }
    source.onended = () => {
      if (source?.loop === false) stop();
    };
    startedAt = audio.currentTime;
    startFrame = from;
    source.start(0, from / SAMPLE_RATE);
    tick();
  }

  function tick() {
    if (!audio || !source) return;
    let pos = startFrame + (audio.currentTime - startedAt) * SAMPLE_RATE;
    if (loopRange && pos >= loopRange.end) {
      const len = loopRange.end - loopRange.start;
      pos = loopRange.start + ((pos - loopRange.start) % len);
    }
    playhead = Math.min(frames, Math.round(pos));
    raf = requestAnimationFrame(tick);
  }

  function stop() {
    cancelAnimationFrame(raf);
    if (source) {
      source.onended = null;
      try {
        source.stop();
      } catch {
        // Already stopped.
      }
      source.disconnect();
    }
    source = null;
    playhead = null;
  }

  const playing = () => source !== null;

  // Editing
  function addMarker(at: number) {
    at = Math.round(at);
    if (at <= 0 || at >= frames || markers.includes(at)) return;
    markers = [...markers, at].sort((a, b) => a - b);
    selectedMarker = markers.indexOf(at);
  }

  function removeSelected() {
    if (selectedMarker === null) return;
    markers = markers.filter((_, i) => i !== selectedMarker);
    selectedMarker = null;
  }

  function setSliceMode(mode: SliceMode) {
    sliceMode = mode;
    sliceMin = mode === "phrases" ? 4 : 0.5;
    sliceLongest = mode === "phrases" ? 15 : 5;
  }

  async function runAutoSlice() {
    slicing = true;
    try {
      const max = Math.max(1, Math.min(MAX_MARKERS + 1, Math.floor(sliceMax)));
      const min = Math.max(0.05, sliceMin);
      const longest = sliceLongest ? Math.max(sliceLongest, 2 * min) : null;
      markers = await autoSlice(root, slot.index, sliceMode, min, longest, max - 1);
      selectedMarker = null;
      notify(`Found ${markers.length + 1} splices. Not saved yet: listen through, adjust, then save or revert.`);
    } catch (e) {
      notify(errorText(e), true);
    } finally {
      slicing = false;
    }
  }

  function splitEvenly() {
    const n = Math.max(1, Math.min(MAX_MARKERS + 1, Math.floor(splitCount)));
    markers = Array.from({ length: n - 1 }, (_, i) => Math.round((frames * (i + 1)) / n));
    selectedMarker = null;
  }

  function selectSplice(i: number) {
    if (i < 0 || i >= splices.length) return;
    cursor = splices[i].start;
    selectedMarker = null;
    waveform?.reveal(cursor);
    if (playing()) play();
  }

  async function save() {
    if (!reel) return;
    saving = true;
    try {
      const updated = await saveMarkers(root, slot.index, markers, reel.audioId);
      reel.audioId = updated.audioId;
      saved = markers.slice();
      onsaved(updated.slot);
      notify(`Saved ${markers.length + 1} splices to ${slot.fileName}`);
    } catch (e) {
      notify(errorText(e), true);
    } finally {
      saving = false;
    }
  }

  function onKey(e: KeyboardEvent) {
    const target = e.target as HTMLElement;
    if (target.closest("input, textarea, select")) return;
    if ((e.metaKey || e.ctrlKey) && e.key === "s") {
      e.preventDefault();
      if (dirty && !tooMany) save();
      return;
    }
    if (e.metaKey || e.ctrlKey) return;
    switch (e.key) {
      case " ":
        e.preventDefault();
        if (playing()) stop();
        else play();
        break;
      case "Escape":
        stop();
        break;
      case "m":
        addMarker(playhead ?? cursor);
        break;
      case "l":
        loop = !loop;
        if (playing()) play(playhead ?? cursor);
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
    audio?.close();
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
      <button onclick={() => (markers = saved.slice())} disabled={!dirty || saving}>Revert</button>
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
      <button class="primary" onclick={() => (playing() ? stop() : play())}>
        {playhead === null ? "Play" : "Stop"}
      </button>
      <label><input type="checkbox" bind:checked={loop} /> Loop splice</label>
      <button onclick={() => addMarker(playhead ?? cursor)}>Add marker</button>
      <button onclick={removeSelected} disabled={selectedMarker === null}>Delete marker</button>
      <span class="spacer"></span>
      <button onclick={() => waveform?.zoom(0.5)} title="Zoom out (-)">−</button>
      <button onclick={() => waveform?.fit()} title="Show all (0)">Fit</button>
      <button onclick={() => waveform?.zoom(2)} title="Zoom in (+)">+</button>
    </div>

    <Waveform
      bind:this={waveform}
      left={reel.left}
      right={reel.right}
      bind:markers
      bind:cursor
      bind:selectedMarker
      {playhead}
      onseek={(f) => playing() && play(f)}
    />

    <p class="hint muted">
      Space plays from the cursor. Double-click or press M to add a marker, drag one to move it,
      Delete removes the selected one. ← → step through splices, L loops the current splice. Cmd-S saves.
    </p>

    <div class="lower">
      <div class="list">
        <table>
          <thead><tr><th>#</th><th>Start</th><th>Length</th></tr></thead>
          <tbody>
            {#each splices as s, i}
              <tr class:current={i === current} onclick={() => selectSplice(i)}>
                <td class="num">{i + 1}</td>
                <td class="num">{formatTime(s.start)}</td>
                <td class="num">{((s.end - s.start) / SAMPLE_RATE).toFixed(2)} s</td>
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
          Cuts go in the quiet moments between sounds. Replaces the current markers.
        </p>

        <h3>Markers</h3>
        <label>
          Split into
          <input type="number" min="1" max={MAX_MARKERS + 1} bind:value={splitCount} />
          equal splices
        </label>
        <button onclick={splitEvenly}>Replace markers</button>
        <button class="danger" onclick={() => (markers = [])} disabled={markers.length === 0}>
          Remove all markers
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
</style>
