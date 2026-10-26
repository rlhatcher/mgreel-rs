<script lang="ts">
  import "../app.css";
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { ask, open } from "@tauri-apps/plugin-dialog";
  import Builder from "$lib/Builder.svelte";
  import Editor from "$lib/Editor.svelte";
  import Library from "$lib/Library.svelte";
  import OptionsEditor from "$lib/OptionsEditor.svelte";
  import { errorText, listCards, listSlots, type Card, type Slot } from "$lib/api";

  type Tab = "library" | "editor" | "builder" | "options";

  let cards = $state<Card[]>([]);
  /** A folder opened by hand, such as a copy of a card on disk. */
  let manual = $state<Card | null>(null);
  let root = $state<string | null>(null);
  let slots = $state<Slot[]>([]);
  let tab = $state<Tab>("library");
  let editing = $state<number | null>(null);
  let editorDirty = $state(false);
  let buildTarget = $state<number | null>(null);
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
    if (root && !allCards.some((c) => c.root === root)) {
      notify("The card was removed.", true);
      root = null;
      editing = null;
      tab = "library";
    }
    if (!root && allCards.length) root = allCards[0].root;
  }

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
    manual = { root: dir, label: dir.split("/").pop() || dir };
    root = dir;
  }

  async function chooseCard(next: string) {
    if (await leaveEditor()) {
      root = next;
      editing = null;
      if (tab === "editor") tab = "library";
    }
  }

  onMount(() => {
    refreshCards();
    const unlisten = listen("cards-changed", () => refreshCards());
    return () => {
      unlisten.then((f) => f());
    };
  });
</script>

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
    {#if !root && tab !== "builder"}
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
      <Library {root} {slots} onedit={edit} onbuild={buildInto} onchanged={refreshSlots} {notify} />
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
