//! Reels built by the core must match what mgreel 1.0.0 wrote.

use mgreel_core::{BuildOptions, Clip, Reel, ReelFile};
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn float_inputs_match_mgreel_1_0_0() {
    // golden.wav is `mgreel --out golden.wav a.wav b.wav c.wav` from 1.0.0.
    // b.wav is mono, so this also covers mono-to-stereo.
    let clips = ["a.wav", "b.wav", "c.wav"]
        .iter()
        .map(|n| Clip::open(&fixture(n)).unwrap())
        .collect();
    let bytes = Reel::build(clips, BuildOptions::default())
        .to_wav_bytes()
        .unwrap();
    assert_eq!(bytes, std::fs::read(fixture("golden.wav")).unwrap());
}

#[test]
fn golden_reel_reads_back() {
    let reel = Reel::open(&fixture("golden.wav")).unwrap();
    assert_eq!(reel.frames(), 960 + 600 + 480);
    assert_eq!(reel.markers, vec![960, 1560]);
    let file = ReelFile::open(&fixture("golden.wav")).unwrap();
    assert_eq!(file.markers(), vec![960, 1560]);
    assert!(mgreel_core::reel::is_morphagene_format(
        &file.format().unwrap()
    ));
}

#[test]
fn rewriting_markers_keeps_audio() {
    let original = std::fs::read(fixture("golden.wav")).unwrap();
    let mut file = ReelFile::parse(&original).unwrap();
    let audio = file.decode().unwrap();
    file.set_markers(&[100, 200, 300]);
    let edited = ReelFile::parse(&file.to_bytes()).unwrap();
    assert_eq!(edited.markers(), vec![100, 200, 300]);
    assert_eq!(edited.decode().unwrap(), audio);
}
