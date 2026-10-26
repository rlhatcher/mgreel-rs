<script lang="ts">
  import { ask, open } from "@tauri-apps/plugin-dialog";
  import { revealItemInDir } from "@tauri-apps/plugin-opener";
  import {
    MAX_FRAMES,
    backupCard,
    backupsFolder,
    clearSlot,
    errorText,
    formatTime,
    moveSlot,
    restoreCard,
    swapSlots,
    type Slot,
  } from "./api";

  type Props = {
    root: string;
    slots: Slot[];
    onedit: (index: number) => void;
    onbuild: (index: number) => void;
    onchanged: () => Promise<void>;
    notify: (text: string, error?: boolean) => void;
  };
  let { root, slots, onedit, onbuild, onchanged, notify }: Props = $props();

  let dragFrom = $state<number | null>(null);
  let dropOn = $state<number | null>(null);
  let busy = $state(false);

  const used = $derived(slots.filter((s) => s.present).length);

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

  function onDrop(event: DragEvent, to: number) {
    event.preventDefault();
    const from = dragFrom;
    dragFrom = dropOn = null;
    if (from === null || from === to) return;
    // Option-drop swaps; a plain drop inserts and shifts the slots between.
    if (event.altKey) {
      run(() => swapSlots(root, from, to), `Swapped ${slots[from].fileName} and ${slots[to].fileName}`);
    } else {
      run(() => moveSlot(root, from, to), `Moved ${slots[from].fileName} to ${slots[to].fileName}`);
    }
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

<section>
  <header>
    <p class="muted">
      {used} of 32 slots used. Drag a reel to move it; hold Option while dropping to swap.
    </p>
    <div class="actions">
      <button onclick={backup} disabled={busy}>Back up card</button>
      <button onclick={restore} disabled={busy}>Restore…</button>
      <button onclick={reveal}>Show backups</button>
    </div>
  </header>

  <ol class="grid">
    {#each slots as slot (slot.index)}
      <li
        class="slot"
        class:empty={!slot.present}
        class:bad={slot.problems.length > 0}
        class:target={dropOn === slot.index && dragFrom !== slot.index}
        draggable={slot.present && !busy}
        ondragstart={(e) => {
          dragFrom = slot.index;
          e.dataTransfer?.setData("text/plain", String(slot.index));
        }}
        ondragend={() => (dragFrom = dropOn = null)}
        ondragover={(e) => {
          if (dragFrom !== null) {
            e.preventDefault();
            dropOn = slot.index;
          }
        }}
        ondragleave={() => dropOn === slot.index && (dropOn = null)}
        ondrop={(e) => onDrop(e, slot.index)}
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
  .slot[draggable="true"] {
    cursor: grab;
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
