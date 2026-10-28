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
    previewReel,
    removeClutter,
    removeNames,
    restoreCard,
    restoreFile,
    SAMPLE_RATE,
    setReelName,
    swapSlots,
    type Backup,
    type BackupKind,
    type CardCheck,
    type Slot,
  } from "./api";
  import { reelSpots } from "./reelmode";
  import { nameSaves, queueNameSave } from "./saves";

  type Props = {
    root: string;
    slots: Slot[];
    onedit: (index: number) => void;
    onbuild: (index: number) => void;
    onchanged: () => Promise<void>;
    /** Called with the card that was ejected. */
    onejected: (root: string) => void;
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
    !!check &&
      (check.wrongFilesystem !== null ||
        check.stray.length > 0 ||
        check.clutter > 0 ||
        check.namesError !== null),
  );
  const spots = $derived(reelSpots(slots));
  const cardName = $derived(root.split("/").pop() || root);

  // Look the card over whenever it or its slots change.
  /** The card the shown check belongs to. */
  let checkedRoot: string | null = null;
  $effect(() => {
    const r = root;
    void slots;
    // Another card's result, and its fix buttons, must not show for this one.
    if (r !== checkedRoot) {
      check = null;
      checkedRoot = r;
    }
    recheck(r);
  });

  /** Numbers each check, so only the latest one shows its result. */
  let checkRun = 0;

  /** A check that fails clears the last result, so it can't look clean. */
  function recheck(r: string) {
    const run = ++checkRun;
    const latest = () => run === checkRun && r === root;
    return checkCard(r)
      .then((c) => {
        if (latest()) check = c;
      })
      .catch((e) => {
        if (!latest()) return;
        check = null;
        notify(errorText(e), true);
      });
  }

  /**
   * Change the card shown now. The action gets that card, so switching cards
   * while queued name saves finish can't send it to the other one.
   */
  async function run(action: (card: string) => Promise<unknown>, done: string) {
    const card = root;
    // The reels may move or change, so the preview stops first.
    stopPlaying();
    busy = true;
    try {
      // A queued name save names a slot, so it lands before reels move.
      await nameSaves();
      await action(card);
      await onchanged();
      notify(done);
    } catch (e) {
      notify(errorText(e), true);
    } finally {
      busy = false;
    }
  }

  /**
   * After a dialog: true when the card it was opened on is still the one
   * shown. A card taken out while the dialog is up switches to another, and
   * the action must not run there.
   */
  function stillOn(card: string) {
    if (card === root) return true;
    notify("The card changed or was removed while the dialog was open, so nothing was changed.", true);
    return false;
  }

  // Reels move with pointer events, not HTML5 drag and drop, because the
  // Tauri window takes drag events for its own file drop.
  /** A press on a reel. It turns into a drag once the pointer moves. */
  let press: { index: number; x: number; y: number } | null = null;
  let dragFrom = $state<number | null>(null);
  let dropOn = $state<number | null>(null);

  function startPress(e: PointerEvent, slot: Slot) {
    if (e.button !== 0) return;
    selected = slot.index;
    const onControl = !!(e.target as Element).closest("button, input");
    if (!slot.present || busy || onControl) return;
    press = { index: slot.index, x: e.clientX, y: e.clientY };
  }

  /** The slot picked in the list; the toolbar acts on it. */
  let selected = $state<number | null>(null);
  const sel = $derived(selected === null ? null : (slots[selected] ?? null));
  /**
   * A double-click opens the tile. The browser sends it only once both
   * clicks are done, so pressing twice to start a drag doesn't open it, and
   * a double-click in the name or note selects a word as usual.
   */
  function doubleClick(e: MouseEvent, index: number) {
    if ((e.target as Element).closest("button, input")) return;
    openSlot(index);
  }

  /** How many tiles sit in a row of the grid now. */
  function columns() {
    const grid = document.querySelector("ol.grid");
    if (!grid) return 1;
    return getComputedStyle(grid).gridTemplateColumns.split(" ").filter(Boolean).length || 1;
  }

  /** Edit a reel, or build one into an empty slot. */
  function openSlot(index: number) {
    const slot = slots[index];
    if (!slot || busy) return;
    if (slot.present) onedit(index);
    else onbuild(index);
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      cancelDrag();
      return;
    }
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    // Keys typed into a field or pressed on a button are theirs.
    if ((e.target as Element | null)?.closest?.("input, textarea, select, button")) return;
    const across = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
    const down = e.key === "ArrowDown" ? 1 : e.key === "ArrowUp" ? -1 : 0;
    if (across || down) {
      e.preventDefault();
      const step = across || down * columns();
      const next = selected === null ? 0 : Math.max(0, Math.min(slots.length - 1, selected + step));
      selected = next;
      document.querySelector(`li[data-slot="${next}"]`)?.scrollIntoView({ block: "nearest" });
    } else if (e.key === "Enter" && selected !== null) {
      e.preventDefault();
      openSlot(selected);
    }
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
    // The picked reel goes with it.
    selected = to;
    // Option-drop swaps; a plain drop inserts and shifts the slots between.
    if (e.altKey) {
      run((card) => swapSlots(card, from, to), `Swapped ${slots[from].fileName} and ${slots[to].fileName}`);
    } else {
      run((card) => moveSlot(card, from, to), `Moved ${slots[from].fileName} to ${slots[to].fileName}`);
    }
  }

  function cancelDrag() {
    press = null;
    dragFrom = dropOn = null;
  }

  function convert(slot: Slot) {
    run((card) => convertSlot(card, slot.index), `Converted ${slot.fileName} to 48 kHz 32-bit float stereo`);
  }

  function adopt(path: string, name: string) {
    const target = freeSlot;
    if (!target) return;
    run((card) => adoptFile(card, path, target.index), `Put ${name} in ${target.fileName}`);
  }

  function tidy() {
    busy = true;
    removeClutter(root)
      .then((n) => notify(`Removed ${n} macOS metadata file${n === 1 ? "" : "s"}`))
      .catch((e) => notify(errorText(e), true))
      .finally(async () => {
        busy = false;
        await recheck(root);
      });
  }

  /**
   * Save a name or note when its field loses focus or Enter is pressed. Both
   * fields are read from the card, not from `slot`, which may not have
   * caught up with the save before this one.
   */
  function saveLabel(slot: Slot, field: "name" | "note", input: HTMLInputElement) {
    const fields = input.closest("li")!.querySelectorAll<HTMLInputElement>("input.label");
    const name = fields[0].value.trim();
    const note = fields[1].value.trim();
    const index = slot.index;
    // The card can change before a queued save runs; the save stays with
    // the card it was typed on.
    const card = root;
    queueNameSave(async () => {
      // The slots as the last save left them, while still on that card.
      const now = card === root ? slots[index] : null;
      if (now && name === now.name && note === now.note) return;
      try {
        await setReelName(card, index, name, note);
        if (card === root) await onchanged();
      } catch (e) {
        if (card === root) input.value = slots[index][field];
        notify(errorText(e), true);
      }
    });
  }

  function labelKey(e: KeyboardEvent, slot: Slot, field: "name" | "note") {
    const input = e.currentTarget as HTMLInputElement;
    if (e.key === "Enter") input.blur();
    if (e.key === "Escape") {
      input.value = slot[field];
      input.blur();
    }
  }

  async function dropNames() {
    const card = root;
    const yes = await ask(
      "Remove mgreel.json? Every reel on the card loses its name and note. A copy goes to the backups folder.",
      { title: "Remove reel names", kind: "warning" },
    );
    if (!yes || !stillOn(card)) return;
    await run((card) => removeNames(card), "Removed mgreel.json");
    await recheck(root);
  }

  // One reel plays at a time, from its start, like the module's Reel mode
  // preview.
  let playing = $state<number | null>(null);
  /** The source is null while the reel loads. */
  let player: { ctx: AudioContext; source: AudioBufferSourceNode | null } | null = null;
  /** Numbers each play, so a reel that finishes loading late doesn't start. */
  let playRun = 0;

  function stopPlaying() {
    playRun++;
    player?.source?.stop();
    player?.ctx.close();
    player = null;
    playing = null;
  }

  async function togglePlay(slot: Slot) {
    const again = playing === slot.index;
    stopPlaying();
    if (again) return;
    const run = playRun;
    playing = slot.index;
    const fail = (e: unknown) => {
      if (run === playRun) {
        stopPlaying();
        notify(errorText(e), true);
      }
    };
    try {
      // Made during the click: WebKit may keep a context made after a long
      // load suspended, and the reel can take a while to load.
      const ctx = new AudioContext({ sampleRate: SAMPLE_RATE });
      const mine: { ctx: AudioContext; source: AudioBufferSourceNode | null } = { ctx, source: null };
      player = mine;
      if (ctx.state === "suspended") ctx.resume().catch(fail);
      const reel = await previewReel(root, slot.index);
      if (run !== playRun) return;
      const buffer = ctx.createBuffer(2, Math.max(1, reel.left.length), SAMPLE_RATE);
      buffer.copyToChannel(reel.left as Float32Array<ArrayBuffer>, 0);
      buffer.copyToChannel(reel.right as Float32Array<ArrayBuffer>, 1);
      const source = ctx.createBufferSource();
      source.buffer = buffer;
      source.connect(ctx.destination);
      source.onended = () => {
        if (run === playRun) stopPlaying();
      };
      source.start();
      mine.source = source;
    } catch (e) {
      fail(e);
    }
  }

  // Switching cards stops the preview and closes another card's history,
  // and so does leaving.
  $effect(() => {
    void root;
    return () => {
      stopPlaying();
      history = null;
      selected = null;
    };
  });

  async function eject() {
    // The card picker stays live while saves finish, so hold on to this card.
    const card = root;
    busy = true;
    try {
      await nameSaves();
      await ejectCard(card);
      onejected(card);
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
    const card = root;
    try {
      const list = await listBackups(card);
      if (card === root) history = list;
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
    const card = root;
    const yes = await ask(`Put this copy of ${file} back on the card? What's there now goes to the backups folder first.`, {
      title: "Restore file",
      kind: "warning",
    });
    if (!yes || !stillOn(card)) return;
    await run((card) => restoreFile(card, backup.dir, file), `Restored ${file}`);
    const list = await listBackups(card).catch(() => null);
    if (list && card === root && history) history = list;
  }

  async function clear(slot: Slot) {
    const card = root;
    const yes = await ask(`Remove ${slot.fileName} from the card? A copy goes to the backups folder.`, {
      title: "Remove reel",
      kind: "warning",
    });
    if (yes && stillOn(card)) run((card) => clearSlot(card, slot.index), `Removed ${slot.fileName}`);
  }

  async function backup() {
    const card = root;
    busy = true;
    try {
      await nameSaves();
      const dir = await backupCard(card);
      notify(`Backed up to ${dir}`);
    } catch (e) {
      notify(errorText(e), true);
    } finally {
      busy = false;
    }
  }

  async function restore() {
    const card = root;
    const from = await open({ directory: true, defaultPath: await backupsFolder(card), title: "Choose a backup folder" });
    if (typeof from !== "string") return;
    const yes = await ask(`Replace every reel and options.txt on the card with the contents of ${from}? The card is backed up first.`, {
      title: "Restore backup",
      kind: "warning",
    });
    if (yes && stillOn(card)) run((card) => restoreCard(card, from), "Restored backup");
  }

  async function reveal() {
    await revealItemInDir(await backupsFolder(root));
  }
</script>

<svelte:window
  onpointermove={onPointerMove}
  onpointerup={onPointerUp}
  onpointercancel={cancelDrag}
  onkeydown={onKey}
/>

<section>
  <header class="page-header">
    <p><strong>{cardName}</strong> <span class="muted">· {used} of 32 slots used</span></p>
    <div class="actions">
      <button onclick={backup} disabled={busy}>Back up card</button>
      <button onclick={restore} disabled={busy}>Restore…</button>
      <button onclick={toggleHistory} class:active={history !== null}>History</button>
      <button onclick={reveal} title="Open this card's backups folder">Show in Finder</button>
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
      {#if check.namesError}
        <div class="issue">
          <span class="small">
            mgreel.json, where the app keeps reel names, can't be read: {check.namesError}. Until it's fixed or
            removed, the app won't move, remove or name reels.
          </span>
          <button onclick={dropNames} disabled={busy}>Remove</button>
        </div>
      {/if}
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

  <div class="toolbar">
    {#if sel?.present}
      <span class="picked"><strong class="num">{sel.fileName.replace(".wav", "")}</strong> {sel.name}</span>
      {#if sel.problems.length}
        <span class="error small problems">{sel.problems.join("; ")}</span>
      {/if}
      <div class="actions">
        <button onclick={() => onbuild(sel.index)} disabled={busy} title="Build a new reel into this slot">Replace…</button>
        {#if sel.convertible}
          <button onclick={() => convert(sel)} disabled={busy} title="Rewrite it as 48 kHz 32-bit float stereo, keeping its markers">
            Convert
          </button>
        {/if}
        <button class="danger" onclick={() => clear(sel)} disabled={busy}>Remove</button>
      </div>
    {:else if sel}
      <span class="picked"><strong class="num">{sel.fileName.replace(".wav", "")}</strong> <span class="muted">empty</span></span>
      <span class="muted small">Build a reel here with the tile's Build button.</span>
    {:else}
      <span class="muted small">
        Pick a tile for more actions. Drag a reel to move it, Option-drag to swap.
      </span>
    {/if}
  </div>

  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <ol
    class="grid"
    tabindex="0"
    aria-label="Reel slots. Arrow keys pick a slot, Enter opens it."
  >
    {#each slots as slot (slot.index)}
      {@const spot = spots.get(slot.index)}
      <li
        class="slot"
        class:empty={!slot.present}
        class:bad={slot.problems.length > 0}
        class:selected={selected === slot.index}
        class:movable={slot.present && !busy}
        class:dragging={dragFrom === slot.index}
        class:target={dropOn === slot.index && dragFrom !== null && dragFrom !== slot.index}
        data-slot={slot.index}
        onpointerdown={(e) => startPress(e, slot)}
        ondblclick={(e) => doubleClick(e, slot.index)}
      >
        <div class="top">
          <span class="id">
            {#if spot}
              <span
                class="spot"
                title="Reel {spot.position + 1} of {spot.count} in Reel mode: {spot.colour.name}, Organize near {spot.clock}"
              >
                <i class="dot" style:background={spot.colour.css}></i>
                <svg class="dial" viewBox="-8 -8 16 16" aria-hidden="true">
                  <circle r="7" />
                  <line x2="0" y2="-6" transform="rotate({spot.angle})" />
                </svg>
              </span>
            {/if}
            <span class="name num">{slot.fileName.replace(".wav", "")}</span>
            {#if slot.problems.length}
              <span class="problem" title={slot.problems.join("\n")}>!</span>
            {/if}
          </span>
          {#if slot.present}
            <span class="num">{formatTime(slot.frames)}</span>
          {/if}
        </div>

        {#if slot.present}
          <input
            class="label"
            value={slot.name}
            placeholder="Name"
            aria-label="Name of {slot.fileName}"
            maxlength="80"
            disabled={busy}
            onchange={(e) => saveLabel(slot, "name", e.currentTarget)}
            onkeydown={(e) => labelKey(e, slot, "name")}
          />
          <input
            class="label note"
            value={slot.note}
            placeholder="Note"
            aria-label="Note for {slot.fileName}"
            maxlength="300"
            disabled={busy}
            onchange={(e) => saveLabel(slot, "note", e.currentTarget)}
            onkeydown={(e) => labelKey(e, slot, "note")}
          />
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
          <div class="buttons">
            <button
              onclick={() => togglePlay(slot)}
              disabled={busy || !slot.formatOk}
              title={slot.formatOk ? "Play the reel from its start" : "Convert it to play it here"}
            >
              {playing === slot.index ? "Stop" : "Play"}
            </button>
            <button onclick={() => onedit(slot.index)} disabled={busy} title="Edit the splices">Edit</button>
          </div>
        {:else}
          <div class="meta muted">Empty</div>
          <div class="buttons">
            <button onclick={() => onbuild(slot.index)} disabled={busy} title="Build a reel into this slot">Build</button>
          </div>
        {/if}
      </li>
    {/each}
  </ol>
</section>

<style>
  .toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
    gap: var(--s2) var(--s3);
    min-height: 36px;
    padding: var(--s1) var(--s3);
    margin-bottom: var(--s2);
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 8px;
  }
  .grid:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 4px;
    border-radius: 8px;
  }
  .problems {
    flex: 1;
    min-width: 0;
  }
  .picked {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .grid {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(190px, 1fr));
    gap: 10px;
  }
  /* Every tile has the same parts, so a row of tiles lines up. */
  .slot {
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 10px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-height: 150px;
    user-select: none;
    -webkit-user-select: none;
  }
  .slot.movable {
    cursor: grab;
  }
  .slot.selected {
    border-color: var(--accent);
    box-shadow: 0 0 0 1px var(--accent);
  }
  .slot.dragging {
    opacity: 0.5;
  }
  .slot.empty {
    background: transparent;
    border-style: dashed;
  }
  .slot.empty.selected {
    border-style: solid;
  }
  .slot.bad:not(.selected) {
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
  .id {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .spot {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .name {
    font-weight: 700;
    font-size: 15px;
  }
  .meta {
    font-size: var(--t-small);
  }
  .buttons {
    margin-top: auto;
    display: flex;
    justify-content: space-between;
    gap: 6px;
  }
  .buttons button {
    padding: 2px 10px;
    font-size: var(--t-small);
  }
  .problem {
    display: inline-block;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    background: var(--danger);
    color: var(--panel);
    font-size: 11px;
    font-weight: 700;
    line-height: 16px;
    text-align: center;
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

  .dot {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    box-shadow: 0 0 0 1px var(--line);
  }
  .dial {
    width: 14px;
    height: 14px;
  }
  .dial circle {
    fill: none;
    stroke: var(--line);
    stroke-width: 1.5;
  }
  .dial line {
    stroke: currentColor;
    stroke-width: 1.5;
    stroke-linecap: round;
  }
  .label {
    font-size: 12px;
    padding: 2px 6px;
    user-select: text;
    -webkit-user-select: text;
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

</style>
