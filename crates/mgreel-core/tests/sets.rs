use mgreel_core::card::{slot_file_name, Card};
use mgreel_core::names::{Names, ReelLabel, NAMES_FILE};
use mgreel_core::reel::{self, ReelFile};
use mgreel_core::{sets, Reel};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

fn tempdir() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "mgreel-sets-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A Morphagene reel whose length identifies it.
fn reel_bytes(frames: usize, markers: Vec<u32>) -> Vec<u8> {
    Reel {
        left: vec![0.25; frames],
        right: vec![-0.25; frames],
        markers,
    }
    .to_wav_bytes()
    .unwrap()
}

/// A 44.1 kHz 16-bit mono WAV, as a pack might hold.
fn cd_mono(path: &Path, frames: usize) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 44_100,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for _ in 0..frames {
        w.write_sample(1000i16).unwrap();
    }
    w.finalize().unwrap();
}

fn named(name: &str) -> ReelLabel {
    ReelLabel {
        name: name.into(),
        note: String::new(),
    }
}

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn save_a_card_and_list_it() {
    let root = tempdir();
    let sets_root = tempdir().join("Morphagene Sets");
    fs::write(root.join("mg1.wav"), reel_bytes(10, vec![])).unwrap();
    fs::write(root.join("mga.wav"), reel_bytes(20, vec![])).unwrap();
    fs::write(root.join("options.txt"), "vsop 1\n").unwrap();
    let card = Card::new(&root);
    card.set_name(9, named("Harbour")).unwrap();

    let dir = sets::save(&card, &sets_root, "  Drones ").unwrap();
    assert_eq!(dir, sets_root.join("Drones"));
    assert_eq!(
        entries(&dir),
        vec!["mg1.wav", "mga.wav", "mgreel.json", "options.txt"]
    );
    let listed = sets::list(&sets_root).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "Drones");
    assert_eq!(listed[0].reels, vec![0, 9]);
    assert_eq!(listed[0].names, vec!["Harbour"]);
    assert!(listed[0].has_options);

    // The name is taken, and nothing half made is left behind.
    assert!(sets::save(&card, &sets_root, "Drones").is_err());
    assert!(sets::save(&card, &sets_root, "a/b").is_err());
    assert_eq!(entries(&sets_root), vec!["Drones"]);
}

#[test]
fn listing_needs_no_sets_folder_but_reports_one_it_cant_read() {
    let root = tempdir();
    assert!(sets::list(&root.join("not made yet")).unwrap().is_empty());
    // A file where the folder should be can't be read as one.
    fs::write(root.join("a file"), b"").unwrap();
    assert!(sets::list(&root.join("a file")).is_err());
}

#[test]
fn an_empty_card_saves_nothing() {
    let root = tempdir();
    let sets_root = tempdir();
    assert!(sets::save(&Card::new(&root), &sets_root, "Empty").is_err());
    assert!(entries(&sets_root).is_empty());
}

#[test]
fn a_set_loads_onto_another_card_with_its_names() {
    let a = tempdir();
    let b = tempdir();
    let sets_root = tempdir();
    let backups = tempdir();
    fs::write(a.join("mg2.wav"), reel_bytes(10, vec![])).unwrap();
    Card::new(&a).set_name(1, named("Gulls")).unwrap();
    fs::write(b.join("mg1.wav"), reel_bytes(30, vec![])).unwrap();
    Card::new(&b).set_name(0, named("Old")).unwrap();

    let set = sets::save(&Card::new(&a), &sets_root, "Field").unwrap();
    let card = Card::new(&b);
    card.restore(&set, &backups).unwrap();
    assert_eq!(entries(&b), vec!["mg2.wav", "mgreel.json"]);
    assert_eq!(card.names().unwrap().get(1).name, "Gulls");
    assert!(card.names().unwrap().get(0).is_empty());
}

#[test]
fn import_a_pack() {
    let pack = tempdir().join("Harbour Pack");
    let sets_root = tempdir();
    fs::create_dir_all(pack.join("sub")).unwrap();
    fs::create_dir_all(pack.join("__MACOSX")).unwrap();
    // Keeps its slot, its markers and its name from the pack's names file.
    fs::write(pack.join("mg3.wav"), reel_bytes(4800, vec![1200, 2400])).unwrap();
    let mut names = Names::default();
    names.set(2, named("Harbour"));
    fs::write(pack.join(NAMES_FILE), names.to_json()).unwrap();
    // The rest go in Finder order into the free slots.
    fs::write(pack.join("Take 10.wav"), reel_bytes(100, vec![])).unwrap();
    fs::write(pack.join("Take 2.wav"), reel_bytes(200, vec![])).unwrap();
    cd_mono(&pack.join("sub/Gulls.WAV"), 44_100);
    fs::write(pack.join("broken.wav"), b"not a wav").unwrap();
    fs::write(pack.join("song.mp3"), b"mp3").unwrap();
    fs::write(pack.join("notes.txt"), b"hello").unwrap();
    fs::write(pack.join(".hidden.wav"), b"junk").unwrap();
    fs::write(pack.join("__MACOSX/._Take 2.wav"), b"junk").unwrap();
    fs::write(pack.join("options.txt"), "vsop 1\n").unwrap();

    let imported = sets::import(&pack, &sets_root).unwrap();
    assert_eq!(imported.name, "Harbour Pack");
    let placed: Vec<(usize, &str)> = imported
        .placed
        .iter()
        .map(|p| (p.slot, p.source.as_str()))
        .collect();
    // broken.wav comes first in name order but can't be read, so its slot
    // goes to the next file.
    assert_eq!(
        placed,
        vec![
            (0, "sub/Gulls.WAV"),
            (1, "Take 2.wav"),
            (2, "mg3.wav"),
            (3, "Take 10.wav")
        ]
    );
    let mut skipped: Vec<&str> = imported.skipped.iter().map(|s| s.source.as_str()).collect();
    skipped.sort();
    assert_eq!(skipped, vec!["broken.wav", "song.mp3"]);

    let card = Card::new(&imported.dir);
    assert_eq!(
        entries(&imported.dir),
        vec![
            "mg1.wav",
            "mg2.wav",
            "mg3.wav",
            "mg4.wav",
            "mgreel.json",
            "options.txt"
        ]
    );
    for slot in card.slots().iter().filter(|s| s.path.is_some()) {
        assert!(
            slot.problems.is_empty(),
            "{}: {:?}",
            slot.file_name,
            slot.problems
        );
    }
    // One second at 44.1 kHz is one second at 48 kHz.
    assert_eq!(card.slots()[0].frames, 48_000);
    let mg3 = ReelFile::open(&imported.dir.join(slot_file_name(2))).unwrap();
    assert_eq!(mg3.markers(), vec![1200, 2400]);
    let labels = card.names().unwrap();
    assert_eq!(labels.get(0).name, "sub/Gulls");
    assert_eq!(labels.get(1).name, "Take 2");
    assert_eq!(labels.get(2).name, "Harbour");
    assert_eq!(labels.get(3).name, "Take 10");

    // The same pack again gets a numbered name.
    assert_eq!(
        sets::import(&pack, &sets_root).unwrap().name,
        "Harbour Pack 2"
    );
    assert_eq!(entries(&sets_root), vec!["Harbour Pack", "Harbour Pack 2"]);
}

#[test]
fn import_reports_reels_the_module_would_refuse() {
    let pack = tempdir();
    let sets_root = tempdir();
    let many: Vec<u32> = (1..=reel::MAX_MARKERS as u32 + 1).collect();
    fs::write(pack.join("busy.wav"), reel_bytes(1000, many)).unwrap();
    let imported = sets::import(&pack, &sets_root).unwrap();
    assert_eq!(imported.placed[0].problems.len(), 1);
}

#[test]
fn a_named_slot_that_cant_be_read_stays_empty() {
    let pack = tempdir();
    let sets_root = tempdir();
    fs::write(pack.join("mg1.wav"), b"not a wav").unwrap();
    fs::write(pack.join("Gulls.wav"), reel_bytes(10, vec![])).unwrap();
    let imported = sets::import(&pack, &sets_root).unwrap();
    assert_eq!(imported.placed[0].slot, 1);
    assert_eq!(imported.skipped[0].source, "mg1.wav");
    assert_eq!(entries(&imported.dir), vec!["mg2.wav", "mgreel.json"]);
}

#[test]
fn a_pack_too_big_for_a_card_is_refused() {
    let pack = tempdir();
    let sets_root = tempdir();
    for i in 0..33 {
        fs::write(pack.join(format!("take {i}.wav")), reel_bytes(10, vec![])).unwrap();
    }
    let e = format!("{:#}", sets::import(&pack, &sets_root).unwrap_err());
    assert!(e.contains("33 WAV files"), "{e}");
    assert!(entries(&sets_root).is_empty());
}

#[test]
fn a_pack_of_unreadable_files_leaves_nothing() {
    let pack = tempdir();
    let sets_root = tempdir();
    fs::write(pack.join("a.wav"), b"not a wav").unwrap();
    let e = format!("{:#}", sets::import(&pack, &sets_root).unwrap_err());
    assert!(e.contains("a.wav"), "{e}");
    assert!(entries(&sets_root).is_empty());
}
