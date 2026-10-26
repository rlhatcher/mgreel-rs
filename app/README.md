# Morphagene Reels

Desktop app for a Make Noise Morphagene microSD card, built on
`mgreel-core` with Tauri v2 and SvelteKit.

- Library: the card's 32 reel slots. Drag to move a reel, Option-drag to
  swap, back up and restore the whole card.
- Splices: the reel editor.
  - Playback that behaves like the module: the selected splice loops, and
    Organize (0 to 5 V) or Shift picks the next one, which starts when the
    current splice ends. Vari-Speed up 12 and down 26 semitones, either
    direction.
  - Markers: add, drag, lock, snap to zero crossings, auto-slice for any
    sound, equal split. The splice list shows each splice's Organize
    voltage range and flags splices that jump from end to start.
  - Splice edits: cut, copy and paste (also between reels), duplicate,
    delete, drag to reorder, shuffle, reverse, gain, normalize, match
    levels, fades, smooth loop, remove DC, high-pass, and a tape-style speed
    change.
  - Add audio: insert WAV files as new splices (or drop them on the
    editor), replace a splice with a file, mix a file in with an S.O.S.
    balance, export splices as WAV files.
  - Undo and redo for all of it, and a spectrogram view.
- Build: join WAV files into a new reel, with trims, normalize and fades.
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
