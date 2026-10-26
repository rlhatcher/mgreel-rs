<script lang="ts">
  import { onMount } from "svelte";
  import { errorText, readOptions, writeOptions, writeOptionsText, type Options } from "./api";

  type Props = { root: string; notify: (text: string, error?: boolean) => void };
  let { root, notify }: Props = $props();

  let options = $state<Options | null>(null);
  let values = $state<Record<string, string>>({});
  let raw = $state(false);
  let text = $state("");
  let saving = $state(false);

  const changes = $derived(
    (options?.entries ?? [])
      .filter((e) => values[e.key] !== e.value)
      .map((e) => ({ key: e.key, value: values[e.key] })),
  );
  const dirty = $derived(raw ? text !== options?.text : changes.length > 0);

  async function load() {
    try {
      options = await readOptions(root);
      values = Object.fromEntries(options.entries.map((e) => [e.key, e.value]));
      text = options.text;
    } catch (e) {
      notify(errorText(e), true);
    }
  }

  async function saveAll() {
    saving = true;
    try {
      if (raw) await writeOptionsText(root, text);
      else await writeOptions(root, changes);
      notify("Saved options.txt. Put the card back in the Morphagene to apply it.");
      await load();
    } catch (e) {
      notify(errorText(e), true);
    } finally {
      saving = false;
    }
  }

  onMount(load);
</script>

<section>
  {#if !options}
    <p class="muted">Loading…</p>
  {:else if !options.exists}
    <p class="muted">
      This card has no options.txt. The Morphagene writes one the first time it mounts the card with
      firmware MG155 or later.
    </p>
  {:else}
    <header>
      <p class="muted">
        {#if options.firmware}Written by firmware {options.firmware}.{/if}
        0 is the default for every choice. Only the value changes; the comments the Morphagene wrote stay as they are.
      </p>
      <label><input type="checkbox" bind:checked={raw} /> Edit as text</label>
      <button onclick={load} disabled={!dirty || saving}>Revert</button>
      <button class="primary" onclick={saveAll} disabled={!dirty || saving}>
        {saving ? "Saving…" : "Save to card"}
      </button>
    </header>

    {#if raw}
      <textarea class="num" bind:value={text} spellcheck="false"></textarea>
    {:else}
      <div class="grid">
        {#each options.entries as entry (entry.key)}
          <div class="row" class:changed={values[entry.key] !== entry.value}>
            <div>
              <div class="name">{entry.name ?? entry.key}</div>
              <div class="muted small"><span class="num">{entry.key}</span> · {entry.comment}</div>
            </div>
            {#if entry.choices}
              <select bind:value={values[entry.key]}>
                {#each entry.choices as choice}
                  <option value={choice.value}>{choice.value}: {choice.label}</option>
                {/each}
              </select>
            {:else if entry.ratio}
              <input
                class="num"
                type="number"
                step="0.00001"
                min="-16"
                max="16"
                title="0.0625 to 16; negative plays the gene in reverse"
                bind:value={
                  () => values[entry.key],
                  (v) => (values[entry.key] = Number(v).toFixed(5))
                }
              />
            {:else}
              <input class="num" bind:value={values[entry.key]} />
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  {/if}
</section>

<style>
  header {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-bottom: 14px;
  }
  header p {
    margin: 0;
    flex: 1;
  }
  header label {
    display: flex;
    gap: 4px;
    align-items: center;
  }
  .grid {
    display: grid;
    gap: 6px;
  }
  .row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 320px;
    gap: 16px;
    align-items: center;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 8px 12px;
  }
  .row.changed {
    border-color: var(--accent);
  }
  .name {
    font-weight: 600;
  }
  .small {
    font-size: 12px;
  }
  select,
  input {
    width: 100%;
  }
  textarea {
    width: 100%;
    min-height: 420px;
    font-size: 12px;
    white-space: pre;
  }
</style>
