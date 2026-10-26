//! Reading source files of other formats.

use mgreel_core::Clip;
use std::path::PathBuf;

fn write(name: &str, spec: hound::WavSpec, samples: &[i32]) -> PathBuf {
    let path = std::env::temp_dir().join(format!("mgreel-src-{}-{name}", std::process::id()));
    let mut w = hound::WavWriter::create(&path, spec).unwrap();
    for &s in samples {
        w.write_sample(s).unwrap();
    }
    w.finalize().unwrap();
    path
}

fn int_spec(bits: u16, rate: u32, channels: u16) -> hound::WavSpec {
    hound::WavSpec {
        channels,
        sample_rate: rate,
        bits_per_sample: bits,
        sample_format: hound::SampleFormat::Int,
    }
}

#[test]
fn integer_sources_keep_their_level() {
    // Half scale at each bit depth must read as 0.5. mgreel 1.0.0 divided
    // every depth by i32::MAX, so 16-bit came out at about 0.0000153.
    let cases = [(16, 1 << 14), (24, 1 << 22), (32, 1 << 30)];
    for (bits, half) in cases {
        let path = write(
            &format!("{bits}.wav"),
            int_spec(bits, 48_000, 2),
            &[half, -half],
        );
        let clip = Clip::open(&path).unwrap();
        assert_eq!((clip.left[0], clip.right[0]), (0.5, -0.5), "{bits}-bit");
    }
}

#[test]
fn mono_is_copied_to_both_channels() {
    let path = write("mono.wav", int_spec(16, 48_000, 1), &[1 << 14, 0]);
    let clip = Clip::open(&path).unwrap();
    assert_eq!(clip.left, clip.right);
    assert_eq!(clip.frames(), 2);
}

#[test]
fn resamples_to_48k_without_delay() {
    // One second at 44.1 kHz with an impulse at 0.5 s.
    let mut samples = vec![0; 44_100];
    samples[22_050] = 1 << 14;
    let path = write("44k.wav", int_spec(16, 44_100, 1), &samples);
    let clip = Clip::open(&path).unwrap();
    assert_eq!(clip.frames(), 48_000);
    let peak = (0..clip.left.len())
        .max_by(|&a, &b| clip.left[a].abs().total_cmp(&clip.left[b].abs()))
        .unwrap();
    assert!(
        (23_999..=24_001).contains(&peak),
        "impulse moved to frame {peak}"
    );
}

#[test]
fn rejects_surround() {
    let path = write("6ch.wav", int_spec(16, 48_000, 6), &[0; 6]);
    assert!(Clip::open(&path).is_err());
}
