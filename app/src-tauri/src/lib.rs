//! Tauri commands over mgreel-core. Every command that changes the card
//! backs up what it replaces into the app's data folder first.

use mgreel_core::card::{self, Card, CardIssue, SlotInfo};
use mgreel_core::edit::{Clipboard, Op, Session, Span, Stereo};
use mgreel_core::history::{self, BackupKind};
use mgreel_core::names::ReelLabel;
use mgreel_core::options::{self, Kind, OptionsFile};
use mgreel_core::reel::{self, Problem};
use mgreel_core::riff::{self, Format};
use mgreel_core::slice;
use mgreel_core::{BuildOptions, Clip, Reel, ReelFile};
use notify::{RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::ipc::Response;
use tauri::{AppHandle, Emitter, Manager, Runtime, State};

const VOLUMES: &str = "/Volumes";

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    format!("{e:#}")
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CardDto {
    root: String,
    label: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SlotDto {
    index: usize,
    file_name: String,
    present: bool,
    frames: u32,
    seconds: f64,
    markers: Vec<u32>,
    format_ok: bool,
    /// Readable but not in Morphagene format, so Convert can fix it.
    convertible: bool,
    problems: Vec<String>,
    name: String,
    note: String,
}

impl SlotDto {
    fn new(s: SlotInfo, label: &ReelLabel) -> Self {
        SlotDto {
            name: label.name.clone(),
            note: label.note.clone(),
            index: s.index,
            file_name: s.file_name.clone(),
            present: s.path.is_some(),
            frames: s.frames,
            seconds: s.seconds(),
            format_ok: s.format.is_some_and(|f| reel::is_morphagene_format(&f)),
            convertible: s.format.is_some_and(|f| !reel::is_morphagene_format(&f)),
            markers: s.markers,
            problems: s.problems.iter().map(Problem::to_string).collect(),
        }
    }
}

fn backup_dir<R: Runtime>(app: &AppHandle<R>, card: &Card) -> CmdResult<PathBuf> {
    let dir = app.path().app_data_dir().map_err(err)?;
    Ok(dir.join("backups").join(card.label()))
}

fn check_slot(index: usize) -> CmdResult<()> {
    if index >= card::SLOT_COUNT {
        return Err(format!("slot {index} does not exist"));
    }
    Ok(())
}

// Library

#[tauri::command]
async fn list_cards(app: AppHandle) -> Vec<CardDto> {
    let cards = Card::detect(Path::new(VOLUMES));
    snapshot_new_cards(&app, &cards);
    cards
        .into_iter()
        .map(|c| CardDto {
            root: c.root.display().to_string(),
            label: c.label(),
        })
        .collect()
}

#[tauri::command]
async fn list_slots(root: String) -> Vec<SlotDto> {
    let card = Card::new(root);
    // The card check reports a names file that can't be read.
    let names = card.names().unwrap_or_default();
    card.slots()
        .into_iter()
        .map(|s| {
            let label = names.get(s.index);
            SlotDto::new(s, label)
        })
        .collect()
}

#[tauri::command]
async fn set_reel_name(root: String, index: usize, name: String, note: String) -> CmdResult<()> {
    check_slot(index)?;
    Card::new(root)
        .set_name(index, ReelLabel { name, note })
        .map_err(err)
}

#[tauri::command]
async fn remove_names<R: Runtime>(app: AppHandle<R>, root: String) -> CmdResult<()> {
    let card = Card::new(root);
    card.remove_names(&backup_dir(&app, &card)?).map_err(err)
}

#[tauri::command]
async fn swap_slots(root: String, a: usize, b: usize) -> CmdResult<()> {
    check_slot(a)?;
    check_slot(b)?;
    Card::new(root).swap(a, b).map_err(err)
}

#[tauri::command]
async fn move_slot(root: String, from: usize, to: usize) -> CmdResult<()> {
    check_slot(from)?;
    check_slot(to)?;
    Card::new(root).move_slot(from, to).map_err(err)
}

#[tauri::command]
async fn clear_slot(app: AppHandle, root: String, index: usize) -> CmdResult<()> {
    check_slot(index)?;
    let card = Card::new(root);
    card.clear_slot(index, &backup_dir(&app, &card)?)
        .map_err(err)
}

#[tauri::command]
async fn backups_folder(app: AppHandle, root: String) -> CmdResult<String> {
    let dir = backup_dir(&app, &Card::new(root))?;
    std::fs::create_dir_all(&dir).map_err(err)?;
    Ok(dir.display().to_string())
}

#[tauri::command]
async fn backup_card(app: AppHandle, root: String) -> CmdResult<String> {
    let card = Card::new(root);
    let dest = backup_dir(&app, &card)?;
    Ok(card.backup(&dest).map_err(err)?.display().to_string())
}

#[tauri::command]
async fn restore_card(app: AppHandle, root: String, from: String) -> CmdResult<()> {
    let card = Card::new(root);
    let dir = backup_dir(&app, &card)?;
    card.restore(Path::new(&from), &dir).map_err(err)?;
    let state = app.state::<Snapshots>();
    let _guard = state.lock.lock().map_err(err)?;
    history::remember_card(&card, &dir).map_err(err)
}

// Card safety

/// Cards whose reels were checked for changes since this mount, and a lock
/// around the list of reel hashes the app has seen.
#[derive(Default)]
struct Snapshots {
    handled: Mutex<HashSet<String>>,
    lock: Mutex<()>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct SnapshotDto {
    root: String,
    label: String,
    files: Vec<String>,
    error: Option<String>,
}

/// Copy, in the background, the reels each newly mounted card holds that
/// the app hasn't seen, such as ones recorded on the module, before the app
/// changes anything. Emits `card-snapshot` when it copied something or
/// failed.
fn snapshot_new_cards(app: &AppHandle, cards: &[Card]) {
    let state = app.state::<Snapshots>();
    let Ok(mut handled) = state.handled.lock() else {
        return;
    };
    let roots: HashSet<String> = cards.iter().map(|c| c.root.display().to_string()).collect();
    // A card that was removed gets checked again when it comes back.
    handled.retain(|r| roots.contains(r));
    for card in cards {
        let root = card.root.display().to_string();
        if !handled.insert(root.clone()) {
            continue;
        }
        let (app, card) = (app.clone(), card.clone());
        std::thread::spawn(move || {
            let result = backup_dir(&app, &card).and_then(|dir| {
                let state = app.state::<Snapshots>();
                let _guard = state.lock.lock().map_err(err)?;
                history::snapshot(&card, &dir).map_err(err)
            });
            let dto = match result {
                Ok(None) => return,
                Ok(Some((_, files))) => SnapshotDto {
                    root,
                    label: card.label(),
                    files,
                    error: None,
                },
                Err(e) => {
                    // Let the next look at /Volumes try again.
                    if let Ok(mut handled) = app.state::<Snapshots>().handled.lock() {
                        handled.remove(&root);
                    }
                    SnapshotDto {
                        root,
                        label: card.label(),
                        files: Vec::new(),
                        error: Some(e),
                    }
                }
            };
            let _ = app.emit("card-snapshot", dto);
        });
    }
}

/// Note a reel the app wrote, so the next mount doesn't copy it as a change
/// made on the module. A failure here only costs a spare copy later.
fn remember<R: Runtime>(app: &AppHandle<R>, card: &Card, bytes: &[u8]) {
    let Ok(dir) = backup_dir(app, card) else {
        return;
    };
    let state = app.try_state::<Snapshots>();
    let _guard = state.as_ref().map(|s| s.lock.lock());
    if let Err(e) = history::remember(&dir, bytes) {
        eprintln!("not noting {}: {e:#}", card.label());
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StrayDto {
    path: String,
    /// The path from the card root.
    name: String,
    wav: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CardCheckDto {
    /// The file system, when it isn't the FAT32 the module needs.
    wrong_filesystem: Option<String>,
    stray: Vec<StrayDto>,
    clutter: usize,
    /// Why `mgreel.json` can't be read, when it can't.
    names_error: Option<String>,
}

/// The volume's file system as `diskutil` names it, such as "MS-DOS
/// (FAT32)". None for a folder that isn't a mounted volume.
fn filesystem(root: &Path) -> Option<String> {
    if root.parent() != Some(Path::new(VOLUMES)) {
        return None;
    }
    let out = std::process::Command::new("diskutil")
        .args(["info", "-plist"])
        .arg(root)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let after = text.split("<key>FilesystemUserVisibleName</key>").nth(1)?;
    let value = after.split("<string>").nth(1)?.split("</string>").next()?;
    Some(value.trim().to_string())
}

#[tauri::command]
async fn check_card(root: String) -> CmdResult<CardCheckDto> {
    let card = Card::new(&root);
    let mut stray = Vec::new();
    let mut clutter = 0;
    for issue in card.issues().map_err(err)? {
        match issue {
            CardIssue::Stray { path, wav } => stray.push(StrayDto {
                name: path
                    .strip_prefix(&card.root)
                    .unwrap_or(&path)
                    .display()
                    .to_string(),
                path: path.display().to_string(),
                wav,
            }),
            CardIssue::Clutter { paths } => clutter += paths.len(),
        }
    }
    Ok(CardCheckDto {
        wrong_filesystem: filesystem(&card.root).filter(|fs| !fs.contains("FAT32")),
        stray,
        clutter,
        names_error: card.names().err().map(err),
    })
}

#[tauri::command]
async fn remove_clutter(root: String) -> CmdResult<usize> {
    Card::new(root).remove_clutter().map_err(err)
}

#[tauri::command]
async fn convert_slot(app: AppHandle, root: String, index: usize) -> CmdResult<()> {
    check_slot(index)?;
    let card = Card::new(root);
    let bytes = card
        .convert_slot(index, &backup_dir(&app, &card)?)
        .map_err(err)?;
    remember(&app, &card, &bytes);
    Ok(())
}

#[tauri::command]
async fn adopt_file<R: Runtime>(
    app: AppHandle<R>,
    root: String,
    path: String,
    index: usize,
) -> CmdResult<()> {
    check_slot(index)?;
    let card = Card::new(root);
    let bytes = card
        .adopt(Path::new(&path), index, &backup_dir(&app, &card)?)
        .map_err(err)?;
    remember(&app, &card, &bytes);
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BackupDto {
    dir: String,
    kind: &'static str,
    files: Vec<String>,
    /// Milliseconds since 1970.
    created: u64,
}

#[tauri::command]
async fn list_backups<R: Runtime>(app: AppHandle<R>, root: String) -> CmdResult<Vec<BackupDto>> {
    let dir = backup_dir(&app, &Card::new(root))?;
    Ok(history::list(&dir)
        .into_iter()
        .map(|b| BackupDto {
            dir: b.dir.display().to_string(),
            kind: match b.kind {
                BackupKind::BeforeChange => "beforeChange",
                BackupKind::WholeCard => "wholeCard",
                BackupKind::FromCard => "fromCard",
            },
            files: b.files,
            created: b
                .created
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64),
        })
        .collect())
}

/// Put one file from this card's backups back on the card.
#[tauri::command]
async fn restore_file<R: Runtime>(
    app: AppHandle<R>,
    root: String,
    dir: String,
    file: String,
) -> CmdResult<()> {
    let card = Card::new(root);
    let backups = backup_dir(&app, &card)?;
    let from = Path::new(&dir).join(&file);
    let inside = std::fs::canonicalize(&from)
        .ok()
        .zip(std::fs::canonicalize(&backups).ok())
        .is_some_and(|(f, b)| f.starts_with(b));
    if !inside || file.contains('/') {
        return Err(format!("{} isn't in this card's backups", from.display()));
    }
    let bytes = card.restore_file(&from, &backups).map_err(err)?;
    if card::slot_index(&file).is_some() {
        remember(&app, &card, &bytes);
    }
    Ok(())
}

/// Unmount the card so it can be pulled out safely.
#[tauri::command]
async fn eject_card(root: String) -> CmdResult<()> {
    let root = PathBuf::from(root);
    if root.parent() != Some(Path::new(VOLUMES)) {
        return Err(format!("{} isn't a card", root.display()));
    }
    let out = std::process::Command::new("diskutil")
        .arg("eject")
        .arg(&root)
        .output()
        .map_err(err)?;
    if out.status.success() {
        Ok(())
    } else {
        let msg = String::from_utf8_lossy(&out.stderr);
        let msg = if msg.trim().is_empty() {
            String::from_utf8_lossy(&out.stdout)
        } else {
            msg
        };
        Err(format!("Couldn't eject: {}", msg.trim()))
    }
}

// Reel editor
//
// The open reel's audio lives here while it is edited. The editor holds the
// markers and sends them with each command; edits come back as spans for
// the editor to apply to its own copy of the audio.

/// The reel open in the editor.
struct OpenReel {
    root: String,
    index: usize,
    session: Session,
    /// `file_id` of the file on the card when last read or written.
    disk_id: u64,
}

#[derive(Default)]
struct Editing(Mutex<Option<OpenReel>>);

/// Splices copied in the editor. Stays across reels, so splices can be
/// pasted into another reel.
#[derive(Default)]
struct SpliceClipboard(Mutex<Option<Clipboard>>);

/// A hash of a whole reel file, so any change on the card shows, including
/// markers added on the module. Compared only within one run of the app.
fn file_id(bytes: &[u8]) -> u64 {
    use std::hash::{DefaultHasher, Hasher};
    let mut h = DefaultHasher::new();
    h.write(bytes);
    h.finish()
}

/// Run `f` on the open reel if it is `root`/`index`.
fn with_open<T>(
    editing: &Editing,
    root: &str,
    index: usize,
    f: impl FnOnce(&mut OpenReel) -> CmdResult<T>,
) -> CmdResult<T> {
    let mut guard = editing.0.lock().map_err(err)?;
    match guard.as_mut() {
        Some(open) if open.root == root && open.index == index => f(open),
        _ => Err(format!(
            "{} isn't open in the editor any more. Open it again.",
            card::slot_file_name(index)
        )),
    }
}

/// Open a reel for editing, as 48 kHz stereo floats for drawing and playback.
/// Layout, little-endian: u32 marker count, the u64 audio id, the markers as
/// u32 frames, then every left sample followed by every right sample as f32.
#[tauri::command]
async fn load_reel(editing: State<'_, Editing>, root: String, index: usize) -> CmdResult<Response> {
    check_slot(index)?;
    let bytes = std::fs::read(Card::new(&root).slot_path(index)).map_err(err)?;
    let file = ReelFile::parse(&bytes).map_err(err)?;
    let reel = Reel::from_file(&file).map_err(err)?;
    let disk_id = file_id(&bytes);
    let mut out = Vec::with_capacity(12 + reel.markers.len() * 4 + reel.left.len() * 8);
    out.extend_from_slice(&(reel.markers.len() as u32).to_le_bytes());
    out.extend_from_slice(&file.audio_id().to_le_bytes());
    for m in &reel.markers {
        out.extend_from_slice(&m.to_le_bytes());
    }
    for s in reel.left.iter().chain(&reel.right) {
        out.extend_from_slice(&s.to_le_bytes());
    }
    *editing.0.lock().map_err(err)? = Some(OpenReel {
        root,
        index,
        session: Session::new(Stereo::new(reel.left, reel.right)),
        disk_id,
    });
    Ok(Response::new(out))
}

/// Where the history stands after a command, for the editor.
struct Status {
    pos: usize,
    oldest: usize,
    /// Id of the current audio state.
    state: u64,
    /// The position holding the audio on the card, if undo or redo reach it.
    saved_pos: Option<usize>,
}

impl Status {
    fn of(s: &Session) -> Status {
        Status {
            pos: s.position(),
            oldest: s.oldest(),
            state: s.state(),
            saved_pos: s.saved_position(),
        }
    }
}

/// An edit's result. Layout, little-endian u32s unless noted: history
/// position, oldest reachable position, current state id, the position of
/// the saved audio (u32::MAX if unreachable), 1 if markers follow (else 0),
/// the marker count and markers, the span count, then for each span its
/// start, frames removed and frames inserted, then each span's inserted
/// left then right samples as f32.
fn edit_result(status: Status, markers: Option<&[u32]>, spans: &[Span]) -> Response {
    let samples: usize = spans.iter().map(|s| s.inserted.len() * 2).sum();
    let mut out = Vec::with_capacity(
        32 + markers.map_or(0, <[u32]>::len) * 4 + spans.len() * 12 + samples * 4,
    );
    let mut put = |v: u32| out.extend_from_slice(&v.to_le_bytes());
    put(status.pos as u32);
    put(status.oldest as u32);
    put(status.state as u32);
    put(status.saved_pos.map_or(u32::MAX, |p| p as u32));
    put(markers.is_some() as u32);
    let markers = markers.unwrap_or_default();
    put(markers.len() as u32);
    for &m in markers {
        put(m);
    }
    put(spans.len() as u32);
    for s in spans {
        put(s.start as u32);
        put(s.removed as u32);
        put(s.inserted.len() as u32);
    }
    for s in spans {
        for x in s.inserted.left.iter().chain(&s.inserted.right) {
            out.extend_from_slice(&x.to_le_bytes());
        }
    }
    Response::new(out)
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum EditOp {
    Delete,
    Duplicate,
    MoveTo { to: usize },
    Shuffle { seed: u64 },
    Reverse,
    Gain { db: f32 },
    Normalize { db: f32 },
    MatchLevels,
    FadeIn { ms: f32 },
    FadeOut { ms: f32 },
    SmoothLoop { ms: f32 },
    RemoveDc,
    HighPass { hz: f32 },
    Speed { semitones: f32 },
}

impl From<EditOp> for Op {
    fn from(op: EditOp) -> Op {
        match op {
            EditOp::Delete => Op::Delete,
            EditOp::Duplicate => Op::Duplicate,
            EditOp::MoveTo { to } => Op::MoveTo(to),
            EditOp::Shuffle { seed } => Op::Shuffle(seed),
            EditOp::Reverse => Op::Reverse,
            EditOp::Gain { db } => Op::GainDb(db),
            EditOp::Normalize { db } => Op::NormalizeDb(db),
            EditOp::MatchLevels => Op::MatchLevels,
            EditOp::FadeIn { ms } => Op::FadeInMs(ms),
            EditOp::FadeOut { ms } => Op::FadeOutMs(ms),
            EditOp::SmoothLoop { ms } => Op::SmoothLoopMs(ms),
            EditOp::RemoveDc => Op::RemoveDc,
            EditOp::HighPass { hz } => Op::HighPassHz(hz),
            EditOp::Speed { semitones } => Op::SpeedSemitones(semitones),
        }
    }
}

/// Apply `op` to splices `first..=last` of the open reel.
#[tauri::command]
async fn edit_reel(
    editing: State<'_, Editing>,
    root: String,
    index: usize,
    markers: Vec<u32>,
    first: usize,
    last: usize,
    op: EditOp,
) -> CmdResult<Response> {
    with_open(&editing, &root, index, |open| {
        let s = &mut open.session;
        let before = s.position();
        let markers = s.apply(&markers, first, last, op.into()).map_err(err)?;
        let spans: Vec<Span> = if s.position() != before {
            s.last_span().into_iter().collect()
        } else {
            Vec::new()
        };
        Ok(edit_result(Status::of(s), Some(&markers), &spans))
    })
}

/// Copy splices `first..=last` of the open reel. Returns how many.
#[tauri::command]
async fn copy_splices(
    editing: State<'_, Editing>,
    clipboard: State<'_, SpliceClipboard>,
    root: String,
    index: usize,
    markers: Vec<u32>,
    first: usize,
    last: usize,
) -> CmdResult<usize> {
    let clip = with_open(&editing, &root, index, |open| {
        open.session.copy(&markers, first, last).map_err(err)
    })?;
    let count = clip.lengths.len();
    *clipboard.0.lock().map_err(err)? = Some(clip);
    Ok(count)
}

/// Paste the copied splices before splice `at` of the open reel.
#[tauri::command]
async fn paste_splices(
    editing: State<'_, Editing>,
    clipboard: State<'_, SpliceClipboard>,
    root: String,
    index: usize,
    markers: Vec<u32>,
    at: usize,
) -> CmdResult<Response> {
    let clip = clipboard
        .0
        .lock()
        .map_err(err)?
        .clone()
        .ok_or("Copy some splices first.")?;
    with_open(&editing, &root, index, |open| {
        let s = &mut open.session;
        let markers = s.paste(&markers, at, &clip).map_err(err)?;
        let spans: Vec<Span> = s.last_span().into_iter().collect();
        Ok(edit_result(Status::of(s), Some(&markers), &spans))
    })
}

fn open_clip(path: &str) -> CmdResult<Stereo> {
    let clip = Clip::open(Path::new(path)).map_err(|e| {
        let name = Path::new(path)
            .file_name()
            .map_or(path.into(), |n| n.to_string_lossy());
        format!("{name}: {e:#}")
    })?;
    Ok(Stereo::new(clip.left, clip.right))
}

/// Insert WAV files as new splices before splice `at`, one splice each.
/// Any sample rate, bit depth, mono or stereo.
#[tauri::command]
async fn insert_files(
    editing: State<'_, Editing>,
    root: String,
    index: usize,
    markers: Vec<u32>,
    at: usize,
    paths: Vec<String>,
) -> CmdResult<Response> {
    let pieces = paths
        .iter()
        .map(|p| open_clip(p))
        .collect::<CmdResult<Vec<_>>>()?;
    with_open(&editing, &root, index, |open| {
        let s = &mut open.session;
        let markers = s.insert(&markers, at, &pieces).map_err(err)?;
        let spans: Vec<Span> = s.last_span().into_iter().collect();
        Ok(edit_result(Status::of(s), Some(&markers), &spans))
    })
}

/// Replace splice `splice` with a WAV file, or with `sos` set, mix the file
/// into it: 0 keeps only the file, 1 only what was there.
#[tauri::command]
async fn put_file(
    editing: State<'_, Editing>,
    root: String,
    index: usize,
    markers: Vec<u32>,
    splice: usize,
    path: String,
    sos: Option<f32>,
) -> CmdResult<Response> {
    let audio = open_clip(&path)?;
    with_open(&editing, &root, index, |open| {
        let s = &mut open.session;
        let before = s.position();
        let markers = match sos {
            Some(sos) => s.mix_splice(&markers, splice, &audio, sos),
            None => s.replace_splice(&markers, splice, audio),
        }
        .map_err(err)?;
        let spans: Vec<Span> = if s.position() != before {
            s.last_span().into_iter().collect()
        } else {
            Vec::new()
        };
        Ok(edit_result(Status::of(s), Some(&markers), &spans))
    })
}

/// Write splices `first..=last` to `dir` as 48 kHz 32-bit float WAVs named
/// after the reel and splice number, such as `mg3-07.wav`. Never
/// overwrites: a name that exists gets -2, -3 and so on. Returns the paths.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn export_splices(
    editing: State<'_, Editing>,
    root: String,
    index: usize,
    markers: Vec<u32>,
    first: usize,
    last: usize,
    dir: String,
) -> CmdResult<Vec<String>> {
    let pieces = with_open(&editing, &root, index, |open| {
        open.session.splices(&markers, first, last).map_err(err)
    })?;
    let stem = card::slot_file_name(index)
        .trim_end_matches(".wav")
        .to_string();
    let mut written = Vec::new();
    for (k, piece) in pieces.into_iter().enumerate() {
        let bytes = Reel {
            left: piece.left,
            right: piece.right,
            markers: Vec::new(),
        }
        .to_wav_bytes()
        .map_err(err)?;
        let base = format!("{stem}-{:02}", first + k + 1);
        let mut path = Path::new(&dir).join(format!("{base}.wav"));
        let mut n = 2;
        while path.exists() {
            path = Path::new(&dir).join(format!("{base}-{n}.wav"));
            n += 1;
        }
        std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        written.push(path.display().to_string());
    }
    Ok(written)
}

/// Undo or redo audio edits until the history is at `pos`. With `state`,
/// refuse unless `pos` still holds that audio: an edit after undo replaces
/// the history past its position.
#[tauri::command]
async fn seek_history(
    editing: State<'_, Editing>,
    root: String,
    index: usize,
    pos: usize,
    state: Option<u64>,
) -> CmdResult<Response> {
    with_open(&editing, &root, index, |open| {
        // Work out the status first: the spans borrow the session.
        let s = &open.session;
        let found = s.state_at(pos).ok_or(if pos < s.oldest() {
            "that edit is too old to undo: the history keeps about 380 MB"
        } else {
            "that step was replaced by later edits"
        })?;
        if state.is_some_and(|want| want != found) {
            return Err("that step was replaced by later edits".into());
        }
        let status = Status {
            pos,
            oldest: s.oldest(),
            state: found,
            saved_pos: s.saved_position(),
        };
        let spans = open.session.seek(pos).map_err(err)?;
        Ok(edit_result(status, None, &spans))
    })
}

/// Write the open reel to its slot. If the audio matches the card, only the
/// cue chunk changes and every other chunk is kept; otherwise the whole
/// reel is written. Refuses if the file on the card changed since it was
/// read, as happens when a different card with the same name is mounted.
#[tauri::command]
async fn save_reel<R: Runtime>(
    app: AppHandle<R>,
    editing: State<'_, Editing>,
    root: String,
    index: usize,
    markers: Vec<u32>,
) -> CmdResult<SlotDto> {
    check_slot(index)?;
    with_open(&editing, &root, index, |open| {
        let card = Card::new(&root);
        let path = card.slot_path(index);
        // Any change to the file counts, not just its audio: the module
        // writes markers to the card when you splice on it.
        let on_card = std::fs::read(&path).map_err(err)?;
        if file_id(&on_card) != open.disk_id {
            return Err(format!(
                "{} has changed on the card since it was opened, so nothing was saved. \
                 Splicing on the module, another app or a different card with the same \
                 name can do this. Open the reel again to see the card's version.",
                card::slot_file_name(index)
            ));
        }
        let mut file = ReelFile::parse(&on_card).map_err(err)?;
        let format = file.format().map_err(err)?;
        let s = &open.session;
        let frames = s.frames() as u32;
        let markers = reel::clean_markers(markers, frames);
        check_limits(frames, markers.len())?;
        let bytes = if s.matches_saved() && reel::is_morphagene_format(&format) {
            file.set_markers(&markers);
            file.to_bytes()
        } else {
            let audio = s.audio();
            Reel {
                left: audio.left.clone(),
                right: audio.right.clone(),
                markers,
            }
            .to_wav_bytes()
            .map_err(err)?
        };
        card.write_slot(index, &bytes, &backup_dir(&app, &card)?)
            .map_err(err)?;
        remember(&app, &card, &bytes);
        open.disk_id = file_id(&bytes);
        open.session.mark_saved();
        let names = card.names().unwrap_or_default();
        Ok(SlotDto::new(
            card.slots().swap_remove(index),
            names.get(index),
        ))
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum SliceMode {
    Phrases,
    Gestures,
}

/// Propose markers for the open reel, edits included. Nothing is written;
/// the editor shows them as unsaved changes.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
async fn auto_slice(
    editing: State<'_, Editing>,
    root: String,
    index: usize,
    mode: SliceMode,
    min_seconds: f32,
    max_seconds: Option<f32>,
    max_markers: usize,
    keep: Vec<u32>,
) -> CmdResult<Vec<u32>> {
    let options = slice::Options {
        mode: match mode {
            SliceMode::Phrases => slice::Mode::Phrases,
            SliceMode::Gestures => slice::Mode::Gestures,
        },
        min_seconds,
        max_seconds,
        max_markers,
    };
    with_open(&editing, &root, index, |open| {
        let audio = open.session.audio();
        Ok(slice::auto_slice_keeping(
            &audio.left,
            &audio.right,
            options,
            &keep,
        ))
    })
}

fn check_limits(frames: u32, markers: usize) -> CmdResult<()> {
    let problems = reel::problems(frames, markers, None);
    if problems.is_empty() {
        return Ok(());
    }
    let text: Vec<String> = problems.iter().map(Problem::to_string).collect();
    Err(format!(
        "the Morphagene will not load this reel: {}",
        text.join("; ")
    ))
}

// Builder

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceDto {
    path: String,
    name: String,
    /// Length once resampled to 48 kHz.
    frames: u32,
    error: Option<String>,
}

/// Read source headers to get lengths without decoding.
#[tauri::command]
async fn probe_sources(paths: Vec<String>) -> Vec<SourceDto> {
    paths
        .into_iter()
        .map(|path| {
            let name = Path::new(&path)
                .file_name()
                .map_or(path.clone(), |n| n.to_string_lossy().into_owned());
            match probe(Path::new(&path)) {
                Ok(frames) => SourceDto {
                    path,
                    name,
                    frames,
                    error: None,
                },
                Err(e) => SourceDto {
                    path,
                    name,
                    frames: 0,
                    error: Some(format!("{e:#}")),
                },
            }
        })
        .collect()
}

fn probe(path: &Path) -> anyhow::Result<u32> {
    use anyhow::Context;
    let chunks = riff::scan_file(path)?;
    let find = |id: &[u8; 4]| chunks.iter().find(|c| &c.id == id);
    let format = Format::parse(&riff::read_chunk(
        path,
        find(b"fmt ").context("no fmt chunk")?,
    )?)?;
    if format.channels == 0 || format.channels > 2 {
        anyhow::bail!("{} channels; only mono and stereo work", format.channels);
    }
    let frames = find(b"data").map_or(0, |c| c.size as u64) / format.frame_bytes().max(1) as u64;
    Ok((frames * reel::SAMPLE_RATE as u64 / format.sample_rate.max(1) as u64) as u32)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourceSpec {
    path: String,
    /// Trim range in 48 kHz frames. None keeps the whole clip.
    start: Option<u32>,
    end: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
enum Target {
    Slot { root: String, index: usize },
    File { path: String },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BuildResult {
    frames: u32,
    markers: usize,
    written_to: String,
}

#[tauri::command]
async fn build_reel(
    app: AppHandle,
    sources: Vec<SourceSpec>,
    normalize: bool,
    fade_ms: u32,
    target: Target,
) -> CmdResult<BuildResult> {
    if sources.is_empty() {
        return Err("add at least one source file".into());
    }
    let mut clips = Vec::new();
    for source in &sources {
        let mut clip =
            Clip::open(Path::new(&source.path)).map_err(|e| format!("{}: {e:#}", source.path))?;
        if source.start.is_some() || source.end.is_some() {
            let end = source.end.unwrap_or(clip.frames());
            clip.trim(source.start.unwrap_or(0), end);
        }
        clips.push(clip);
    }
    let options = BuildOptions {
        // -1 dBFS leaves a little headroom under full scale.
        normalize: normalize.then_some(0.891),
        // The UI caps fades at 500 ms. Clamp here too so a larger value
        // can't overflow the frame count.
        fade_frames: fade_ms.min(500) * reel::SAMPLE_RATE / 1000,
    };
    let reel = Reel::build(clips, options);
    check_limits(reel.frames(), reel.markers.len())?;
    let bytes = reel.to_wav_bytes().map_err(err)?;
    let written_to = match target {
        Target::Slot { root, index } => {
            check_slot(index)?;
            let card = Card::new(root);
            card.write_slot(index, &bytes, &backup_dir(&app, &card)?)
                .map_err(err)?;
            remember(&app, &card, &bytes);
            card.slot_path(index)
        }
        Target::File { path } => {
            std::fs::write(&path, &bytes).map_err(err)?;
            PathBuf::from(path)
        }
    };
    Ok(BuildResult {
        frames: reel.frames(),
        markers: reel.markers.len(),
        written_to: written_to.display().to_string(),
    })
}

// options.txt

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ChoiceDto {
    value: String,
    label: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OptionDto {
    key: String,
    value: String,
    comment: String,
    name: Option<&'static str>,
    /// Allowed values, when the option is a fixed choice.
    choices: Option<Vec<ChoiceDto>>,
    ratio: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OptionsDto {
    exists: bool,
    firmware: Option<u32>,
    text: String,
    entries: Vec<OptionDto>,
}

#[tauri::command]
async fn read_options(root: String) -> CmdResult<OptionsDto> {
    let path = Card::new(root).options_path();
    if !path.is_file() {
        return Ok(OptionsDto {
            exists: false,
            firmware: None,
            text: String::new(),
            entries: Vec::new(),
        });
    }
    let text = std::fs::read_to_string(&path).map_err(err)?;
    let file = OptionsFile::parse(&text);
    let entries = file
        .entries()
        .into_iter()
        .map(|e| OptionDto {
            name: e.spec.map(|s| s.name),
            choices: e.spec.and_then(|s| match s.kind {
                Kind::Choice(choices) => Some(
                    choices
                        .iter()
                        .map(|(v, label)| ChoiceDto {
                            value: v.to_string(),
                            label,
                        })
                        .collect(),
                ),
                Kind::Ratio => None,
            }),
            ratio: e.spec.is_some_and(|s| s.kind == Kind::Ratio),
            key: e.key,
            value: e.value,
            comment: e.comment,
        })
        .collect();
    Ok(OptionsDto {
        exists: true,
        firmware: file.firmware(),
        text,
        entries,
    })
}

#[derive(Deserialize)]
struct OptionChange {
    key: String,
    value: String,
}

#[tauri::command]
async fn write_options(app: AppHandle, root: String, changes: Vec<OptionChange>) -> CmdResult<()> {
    let card = Card::new(root);
    let text = std::fs::read_to_string(card.options_path()).map_err(err)?;
    let mut file = OptionsFile::parse(&text);
    for change in &changes {
        file.set(&change.key, &change.value).map_err(err)?;
    }
    card.write_options(&file.to_text(), &backup_dir(&app, &card)?)
        .map_err(err)
}

/// Save hand-edited text. Every setting line must still hold a valid value.
#[tauri::command]
async fn write_options_text(app: AppHandle, root: String, text: String) -> CmdResult<()> {
    for entry in OptionsFile::parse(&text).entries() {
        options::validate(&entry.key, &entry.value).map_err(err)?;
    }
    let card = Card::new(root);
    card.write_options(&text, &backup_dir(&app, &card)?)
        .map_err(err)
}

/// Tell the window when a volume mounts or unmounts.
fn watch_volumes(app: &AppHandle) -> notify::Result<notify::RecommendedWatcher> {
    let handle = app.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_ok() {
            let _ = handle.emit("cards-changed", ());
        }
    })?;
    watcher.watch(Path::new(VOLUMES), RecursiveMode::NonRecursive)?;
    Ok(watcher)
}

/// Keeps the watcher alive for the life of the app.
struct VolumeWatcher(#[allow(dead_code)] Option<notify::RecommendedWatcher>);

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let watcher = watch_volumes(app.handle())
                .map_err(|e| eprintln!("not watching {VOLUMES}: {e}"))
                .ok();
            app.manage(VolumeWatcher(watcher));
            app.manage(Editing::default());
            app.manage(SpliceClipboard::default());
            app.manage(Snapshots::default());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_cards,
            list_slots,
            swap_slots,
            move_slot,
            clear_slot,
            backups_folder,
            backup_card,
            restore_card,
            check_card,
            remove_clutter,
            set_reel_name,
            remove_names,
            convert_slot,
            adopt_file,
            list_backups,
            restore_file,
            eject_card,
            load_reel,
            save_reel,
            edit_reel,
            copy_splices,
            paste_splices,
            seek_history,
            insert_files,
            put_file,
            export_splices,
            auto_slice,
            probe_sources,
            build_reel,
            read_options,
            write_options,
            write_options_text,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    //! The editor commands through Tauri's IPC layer on a mock runtime,
    //! against a card folder in the temp directory.

    use super::*;
    use tauri::WebviewWindow;
    use tauri::ipc::{CallbackFn, InvokeBody, InvokeResponseBody};
    use tauri::test::{
        INVOKE_KEY, MockRuntime, get_ipc_response, mock_builder, mock_context, noop_assets,
    };
    use tauri::webview::InvokeRequest;

    fn window() -> (tauri::App<MockRuntime>, WebviewWindow<MockRuntime>) {
        let app = mock_builder()
            .manage(Editing::default())
            .manage(SpliceClipboard::default())
            .invoke_handler(tauri::generate_handler![
                load_reel,
                edit_reel,
                copy_splices,
                paste_splices,
                seek_history,
                insert_files,
                put_file,
                export_splices,
                save_reel,
                check_card,
                adopt_file,
                list_backups,
                restore_file,
                list_slots,
                swap_slots,
                set_reel_name,
                remove_names,
            ])
            .build(mock_context(noop_assets()))
            .unwrap();
        let w = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        (app, w)
    }

    fn call(
        w: &WebviewWindow<MockRuntime>,
        cmd: &str,
        args: serde_json::Value,
    ) -> Result<InvokeResponseBody, serde_json::Value> {
        get_ipc_response(
            w,
            InvokeRequest {
                cmd: cmd.into(),
                callback: CallbackFn(0),
                error: CallbackFn(1),
                url: "tauri://localhost".parse().unwrap(),
                body: InvokeBody::Json(args),
                headers: Default::default(),
                invoke_key: INVOKE_KEY.to_string(),
            },
        )
    }

    fn json(body: InvokeResponseBody) -> serde_json::Value {
        match body {
            InvokeResponseBody::Json(json) => serde_json::from_str(&json).unwrap(),
            InvokeResponseBody::Raw(_) => panic!("expected JSON, got bytes"),
        }
    }

    fn raw(body: InvokeResponseBody) -> Vec<u8> {
        match body {
            InvokeResponseBody::Raw(bytes) => bytes,
            InvokeResponseBody::Json(json) => panic!("expected bytes, got {json}"),
        }
    }

    /// The edit reply as the frontend reads it: position, oldest, markers,
    /// and (start, removed, inserted left samples) per span.
    #[allow(clippy::type_complexity)]
    fn decode(bytes: &[u8]) -> (u32, u32, Option<Vec<u32>>, Vec<(u32, u32, Vec<f32>)>) {
        let mut at = 0;
        let mut u32_ = || {
            let v = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
            at += 4;
            v
        };
        let pos = u32_();
        let oldest = u32_();
        let _state = u32_();
        let _saved_pos = u32_();
        let has = u32_() == 1;
        let n = u32_();
        let markers: Vec<u32> = (0..n).map(|_| u32_()).collect();
        let count = u32_();
        let heads: Vec<(u32, u32, u32)> = (0..count).map(|_| (u32_(), u32_(), u32_())).collect();
        let mut spans = Vec::new();
        for (start, removed, inserted) in heads {
            let left = (0..inserted)
                .map(|_| f32::from_bits(u32_()))
                .collect::<Vec<_>>();
            (0..inserted).for_each(|_| {
                u32_();
            });
            spans.push((start, removed, left));
        }
        assert_eq!(at, bytes.len(), "reply has trailing bytes");
        (pos, oldest, has.then_some(markers), spans)
    }

    #[test]
    fn card_check_adopt_and_restore_through_ipc() {
        let root = std::env::temp_dir().join(format!("mgreel-ipc-check-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("field")).unwrap();
        let root_s = root.display().to_string();
        let reel = |v: f32| {
            Reel {
                left: vec![v; 4800],
                right: vec![v; 4800],
                markers: vec![2400],
            }
            .to_wav_bytes()
            .unwrap()
        };
        std::fs::write(root.join("mg1.wav"), reel(0.1)).unwrap();
        std::fs::write(root.join("field/rain.wav"), reel(0.2)).unwrap();
        std::fs::write(root.join("._mg1.wav"), b"junk").unwrap();
        let (_app, w) = window();

        let check = json(call(&w, "check_card", serde_json::json!({ "root": root_s })).unwrap());
        assert_eq!(check["clutter"], 1);
        assert_eq!(check["stray"][0]["name"], "field/rain.wav");
        assert_eq!(check["wrongFilesystem"], serde_json::Value::Null);

        let stray = check["stray"][0]["path"].as_str().unwrap();
        call(
            &w,
            "adopt_file",
            serde_json::json!({ "root": root_s, "path": stray, "index": 1 }),
        )
        .unwrap();
        assert_eq!(std::fs::read(root.join("mg2.wav")).unwrap(), reel(0.2));
        assert!(!root.join("field/rain.wav").exists());

        // The adopted file's backup can go back on the card, but nothing
        // outside the backups folder can.
        let backups =
            json(call(&w, "list_backups", serde_json::json!({ "root": root_s })).unwrap());
        let dir = backups[0]["dir"].as_str().unwrap();
        assert_eq!(backups[0]["kind"], "beforeChange");
        assert_eq!(backups[0]["files"][0], "rain.wav");
        let refused = call(
            &w,
            "restore_file",
            serde_json::json!({ "root": root_s, "dir": dir, "file": "../../../etc/hosts" }),
        );
        assert!(refused.is_err());
        let refused = call(
            &w,
            "restore_file",
            serde_json::json!({ "root": root_s, "dir": root_s, "file": "mg1.wav" }),
        );
        assert!(refused.is_err(), "the card isn't a backup folder");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reel_names_through_ipc() {
        let root = std::env::temp_dir().join(format!("mgreel-ipc-names-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root_s = root.display().to_string();
        let reel = Reel {
            left: vec![0.1; 4800],
            right: vec![0.1; 4800],
            markers: vec![],
        }
        .to_wav_bytes()
        .unwrap();
        std::fs::write(root.join("mg1.wav"), &reel).unwrap();
        std::fs::write(root.join("mg2.wav"), &reel).unwrap();
        let (_app, w) = window();
        let slots = |w: &WebviewWindow<MockRuntime>| {
            json(call(w, "list_slots", serde_json::json!({ "root": root_s })).unwrap())
        };

        call(
            &w,
            "set_reel_name",
            serde_json::json!({ "root": root_s, "index": 0, "name": " Harbour ", "note": "gulls at dawn" }),
        )
        .unwrap();
        assert_eq!(slots(&w)[0]["name"], "Harbour");
        assert_eq!(slots(&w)[0]["note"], "gulls at dawn");
        call(
            &w,
            "swap_slots",
            serde_json::json!({ "root": root_s, "a": 0, "b": 1 }),
        )
        .unwrap();
        assert_eq!(slots(&w)[0]["name"], "");
        assert_eq!(slots(&w)[1]["name"], "Harbour");

        // A broken names file shows in the card check and stops moves until
        // it is removed.
        std::fs::write(root.join("mgreel.json"), "{").unwrap();
        let check = json(call(&w, "check_card", serde_json::json!({ "root": root_s })).unwrap());
        assert!(check["namesError"].is_string());
        assert!(
            call(
                &w,
                "swap_slots",
                serde_json::json!({ "root": root_s, "a": 0, "b": 1 }),
            )
            .is_err()
        );
        call(&w, "remove_names", serde_json::json!({ "root": root_s })).unwrap();
        let check = json(call(&w, "check_card", serde_json::json!({ "root": root_s })).unwrap());
        assert!(check["namesError"].is_null());
        assert!(!root.join("mgreel.json").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn edit_undo_paste_and_save_through_ipc() {
        let root = std::env::temp_dir().join(format!("mgreel-ipc-test-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let root_s = root.display().to_string();
        let card = Card::new(&root);
        // Three splices of 0.1, 0.2 and 0.3, lasting 0.1, 0.2 and 0.3 s.
        let mut left = Vec::new();
        for (v, n) in [(0.1f32, 4800), (0.2, 9600), (0.3, 14400)] {
            left.extend(std::iter::repeat_n(v, n));
        }
        let reel = Reel {
            right: left.clone(),
            left,
            markers: vec![4800, 14400],
        };
        std::fs::write(card.slot_path(0), reel.to_wav_bytes().unwrap()).unwrap();

        let (app, w) = window();
        let args = |extra: serde_json::Value| {
            let mut v = serde_json::json!({ "root": root_s, "index": 0 });
            v.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            v
        };

        let loaded = raw(call(&w, "load_reel", args(serde_json::json!({}))).unwrap());
        assert_eq!(u32::from_le_bytes(loaded[0..4].try_into().unwrap()), 2);

        // Delete the middle splice.
        let reply = raw(call(
            &w,
            "edit_reel",
            args(serde_json::json!({ "markers": [4800, 14400], "first": 1, "last": 1, "op": { "kind": "delete" } })),
        )
        .unwrap());
        // MGREEL_DUMP_DIR keeps the replies, to check the frontend's parser.
        if let Ok(dir) = std::env::var("MGREEL_DUMP_DIR") {
            std::fs::write(Path::new(&dir).join("load.bin"), &loaded).unwrap();
            std::fs::write(Path::new(&dir).join("delete.bin"), &reply).unwrap();
        }
        let (pos, _, markers, spans) = decode(&reply);
        assert_eq!(pos, 1);
        assert_eq!(markers, Some(vec![4800]));
        assert_eq!(spans, vec![(4800, 9600, vec![])]);

        // Undo puts the 0.2 splice back; redo takes it out again.
        let undo = raw(call(&w, "seek_history", args(serde_json::json!({ "pos": 0 }))).unwrap());
        if let Ok(dir) = std::env::var("MGREEL_DUMP_DIR") {
            std::fs::write(Path::new(&dir).join("undo.bin"), &undo).unwrap();
        }
        let (pos, _, markers, spans) = decode(&undo);
        assert_eq!((pos, markers), (0, None));
        assert_eq!(spans.len(), 1);
        assert_eq!((spans[0].0, spans[0].1, spans[0].2.len()), (4800, 0, 9600));
        assert!(spans[0].2.iter().all(|&x| x == 0.2));
        let (pos, ..) = decode(&raw(call(
            &w,
            "seek_history",
            args(serde_json::json!({ "pos": 1 })),
        )
        .unwrap()));
        assert_eq!(pos, 1);
        // Asking for position 0 with the wrong state id is refused.
        let stale = call(
            &w,
            "seek_history",
            args(serde_json::json!({ "pos": 0, "state": 99 })),
        )
        .unwrap_err();
        assert!(stale.to_string().contains("replaced"), "{stale}");

        // A gain change in dB arrives with camelCase fields.
        let (pos, _, _, spans) = decode(&raw(call(
            &w,
            "edit_reel",
            args(serde_json::json!({ "markers": [4800], "first": 0, "last": 0, "op": { "kind": "gain", "db": -6.0206 } })),
        )
        .unwrap()));
        assert_eq!(pos, 2);
        assert!((spans[0].2[0] - 0.05).abs() < 1e-4);

        // Copy the first splice and paste it at the end.
        call(
            &w,
            "copy_splices",
            args(serde_json::json!({ "markers": [4800], "first": 0, "last": 0 })),
        )
        .unwrap();
        let (_, _, markers, _) = decode(&raw(call(
            &w,
            "paste_splices",
            args(serde_json::json!({ "markers": [4800], "at": 2 })),
        )
        .unwrap()));
        assert_eq!(markers, Some(vec![4800, 19200]));

        // Insert a file as a new splice at the start, then undo it.
        let extra = root.join("extra.wav");
        let one = Reel {
            left: vec![0.7; 480],
            right: vec![0.7; 480],
            markers: vec![],
        };
        std::fs::write(&extra, one.to_wav_bytes().unwrap()).unwrap();
        let (pos, _, markers, _) = decode(&raw(call(
            &w,
            "insert_files",
            args(serde_json::json!({ "markers": [4800, 19200], "at": 0, "paths": [extra.display().to_string()] })),
        )
        .unwrap()));
        assert_eq!(markers, Some(vec![480, 5280, 19680]));
        call(
            &w,
            "seek_history",
            args(serde_json::json!({ "pos": pos - 1 })),
        )
        .unwrap();

        // Mix it half and half into the first splice.
        let (_, _, markers, spans) = decode(&raw(call(
            &w,
            "put_file",
            args(serde_json::json!({ "markers": [4800, 19200], "splice": 0, "path": extra.display().to_string(), "sos": 0.5 })),
        )
        .unwrap()));
        assert_eq!(markers, Some(vec![4800, 19200]));
        assert!((spans[0].2[0] - (0.05 * 0.5 + 0.7 * 0.5)).abs() < 1e-4);
        assert!(
            (spans[0].2[480] - 0.05 * 0.5).abs() > 0.01,
            "past the file the splice is unchanged"
        );

        // Export the last two splices.
        let out = root.join("export");
        std::fs::create_dir_all(&out).unwrap();
        let body = call(
            &w,
            "export_splices",
            args(serde_json::json!({ "markers": [4800, 19200], "first": 1, "last": 2, "dir": out.display().to_string() })),
        )
        .unwrap();
        let paths: Vec<String> = body.deserialize().unwrap();
        assert_eq!(paths.len(), 2);
        assert!(paths[0].ends_with("mg1-02.wav"), "{paths:?}");
        assert_eq!(Reel::open(Path::new(&paths[1])).unwrap().frames(), 4800);
        std::fs::remove_file(&extra).unwrap();
        std::fs::remove_dir_all(&out).unwrap();
        // Undo the mix so the save below sees the pasted reel.
        call(&w, "seek_history", args(serde_json::json!({ "pos": 3 }))).unwrap();

        // Save writes the edited audio and markers.
        call(
            &w,
            "save_reel",
            args(serde_json::json!({ "markers": [4800, 19200] })),
        )
        .unwrap();
        let saved = Reel::open(&card.slot_path(0)).unwrap();
        assert_eq!(saved.frames(), 24000);
        assert_eq!(saved.markers, vec![4800, 19200]);
        assert!((saved.left[0] - 0.05).abs() < 1e-4);
        assert_eq!(saved.left[4800], 0.3);
        assert!((saved.left[23999] - 0.05).abs() < 1e-4);

        // Save, undo, then a different edit at the same history position:
        // the next save must write that edit's audio, not keep the card's.
        call(&w, "seek_history", args(serde_json::json!({ "pos": 2 }))).unwrap();
        let (pos, ..) = decode(&raw(call(
            &w,
            "edit_reel",
            args(serde_json::json!({ "markers": [4800], "first": 1, "last": 1, "op": { "kind": "gain", "db": -20.0 } })),
        )
        .unwrap()));
        assert_eq!(pos, 3);
        call(
            &w,
            "save_reel",
            args(serde_json::json!({ "markers": [4800] })),
        )
        .unwrap();
        let saved = Reel::open(&card.slot_path(0)).unwrap();
        assert_eq!(saved.frames(), 19200);
        assert!(
            (saved.left[4800] - 0.03).abs() < 1e-4,
            "{}",
            saved.left[4800]
        );

        // Markers changed on the card, as splicing on the module does, make
        // save refuse even though the audio is the same.
        let mut on_card = ReelFile::open(&card.slot_path(0)).unwrap();
        on_card.set_markers(&[4800, 9600]);
        std::fs::write(card.slot_path(0), on_card.to_bytes()).unwrap();
        let refused = call(
            &w,
            "save_reel",
            args(serde_json::json!({ "markers": [4800] })),
        )
        .unwrap_err();
        assert!(
            refused.to_string().contains("changed on the card"),
            "{refused}"
        );

        // A different file in the slot makes save refuse.
        std::fs::write(card.slot_path(0), reel.to_wav_bytes().unwrap()).unwrap();
        let refused = call(
            &w,
            "save_reel",
            args(serde_json::json!({ "markers": [4800] })),
        )
        .unwrap_err();
        assert!(
            refused.to_string().contains("changed on the card"),
            "{refused}"
        );

        // Clean up the card and the backups the saves made.
        let backups = backup_dir(app.handle(), &card).unwrap();
        std::fs::remove_dir_all(&backups).ok();
        std::fs::remove_dir_all(&root).unwrap();
    }
}
