//! Reels: stereo 48 kHz 32-bit float audio plus splice markers.

use crate::riff::{self, Chunk, Format};
use anyhow::{bail, Context, Result};
use hound::{WavSpec, WavWriter};
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use std::hash::{DefaultHasher, Hasher};
use std::path::Path;

pub const SAMPLE_RATE: u32 = 48_000;
/// 2.9 minutes, the most a reel can hold (manual p.36).
pub const MAX_FRAMES: u32 = 8_352_000;
/// 299 markers make the 300-splice limit (manual p.15).
pub const MAX_MARKERS: usize = 299;

/// Stereo audio at 48 kHz, ready to go into a reel.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Clip {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl Clip {
    /// Read any PCM or float WAV, converting it to stereo at 48 kHz.
    /// Mono is copied to both channels; more than two channels is an error.
    pub fn open(path: &Path) -> Result<Clip> {
        let bytes = std::fs::read(path)?;
        let file = ReelFile::parse(&bytes)?;
        let format = file.format()?;
        let (left, right) = file.decode()?;
        if format.sample_rate == SAMPLE_RATE {
            return Ok(Clip { left, right });
        }
        Ok(Clip {
            left: resample(&left, format.sample_rate, SAMPLE_RATE)?,
            right: resample(&right, format.sample_rate, SAMPLE_RATE)?,
        })
    }

    pub fn frames(&self) -> u32 {
        self.left.len() as u32
    }

    /// Keep frames `start..end`, clamped to the clip.
    pub fn trim(&mut self, start: u32, end: u32) {
        let end = (end as usize).min(self.left.len());
        let start = (start as usize).min(end);
        self.left.truncate(end);
        self.right.truncate(end);
        self.left.drain(..start);
        self.right.drain(..start);
    }

    /// Scale so the loudest sample in either channel hits `peak`.
    pub fn normalize(&mut self, peak: f32) {
        let max = self
            .left
            .iter()
            .chain(&self.right)
            .fold(0.0f32, |m, s| m.max(s.abs()));
        if max > 0.0 {
            let gain = peak / max;
            self.left
                .iter_mut()
                .chain(&mut self.right)
                .for_each(|s| *s *= gain);
        }
    }

    /// Linear fade in and out over `frames` at each end, to avoid clicks at
    /// splice boundaries.
    pub fn fade_edges(&mut self, frames: u32) {
        let n = (frames as usize).min(self.left.len() / 2);
        let len = self.left.len();
        for i in 0..n {
            let g = i as f32 / n as f32;
            for ch in [&mut self.left, &mut self.right] {
                ch[i] *= g;
                ch[len - 1 - i] *= g;
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BuildOptions {
    /// Normalize each clip to this peak (linear) before joining.
    pub normalize: Option<f32>,
    /// Fade each clip's edges over this many frames.
    pub fade_frames: u32,
}

/// A reel held in memory.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reel {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    /// Frame offsets where a new splice starts. Never 0.
    pub markers: Vec<u32>,
}

impl Reel {
    /// Join clips end to end with a marker at the start of each clip after
    /// the first.
    pub fn build(clips: Vec<Clip>, options: BuildOptions) -> Reel {
        let total: usize = clips.iter().map(|c| c.left.len()).sum();
        let mut reel = Reel {
            left: Vec::with_capacity(total),
            right: Vec::with_capacity(total),
            markers: Vec::new(),
        };
        for mut clip in clips {
            if let Some(peak) = options.normalize {
                clip.normalize(peak);
            }
            if options.fade_frames > 0 {
                clip.fade_edges(options.fade_frames);
            }
            if !reel.left.is_empty() {
                reel.markers.push(reel.frames());
            }
            reel.left.extend_from_slice(&clip.left);
            reel.right.extend_from_slice(&clip.right);
        }
        reel
    }

    pub fn open(path: &Path) -> Result<Reel> {
        Reel::from_file(&ReelFile::open(path)?)
    }

    pub fn from_file(file: &ReelFile) -> Result<Reel> {
        let (left, right) = file.decode()?;
        let markers = file.markers();
        let format = file.format()?;
        if format.sample_rate == SAMPLE_RATE {
            return Ok(Reel {
                left,
                right,
                markers,
            });
        }
        let ratio = SAMPLE_RATE as f64 / format.sample_rate as f64;
        let left = resample(&left, format.sample_rate, SAMPLE_RATE)?;
        let right = resample(&right, format.sample_rate, SAMPLE_RATE)?;
        let markers = markers
            .iter()
            .map(|&m| (m as f64 * ratio).round() as u32)
            .collect();
        let markers = clean_markers(markers, left.len() as u32);
        Ok(Reel {
            left,
            right,
            markers,
        })
    }

    pub fn frames(&self) -> u32 {
        self.left.len() as u32
    }

    /// Encode as a Morphagene WAV: 48 kHz, 32-bit float, stereo, with a
    /// `cue ` chunk ahead of the audio when there are markers.
    pub fn to_wav_bytes(&self) -> Result<Vec<u8>> {
        let spec = WavSpec {
            channels: 2,
            sample_rate: SAMPLE_RATE,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut buffer = Vec::new();
        {
            let mut writer = WavWriter::new(std::io::Cursor::new(&mut buffer), spec)?;
            for (l, r) in self.left.iter().zip(&self.right) {
                writer.write_sample(*l)?;
                writer.write_sample(*r)?;
            }
            writer.finalize()?;
        }
        if self.markers.is_empty() {
            return Ok(buffer);
        }
        let mut file = ReelFile::parse(&buffer)?;
        file.set_markers(&self.markers);
        Ok(file.to_bytes())
    }

    pub fn problems(&self) -> Vec<Problem> {
        problems(self.frames(), self.markers.len(), None)
    }
}

/// Something that stops the Morphagene from loading a reel.
#[derive(Debug, Clone, PartialEq)]
pub enum Problem {
    TooLong { frames: u32 },
    TooManyMarkers { count: usize },
    WrongFormat { format: Format },
    Unreadable { reason: String },
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Problem::TooLong { frames } => write!(
                f,
                "{:.1} s long, the limit is {:.1} s",
                *frames as f64 / SAMPLE_RATE as f64,
                MAX_FRAMES as f64 / SAMPLE_RATE as f64
            ),
            Problem::TooManyMarkers { count } => {
                write!(f, "{count} splice markers, the limit is {MAX_MARKERS}")
            }
            Problem::WrongFormat { format } => write!(
                f,
                "{} Hz {}-bit {} {}-channel, needs 48000 Hz 32-bit float stereo",
                format.sample_rate,
                format.bits_per_sample,
                if format.float { "float" } else { "PCM" },
                format.channels
            ),
            Problem::Unreadable { reason } => write!(f, "unreadable: {reason}"),
        }
    }
}

pub fn problems(frames: u32, markers: usize, format: Option<Format>) -> Vec<Problem> {
    let mut out = Vec::new();
    if let Some(format) = format {
        if !is_morphagene_format(&format) {
            out.push(Problem::WrongFormat { format });
        }
    }
    if frames > MAX_FRAMES {
        out.push(Problem::TooLong { frames });
    }
    if markers > MAX_MARKERS {
        out.push(Problem::TooManyMarkers { count: markers });
    }
    out
}

pub fn is_morphagene_format(format: &Format) -> bool {
    format.float
        && format.channels == 2
        && format.sample_rate == SAMPLE_RATE
        && format.bits_per_sample == 32
}

/// Sort, de-duplicate and drop markers that can't start a splice.
pub fn clean_markers(mut markers: Vec<u32>, frames: u32) -> Vec<u32> {
    markers.retain(|&m| m > 0 && m < frames);
    markers.sort_unstable();
    markers.dedup();
    markers
}

/// The frame range of each splice.
pub fn splice_ranges(markers: &[u32], frames: u32) -> Vec<std::ops::Range<u32>> {
    let mut starts = vec![0];
    starts.extend(markers.iter().copied().filter(|&m| m > 0 && m < frames));
    starts
        .iter()
        .zip(starts.iter().skip(1).chain(std::iter::once(&frames)))
        .map(|(&s, &e)| s..e)
        .collect()
}

/// A WAV file kept as chunks, so markers can change without touching the
/// audio or any chunk this crate doesn't understand.
#[derive(Debug, Clone)]
pub struct ReelFile {
    pub chunks: Vec<Chunk>,
}

impl ReelFile {
    pub fn parse(bytes: &[u8]) -> Result<ReelFile> {
        Ok(ReelFile {
            chunks: riff::parse(bytes)?,
        })
    }

    pub fn open(path: &Path) -> Result<ReelFile> {
        ReelFile::parse(&std::fs::read(path)?)
    }

    fn chunk(&self, id: &[u8; 4]) -> Option<&Chunk> {
        self.chunks.iter().find(|c| &c.id == id)
    }

    pub fn format(&self) -> Result<Format> {
        Format::parse(&self.chunk(b"fmt ").context("no fmt chunk")?.data)
    }

    pub fn frames(&self) -> Result<u32> {
        let data = self.chunk(b"data").map_or(0, |c| c.data.len());
        Ok((data / self.format()?.frame_bytes().max(1)) as u32)
    }

    /// A hash of the format and audio. Markers live in other chunks, so
    /// saving markers keeps it the same, and a different reel at the same
    /// path almost certainly changes it. Only compare values from the same
    /// build: the hash isn't stable across Rust versions.
    pub fn audio_id(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        for id in [b"fmt ", b"data"] {
            // The length keeps a byte from counting toward either chunk.
            let data = self.chunk(id).map_or(&[][..], |c| &c.data);
            hasher.write_u64(data.len() as u64);
            hasher.write(data);
        }
        hasher.finish()
    }

    pub fn markers(&self) -> Vec<u32> {
        self.chunk(b"cue ")
            .map(|c| riff::decode_cues(&c.data))
            .unwrap_or_default()
    }

    /// Replace the `cue ` chunk. Labels in a `LIST`/`adtl` chunk point at
    /// the old cue IDs, so that chunk goes too.
    pub fn set_markers(&mut self, markers: &[u32]) {
        self.chunks
            .retain(|c| &c.id != b"cue " && !(&c.id == b"LIST" && c.data.starts_with(b"adtl")));
        if markers.is_empty() {
            return;
        }
        let at = self
            .chunks
            .iter()
            .position(|c| &c.id == b"data")
            .unwrap_or(self.chunks.len());
        self.chunks.insert(at, riff::encode_cues(markers));
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        riff::serialize(&self.chunks)
    }

    /// Decode the audio to stereo floats. Integer formats scale by their own
    /// bit depth, so 16-bit sources keep their level.
    pub fn decode(&self) -> Result<(Vec<f32>, Vec<f32>)> {
        let format = self.format()?;
        let data = &self.chunk(b"data").context("no data chunk")?.data;
        let width = format.bits_per_sample as usize / 8;
        let channels = format.channels as usize;
        if channels == 0 || channels > 2 {
            bail!("Unsupported number of channels: {channels}");
        }
        let sample = |b: &[u8]| -> Result<f32> {
            Ok(match (format.float, width) {
                (true, 4) => f32::from_le_bytes(b.try_into().unwrap()),
                (true, 8) => f64::from_le_bytes(b.try_into().unwrap()) as f32,
                (false, 1) => (b[0] as f32 - 128.0) / 128.0,
                (false, 2) => i16::from_le_bytes([b[0], b[1]]) as f32 / 32_768.0,
                (false, 3) => (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0,
                (false, 4) => i32::from_le_bytes(b.try_into().unwrap()) as f32 / 2_147_483_648.0,
                _ => bail!(
                    "unsupported sample format: {} bits {}",
                    format.bits_per_sample,
                    if format.float { "float" } else { "PCM" }
                ),
            })
        };
        let frames = data.len() / (width * channels).max(1);
        let mut left = Vec::with_capacity(frames);
        let mut right = Vec::with_capacity(frames);
        for frame in data.chunks_exact(width * channels) {
            let l = sample(&frame[..width])?;
            let r = if channels == 2 {
                sample(&frame[width..])?
            } else {
                l
            };
            left.push(l);
            right.push(r);
        }
        Ok((left, right))
    }

    /// Min and max of both channels over `buckets` equal slices, for drawing
    /// a waveform without shipping the audio to the UI.
    pub fn peaks(&self, buckets: usize) -> Result<Peaks> {
        let (left, right) = self.decode()?;
        Ok(Peaks::compute(&left, &right, buckets))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Peaks {
    pub min: Vec<f32>,
    pub max: Vec<f32>,
}

impl Peaks {
    pub fn compute(left: &[f32], right: &[f32], buckets: usize) -> Peaks {
        let frames = left.len();
        let buckets = buckets.max(1).min(frames.max(1));
        let mut peaks = Peaks {
            min: vec![0.0; buckets],
            max: vec![0.0; buckets],
        };
        for i in 0..buckets {
            let range = i * frames / buckets..(i + 1) * frames / buckets;
            for s in left[range.clone()].iter().chain(&right[range]) {
                peaks.min[i] = peaks.min[i].min(*s);
                peaks.max[i] = peaks.max[i].max(*s);
            }
        }
        peaks
    }
}

fn resample(samples: &[f32], from_rate: u32, to_rate: u32) -> Result<Vec<f32>> {
    if from_rate == to_rate {
        return Ok(samples.to_vec());
    }
    resample_by(samples, to_rate as f64 / from_rate as f64)
}

/// Resample to `ratio` times as many samples. Above 1 the sound gets
/// longer and lower when played at the same rate, like slowed tape.
pub(crate) fn resample_by(samples: &[f32], ratio: f64) -> Result<Vec<f32>> {
    if ratio == 1.0 || samples.is_empty() {
        return Ok(samples.to_vec());
    }

    let params = SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 256,
        window: WindowFunction::BlackmanHarris2,
    };

    // A single chunk stops half a filter length short of the input's end,
    // so pad with silence to get the tail out, then cut to length.
    let wanted = (samples.len() as f64 * ratio).round() as usize;
    let mut padded = samples.to_vec();
    padded.resize(samples.len() + params.sinc_len, 0.0);

    let mut resampler = SincFixedIn::<f32>::new(ratio, 2.0, params, padded.len(), 1)?;
    let mut out = resampler
        .process(&[padded], None)?
        .into_iter()
        .next()
        .unwrap();
    out.resize(wanted, 0.0);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reel_file(samples: &[f32]) -> ReelFile {
        let clip = Clip {
            left: samples.to_vec(),
            right: samples.to_vec(),
        };
        let bytes = Reel::build(vec![clip], BuildOptions::default())
            .to_wav_bytes()
            .unwrap();
        ReelFile::parse(&bytes).unwrap()
    }

    #[test]
    fn audio_id_follows_the_audio_not_the_markers() {
        let mut file = reel_file(&[0.1, 0.2, 0.3, 0.4]);
        let id = file.audio_id();
        file.set_markers(&[2]);
        assert_eq!(file.audio_id(), id);
        assert_ne!(reel_file(&[0.1, 0.2, 0.3, 0.5]).audio_id(), id);
    }

    #[test]
    fn splice_ranges_cover_the_reel() {
        assert_eq!(splice_ranges(&[10, 30], 50), vec![0..10, 10..30, 30..50]);
        assert_eq!(splice_ranges(&[], 50), vec![0..50]);
    }

    #[test]
    fn clean_markers_drops_edges_and_duplicates() {
        assert_eq!(clean_markers(vec![0, 40, 10, 10, 50, 60], 50), vec![10, 40]);
    }

    #[test]
    fn limits() {
        assert!(problems(MAX_FRAMES, MAX_MARKERS, None).is_empty());
        assert_eq!(
            problems(MAX_FRAMES + 1, MAX_MARKERS + 1, None),
            vec![
                Problem::TooLong {
                    frames: MAX_FRAMES + 1
                },
                Problem::TooManyMarkers {
                    count: MAX_MARKERS + 1
                }
            ]
        );
    }

    #[test]
    fn trim_and_fade() {
        let mut clip = Clip {
            left: vec![1.0; 10],
            right: vec![1.0; 10],
        };
        clip.trim(2, 8);
        assert_eq!(clip.frames(), 6);
        clip.fade_edges(2);
        assert_eq!(clip.left, vec![0.0, 0.5, 1.0, 1.0, 0.5, 0.0]);
    }

    #[test]
    fn normalize_hits_peak() {
        let mut clip = Clip {
            left: vec![0.25, -0.5],
            right: vec![0.1, 0.2],
        };
        clip.normalize(1.0);
        assert_eq!(clip.left, vec![0.5, -1.0]);
    }

    #[test]
    fn set_markers_drops_labels_and_keeps_other_chunks() {
        let reel = Reel {
            left: vec![0.0; 100],
            right: vec![0.0; 100],
            markers: vec![40],
        };
        let mut file = ReelFile::parse(&reel.to_wav_bytes().unwrap()).unwrap();
        file.chunks.push(Chunk {
            id: *b"LIST",
            data: b"adtllabl".to_vec(),
        });
        file.chunks.push(Chunk {
            id: *b"LIST",
            data: b"INFOISFT".to_vec(),
        });
        file.set_markers(&[20, 60]);
        let ids: Vec<_> = file.chunks.iter().map(|c| c.id).collect();
        assert_eq!(ids, vec![*b"fmt ", *b"cue ", *b"data", *b"LIST"]);
        assert_eq!(file.markers(), vec![20, 60]);
        assert_eq!(file.frames().unwrap(), 100);
    }

    #[test]
    fn peaks_bucket_min_max() {
        let peaks = Peaks::compute(&[0.5, -0.5, 0.1, 0.2], &[0.0, 0.0, -0.3, 0.0], 2);
        assert_eq!(peaks.min, vec![-0.5, -0.3]);
        assert_eq!(peaks.max, vec![0.5, 0.2]);
    }
}
