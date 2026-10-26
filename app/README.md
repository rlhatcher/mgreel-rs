# Morphagene Reels

Desktop app for a Make Noise Morphagene microSD card, built on
`mgreel-core` with Tauri v2 and SvelteKit.

- Library: the card's 32 reel slots. Drag to move a reel, Option-drag to
  swap, back up and restore the whole card.
- Splices: waveform editor with playback, splice looping and auto-slicing.
- Build: join WAV files into a reel, with trims, normalize and fades.
- options.txt: edit firmware options; only the values change.

Every change to the card copies the old file to
`~/Library/Application Support/com.rlhatcher.mgreel/backups/` first.

## Develop

```bash
npm install
npm run tauri dev
```

`npm run check` type-checks the frontend. `cargo test --workspace` from the
repo root tests the Rust side.
