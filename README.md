# mgreel (Rust version)

A Rust implementation of [mgreel](https://github.com/olt/mgreel) - a command-line tool to convert Wave audio files into a format compatible with the Make Noise Morphagene eurorack module.

> **Note**: This is a Rust port of the original JavaScript implementation by Oliver Tonnhofer. The original project can be found at https://github.com/olt/mgreel

## Features

- Concatenates multiple Wave files into a single output file
- Converts all audio to Morphagene's required format: 32-bit float, 48kHz, stereo
- Automatically sets splice markers (cue points) at the end of each input file
- Converts mono files to stereo
- High-quality resampling using sinc interpolation
- Fast and memory-efficient

## Installation

### From source

```bash
cargo build --release
```

The binary will be available at `target/release/mgreel`

### Install globally

```bash
cargo install --locked --git https://github.com/rlhatcher/mgreel-rs.git mgreel-rs
```

Or `cargo install --locked --path crates/mgreel-cli` from a checkout.

### The app

With Homebrew, on an Apple Silicon Mac:

```bash
brew tap rlhatcher/mgreel https://github.com/rlhatcher/mgreel-rs
brew trust rlhatcher/mgreel
brew install --cask rlhatcher/mgreel/morphagene-reels
```

The app is ad-hoc signed, not notarized. The cask clears the quarantine flag
Homebrew sets, so it opens without a warning. Each release on GitHub also has
`Morphagene-Reels-<version>-arm64.dmg`; installed from that, macOS blocks the
first launch until you allow it in System Settings, Privacy & Security, Open
Anyway.

### Releasing

1. Set the same version in `Cargo.toml` (workspace), `app/package.json` and
   `app/src-tauri/tauri.conf.json`, commit it and push it to `main`.
2. Tag that commit and push the tag:
   `git tag v1.2.0 && git push origin v1.2.0`.
3. The Release workflow builds the DMG and attaches it to a new GitHub
   release. It fails if the tag doesn't match the version in
   `tauri.conf.json`. Then it commits the new version and sha256 to
   `Casks/morphagene-reels.rb` on `main`, so `brew upgrade` picks it up. A
   failed run can be rerun; an older release never replaces a newer one in
   the cask.

## Layout

The repo is a Cargo workspace:

- `crates/mgreel-core`: library for reels (WAV and `cue ` chunk I/O,
  resampling, splice markers, limit checks, auto-slicing at phrase or
  gesture level from level and spectrum alone, so it suits found sound and
  field recordings as well as music), the SD card (32 slots, swap,
  reorder, backup, restore) and `options.txt` (edits change only the value,
  as the manual asks).
- `crates/mgreel-cli`: the `mgreel` binary, package name `mgreel-rs`.

## Usage

```bash
mgreel --out output.wav input1.wav input2.wav input3.wav
```

Or pipe to stdout:

```bash
mgreel input1.wav input2.wav > output.wav
```

### Examples

Convert all WAV files in a directory:

```bash
mgreel --out morphagene-reel.wav samples/*.wav
```

Keep the markers saved in the files, such as cue points from a DAW or the
region ends RX and Pro Tools write, as splices too:

```bash
mgreel --keep-markers --out morphagene-reel.wav take1.wav take2.wav
```

## How it works

1. Reads each input WAV file (8, 16, 24 or 32-bit PCM, or 32/64-bit float)
2. Converts to 32-bit float samples, scaled by the source's bit depth
3. Resamples to 48kHz if necessary
4. Converts mono to stereo (duplicates channel)
5. Concatenates all samples
6. Adds cue points at splice boundaries
7. Writes final WAV file with proper Morphagene format
8. Warns if the reel is over 2.9 minutes or has more than 299 markers, the
   Morphagene's limits

## Dependencies

- `clap` - Command-line argument parsing
- `hound` - WAV file reading and writing
- `rubato` - High-quality audio resampling
- `anyhow` - Error handling

## Differences from JavaScript version

- Written in Rust for better performance and memory safety
- Uses high-quality sinc interpolation for resampling
- Statically typed with compile-time guarantees
- Single binary with no runtime dependencies

## Acknowledgments

This is a Rust port of the original [mgreel](https://github.com/olt/mgreel) JavaScript implementation by Oliver Tonnhofer. The original project provided the foundation and specification for this implementation.

## License

ISC License

Copyright (c) 2020, Oliver Tonnhofer (original JavaScript implementation)
Copyright (c) 2026, [Your Name] (Rust port)

See LICENSE file for full text.

## Authors

- **Original JavaScript version**: Oliver Tonnhofer (2020) - https://github.com/olt/mgreel
- **Rust port**: [Your Name] (2026)
