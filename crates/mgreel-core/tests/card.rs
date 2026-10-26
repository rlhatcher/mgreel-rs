use mgreel_core::card::{slot_file_name, Card};
use mgreel_core::{BuildOptions, Clip, OptionsFile, Reel};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

fn tempdir() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "mgreel-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A reel whose length identifies it.
fn reel_bytes(frames: usize) -> Vec<u8> {
    let clip = Clip {
        left: vec![0.25; frames],
        right: vec![-0.25; frames],
    };
    Reel::build(vec![clip.clone(), clip], BuildOptions::default())
        .to_wav_bytes()
        .unwrap()
}

fn frames_by_slot(card: &Card) -> Vec<(usize, u32)> {
    card.slots()
        .into_iter()
        .filter(|s| s.path.is_some())
        .map(|s| (s.index, s.frames))
        .collect()
}

fn make_card(root: &Path, slots: &[(usize, usize)]) -> Card {
    for &(index, frames) in slots {
        fs::write(root.join(slot_file_name(index)), reel_bytes(frames)).unwrap();
    }
    Card::new(root)
}

#[test]
fn lists_slots_from_headers() {
    let root = tempdir();
    let card = make_card(&root, &[(0, 100), (9, 50)]);
    fs::write(root.join("._mg2.wav"), b"junk").unwrap();
    let slots = card.slots();
    assert_eq!(slots.len(), 32);
    assert_eq!(slots[0].frames, 200);
    assert_eq!(slots[0].markers, vec![100]);
    assert!(slots[0].problems.is_empty());
    assert!(slots[1].path.is_none());
    assert_eq!(slots[9].file_name, "mga.wav");
    assert!(Card::looks_like_card(&root));
    assert_eq!(
        Card::detect(root.parent().unwrap())
            .iter()
            .filter(|c| c.root == root)
            .count(),
        1
    );
}

#[test]
fn flags_wrong_format() {
    let root = tempdir();
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 44_100,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(root.join("mg1.wav"), spec).unwrap();
    w.write_sample(0i16).unwrap();
    w.finalize().unwrap();
    let slot = &Card::new(&root).slots()[0];
    assert_eq!(slot.problems.len(), 1);
    assert!(slot.problems[0]
        .to_string()
        .contains("needs 48000 Hz 32-bit float stereo"));
}

#[test]
fn swap_and_move() {
    let root = tempdir();
    let card = make_card(&root, &[(0, 10), (1, 20), (2, 30)]);
    card.swap(0, 5).unwrap();
    assert_eq!(frames_by_slot(&card), vec![(1, 40), (2, 60), (5, 20)]);
    // Drag slot 5 to the front: 0..4 shift down by one.
    card.move_slot(5, 0).unwrap();
    assert_eq!(frames_by_slot(&card), vec![(0, 20), (2, 40), (3, 60)]);
    assert!(fs::read_dir(&root)
        .unwrap()
        .flatten()
        .all(|e| !e.file_name().to_string_lossy().starts_with(".mgreel")));
}

#[test]
fn write_slot_backs_up_first() {
    let root = tempdir();
    let backups = tempdir();
    let card = make_card(&root, &[(3, 10)]);
    card.write_slot(3, &reel_bytes(99), &backups).unwrap();
    assert_eq!(card.slots()[3].frames, 198);
    let saved: Vec<_> = walk(&backups);
    assert_eq!(saved.len(), 1);
    assert_eq!(fs::read(&saved[0]).unwrap(), reel_bytes(10));
}

#[test]
fn backup_and_restore() {
    let root = tempdir();
    let backups = tempdir();
    let card = make_card(&root, &[(0, 10), (4, 20)]);
    fs::write(card.options_path(), "omod 0 //x\n").unwrap();
    let snapshot = card.backup(&backups).unwrap();

    card.clear_slot(0, &backups).unwrap();
    make_card(&root, &[(7, 5)]);
    let mut options = OptionsFile::parse(&fs::read_to_string(card.options_path()).unwrap());
    options.set("omod", "1").unwrap();
    card.write_options(&options.to_text(), &backups).unwrap();
    assert!(walk(&backups).iter().any(|p| p.ends_with("options.txt")));

    card.restore(&snapshot, &backups).unwrap();
    assert_eq!(frames_by_slot(&card), vec![(0, 20), (4, 40)]);
    assert_eq!(
        fs::read_to_string(card.options_path()).unwrap(),
        "omod 0 //x\n"
    );
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir).unwrap().flatten() {
        if e.path().is_dir() {
            out.extend(walk(&e.path()))
        } else {
            out.push(e.path())
        }
    }
    out
}
