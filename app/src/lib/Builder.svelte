<script lang="ts">
  import { onMount } from "svelte";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { ask, open, save } from "@tauri-apps/plugin-dialog";
  import {
    MAX_FRAMES,
    MAX_MARKERS,
    SAMPLE_RATE,
    buildReel,
    errorText,
    formatTime,
    probeSources,
    type Slot,
    type Source,
    type Target,
  } from "./api";

  type Props = {
    root: string | null;
    slots: Slot[];
    target: number | null;
    notify: (text: string, error?: boolean) => void;
    onbuilt: () => Promise<void>;
  };
  let { root, slots, target = $bindable(), notify, onbuilt }: Props = $props();

  type Row = Source & { id: number; start: string; end: string };
  let rows = $state<Row[]>([]);
  let normalize = $state(false);
  let keepMarkers = $state(true);
  let fadeMs = $state(0);
  let building = $state(false);
  let dropping = $state(false);
  let nextId = 0;

  const toFrames = (seconds: string) => {
    const s = parseFloat(seconds);
    return Number.isFinite(s) && s >= 0 ? Math.round(s * SAMPLE_RATE) : null;
  };

  function range(row: Row): [number, number] {
    const start = Math.min(row.frames, toFrames(row.start) ?? 0);
    const end = Math.min(row.frames, toFrames(row.end) ?? row.frames);
    return [start, Math.max(start, end)];
  }

  /** The file's own markers inside its trim range. */
  function inside(row: Row) {
    const [start, end] = range(row);
    return row.markers.filter((m) => m > start && m < end).length;
  }

  const usable = $derived(rows.filter((r) => !r.error));
  const total = $derived(usable.reduce((sum, r) => sum + (range(r)[1] - range(r)[0]), 0));
  const kept = $derived(keepMarkers ? usable.reduce((sum, r) => sum + inside(r), 0) : 0);
  const spliceCount = $derived(usable.length + kept);
  const tooLong = $derived(total > MAX_FRAMES);
  const tooMany = $derived(spliceCount - 1 > MAX_MARKERS);

  async function add(paths: string[]) {
    const wavs = paths.filter((p) => /\.wave?$/i.test(p));
    if (wavs.length < paths.length) notify("Skipped files that aren't .wav", true);
    if (!wavs.length) return;
    const probed = await probeSources(wavs);
    rows = [...rows, ...probed.map((s) => ({ ...s, id: nextId++, start: "", end: "" }))];
  }

  async function pick() {
    const picked = await open({
      multiple: true,
      filters: [{ name: "WAV audio", extensions: ["wav", "wave"] }],
    });
    if (picked) add(Array.isArray(picked) ? picked : [picked]);
  }

  function shift(i: number, by: number) {
    const j = i + by;
    if (j < 0 || j >= rows.length) return;
    const next = rows.slice();
    [next[i], next[j]] = [next[j], next[i]];
    rows = next;
  }

  async function build() {
    let dest: Target;
    if (target === null) {
      const path = await save({ defaultPath: "reel.wav", filters: [{ name: "WAV audio", extensions: ["wav"] }] });
      if (!path) return;
      dest = { kind: "file", path };
    } else {
      if (!root) return;
      const slot = slots[target];
      if (slot.present) {
        const yes = await ask(`Replace ${slot.fileName} (${formatTime(slot.frames)})? A copy goes to the backups folder.`, {
          title: "Replace reel",
          kind: "warning",
        });
        if (!yes) return;
      }
      dest = { kind: "slot", root, index: target };
    }
    building = true;
    try {
      const specs = usable.map((r) => {
        const [start, end] = range(r);
        const trimmed = start > 0 || end < r.frames;
        return { path: r.path, start: trimmed ? start : null, end: trimmed ? end : null };
      });
      const result = await buildReel(specs, normalize, fadeMs, keepMarkers, dest);
      notify(`Wrote ${formatTime(result.frames)} with ${result.markers + 1} splices to ${result.writtenTo}`);
      await onbuilt();
    } catch (e) {
      notify(errorText(e), true);
    } finally {
      building = false;
    }
  }

  onMount(() => {
    const unlisten = getCurrentWebview().onDragDropEvent((event) => {
      const p = event.payload;
      if (p.type === "enter" || p.type === "over") dropping = true;
      else if (p.type === "leave") dropping = false;
      else if (p.type === "drop") {
        dropping = false;
        add(p.paths);
      }
    });
    return () => {
      unlisten.then((f) => f());
    };
  });
</script>

<section class:dropping>
  <header class="page-header">
    <p
      class="muted"
      title="Any sample rate, bit depth, mono or stereo. The reel is written as 48 kHz 32-bit float stereo."
    >
      Join WAV files into a reel, one splice each, in this order.
    </p>
    <div class="actions">
      <button onclick={pick}>Add files…</button>
      <label class="field">
        Write to
        <select bind:value={target} disabled={!root}>
          <option value={null}>A file…</option>
          {#each slots as slot}
            <option value={slot.index}>
              {slot.fileName}{slot.name ? ` ${slot.name}` : ""}{slot.present
                ? ` (replaces ${formatTime(slot.frames)})`
                : " (empty)"}
            </option>
          {/each}
        </select>
      </label>
      <button class="primary" onclick={build} disabled={building || usable.length === 0 || tooLong || tooMany}>
        {building ? "Building…" : "Build reel"}
      </button>
    </div>
  </header>

  {#if rows.length === 0}
    <div class="drop">Drop WAV files here</div>
  {:else}
    <table>
      <thead>
        <tr><th>#</th><th>File</th><th>Length</th><th>Start (s)</th><th>End (s)</th><th></th></tr>
      </thead>
      <tbody>
        {#each rows as row, i (row.id)}
          <tr>
            <td class="num">{i + 1}</td>
            <td>
              {row.name}
              {#if row.error}<div class="error small">{row.error}</div>{/if}
              {#if !row.error && inside(row) > 0}
                <div class="muted small">
                  {inside(row)} marker{inside(row) === 1 ? "" : "s"}{keepMarkers ? "" : ", not kept"}
                </div>
              {/if}
            </td>
            <td class="num">{formatTime(range(row)[1] - range(row)[0])}</td>
            <td><input class="num" placeholder="0" bind:value={row.start} disabled={!!row.error} /></td>
            <td>
              <input
                class="num"
                placeholder={(row.frames / SAMPLE_RATE).toFixed(2)}
                bind:value={row.end}
                disabled={!!row.error}
              />
            </td>
            <td class="row-actions">
              <button onclick={() => shift(i, -1)} disabled={i === 0} title="Move up">↑</button>
              <button onclick={() => shift(i, 1)} disabled={i === rows.length - 1} title="Move down">↓</button>
              <button onclick={() => (rows = rows.filter((r) => r.id !== row.id))} title="Remove">✕</button>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
    <p class="muted small">Drop more files anywhere in this view to add them.</p>
  {/if}

  <div class="meter" class:over={tooLong}>
    <div class="fill" style:width="{Math.min(1, total / MAX_FRAMES) * 100}%"></div>
  </div>
  <p class="num" class:error={tooLong || tooMany}>
    {formatTime(total)} of {formatTime(MAX_FRAMES)} · {spliceCount} splices
    {#if tooLong} · too long for the Morphagene{/if}
    {#if tooMany} · more than {MAX_MARKERS + 1} splices{/if}
  </p>

  <div class="options">
    <label><input type="checkbox" bind:checked={normalize} /> Normalize each file to −1 dBFS</label>
    <label
      title="Cue points and region ends saved in the files, from a DAW, RX or the module, become splices too"
    >
      <input type="checkbox" bind:checked={keepMarkers} /> Keep the files' markers
    </label>
    <label class="field">
      Fade edges <input class="num" type="number" min="0" max="500" bind:value={fadeMs} /> ms
    </label>
  </div>
</section>

<style>
  section {
    border-radius: 8px;
  }
  section.dropping {
    outline: 2px dashed var(--accent);
    outline-offset: 6px;
  }
  .drop {
    border: 2px dashed var(--line);
    border-radius: 8px;
    padding: 48px;
    text-align: center;
    color: var(--muted);
    margin-bottom: 16px;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 8px;
    margin-bottom: 6px;
  }
  th {
    text-align: left;
    font-size: 12px;
    font-weight: 600;
    background: var(--panel-2);
  }
  th,
  td {
    padding: 5px 10px;
    border-bottom: 1px solid var(--line);
  }
  td input {
    width: 80px;
  }
  .row-actions {
    white-space: nowrap;
    text-align: right;
  }
  .row-actions button {
    padding: 1px 7px;
  }
  .meter {
    height: 12px;
    background: var(--panel-2);
    border-radius: 4px;
    overflow: hidden;
    margin-top: 12px;
  }
  .meter .fill {
    height: 100%;
    background: var(--accent);
  }
  .meter.over .fill {
    background: var(--danger);
  }
  .options {
    display: flex;
    align-items: center;
    gap: 18px;
    flex-wrap: wrap;
  }
  .options label {
    display: flex;
    align-items: center;
    gap: 6px;
  }
</style>
