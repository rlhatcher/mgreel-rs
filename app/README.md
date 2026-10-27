# Morphagene Reels

Desktop app for a Make Noise Morphagene microSD card, built on
`mgreel-core` with Tauri v2 and SvelteKit.

- Library: the card's 32 reel slots. Drag to move a reel, Option-drag to
  swap, back up and restore the whole card, eject it.
  - Card check: flags reels the module can't play and converts them to
    48 kHz 32-bit float stereo with their markers, puts WAV files with the
    wrong name or in a folder into a free slot, removes macOS `._` and
    `.DS_Store` files, and warns when the card isn't FAT32.
  - When a card mounts, the app copies any reel it hasn't seen, such as
    one recorded or spliced on the module, before anything can replace it.
    History lists every copy and puts any single file back.
  - Names and notes: give each reel a name and a short note. The app keeps
    them in `mgreel.json` at the card root, moves them with the reel when
    you move or swap it in the app, and includes them in card backups.
    Names don't follow a reel that is moved or deleted on the module or in
    Finder.
  - Reel mode: each reel shows the colour the module gives it in Reel mode
    and a dial at about the Organize position that selects it. Play
    previews a reel from its start.
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
  - Import markers from an Audacity label file, a REAPER marker export
    (.csv, with time as minutes:seconds or seconds), an Ableton Live set's
    locators (at the set's tempo; a set whose tempo changes gets a
    warning), or a WAV file's cue points and region ends. A region's end
    becomes a marker too. If the audio was exported from later than the
    start of the timeline, say how much later.
  - Tidy, for reels recorded or spliced on the module: delete silent
    splices, trim the silence at each splice's start (keeping 5 ms before
    the sound), and merge splices shorter than a length into a neighbour.
    Locked markers stay.
  - Undo and redo for all of it, and a spectrogram view.
- Build: join WAV files into a new reel, with trims, normalize and fades.
  Markers saved in the files (cue points, and the region ends RX and Pro
  Tools write) split them further, unless you turn that off.
- options.txt: edit firmware options; only the values change.
- Sets: whole cards kept in `~/Music/Morphagene Sets`, one folder per set
  laid out like a card (reels, `options.txt`, `mgreel.json`). Save the open
  card as a set, load a set onto a card (the card is backed up first), or
  import a folder of WAV files such as a reel pack. Import converts each
  file to 48 kHz 32-bit float stereo with its markers. Files named `mg1.wav`
  to `mgw.wav` keep their slots; the rest fill the free slots in Finder's
  name order, subfolders included, and each file name becomes the reel's
  name. A folder with more than 32 WAV files is refused.

Every change to the card copies the old file to
`~/Library/Application Support/com.rlhatcher.mgreel/backups/` first. The
first time the app sees a card it copies every reel there, so expect that
folder to hold about one card's worth per card.

## Develop

```bash
npm install
npm run tauri dev
```

`npm run check` type-checks the frontend. `cargo test --workspace` from the
repo root tests the Rust side.
