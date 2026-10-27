<script lang="ts">
  import { ask, open } from "@tauri-apps/plugin-dialog";
  import { revealItemInDir } from "@tauri-apps/plugin-opener";
  import {
    MAX_FRAMES,
    adoptFile,
    backupCard,
    backupsFolder,
    checkCard,
    clearSlot,
    convertSlot,
    ejectCard,
    errorText,
    formatTime,
    listBackups,
    moveSlot,
    removeClutter,
    restoreCard,
    restoreFile,
    swapSlots,
    type Backup,
    type BackupKind,
    type CardCheck,
    type Slot,
  } from "./api";

  type Props = {
    root: string;
    slots: Slot[];
    onedit: (index: number) => void;
    onbuild: (index: number) => void;
    onchanged: () => Promise<void>;
    onejected: () => void;
    notify: (text: string, error?: boolean) => void;
  };
  let { root, slots, onedit, onbuild, onchanged, onejected, notify }: Props = $props();

  let busy = $state(false);
  let check = $state<CardCheck | null>(null);
  let history = $state<Backup[] | null>(null);

  const used = $derived(slots.filter((s) => s.present).length);
  const freeSlot = $derived(slots.find((s) => !s.present) ?? null);
  /** Folders under /Volumes are mounted cards; anything else was opened by hand. */
  const mounted = $derived(/^\/Volumes\/[^/]+$/.test(root));
  const hasIssues = $derived(
    !!check && (check.wrongFilesystem !== null || check.stray.length > 0 || check.clutter > 0),
  );

  // Look the card over whenever it or its slots change.
  $effect(() => {
    const r = root;
    void slots;
    checkCard(r)
      .then((c) => {
        if (r === root) check = c;
      })
      .catch((e) => notify(errorText(e), true));
  });

  async function run(action: () => Promise<unknown>, done: string) {
    busy = true;
    try {
      await action();
      await onchanged();
      notify(done);
    } catch (e) {
      notify(errorText(e), true);
    } finally {
      busy = false;
    }
  }

  // Reels move with pointer events, not HTML5 drag and drop, because the
  // Tauri window takes drag events for its own file drop.
  /** A press on a reel. It turns into a drag once the pointer moves. */
  let press: { index: number; x: number; y: number } | null = null;
  let dragFrom = $state<number | null>(null);
  let dropOn = $state<number | null>(null);

  function startPress(e: PointerEvent, slot: Slot) {
    if (e.button !== 0 || !slot.present || busy) return;
    if ((e.target as Element).closest("button")) return;
    press = { index: slot.index, x: e.clientX, y: e.clientY };
  }

  function onPointerMove(e: PointerEvent) {
    if (!press) return;
    // The button came up where the window couldn't see it, as outside it.
    if (e.buttons === 0) {
      cancelDrag();
      return;
    }
    if (dragFrom === null) {
      if (Math.hypot(e.clientX - press.x, e.clientY - press.y) < 4) return;
      dragFrom = press.index;
    }
    const el = document.elementFromPoint(e.clientX, e.clientY)?.closest<HTMLElement>("li[data-slot]");
    dropOn = el ? Number(el.dataset.slot) : null;
  }

  function onPointerUp(e: PointerEvent) {
    if (!press) return;
    const from = dragFrom;
    const to = dropOn;
    cancelDrag();
    if (from === null || to === null || from === to) return;
    // Option-drop swaps; a plain drop inserts and shifts the slots between.
    if (e.altKey) {
      run(() => swapSlots(root, from, to), `Swapped ${slots[from].fileName} and ${slots[to].fileName}`);
    } else {
      run(() => moveSlot(root, from, to), `Moved ${slots[from].fileName} to ${slots[to].fileName}`);
    }
  }

  function cancelDrag() {
    press = null;
    dragFrom = dropOn = null;
  }

  function convert(slot: Slot) {
    run(() => convertSlot(root, slot.index), `Converted ${slot.fileName} to 48 kHz 32-bit float stereo`);
  }

  function adopt(path: string, name: string) {
    const target = freeSlot;
    if (!target) return;
    run(() => adoptFile(root, path, target.index), `Put ${name} in ${target.fileName}`);
  }

  function tidy() {
    busy = true;
    removeClutter(root)
      .then((n) => notify(`Removed ${n} macOS metadata file${n === 1 ? "" : "s"}`))
      .catch((e) => notify(errorText(e), true))
      .finally(async () => {
        busy = false;
        check = await checkCard(root).catch(() => check);
      });
  }

  async function eject() {
    busy = true;
    try {
      await ejectCard(root);
      onejected();
    } catch (e) {
      notify(errorText(e), true);
    } finally {
      busy = false;
    }
  }

  async function toggleHistory() {
    if (history) {
      history = null;
      return;
    }
    try {
      history = await listBackups(root);
    } catch (e) {
      notify(errorText(e), true);
    }
  }

  const KIND: Record<BackupKind, string> = {
    fromCard: "Changed on the card, copied when it mounted",
    beforeChange: "Before the app changed it",
    wholeCard: "Whole card backup",
  };

  async function restoreOne(backup: Backup, file: string) {
    const yes = await ask(`Put this copy of ${file} back on the card? What's there now goes to the backups folder first.`, {
      title: "Restore file",
      kind: "warning",
    });
    if (!yes) return;
    await run(() => restoreFile(root, backup.dir, file), `Restored ${file}`);
    history = await listBackups(root).catch(() => history);
  }

  async function clear(slot: Slot) {
    const yes = await ask(`Remove ${slot.fileName} from the card? A copy goes to the backups folder.`, {
      title: "Remove reel",
      kind: "warning",
    });
    if (yes) run(() => clearSlot(root, slot.index), `Removed ${slot.fileName}`);
  }

  async function backup() {
    busy = true;
    try {
      const dir = await backupCard(root);
      notify(`Backed up to ${dir}`);
    } catch (e) {
      notify(errorText(e), true);
    } finally {
      busy = false;
    }
  }

  async function restore() {
    const from = await open({ directory: true, defaultPath: await backupsFolder(root), title: "Choose a backup folder" });
    if (typeof from !== "string") return;
    const yes = await ask(`Replace every reel and options.txt on the card with the contents of ${from}? The card is backed up first.`, {
      title: "Restore backup",
      kind: "warning",
    });
    if (yes) run(() => restoreCard(root, from), "Restored backup");
  }

  async function reveal() {
    await revealItemInDir(await backupsFolder(root));
  }
</script>

<svelte:window
  onpointermove={onPointerMove}
  onpointerup={onPointerUp}
  onpointercancel={cancelDrag}
  onkeydown={(e) => e.key === "Escape" && cancelDrag()}
/>

<section>
  <header>
    <p class="muted">
      {used} of 32 slots used. Drag a reel to move it; hold Option while dropping to swap.
    </p>
    <div class="actions">
      <button onclick={backup} disabled={busy}>Back up card</button>
      <button onclick={restore} disabled={busy}>Restore…</button>
      <button onclick={toggleHistory} class:active={history !== null}>History</button>
      <button onclick={reveal}>Show backups</button>
      {#if mounted}
        <button onclick={eject} disabled={busy} title="Unmount the card so you can take it out">Eject</button>
      {/if}
    </div>
  </header>

  {#if hasIssues && check}
    <div class="panel issues">
      <h3>Card check</h3>
      {#if check.wrongFilesystem}
        <p class="error small">
          The card is formatted as {check.wrongFilesystem}, and the module reads only FAT32 cards. Back the
          card up, format it as MS-DOS (FAT) in Disk Utility, then restore the backup.
        </p>
      {/if}
      {#each check.stray as file}
        <div class="issue">
          <span class="small">
            <span class="num">{file.name}</span>:
            {#if !file.wav}
              not a WAV file, so the module can't play it. Make a WAV of it on the computer first.
            {:else if file.name.includes("/")}
              in a folder, so the module won't load it.
            {:else}
              the module loads only mg1 to mgw.
            {/if}
          </span>
          {#if file.wav}
            <button
              onclick={() => adopt(file.path, file.name)}
              disabled={busy || !freeSlot}
              title={freeSlot ? "Converted if needed, with its markers. The file is backed up, then removed." : "Every slot is full"}
            >
              {freeSlot ? `Put in ${freeSlot.fileName.replace(".wav", "")}` : "No free slot"}
            </button>
          {/if}
        </div>
      {/each}
      {#if check.clutter > 0}
        <div class="issue">
          <span class="small">
            {check.clutter} macOS metadata file{check.clutter === 1 ? "" : "s"} (._ files and .DS_Store). The
            module doesn't use them.
          </span>
          <button onclick={tidy} disabled={busy}>Remove</button>
        </div>
      {/if}
    </div>
  {/if}

  {#if history}
    <div class="panel history">
      <h3>History</h3>
      {#if history.length === 0}
        <p class="muted small">No backups of this card yet.</p>
      {/if}
      {#each history as b (b.dir)}
        <div class="entry">
          <div class="small">
            <strong>{new Date(b.created).toLocaleString()}</strong>
            <span class="muted">{KIND[b.kind]}</span>
          </div>
          <div class="files">
            {#each b.files as file}
              <button class="file" onclick={() => restoreOne(b, file)} disabled={busy} title="Put this copy back on the card">
                {file}
              </button>
            {/each}
          </div>
        </div>
      {/each}
    </div>
  {/if}

  <ol class="grid">
    {#each slots as slot (slot.index)}
      <li
        class="slot"
        class:empty={!slot.present}
        class:bad={slot.problems.length > 0}
        class:movable={slot.present && !busy}
        class:dragging={dragFrom === slot.index}
        class:target={dropOn === slot.index && dragFrom !== null && dragFrom !== slot.index}
        data-slot={slot.index}
        onpointerdown={(e) => startPress(e, slot)}
      >
        <div class="top">
          <span class="name num">{slot.fileName.replace(".wav", "")}</span>
          {#if slot.present}
            <span class="num">{formatTime(slot.frames)}</span>
          {/if}
        </div>

        {#if slot.present}
          <div class="bar" title="Length against the 2.9 minute limit, with splice markers">
            <div class="fill" style:width="{Math.min(1, slot.frames / MAX_FRAMES) * 100}%">
              {#each slot.markers as m}
                <i style:left="{(m / slot.frames) * 100}%"></i>
              {/each}
            </div>
          </div>
          <div class="meta muted">
            {slot.markers.length + 1} splice{slot.markers.length ? "s" : ""}
          </div>
          {#each slot.problems as problem}
            <div class="error small">{problem}</div>
          {/each}
          <div class="buttons">
            {#if slot.convertible}
              <button onclick={() => convert(slot)} disabled={busy} title="Rewrite it as 48 kHz 32-bit float stereo, keeping its markers">
                Convert
              </button>
            {/if}
            <button onclick={() => onedit(slot.index)} disabled={busy}>Edit</button>
            <button onclick={() => onbuild(slot.index)} disabled={busy}>Replace</button>
            <button class="danger" onclick={() => clear(slot)} disabled={busy}>Remove</button>
          </div>
        {:else}
          <div class="meta muted">Empty</div>
          <div class="buttons">
            <button onclick={() => onbuild(slot.index)} disabled={busy}>Build reel here</button>
          </div>
        {/if}
      </li>
    {/each}
  </ol>
</section>

<style>
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    flex-wrap: wrap;
    margin-bottom: 14px;
  }
  header p {
    margin: 0;
  }
  .actions {
    display: flex;
    gap: 8px;
  }
  .grid {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(190px, 1fr));
    gap: 10px;
  }
  .slot {
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 10px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-height: 128px;
  }
  .slot {
    user-select: none;
    -webkit-user-select: none;
  }
  .slot.movable {
    cursor: grab;
  }
  .slot.dragging {
    opacity: 0.5;
  }
  .panel {
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 10px 12px;
    margin-bottom: 14px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .panel h3 {
    margin: 0;
    font-size: 13px;
  }
  .panel p {
    margin: 0;
  }
  .issues {
    border-color: var(--danger);
  }
  .issue {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }
  .history {
    max-height: 280px;
    overflow: auto;
  }
  .entry {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 4px 0;
    border-top: 1px solid var(--line);
  }
  .entry .muted {
    margin-left: 8px;
  }
  .files {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }
  .file {
    padding: 1px 8px;
    font-size: 12px;
  }
  .actions button.active {
    background: var(--panel-2);
  }
  .slot.empty {
    background: transparent;
    border-style: dashed;
  }
  .slot.bad {
    border-color: var(--danger);
  }
  .slot.target {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .top {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
  }
  .name {
    font-weight: 700;
    font-size: 15px;
  }
  .bar {
    height: 10px;
    background: var(--panel-2);
    border-radius: 3px;
    overflow: hidden;
  }
  .fill {
    position: relative;
    height: 100%;
    background: var(--wave-dim);
  }
  .fill i {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 1px;
    background: var(--marker);
  }
  .meta,
  .small {
    font-size: 12px;
  }
  .buttons {
    margin-top: auto;
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
  }
  .buttons button {
    padding: 2px 8px;
    font-size: 12px;
  }
</style>
