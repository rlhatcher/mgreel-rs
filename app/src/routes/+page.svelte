<script lang="ts">
  import "../app.css";
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { ask, open } from "@tauri-apps/plugin-dialog";
  import Builder from "$lib/Builder.svelte";
  import Editor from "$lib/Editor.svelte";
  import Library from "$lib/Library.svelte";
  import OptionsEditor from "$lib/OptionsEditor.svelte";
  import Sets from "$lib/Sets.svelte";
  import { errorText, listCards, listSlots, type Card, type CardSnapshot, type Slot } from "$lib/api";

  type Tab = "library" | "editor" | "builder" | "options" | "sets";

  let cards = $state<Card[]>([]);
  /** A folder opened by hand, such as a copy of a card on disk. */
  let manual = $state<Card | null>(null);
  let root = $state<string | null>(null);
  let slots = $state<Slot[]>([]);
  let tab = $state<Tab>("library");
  let editing = $state<number | null>(null);
  let editorDirty = $state(false);
  /** The open card has gone from /Volumes. */
  let cardMissing = $state(false);
  let buildTarget = $state<number | null>(null);
  /** The card the user just ejected, so its going isn't reported as a removal. */
  let ejected: string | null = null;
  let status = $state({ text: "", error: false });
  let statusTimer: ReturnType<typeof setTimeout> | undefined;

  const allCards = $derived(manual && !cards.some((c) => c.root === manual!.root) ? [...cards, manual] : cards);

  function notify(text: string, error = false) {
    status = { text, error };
    clearTimeout(statusTimer);
    if (!error) statusTimer = setTimeout(() => (status = { text: "", error: false }), 6000);
  }

  async function refreshCards() {
    try {
      cards = await listCards();
    } catch (e) {
      notify(errorText(e), true);
    }
    cardMissing = root !== null && !allCards.some((c) => c.root === root);
    // Forget an ejected card once a different card is open.
    if (ejected !== root) ejected = null;
    if (cardMissing && holdingEdits) {
      notify("The card was removed. Put it back to save your marker changes.", true);
    } else {
      dropMissingCard();
    }
  }

  // Unsaved marker edits keep a removed card open, so reinserting it lets
  // them be saved. Once the edits are saved, reverted or discarded, close it.
  const holdingEdits = $derived(tab === "editor" && editorDirty);

  function dropMissingCard() {
    if (cardMissing) {
      if (root === ejected) notify(`Ejected ${root?.split("/").pop()}. You can take the card out.`);
      else notify("The card was removed.", true);
      ejected = null;
      cardMissing = false;
      root = null;
      editing = null;
      // Sets work without a card, so an import in progress keeps its tab.
      if (tab !== "sets") tab = "library";
    }
    if (!root && allCards.length) root = allCards[0].root;
  }

  $effect(() => {
    if (cardMissing && !holdingEdits) dropMissingCard();
  });

  async function refreshSlots() {
    slots = root ? await listSlots(root) : [];
  }

  $effect(() => {
    void root;
    refreshSlots().catch((e) => notify(errorText(e), true));
  });

  async function leaveEditor(): Promise<boolean> {
    if (tab !== "editor" || !editorDirty) return true;
    return ask("Discard unsaved marker changes?", { title: "Unsaved changes", kind: "warning" });
  }

  async function go(next: Tab) {
    if (next !== tab && (await leaveEditor())) tab = next;
  }

  async function edit(index: number) {
    if (!(await leaveEditor())) return;
    editing = index;
    tab = "editor";
  }

  async function buildInto(index: number) {
    buildTarget = index;
    await go("builder");
  }

  async function openFolder() {
    const dir = await open({ directory: true, title: "Open a card or a folder of reels" });
    if (typeof dir !== "string") return;
    if (!(await leaveEditor())) return;
    manual = { root: dir, label: dir.split("/").pop() || dir };
    switchTo(dir);
  }

  async function chooseCard(next: string) {
    if (await leaveEditor()) switchTo(next);
  }

  function switchTo(next: string) {
    cardMissing = false;
    root = next;
    editing = null;
    if (tab === "editor") tab = "library";
  }

  /** Cmd-1 to Cmd-5 open the tabs in the order they appear. */
  function onKey(e: KeyboardEvent) {
    if (!(e.metaKey || e.ctrlKey) || e.shiftKey || e.altKey) return;
    const order: Tab[] = ["library", "editor", "builder", "options", "sets"];
    const next = order[Number(e.key) - 1];
    if (!next) return;
    e.preventDefault();
    if (next === "builder" || next === "sets" || (root && (next !== "editor" || editing !== null))) go(next);
  }

  onMount(() => {
    refreshCards();
    const unlisten = listen("cards-changed", () => refreshCards());
    const unlistenSnapshot = listen<CardSnapshot>("card-snapshot", ({ payload: s }) => {
      if (s.error) {
        notify(`Couldn't copy the reels on ${s.label} to the backups folder: ${s.error}`, true);
      } else {
        const n = s.files.length;
        const names = s.files.map((f) => f.replace(".wav", "")).join(", ");
        notify(`Kept a copy of ${n} reel${n === 1 ? "" : "s"} on ${s.label} the app hadn't seen yet (${names}). See History.`);
      }
    });
    return () => {
      unlisten.then((f) => f());
      unlistenSnapshot.then((f) => f());
    };
  });
</script>

<svelte:window onkeydown={onKey} />

<div class="app">
  <nav>
    <div class="brand">Morphagene Reels</div>
    <div class="tabs">
      <button class:active={tab === "library"} onclick={() => go("library")} disabled={!root}>Library</button>
      <button class:active={tab === "editor"} onclick={() => go("editor")} disabled={!root || editing === null}>
        Splices{editing !== null && slots[editing] ? `: ${slots[editing].fileName}` : ""}
      </button>
      <button class:active={tab === "builder"} onclick={() => go("builder")}>Build</button>
      <button class:active={tab === "options"} onclick={() => go("options")} disabled={!root}>options.txt</button>
      <button class:active={tab === "sets"} onclick={() => go("sets")}>Sets</button>
    </div>
    <div class="card">
      {#if allCards.length}
        <select
          value={root}
          onchange={async (e) => {
            const el = e.currentTarget;
            await chooseCard(el.value);
            // After a cancel, show the card that is still open.
            el.value = root ?? "";
          }}
        >
          {#each allCards as card}
            <option value={card.root}>{card.label}</option>
          {/each}
        </select>
      {/if}
      <button onclick={openFolder}>Open folder…</button>
    </div>
  </nav>

  <main>
    {#if !root && tab !== "builder" && tab !== "sets"}
      <div class="empty">
        <h1>Insert the Morphagene's microSD card</h1>
        <p class="muted">
          It shows up here when it mounts. You can also open a folder that holds reels, such as a
          backup, or build a reel to a file without a card.
        </p>
        <div class="row">
          <button onclick={openFolder}>Open folder…</button>
          <button onclick={() => go("builder")}>Build a reel</button>
        </div>
      </div>
    {:else if tab === "library" && root}
      <Library
        {root}
        {slots}
        onedit={edit}
        onbuild={buildInto}
        onchanged={refreshSlots}
        onejected={(card) => {
          ejected = card;
          refreshCards();
        }}
        {notify}
      />
    {:else if tab === "editor" && root && editing !== null && slots[editing]}
      {#key `${root}:${editing}`}
        <Editor
          {root}
          slot={slots[editing]}
          bind:dirty={editorDirty}
          onsaved={(s) => (slots[s.index] = s)}
          {notify}
        />
      {/key}
    {:else if tab === "builder"}
      <Builder {root} {slots} bind:target={buildTarget} {notify} onbuilt={refreshSlots} />
    {:else if tab === "options" && root}
      {#key root}
        <OptionsEditor {root} {notify} />
      {/key}
    {:else if tab === "sets"}
      <Sets {root} onchanged={refreshSlots} {notify} />
    {/if}
  </main>

  {#if status.text}
    <footer class:error={status.error}>
      <span>{status.text}</span>
      <button onclick={() => (status = { text: "", error: false })}>Dismiss</button>
    </footer>
  {/if}
</div>

<style>
  .app {
    display: flex;
    flex-direction: column;
    height: 100vh;
  }
  nav {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 10px 16px;
    border-bottom: 1px solid var(--line);
    background: var(--panel);
  }
  .brand {
    font-weight: 700;
    letter-spacing: 0.02em;
  }
  .tabs {
    display: flex;
    gap: 4px;
  }
  .tabs button {
    background: transparent;
    border-color: transparent;
  }
  .tabs button.active {
    background: var(--panel-2);
    border-color: var(--line);
    font-weight: 600;
  }
  .card {
    margin-left: auto;
    display: flex;
    gap: 8px;
  }
  main {
    flex: 1;
    overflow: auto;
    padding: 16px;
  }
  .empty {
    max-width: 520px;
    margin: 12vh auto 0;
    text-align: center;
  }
  .empty h1 {
    font-size: 20px;
  }
  .row {
    display: flex;
    gap: 8px;
    justify-content: center;
  }
  footer {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 8px 16px;
    border-top: 1px solid var(--line);
    background: var(--panel);
  }
  footer span {
    flex: 1;
    overflow-wrap: anywhere;
  }
  footer.error span {
    color: var(--danger);
  }
</style>
