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
cargo install --locked --git https://github.com/rlhatcher/mgreel-rs.git
```

Or `cargo install --locked --path .` from a checkout.

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

## How it works

1. Reads each input WAV file
2. Converts to 32-bit float samples
3. Resamples to 48kHz if necessary
4. Converts mono to stereo (duplicates channel)
5. Concatenates all samples
6. Adds cue points at splice boundaries
7. Writes final WAV file with proper Morphagene format

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
