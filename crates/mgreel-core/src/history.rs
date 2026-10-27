//! Copies of a card's files kept on the computer, in one backup folder per
//! card:
//!
//! - `YYYYMMDD-HHMMSS/`: a file as it was before the app replaced it.
//! - `<label>-YYYYMMDD-HHMMSS/`: a whole-card backup.
//! - `from-card/YYYYMMDD-HHMMSS/`: reels that changed on the card since the
//!   app last saw it, copied when the card mounts. The module writes to the
//!   card whenever it records or splices, so this keeps those reels before
//!   anything else can replace them.

use crate::card::{new_dir, timestamp, Card};
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const FROM_CARD: &str = "from-card";
/// Hashes of reel contents already copied or written by the app.
const KNOWN: &str = "known.txt";

/// A hash of a file's contents that stays the same between runs.
pub fn content_hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn known_path(backup_dir: &Path) -> PathBuf {
    backup_dir.join(FROM_CARD).join(KNOWN)
}

fn read_known(backup_dir: &Path) -> BTreeSet<String> {
    fs::read_to_string(known_path(backup_dir))
        .map(|text| text.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

fn write_known(backup_dir: &Path, known: &BTreeSet<String>) -> Result<()> {
    let path = known_path(backup_dir);
    fs::create_dir_all(path.parent().unwrap())?;
    let text: String = known.iter().map(|h| format!("{h}\n")).collect();
    fs::write(&path, text).with_context(|| format!("writing {}", path.display()))
}

/// Note a reel the app wrote to the card, so the next mount doesn't copy it
/// as a change made on the module.
pub fn remember(backup_dir: &Path, bytes: &[u8]) -> Result<()> {
    let mut known = read_known(backup_dir);
    if known.insert(content_hash(bytes)) {
        write_known(backup_dir, &known)?;
    }
    Ok(())
}

/// Note every reel now on the card as seen, without copying any.
pub fn remember_card(card: &Card, backup_dir: &Path) -> Result<()> {
    let mut known = read_known(backup_dir);
    for (_, path) in card.reel_files() {
        known.insert(content_hash(&fs::read(&path)?));
    }
    write_known(backup_dir, &known)
}

/// Copy the reels whose contents the app hasn't seen into a new
/// `from-card/<timestamp>` folder. Returns that folder and the file names,
/// or None when nothing changed. The first time a card is seen, every reel
/// is copied. Afterwards only the reels on the card count as seen, so the
/// list stays the size of one card.
pub fn snapshot(card: &Card, backup_dir: &Path) -> Result<Option<(PathBuf, Vec<String>)>> {
    let known = read_known(backup_dir);
    let mut seen = BTreeSet::new();
    let mut dir = None;
    let mut copied = Vec::new();
    for (_, path) in card.reel_files() {
        let bytes = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let hash = content_hash(&bytes);
        if !known.contains(&hash) {
            let dir = match &dir {
                Some(d) => d,
                None => dir.insert(new_dir(&backup_dir.join(FROM_CARD).join(timestamp()))?),
            };
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            fs::write(dir.join(&name), &bytes)?;
            copied.push(name);
        }
        seen.insert(hash);
    }
    write_known(backup_dir, &seen)?;
    Ok(dir.map(|d| (d, copied)))
}

/// `YYYYMMDD-HHMMSS`, maybe with the `-N` that `new_dir` adds, as opposed
/// to `<label>-YYYYMMDD-HHMMSS` for a whole-card backup.
fn is_timestamp_name(name: &str) -> bool {
    let b = name.as_bytes();
    let digits =
        |r: std::ops::Range<usize>| b.get(r).is_some_and(|d| d.iter().all(u8::is_ascii_digit));
    let suffix = &b[b.len().min(15)..];
    b.len() >= 15
        && digits(0..8)
        && b[8] == b'-'
        && digits(9..15)
        && (suffix.is_empty()
            || (suffix.len() > 1
                && suffix[0] == b'-'
                && suffix[1..].iter().all(u8::is_ascii_digit)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupKind {
    /// A file as it was before the app replaced it.
    BeforeChange,
    /// A whole-card backup.
    WholeCard,
    /// Reels changed on the card, copied when it mounted.
    FromCard,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Backup {
    pub dir: PathBuf,
    pub kind: BackupKind,
    pub files: Vec<String>,
    pub created: SystemTime,
}

/// Every backup folder for a card, newest first.
pub fn list(backup_dir: &Path) -> Vec<Backup> {
    let mut out = Vec::new();
    let roots = [
        (backup_dir.to_path_buf(), false),
        (backup_dir.join(FROM_CARD), true),
    ];
    for (root, from_card) in roots {
        let Ok(entries) = fs::read_dir(&root) else {
            continue;
        };
        for e in entries.flatten() {
            let dir = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if !dir.is_dir() || name == FROM_CARD {
                continue;
            }
            let kind = if from_card {
                BackupKind::FromCard
            } else if is_timestamp_name(&name) {
                BackupKind::BeforeChange
            } else {
                BackupKind::WholeCard
            };
            let mut files: Vec<String> = fs::read_dir(&dir)
                .map(|it| {
                    it.flatten()
                        .filter(|f| f.path().is_file())
                        .map(|f| f.file_name().to_string_lossy().into_owned())
                        .filter(|n| !n.starts_with('.'))
                        .collect()
                })
                .unwrap_or_default();
            files.sort();
            let created = e
                .metadata()
                .and_then(|m| m.created().or_else(|_| m.modified()))
                .unwrap_or(SystemTime::UNIX_EPOCH);
            out.push(Backup {
                dir,
                kind,
                files,
                created,
            });
        }
    }
    out.sort_by(|a, b| b.created.cmp(&a.created).then_with(|| b.dir.cmp(&a.dir)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_names() {
        assert!(is_timestamp_name("20261026-005856"));
        assert!(is_timestamp_name("20261026-005856-2"));
        assert!(!is_timestamp_name("32GB-20261026-005856"));
        assert!(!is_timestamp_name("20261026-005856-"));
        assert!(!is_timestamp_name("2026"));
    }
}
