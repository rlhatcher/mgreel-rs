<script lang="ts">
  import { onMount } from "svelte";
  import { ask, open } from "@tauri-apps/plugin-dialog";
  import { revealItemInDir } from "@tauri-apps/plugin-opener";
  import {
    errorText,
    importPack,
    listSets,
    restoreCard,
    saveSet,
    setsFolder,
    type CardSet,
    type PackImport,
  } from "./api";
  import { nameSaves } from "./saves";

  type Props = {
    /** The open card, or null when there is none. */
    root: string | null;
    onchanged: () => Promise<void>;
    notify: (text: string, error?: boolean) => void;
  };
  let { root, onchanged, notify }: Props = $props();

  let sets = $state<CardSet[] | null>(null);
  let busy = $state(false);
  let saveName = $state("");
  /** What the last import did, until it's closed. */
  let report = $state<PackImport | null>(null);

  const label = (card: string) => card.split("/").pop() || card;

  async function refresh() {
    try {
      sets = await listSets();
    } catch (e) {
      notify(errorText(e), true);
    }
  }

  onMount(refresh);

  async function save(e: SubmitEvent) {
    e.preventDefault();
    const card = root;
    const name = saveName.trim();
    if (!card || !name) return;
    busy = true;
    try {
      // A name typed just before must be in the set.
      await nameSaves();
      await saveSet(card, name);
      saveName = "";
      notify(`Saved ${label(card)} as the set ${name}`);
      await refresh();
    } catch (e) {
      notify(errorText(e), true);
    } finally {
      busy = false;
    }
  }

  async function load(set: CardSet) {
    const card = root;
    if (!card) return;
    const yes = await ask(
      `Replace every reel, options.txt and the reel names on ${label(card)} with the set ${set.name}? The card is backed up first.`,
      { title: "Load set", kind: "warning" },
    );
    if (!yes) return;
    // A card taken out while the dialog was up may have been replaced by
    // another, and the set must not land there.
    if (card !== root) {
      notify("The card changed or was removed while the dialog was open, so nothing was changed.", true);
      return;
    }
    busy = true;
    try {
      // A name save that landed after the load would name the set's reel.
      await nameSaves();
      await restoreCard(card, set.dir);
      await onchanged();
      notify(`Loaded ${set.name} onto ${label(card)}`);
    } catch (e) {
      notify(errorText(e), true);
    } finally {
      busy = false;
    }
  }

  async function importFolder() {
    const from = await open({ directory: true, title: "Choose a folder of WAV files" });
    if (typeof from !== "string") return;
    busy = true;
    try {
      report = await importPack(from);
      notify(`Imported ${report.placed.length} reel${report.placed.length === 1 ? "" : "s"} as ${report.name}`);
      await refresh();
    } catch (e) {
      notify(errorText(e), true);
    } finally {
      busy = false;
    }
  }

  async function reveal(dir?: string) {
    try {
      await revealItemInDir(dir ?? (await setsFolder()));
    } catch (e) {
      notify(errorText(e), true);
    }
  }

  /** The first few reel names, and how many more there are. */
  function preview(names: string[]) {
    const shown = names.slice(0, 6).join(", ");
    return names.length > 6 ? `${shown} and ${names.length - 6} more` : shown;
  }
</script>

<section>
  <header>
    <p class="muted">
      A set is a whole card kept in Music/Morphagene Sets: its reels, options.txt and reel names. Loading one
      backs the card up, then replaces everything on it.
    </p>
    <div class="actions">
      <form onsubmit={save}>
        <input
          bind:value={saveName}
          placeholder="Set name"
          aria-label="Set name"
          disabled={!root || busy}
          title={root ? "" : "Open a card to save it as a set"}
        />
        <button type="submit" disabled={!root || busy || !saveName.trim()}>Save card as set</button>
      </form>
      <button onclick={importFolder} disabled={busy} title="Make a set from a folder of WAV files, such as a reel pack">
        Import folder…
      </button>
      <button onclick={() => reveal()}>Show in Finder</button>
    </div>
  </header>

  {#if report}
    <div class="panel">
      <div class="title">
        <h3>Imported {report.name}</h3>
        <button onclick={() => (report = null)}>Close</button>
      </div>
      {#each report.placed as p}
        <div class="small">
          <span class="num">{p.fileName.replace(".wav", "")}</span>
          {p.source}
          {#if p.problems.length}
            <span class="error">: {p.problems.join(", ")}, so the module won't load it</span>
          {/if}
        </div>
      {/each}
      {#each report.skipped as s}
        <div class="small error">Left out {s.source}: {s.reason}</div>
      {/each}
    </div>
  {/if}

  {#if sets === null}
    <p class="muted">Loading sets…</p>
  {:else if sets.length === 0}
    <p class="muted">No sets yet. Save a card as a set, or import a folder of WAV files.</p>
  {:else}
    <ul>
      {#each sets as set (set.dir)}
        <li>
          <div class="info">
            <strong>{set.name}</strong>
            <span class="muted small">
              {set.reels.length} reel{set.reels.length === 1 ? "" : "s"}{set.hasOptions ? ", options.txt" : ""},
              {new Date(set.modified).toLocaleString()}
            </span>
            {#if set.names.length}
              <span class="small">{preview(set.names)}</span>
            {/if}
          </div>
          <div class="actions">
            <button
              onclick={() => load(set)}
              disabled={!root || busy}
              title={root ? `Replace everything on ${label(root)} with this set` : "Open a card to load a set onto it"}
            >
              Load onto card
            </button>
            <button onclick={() => reveal(set.dir)}>Show</button>
          </div>
        </li>
      {/each}
    </ul>
  {/if}
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
    max-width: 520px;
  }
  .actions,
  form {
    display: flex;
    gap: 8px;
  }
  .panel {
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 10px 12px;
    margin-bottom: 14px;
    display: flex;
    flex-direction: column;
    gap: 4px;
    max-height: 280px;
    overflow: auto;
  }
  .title {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .panel h3 {
    margin: 0;
    font-size: 13px;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  li {
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 10px 12px;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }
  .info {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .info span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .small {
    font-size: 12px;
  }
</style>
