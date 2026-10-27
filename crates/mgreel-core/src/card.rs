//! The Morphagene microSD card: 32 reel slots and `options.txt` at the root.

use crate::names::{Names, ReelLabel, NAMES_FILE};
use crate::reel::{self, Problem, Reel};
use crate::riff::{self, Format};
use anyhow::{bail, Context, Result};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

pub const SLOT_COUNT: usize = 32;
pub const OPTIONS_FILE: &str = "options.txt";
const TEMP_PREFIX: &str = ".mgreel-tmp-";

/// Held while a change reads `mgreel.json` and writes it back, so two
/// changes at once can't each drop the other's edit.
static NAMES_LOCK: Mutex<()> = Mutex::new(());

fn names_lock() -> MutexGuard<'static, ()> {
    // A panic while holding the lock leaves nothing half done in memory.
    NAMES_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// `mg1.wav` to `mg9.wav`, then `mga.wav` to `mgw.wav`.
pub fn slot_file_name(index: usize) -> String {
    assert!(index < SLOT_COUNT, "slot {index} out of range");
    let c = if index < 9 {
        (b'1' + index as u8) as char
    } else {
        (b'a' + (index - 9) as u8) as char
    };
    format!("mg{c}.wav")
}

/// The slot a file name belongs to. FAT32 ignores case, so `MG1.WAV` counts.
pub fn slot_index(file_name: &str) -> Option<usize> {
    let name = file_name.to_ascii_lowercase();
    match *name.strip_prefix("mg")?.strip_suffix(".wav")?.as_bytes() {
        [c @ b'1'..=b'9'] => Some((c - b'1') as usize),
        [c @ b'a'..=b'w'] => Some((c - b'a') as usize + 9),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SlotInfo {
    pub index: usize,
    pub file_name: String,
    /// None when the slot is empty.
    pub path: Option<PathBuf>,
    pub frames: u32,
    pub markers: Vec<u32>,
    pub format: Option<Format>,
    pub problems: Vec<Problem>,
}

impl SlotInfo {
    pub fn seconds(&self) -> f64 {
        self.frames as f64 / reel::SAMPLE_RATE as f64
    }
}

/// Something on the card, outside the reel slots, that needs a look.
#[derive(Debug, Clone, PartialEq)]
pub enum CardIssue {
    /// An audio file the module won't load because of its name or folder.
    /// Only WAV files can be put into a slot.
    Stray { path: PathBuf, wav: bool },
    /// macOS metadata files (`._*` and `.DS_Store`). The module doesn't use
    /// them.
    Clutter { paths: Vec<PathBuf> },
}

const AUDIO_EXTENSIONS: [&str; 7] = ["wav", "wave", "aif", "aiff", "flac", "mp3", "m4a"];

fn is_clutter(name: &str) -> bool {
    name.starts_with("._") || name == ".DS_Store"
}

#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub root: PathBuf,
}

impl Card {
    pub fn new(root: impl Into<PathBuf>) -> Card {
        Card { root: root.into() }
    }

    /// A card has `options.txt` or at least one reel at its root.
    pub fn looks_like_card(root: &Path) -> bool {
        root.join(OPTIONS_FILE).is_file() || !Card::new(root).reel_files().is_empty()
    }

    /// Cards mounted under `volumes` (normally `/Volumes`).
    pub fn detect(volumes: &Path) -> Vec<Card> {
        let Ok(entries) = fs::read_dir(volumes) else {
            return Vec::new();
        };
        let mut cards: Vec<Card> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir() && Card::looks_like_card(p))
            .map(Card::new)
            .collect();
        cards.sort_by(|a, b| a.root.cmp(&b.root));
        cards
    }

    pub fn label(&self) -> String {
        self.root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root.display().to_string())
    }

    pub fn options_path(&self) -> PathBuf {
        self.root.join(OPTIONS_FILE)
    }

    pub fn names_path(&self) -> PathBuf {
        self.root.join(NAMES_FILE)
    }

    /// The reel names on the card. A card without `mgreel.json` has none.
    /// Fails when the file can't be read, and every change that moves or
    /// removes a reel fails with it, so names never end up on the wrong reel.
    pub fn names(&self) -> Result<Names> {
        let path = self.names_path();
        match fs::read_to_string(&path) {
            Ok(text) => Names::parse(&text).with_context(|| format!("in {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Names::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Name a reel and add a note to it. Empty strings clear them.
    pub fn set_name(&self, index: usize, label: ReelLabel) -> Result<()> {
        let _lock = names_lock();
        let mut names = self.names()?;
        names.set(index, label);
        self.write_names(&names)
    }

    /// Back up `mgreel.json`, then remove it, so a file that can't be read
    /// stops blocking changes to the card.
    pub fn remove_names(&self, backup_dir: &Path) -> Result<()> {
        let _lock = names_lock();
        let path = self.names_path();
        if path.exists() {
            backup_file(&path, backup_dir)?;
            fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        }
        Ok(())
    }

    /// Write `mgreel.json`, or remove it when no reel has a name.
    fn write_names(&self, names: &Names) -> Result<()> {
        let path = self.names_path();
        if names.is_empty() {
            match fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    Err(e).with_context(|| format!("removing {}", path.display()))
                }
                _ => Ok(()),
            }
        } else {
            write_atomic(&path, names.to_json().as_bytes())
        }
    }

    /// Before a reel goes into an empty slot, drop any name left there by a
    /// reel deleted outside the app, so the new reel doesn't take it.
    fn forget_if_empty(&self, index: usize) -> Result<()> {
        let _lock = names_lock();
        if self.slot_path(index).exists() {
            return Ok(());
        }
        let mut names = self.names()?;
        if names.get(index).is_empty() {
            return Ok(());
        }
        names.clear(index);
        self.write_names(&names)
    }

    /// Reel files at the root by slot. Skips macOS `._` metadata files.
    pub(crate) fn reel_files(&self) -> Vec<(usize, PathBuf)> {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut files: Vec<(usize, PathBuf)> = entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                Some((slot_index(&name)?, e.path()))
            })
            .collect();
        files.sort();
        files
    }

    /// The existing file for a slot, whatever its case, or the default name.
    pub fn slot_path(&self, index: usize) -> PathBuf {
        self.reel_files()
            .into_iter()
            .find(|(i, _)| *i == index)
            .map(|(_, p)| p)
            .unwrap_or_else(|| self.root.join(slot_file_name(index)))
    }

    /// Every slot, reading only WAV headers and markers.
    pub fn slots(&self) -> Vec<SlotInfo> {
        let mut slots: Vec<SlotInfo> = (0..SLOT_COUNT)
            .map(|index| SlotInfo {
                index,
                file_name: slot_file_name(index),
                path: None,
                frames: 0,
                markers: Vec::new(),
                format: None,
                problems: Vec::new(),
            })
            .collect();
        for (index, path) in self.reel_files() {
            let slot = &mut slots[index];
            match read_header(&path) {
                Ok((format, frames, markers)) => {
                    slot.problems = reel::problems(frames, markers.len(), Some(format));
                    slot.format = Some(format);
                    slot.frames = frames;
                    slot.markers = markers;
                }
                Err(e) => {
                    slot.problems = vec![Problem::Unreadable {
                        reason: format!("{e:#}"),
                    }]
                }
            }
            slot.path = Some(path);
        }
        slots
    }

    /// Write a reel into a slot. The current file is copied to `backup_dir`
    /// first, and the new one is written beside it then renamed into place,
    /// so a failed write never leaves a partial reel in the slot.
    pub fn write_slot(&self, index: usize, bytes: &[u8], backup_dir: &Path) -> Result<()> {
        self.forget_if_empty(index)?;
        write_safely(&self.slot_path(index), bytes, backup_dir)
    }

    /// Write `options.txt` the same way as a reel: back up, then replace.
    pub fn write_options(&self, text: &str, backup_dir: &Path) -> Result<()> {
        write_safely(&self.options_path(), text.as_bytes(), backup_dir)
    }

    /// Back up then delete a slot's reel, and its name.
    pub fn clear_slot(&self, index: usize, backup_dir: &Path) -> Result<()> {
        let _lock = names_lock();
        let mut names = self.names()?;
        let target = self.slot_path(index);
        if target.exists() {
            backup_file(&target, backup_dir)?;
            fs::remove_file(&target)?;
        }
        if !names.get(index).is_empty() {
            names.clear(index);
            self.write_names(&names)?;
        }
        Ok(())
    }

    pub fn swap(&self, a: usize, b: usize) -> Result<()> {
        let mut order: Vec<usize> = (0..SLOT_COUNT).collect();
        order.swap(a, b);
        self.reorder(&order)
    }

    /// Take the reel out of `from` and insert it at `to`, shifting the slots
    /// between them by one, like dragging an item in a list.
    pub fn move_slot(&self, from: usize, to: usize) -> Result<()> {
        let mut order: Vec<usize> = (0..SLOT_COUNT).collect();
        let item = order.remove(from);
        order.insert(to, item);
        self.reorder(&order)
    }

    /// Rearrange reels so slot `i` gets what was in slot `order[i]`, names
    /// included. All moving files go to temporary names first so no rename
    /// overwrites a reel that hasn't moved yet.
    pub fn reorder(&self, order: &[usize]) -> Result<()> {
        let mut seen = [false; SLOT_COUNT];
        if order.len() != SLOT_COUNT
            || order
                .iter()
                .any(|&i| i >= SLOT_COUNT || std::mem::replace(&mut seen[i], true))
        {
            bail!("slot order must list each of the {SLOT_COUNT} slots once");
        }
        // Read first, so a names file that can't be read stops the move. The
        // lock also keeps two moves from sharing temporary names.
        let _lock = names_lock();
        let names = self.names()?;
        let current: Vec<Option<PathBuf>> = {
            let mut v = vec![None; SLOT_COUNT];
            for (i, p) in self.reel_files() {
                v[i] = Some(p);
            }
            v
        };
        // The new names are written before any reel moves, so a full card
        // fails here with nothing changed. Renames need no free space.
        let moved = names.reorder(order);
        let names_tmp = if moved != names {
            Some(stage(&self.names_path(), moved.to_json().as_bytes())?)
        } else {
            None
        };
        let renamed = self.rename_into(order, &current);
        match (renamed, names_tmp) {
            (Ok(()), Some(tmp)) => fs::rename(&tmp, self.names_path())
                .with_context(|| format!("replacing {}", self.names_path().display())),
            (Ok(()), None) => Ok(()),
            (Err(e), tmp) => {
                if let Some(tmp) = tmp {
                    let _ = fs::remove_file(tmp);
                }
                Err(e)
            }
        }
    }

    fn rename_into(&self, order: &[usize], current: &[Option<PathBuf>]) -> Result<()> {
        let mut staged = Vec::new();
        for (dest, &src) in order.iter().enumerate() {
            if dest == src {
                continue;
            }
            if let Some(path) = &current[src] {
                let tmp = self.root.join(format!("{TEMP_PREFIX}{src}.wav"));
                fs::rename(path, &tmp)?;
                staged.push((tmp, dest));
            }
        }
        for (tmp, dest) in staged {
            fs::rename(&tmp, self.root.join(slot_file_name(dest)))?;
        }
        Ok(())
    }

    /// Audio files the module won't load, at the root or one folder down,
    /// and macOS metadata files. Hidden folders such as `.Trashes` are
    /// skipped. Fails if any folder it scans can't be read, so a partial
    /// scan never looks like a clean card.
    pub fn issues(&self) -> Result<Vec<CardIssue>> {
        let mut issues = Vec::new();
        let mut clutter = Vec::new();
        let mut dirs = vec![(self.root.clone(), 0)];
        while let Some((dir, depth)) = dirs.pop() {
            let mut entries = fs::read_dir(&dir)
                .and_then(|it| it.collect::<std::io::Result<Vec<_>>>())
                .with_context(|| format!("reading {}", dir.display()))?;
            entries.sort_by_key(|e| e.file_name());
            for e in entries {
                let path = e.path();
                let kind = e
                    .file_type()
                    .with_context(|| format!("reading {}", path.display()))?;
                // A link can lead off the card, so links are left alone.
                if kind.is_symlink() {
                    continue;
                }
                let name = e.file_name().to_string_lossy().into_owned();
                if is_clutter(&name) && !kind.is_dir() {
                    clutter.push(path);
                } else if name.starts_with('.') {
                    continue;
                } else if kind.is_dir() {
                    if depth == 0 {
                        dirs.push((path, depth + 1));
                    }
                } else if depth > 0 || slot_index(&name).is_none() {
                    let ext = path
                        .extension()
                        .map(|x| x.to_string_lossy().to_ascii_lowercase())
                        .unwrap_or_default();
                    if AUDIO_EXTENSIONS.contains(&ext.as_str()) {
                        let wav = ext == "wav" || ext == "wave";
                        issues.push(CardIssue::Stray { path, wav });
                    }
                }
            }
        }
        if !clutter.is_empty() {
            clutter.sort();
            issues.push(CardIssue::Clutter { paths: clutter });
        }
        Ok(issues)
    }

    /// Delete the macOS metadata files [`Card::issues`] lists. Returns how
    /// many went.
    pub fn remove_clutter(&self) -> Result<usize> {
        let mut removed = 0;
        for issue in self.issues()? {
            if let CardIssue::Clutter { paths } = issue {
                for path in paths {
                    fs::remove_file(&path)
                        .with_context(|| format!("removing {}", path.display()))?;
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }

    /// Rewrite a slot's reel as 48 kHz 32-bit float stereo, keeping its
    /// markers. Backs up the old file first and returns the new bytes.
    pub fn convert_slot(&self, index: usize, backup_dir: &Path) -> Result<Vec<u8>> {
        let bytes = Reel::open(&self.slot_path(index))?.to_wav_bytes()?;
        self.write_slot(index, &bytes, backup_dir)?;
        Ok(bytes)
    }

    /// Put a WAV file from elsewhere on the card into an empty slot,
    /// converted and with its markers, then back it up and remove it from
    /// where it was. Returns the reel's bytes.
    pub fn adopt(&self, path: &Path, index: usize, backup_dir: &Path) -> Result<Vec<u8>> {
        if self.slot_path(index).exists() {
            bail!("{} already holds a reel", slot_file_name(index));
        }
        let on_card = fs::canonicalize(path)
            .ok()
            .zip(fs::canonicalize(&self.root).ok())
            .is_some_and(|(p, root)| p.starts_with(root) && p.is_file());
        if !on_card {
            bail!("{} isn't a file on this card", path.display());
        }
        let bytes = Reel::open(path)?.to_wav_bytes()?;
        // Back up first, so a failure here leaves the slot empty to retry.
        backup_file(path, backup_dir)?;
        self.write_slot(index, &bytes, backup_dir)?;
        fs::remove_file(path)?;
        Ok(bytes)
    }

    /// Put a file from a backup folder back on the card: a reel into the
    /// slot its name gives, or `options.txt`. What it replaces is backed up
    /// first. Returns the file's bytes.
    pub fn restore_file(&self, from: &Path, backup_dir: &Path) -> Result<Vec<u8>> {
        let name = from
            .file_name()
            .context("no file name")?
            .to_string_lossy()
            .into_owned();
        let target = if name.eq_ignore_ascii_case(OPTIONS_FILE) {
            self.options_path()
        } else if name.eq_ignore_ascii_case(NAMES_FILE) {
            self.names_path()
        } else {
            let index = slot_index(&name)
                .with_context(|| format!("{name} isn't a reel, options.txt or {NAMES_FILE}"))?;
            self.forget_if_empty(index)?;
            self.slot_path(index)
        };
        let bytes = fs::read(from).with_context(|| format!("reading {}", from.display()))?;
        write_safely(&target, &bytes, backup_dir)?;
        Ok(bytes)
    }

    /// Copy every reel, `options.txt` and the reel names into a new
    /// timestamped folder under `dest`, and return that folder.
    pub fn backup(&self, dest: &Path) -> Result<PathBuf> {
        // Held so a move can't land between copying the reels and the names.
        let _lock = names_lock();
        self.backup_locked(dest)
    }

    /// [`Card::backup`], for a caller that already holds the names lock.
    fn backup_locked(&self, dest: &Path) -> Result<PathBuf> {
        let dir = new_dir(&dest.join(format!("{}-{}", self.label(), timestamp())))?;
        for (_, path) in self.reel_files() {
            fs::copy(&path, dir.join(path.file_name().unwrap()))?;
        }
        for (path, name) in [
            (self.options_path(), OPTIONS_FILE),
            (self.names_path(), NAMES_FILE),
        ] {
            if path.is_file() {
                fs::copy(&path, dir.join(name))?;
            }
        }
        Ok(dir)
    }

    /// Replace the card's reels, `options.txt` and reel names with those in
    /// a backup folder. A backup without names leaves the reels unnamed.
    /// Reels on the card are backed up to `backup_dir` first.
    pub fn restore(&self, from: &Path, backup_dir: &Path) -> Result<()> {
        if !Card::looks_like_card(from) {
            bail!("{} holds no reels or options.txt", from.display());
        }
        let _lock = names_lock();
        self.backup_locked(backup_dir)?;
        for (_, path) in self.reel_files() {
            fs::remove_file(path)?;
        }
        for (index, path) in Card::new(from).reel_files() {
            fs::copy(&path, self.root.join(slot_file_name(index)))?;
        }
        if from.join(OPTIONS_FILE).is_file() {
            fs::copy(from.join(OPTIONS_FILE), self.options_path())?;
        }
        match fs::read(from.join(NAMES_FILE)) {
            Ok(bytes) => {
                write_atomic(&self.names_path(), &bytes).context("restoring the reel names")?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.write_names(&Names::default())?
            }
            Err(e) => return Err(e).context("restoring the reel names"),
        }
        Ok(())
    }
}

fn read_header(path: &Path) -> Result<(Format, u32, Vec<u32>)> {
    let chunks = riff::scan_file(path)?;
    let find = |id: &[u8; 4]| chunks.iter().find(|c| &c.id == id);
    let format = Format::parse(&riff::read_chunk(
        path,
        find(b"fmt ").context("no fmt chunk")?,
    )?)?;
    let frames = find(b"data").map_or(0, |c| c.size as usize / format.frame_bytes().max(1)) as u32;
    let markers = match find(b"cue ") {
        Some(c) => riff::decode_cues(&riff::read_chunk(path, c)?),
        None => Vec::new(),
    };
    Ok((format, frames, markers))
}

/// Copy `target` into `backup_dir`, write `bytes` to a temporary file beside
/// it, then rename over it, so a failed write never leaves a partial file.
fn write_safely(target: &Path, bytes: &[u8], backup_dir: &Path) -> Result<()> {
    if target.exists() {
        backup_file(target, backup_dir)?;
    }
    write_atomic(target, bytes)
}

/// Write `bytes` to a temporary file beside `target`, then rename it over.
fn write_atomic(target: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = stage(target, bytes)?;
    fs::rename(&tmp, target).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        anyhow::Error::from(e).context(format!("replacing {}", target.display()))
    })
}

/// Write `bytes` to a temporary file beside `target` and return its path,
/// so the caller can rename it into place. This is the step that needs free
/// space. Each call gets its own temporary name, so two at once can't share
/// one.
fn stage(target: &Path, bytes: &[u8]) -> Result<PathBuf> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = target.parent().context("target has no parent folder")?;
    let tmp = dir.join(format!(
        "{TEMP_PREFIX}{}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
        target.file_name().unwrap().to_string_lossy()
    ));
    let written = (|| -> Result<()> {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(e) = written {
        // A full card fails here; don't leave the partial copy behind.
        let _ = fs::remove_file(&tmp);
        return Err(e.context(format!(
            "writing {} (it needs {} MB free while saving)",
            target.display(),
            bytes.len().div_ceil(1_000_000)
        )));
    }
    Ok(tmp)
}

fn backup_file(path: &Path, backup_dir: &Path) -> Result<()> {
    let dir = new_dir(&backup_dir.join(timestamp()))?;
    fs::copy(path, dir.join(path.file_name().unwrap()))
        .with_context(|| format!("backing up {}", path.display()))?;
    Ok(())
}

/// Create `base`, or `base-2`, `base-3`... if it exists, so two backups in
/// the same second never share a folder.
pub(crate) fn new_dir(base: &Path) -> Result<PathBuf> {
    fs::create_dir_all(base.parent().context("backup folder has no parent")?)?;
    for n in 1.. {
        let dir = if n == 1 {
            base.to_path_buf()
        } else {
            PathBuf::from(format!("{}-{n}", base.display()))
        };
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    unreachable!()
}

/// UTC `YYYYMMDD-HHMMSS`, without a date crate.
pub(crate) fn timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_names_round_trip() {
        assert_eq!(slot_file_name(0), "mg1.wav");
        assert_eq!(slot_file_name(8), "mg9.wav");
        assert_eq!(slot_file_name(9), "mga.wav");
        assert_eq!(slot_file_name(31), "mgw.wav");
        for i in 0..SLOT_COUNT {
            assert_eq!(slot_index(&slot_file_name(i)), Some(i));
        }
        assert_eq!(slot_index("MGA.WAV"), Some(9));
        assert_eq!(slot_index("mgx.wav"), None);
        assert_eq!(slot_index("mg0.wav"), None);
        assert_eq!(slot_index("._mg1.wav"), None);
    }

    #[test]
    fn timestamp_shape() {
        let t = timestamp();
        assert_eq!(t.len(), 15);
        assert!(t.starts_with("20"));
    }
}
