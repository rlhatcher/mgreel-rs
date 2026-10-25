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
use hound::{WavReader, WavSpec, WavWriter};
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use std::io::Write;
use std::path::PathBuf;

const TARGET_SAMPLE_RATE: u32 = 48000;
const TARGET_BIT_DEPTH: u16 = 32;

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

struct Splice {
    left: Vec<f32>,
    right: Vec<f32>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    if args.input_files.is_empty() {
        eprintln!("call `mgreel --out out.wav input1.wav input2.wav ...`");
        std::process::exit(1);
    }

    let mut splices = Vec::new();
    for input_file in &args.input_files {
        let splice = process_wav_file(input_file)
            .with_context(|| format!("Failed to process file: {:?}", input_file))?;
        splices.push(splice);
    }

    let total_length: usize = splices.iter().map(|s| s.left.len()).sum();

    // Concatenate all samples
    let mut samples_l = Vec::with_capacity(total_length);
    let mut samples_r = Vec::with_capacity(total_length);

    for splice in &splices {
        samples_l.extend_from_slice(&splice.left);
        samples_r.extend_from_slice(&splice.right);
    }

    // Create cue points at the end of each splice (except the first)
    let mut cue_points = Vec::new();
    let mut current_offset = 0u32;
    for splice in &splices {
        if current_offset > 0 {
            cue_points.push(current_offset);
        }
        current_offset += splice.left.len() as u32;
    }

    write_morphagene_wav(&args.out, &samples_l, &samples_r, &cue_points)?;

    Ok(())
}

fn process_wav_file(path: &PathBuf) -> Result<Splice> {
    let mut reader = WavReader::open(path).context("Failed to open WAV file")?;
    let spec = reader.spec();

    // Read samples as f32
    let samples: Vec<f32> = if spec.sample_format == hound::SampleFormat::Float {
        reader.samples::<f32>().collect::<Result<Vec<_>, _>>()?
    } else {
        reader
            .samples::<i32>()
            .map(|s| s.map(|sample| sample as f32 / i32::MAX as f32))
            .collect::<Result<Vec<_>, _>>()?
    };

    // Split into channels
    let (left, right) = match spec.channels {
        1 => {
            // Mono: duplicate to both channels
            (samples.clone(), samples)
        }
        2 => {
            // Stereo: separate left and right
            let left: Vec<f32> = samples.iter().step_by(2).copied().collect();
            let right: Vec<f32> = samples.iter().skip(1).step_by(2).copied().collect();
            (left, right)
        }
        _ => anyhow::bail!("Unsupported number of channels: {}", spec.channels),
    };

    // Resample if necessary
    let (left, right) = if spec.sample_rate != TARGET_SAMPLE_RATE {
        (
            resample(&left, spec.sample_rate, TARGET_SAMPLE_RATE)?,
            resample(&right, spec.sample_rate, TARGET_SAMPLE_RATE)?,
        )
    } else {
        (left, right)
    };

    Ok(Splice { left, right })
}

fn resample(samples: &[f32], from_rate: u32, to_rate: u32) -> Result<Vec<f32>> {
    if from_rate == to_rate {
        return Ok(samples.to_vec());
    }

    let params = SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 256,
        window: WindowFunction::BlackmanHarris2,
    };

    let mut resampler = SincFixedIn::<f32>::new(
        to_rate as f64 / from_rate as f64,
        2.0,
        params,
        samples.len(),
        1,
    )?;

    let waves_in = vec![samples.to_vec()];
    let waves_out = resampler.process(&waves_in, None)?;

    Ok(waves_out[0].clone())
}

fn write_morphagene_wav(
    output: &Option<PathBuf>,
    samples_l: &[f32],
    samples_r: &[f32],
    cue_points: &[u32],
) -> Result<()> {
    let spec = WavSpec {
        channels: 2,
        sample_rate: TARGET_SAMPLE_RATE,
        bits_per_sample: TARGET_BIT_DEPTH,
        sample_format: hound::SampleFormat::Float,
    };

    let mut buffer = Vec::new();
    {
        let mut writer = WavWriter::new(std::io::Cursor::new(&mut buffer), spec)?;

        // Interleave samples
        for i in 0..samples_l.len() {
            writer.write_sample(samples_l[i])?;
            writer.write_sample(samples_r[i])?;
        }

        writer.finalize()?;
    }

    // Now we need to manually add the cue chunk
    // The hound library doesn't support cue points, so we'll insert them manually
    if !cue_points.is_empty() {
        buffer = add_cue_chunk(buffer, cue_points)?;
    }

    if let Some(path) = output {
        std::fs::write(path, buffer)?;
    } else {
        std::io::stdout().write_all(&buffer)?;
    }

    Ok(())
}

fn add_cue_chunk(mut wav_data: Vec<u8>, cue_points: &[u32]) -> Result<Vec<u8>> {
    // Find the position right before the "data" chunk
    // WAV structure: RIFF header (12 bytes) + fmt chunk + data chunk
    // We need to insert the cue chunk before the data chunk

    let data_pos = wav_data
        .windows(4)
        .position(|w| w == b"data")
        .context("Could not find data chunk")?;

    // Build cue chunk
    let num_cue_points = cue_points.len() as u32;
    let cue_chunk_size = 4 + (num_cue_points * 24); // 4 bytes for count + 24 bytes per cue point

    let mut cue_chunk = Vec::new();
    cue_chunk.extend_from_slice(b"cue ");
    cue_chunk.extend_from_slice(&cue_chunk_size.to_le_bytes());
    cue_chunk.extend_from_slice(&num_cue_points.to_le_bytes());

    for (i, &position) in cue_points.iter().enumerate() {
        let cue_id = (i + 1) as u32;
        cue_chunk.extend_from_slice(&cue_id.to_le_bytes()); // dwName (cue point ID)
        cue_chunk.extend_from_slice(&position.to_le_bytes()); // dwPosition (same as dwSampleOffset for MG)
        cue_chunk.extend_from_slice(b"data"); // fccChunk
        cue_chunk.extend_from_slice(&0u32.to_le_bytes()); // dwChunkStart
        cue_chunk.extend_from_slice(&0u32.to_le_bytes()); // dwBlockStart
        cue_chunk.extend_from_slice(&position.to_le_bytes()); // dwSampleOffset
    }

    // Insert cue chunk before data chunk
    wav_data.splice(data_pos..data_pos, cue_chunk);

    // Update RIFF chunk size (at bytes 4-7)
    let new_size = (wav_data.len() - 8) as u32;
    wav_data[4..8].copy_from_slice(&new_size.to_le_bytes());

    Ok(wav_data)
}
