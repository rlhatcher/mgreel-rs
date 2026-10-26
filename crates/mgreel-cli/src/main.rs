/*
 * ================================== mgreel ===============================
 *
 * mgreel is a Rust CLI tool to convert one or more Wave files to a
 * single Make Noise Morphagene reel (Wave file 32bit/48khz/stereo).
 *
 * All files are concatenated and splice markers are set at the end of each
 * input files. Mono Wave files are converted to stereo.
 *
 * Example:
 *      mgreel --out out.wav input1.wav input2.wav
 *
 */

use anyhow::{Context, Result};
use clap::Parser;
use mgreel_core::{BuildOptions, Clip, Reel};
use std::io::Write;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "mgreel")]
#[command(about = "Convert Wave files to Make Noise Morphagene reel format", long_about = None)]
struct Args {
    /// Output file path (if not specified, writes to stdout)
    #[arg(short, long)]
    out: Option<PathBuf>,

    /// Input WAV files
    #[arg(required = true)]
    input_files: Vec<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let clips = args
        .input_files
        .iter()
        .map(|path| Clip::open(path).with_context(|| format!("Failed to process file: {:?}", path)))
        .collect::<Result<Vec<_>>>()?;

    let reel = Reel::build(clips, BuildOptions::default());
    for problem in reel.problems() {
        eprintln!("warning: the Morphagene will not load this reel: {problem}");
    }
    let bytes = reel.to_wav_bytes()?;

    if let Some(path) = args.out {
        std::fs::write(path, bytes)?;
    } else {
        std::io::stdout().write_all(&bytes)?;
    }

    Ok(())
}
