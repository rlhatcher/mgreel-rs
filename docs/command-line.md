# The command line

`mgreel` joins WAV files into one Morphagene reel, with a splice at the start
of each file. It's the [Build](build.md) page without the app, and works on
any system Rust does.

## Install

```bash
cargo install --locked --git https://github.com/rlhatcher/mgreel-rs.git mgreel-rs
```

The package is `mgreel-rs`; the command it installs is `mgreel`.

## Use

```bash
mgreel --out reel.wav one.wav two.wav three.wav
```

Without `--out` it writes the reel to standard output:

```bash
mgreel one.wav two.wav > reel.wav
```

Every WAV file in a folder, in the order the shell lists them:

```bash
mgreel --out reel.wav samples/*.wav
```

`--keep-markers` also turns the cue points saved in the files, and the region
ends RX and Pro Tools save, into splices:

```bash
mgreel --keep-markers --out reel.wav take1.wav take2.wav
```

## What it does

1. Reads each file: 8, 16, 24 or 32-bit PCM, or 32 or 64-bit float, mono or
   stereo. PCM is scaled by its own bit depth, so a 16-bit file keeps its
   level.
2. Resamples to 48 kHz with sinc interpolation where needed.
3. Copies mono to both channels.
4. Joins the files, with a cue point where each one starts, plus its own
   markers with `--keep-markers`.
5. Writes a 48 kHz 32-bit float stereo WAV.
6. Warns if the reel is longer than 2.9 minutes or has more than 300 splices,
   the module's limits.
