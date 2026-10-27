use mgreel_core::card::{slot_file_name, Card, CardIssue};
use mgreel_core::history::{self, BackupKind};
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

/// A 44.1 kHz 16-bit mono WAV with one cue marker.
fn write_cd_wav(path: &Path) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 44_100,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for _ in 0..44_100 {
        w.write_sample(8192i16).unwrap();
    }
    w.finalize().unwrap();
    // Give it a marker at half a second by rewriting it through a reel file.
    let mut file = mgreel_core::ReelFile::open(path).unwrap();
    file.set_markers(&[22_050]);
    fs::write(path, file.to_bytes()).unwrap();
}

#[test]
fn lists_stray_audio_and_clutter() {
    let root = tempdir();
    let card = make_card(&root, &[(0, 10)]);
    fs::write(root.join("._mg1.wav"), b"junk").unwrap();
    fs::write(root.join(".DS_Store"), b"junk").unwrap();
    fs::write(root.join("mg10.wav"), b"x").unwrap();
    fs::write(root.join("notes.txt"), b"x").unwrap();
    fs::create_dir(root.join("samples")).unwrap();
    fs::write(root.join("samples/mg2.wav"), b"x").unwrap();
    fs::write(root.join("samples/kick.aif"), b"x").unwrap();
    fs::create_dir(root.join(".Trashes")).unwrap();
    fs::write(root.join(".Trashes/lost.wav"), b"x").unwrap();
    // A link to a folder off the card is left alone, clutter and all.
    let outside = tempdir();
    fs::write(outside.join("._keep"), b"x").unwrap();
    fs::write(outside.join("song.wav"), b"x").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("linked")).unwrap();

    let issues = card.issues().unwrap();
    let stray: Vec<(String, bool)> = issues
        .iter()
        .filter_map(|i| match i {
            CardIssue::Stray { path, wav } => Some((
                path.strip_prefix(&root).unwrap().display().to_string(),
                *wav,
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        stray,
        vec![
            ("mg10.wav".to_string(), true),
            ("samples/kick.aif".to_string(), false),
            ("samples/mg2.wav".to_string(), true),
        ]
    );
    assert!(matches!(issues.last(), Some(CardIssue::Clutter { paths }) if paths.len() == 2));

    assert_eq!(card.remove_clutter().unwrap(), 2);
    assert!(outside.join("._keep").exists());
    assert!(!root.join("._mg1.wav").exists());
    assert!(root.join("mg1.wav").exists());
    assert!(!card
        .issues()
        .unwrap()
        .iter()
        .any(|i| matches!(i, CardIssue::Clutter { .. })));
}

#[test]
fn converts_a_slot_keeping_markers() {
    let root = tempdir();
    let backups = tempdir();
    write_cd_wav(&root.join("mg1.wav"));
    let card = Card::new(&root);
    assert!(!card.slots()[0].problems.is_empty());
    let bytes = card.convert_slot(0, &backups).unwrap();
    let slot = &card.slots()[0];
    assert!(slot.problems.is_empty());
    assert_eq!(slot.frames, 48_000);
    assert_eq!(slot.markers, vec![24_000]);
    assert_eq!(fs::read(root.join("mg1.wav")).unwrap(), bytes);
    assert_eq!(walk(&backups).len(), 1);
}

#[test]
fn adopts_a_stray_file_into_a_slot() {
    let root = tempdir();
    let backups = tempdir();
    let card = make_card(&root, &[(0, 10)]);
    fs::create_dir(root.join("in")).unwrap();
    let stray = root.join("in/field.wav");
    write_cd_wav(&stray);

    assert!(card.adopt(&stray, 0, &backups).is_err(), "slot 0 is taken");
    // A real file off the card, so the containment check is what refuses it.
    let outside = tempdir().join("elsewhere.wav");
    write_cd_wav(&outside);
    assert!(
        card.adopt(&outside, 1, &backups).is_err(),
        "outside the card"
    );
    assert!(outside.exists());
    assert!(card.slots()[1].path.is_none());
    card.adopt(&stray, 1, &backups).unwrap();
    assert!(!stray.exists());
    assert_eq!(card.slots()[1].markers, vec![24_000]);
    assert!(walk(&backups).iter().any(|p| p.ends_with("field.wav")));
}

#[test]
fn snapshots_reels_changed_on_the_card() {
    let root = tempdir();
    let backups = tempdir();
    let card = make_card(&root, &[(0, 10), (1, 20)]);

    // First sight copies every reel.
    let (dir, files) = history::snapshot(&card, &backups).unwrap().unwrap();
    assert_eq!(files, vec!["mg1.wav", "mg2.wav"]);
    assert_eq!(fs::read(dir.join("mg1.wav")).unwrap(), reel_bytes(10));
    assert!(history::snapshot(&card, &backups).unwrap().is_none());

    // A move keeps the contents, so nothing is copied.
    card.swap(0, 5).unwrap();
    assert!(history::snapshot(&card, &backups).unwrap().is_none());

    // The module records into a reel: only that one is copied.
    fs::write(root.join("mg2.wav"), reel_bytes(30)).unwrap();
    let (_, files) = history::snapshot(&card, &backups).unwrap().unwrap();
    assert_eq!(files, vec!["mg2.wav"]);

    // A reel the app wrote and remembered isn't copied again.
    let bytes = reel_bytes(40);
    card.write_slot(2, &bytes, &backups).unwrap();
    history::remember(&backups, &bytes).unwrap();
    assert!(history::snapshot(&card, &backups).unwrap().is_none());

    let kinds: Vec<BackupKind> = history::list(&backups).iter().map(|b| b.kind).collect();
    assert_eq!(
        kinds.iter().filter(|k| **k == BackupKind::FromCard).count(),
        2
    );
    assert!(!kinds.contains(&BackupKind::WholeCard));
}

#[test]
fn restores_one_file_from_a_backup() {
    let root = tempdir();
    let backups = tempdir();
    let card = make_card(&root, &[(0, 10)]);
    fs::write(card.options_path(), "omod 0\n").unwrap();
    let whole = card.backup(&backups).unwrap();
    card.write_slot(0, &reel_bytes(50), &backups).unwrap();
    fs::write(card.options_path(), "omod 1\n").unwrap();

    card.restore_file(&whole.join("mg1.wav"), &backups).unwrap();
    assert_eq!(fs::read(root.join("mg1.wav")).unwrap(), reel_bytes(10));
    card.restore_file(&whole.join("options.txt"), &backups)
        .unwrap();
    assert_eq!(fs::read_to_string(card.options_path()).unwrap(), "omod 0\n");

    let list = history::list(&backups);
    assert!(list
        .iter()
        .any(|b| b.kind == BackupKind::WholeCard && b.dir == whole));
    assert!(list.iter().any(|b| b.kind == BackupKind::BeforeChange));
    assert!(card
        .restore_file(&root.join("notes.txt"), &backups)
        .is_err());
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
