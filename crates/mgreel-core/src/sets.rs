//! Card sets: whole cards kept on the computer, one folder per set, laid
//! out like a card with `mg*.wav` reels, `options.txt` and `mgreel.json`.
//! Loading a set onto a card is [`Card::restore`] from its folder, which
//! backs the card up first.

use crate::card::{slot_file_name, slot_index, Card, OPTIONS_FILE, SLOT_COUNT};
use crate::names::{Names, ReelLabel, NAMES_FILE};
use crate::reel::{self, Problem, Reel};
use anyhow::{bail, Context, Result};
use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{self, AtomicU32};
use std::time::SystemTime;

/// Audio files the app can't read. The import report names them so they
/// aren't missed; other files in a pack are left out without a word.
const OTHER_AUDIO: [&str; 7] = ["aif", "aiff", "caf", "flac", "m4a", "mp3", "ogg"];

#[derive(Debug, Clone)]
pub struct SetInfo {
    pub name: String,
    pub dir: PathBuf,
    /// The slots that hold a reel.
    pub reels: Vec<usize>,
    /// Reel names in slot order, without the empty ones.
    pub names: Vec<String>,
    pub has_options: bool,
    pub modified: SystemTime,
}

/// Every set under `root`, by name. No folder yet means no sets.
pub fn list(root: &Path) -> Result<Vec<SetInfo>> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", root.display())),
    };
    let entries = entries
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("reading {}", root.display()))?;
    let mut sets: Vec<SetInfo> = entries
        .into_iter()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let dir = e.path();
            // Hidden folders are saves and imports still being written.
            if name.starts_with('.') || !dir.is_dir() || !Card::looks_like_card(&dir) {
                return None;
            }
            let card = Card::new(&dir);
            let mut reels: Vec<usize> = card.reel_files().into_iter().map(|(i, _)| i).collect();
            reels.dedup();
            let labels = card.names().unwrap_or_default();
            let names = (0..SLOT_COUNT)
                .map(|i| labels.get(i).name.clone())
                .filter(|n| !n.is_empty())
                .collect();
            let modified = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            Some(SetInfo {
                name,
                has_options: card.options_path().is_file(),
                dir,
                reels,
                names,
                modified,
            })
        })
        .collect();
    sets.sort_by(|a, b| finder_order(&a.name, &b.name));
    Ok(sets)
}

/// Save the card as a set named `name` under `root`, and return its folder.
/// The set is written to a hidden folder first and renamed when complete,
/// so a failed save leaves nothing behind.
pub fn save(card: &Card, root: &Path, name: &str) -> Result<PathBuf> {
    let name = check_name(name)?;
    let dest = root.join(&name);
    if dest.exists() {
        bail!("a set named {name} already exists");
    }
    let tmp = temp_dir(root, "saving")?;
    let built = card.copy_into(&tmp).and_then(|()| {
        if !Card::looks_like_card(&tmp) {
            bail!("{} has no reels or options.txt to save", card.label());
        }
        Ok(())
    });
    finish(&tmp, &dest, built)?;
    Ok(dest)
}

#[derive(Debug, Clone)]
pub struct Import {
    pub name: String,
    pub dir: PathBuf,
    /// In slot order.
    pub placed: Vec<Placed>,
    pub skipped: Vec<Skipped>,
}

#[derive(Debug, Clone)]
pub struct Placed {
    pub slot: usize,
    /// The file's path inside the pack.
    pub source: String,
    /// What would still stop the module loading it, such as its length.
    pub problems: Vec<Problem>,
}

#[derive(Debug, Clone)]
pub struct Skipped {
    pub source: String,
    pub reason: String,
}

/// Make a set from a folder of WAV files, such as a downloaded reel pack.
/// Files named `mg1.wav` to `mgw.wav` at the top keep their slots, and
/// their names in the pack's `mgreel.json`. The rest fill the free slots in
/// Finder's name order, subfolders included, each named after its file.
/// Every reel is converted to 48 kHz 32-bit float stereo with its markers.
/// The set takes the folder's name, with a number added if that's taken.
pub fn import(pack: &Path, root: &Path) -> Result<Import> {
    let mut skipped = Vec::new();
    let mut fixed: Vec<(usize, String, PathBuf)> = Vec::new();
    let mut rest: Vec<(String, PathBuf)> = Vec::new();
    for (source, path) in files(pack)? {
        let ext = Path::new(&source)
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase());
        match ext.as_deref() {
            Some("wav") => {
                let top = !source.contains('/');
                match slot_index(&source).filter(|_| top) {
                    Some(i) if fixed.iter().any(|(j, _, _)| *j == i) => skipped.push(Skipped {
                        reason: format!("another file is already {}", slot_file_name(i)),
                        source,
                    }),
                    Some(i) => fixed.push((i, source, path)),
                    None => rest.push((source, path)),
                }
            }
            Some(e) if OTHER_AUDIO.contains(&e) => skipped.push(Skipped {
                source,
                reason: "not a WAV file, so make a WAV of it first".into(),
            }),
            _ => {}
        }
    }
    let folder = pack
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| pack.display().to_string());
    let total = fixed.len() + rest.len();
    if total == 0 {
        bail!("{folder} has no WAV files");
    }
    if total > SLOT_COUNT {
        bail!(
            "{folder} has {total} WAV files and a card holds {SLOT_COUNT} reels. Split it into \
             folders of {SLOT_COUNT} or fewer and import each one."
        );
    }
    fixed.sort_by_key(|(i, _, _)| *i);
    rest.sort_by(|a, b| finder_order(&a.0, &b.0));

    let pack_names = match fs::read_to_string(pack.join(NAMES_FILE)).map_err(anyhow::Error::from) {
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            Names::default()
        }
        read => read
            .and_then(|text| Names::parse(&text))
            .unwrap_or_else(|e| {
                skipped.push(Skipped {
                    source: NAMES_FILE.into(),
                    reason: format!("{e:#}"),
                });
                Names::default()
            }),
    };

    let name = free_name(root, &clean_name(&folder));
    let dest = root.join(&name);
    let tmp = temp_dir(root, "importing")?;
    let mut placed = Vec::new();
    let built = (|| -> Result<()> {
        let mut names = Names::default();
        let mut first_error = None;
        let mut note_error = |source: String, e: anyhow::Error| {
            let reason = format!("{e:#}");
            first_error.get_or_insert_with(|| format!("{source}: {reason}"));
            skipped.push(Skipped { source, reason });
        };
        for (slot, source, path) in fixed.iter().cloned() {
            match convert(&path, &tmp.join(slot_file_name(slot))) {
                Ok(problems) => {
                    names.set(slot, pack_names.get(slot).clone());
                    placed.push(Placed {
                        slot,
                        source,
                        problems,
                    });
                }
                Err(e) => note_error(source, e),
            }
        }
        // A file that can't be read leaves its slot to the next one.
        let mut free = (0..SLOT_COUNT)
            .filter(|i| !fixed.iter().any(|(j, _, _)| j == i))
            .peekable();
        for (source, path) in rest.iter().cloned() {
            let slot = *free.peek().expect("counted above");
            match convert(&path, &tmp.join(slot_file_name(slot))) {
                Ok(problems) => {
                    free.next();
                    let name = Path::new(&source).with_extension("");
                    names.set(
                        slot,
                        ReelLabel {
                            name: name.to_string_lossy().into_owned(),
                            note: String::new(),
                        },
                    );
                    placed.push(Placed {
                        slot,
                        source,
                        problems,
                    });
                }
                Err(e) => note_error(source, e),
            }
        }
        if placed.is_empty() {
            bail!(
                "none of the WAV files in {folder} could be read. The first: {}",
                first_error.unwrap_or_default()
            );
        }
        if pack.join(OPTIONS_FILE).is_file() {
            fs::copy(pack.join(OPTIONS_FILE), tmp.join(OPTIONS_FILE))
                .context("copying options.txt")?;
        }
        if !names.is_empty() {
            fs::write(tmp.join(NAMES_FILE), names.to_json()).context("writing the reel names")?;
        }
        Ok(())
    })();
    finish(&tmp, &dest, built)?;
    placed.sort_by_key(|p| p.slot);
    Ok(Import {
        name,
        dir: dest,
        placed,
        skipped,
    })
}

/// Write the file at `path` to `dest` as a Morphagene reel, and return what
/// would still stop the module loading it.
fn convert(path: &Path, dest: &Path) -> Result<Vec<Problem>> {
    let mut reel = Reel::open(path)?;
    let frames = reel.left.len() as u32;
    reel.markers = reel::clean_markers(reel.markers, frames);
    fs::write(dest, reel.to_wav_bytes()?).with_context(|| format!("writing {}", dest.display()))?;
    Ok(reel::problems(frames, reel.markers.len(), None))
}

/// The files under `dir`, as paths inside it joined with `/`, and their
/// full paths. Hidden files, `__MACOSX` folders from zips and links are
/// left out.
fn files(dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    let mut folders = vec![(String::new(), dir.to_path_buf())];
    while let Some((prefix, folder)) = folders.pop() {
        let entries =
            fs::read_dir(&folder).with_context(|| format!("reading {}", folder.display()))?;
        for e in entries {
            let e = e?;
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name == "__MACOSX" {
                continue;
            }
            let inside = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let kind = e.file_type()?;
            if kind.is_dir() {
                folders.push((inside, e.path()));
            } else if kind.is_file() {
                out.push((inside, e.path()));
            }
        }
    }
    Ok(out)
}

/// A set name as typed, trimmed. It becomes a folder name.
fn check_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        bail!("give the set a name");
    }
    if name.starts_with('.') {
        bail!("a set name can't start with a dot");
    }
    if name.contains(['/', ':']) || name.chars().any(char::is_control) {
        bail!("a set name can't contain / or :");
    }
    if name.len() > 200 {
        bail!("that set name is too long");
    }
    Ok(name.to_string())
}

/// A folder name made safe to use as a set name.
fn clean_name(folder: &str) -> String {
    let name: String = folder
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| if matches!(c, '/' | ':') { '-' } else { c })
        .take(100)
        .collect();
    let name = name.trim().trim_start_matches('.').trim();
    if name.is_empty() {
        "Imported pack".into()
    } else {
        name.into()
    }
}

/// `base`, or `base 2`, `base 3` and so on, whichever no set has yet.
fn free_name(root: &Path, base: &str) -> String {
    (1..)
        .map(|n| {
            if n == 1 {
                base.to_string()
            } else {
                format!("{base} {n}")
            }
        })
        .find(|name| !root.join(name).exists())
        .unwrap()
}

/// A new hidden folder under `root` to build a set in.
fn temp_dir(root: &Path, what: &str) -> Result<PathBuf> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    fs::create_dir_all(root).with_context(|| format!("making {}", root.display()))?;
    let dir = root.join(format!(
        ".{what}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, atomic::Ordering::Relaxed)
    ));
    fs::create_dir(&dir).with_context(|| format!("making {}", dir.display()))?;
    Ok(dir)
}

/// Rename a finished set from its hidden folder to `dest`. If building it
/// or the rename failed, remove the hidden folder.
fn finish(tmp: &Path, dest: &Path, built: Result<()>) -> Result<()> {
    let done = built.and_then(|()| {
        if dest.exists() {
            bail!("{} already exists", dest.display());
        }
        fs::rename(tmp, dest).with_context(|| format!("making {}", dest.display()))
    });
    if done.is_err() {
        let _ = fs::remove_dir_all(tmp);
    }
    done
}

/// Finder's order: case doesn't count, and runs of digits compare as
/// numbers, so "Take 2" comes before "Take 10".
fn finder_order(a: &str, b: &str) -> Ordering {
    fn digits(s: &mut std::iter::Peekable<std::str::Chars>) -> String {
        let mut out = String::new();
        while let Some(c) = s.peek().copied().filter(char::is_ascii_digit) {
            out.push(c);
            s.next();
        }
        out
    }
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        let ord = match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let (n, m) = (digits(&mut x), digits(&mut y));
                let (n, m) = (n.trim_start_matches('0'), m.trim_start_matches('0'));
                n.len().cmp(&m.len()).then_with(|| n.cmp(m))
            }
            (Some(c), Some(d)) => {
                x.next();
                y.next();
                c.to_lowercase().cmp(d.to_lowercase())
            }
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finder_order_counts_numbers() {
        let mut names = vec!["take 10", "Take 2", "take 1", "Gulls", "gulls b", "Take 02"];
        names.sort_by(|a, b| finder_order(a, b));
        assert_eq!(
            names,
            vec!["Gulls", "gulls b", "take 1", "Take 02", "Take 2", "take 10"]
        );
    }

    #[test]
    fn set_names() {
        assert_eq!(check_name("  Drones  ").unwrap(), "Drones");
        for bad in ["", "  ", ".hidden", "a/b", "a:b", "tab\there"] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
        assert_eq!(clean_name("..Pack: vol/1"), "Pack- vol-1");
        assert_eq!(clean_name("..."), "Imported pack");
    }
}
