# Guide

- [The Library](library.md): the card, its slots, names, the card check,
  backups and history.
- [Splices](splices.md): the editor, playback, markers and every tool.
- [Build](build.md): a new reel from WAV files.
- [Sets](sets.md): whole cards kept on your Mac, and reel packs.
- [options.txt](options.md): the module's firmware options.
- [The command line](command-line.md): `mgreel`.
- [Development](development.md): building, testing and releasing.

## The reel format, in short

From the Morphagene manual:

- A reel is a WAV file at 48 kHz, 32-bit float, stereo.
- It holds at most 2.9 minutes (2:54) and 300 splices.
- Splices are marked by cue points; the module starts a splice at each one.
- Files are named `mg1.wav` to `mg9.wav`, then `mga.wav` to `mgw.wav`: 32
  slots, all at the root of the card.
- `options.txt` at the root holds the firmware options.
- The card must be formatted FAT32.

The app writes reels in this format and warns about anything outside it.
