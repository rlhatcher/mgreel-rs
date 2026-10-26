//! Tauri commands over mgreel-core. Every command that changes the card
//! backs up what it replaces into the app's data folder first.

use mgreel_core::card::{self, Card, SlotInfo};
use mgreel_core::options::{self, Kind, OptionsFile};
use mgreel_core::reel::{self, Problem};
use mgreel_core::riff::{self, Format};
use mgreel_core::slice;
use mgreel_core::{BuildOptions, Clip, Reel, ReelFile};
use notify::{RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::ipc::Response;
use tauri::{AppHandle, Emitter, Manager};

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
    problems: Vec<String>,
}

impl From<SlotInfo> for SlotDto {
    fn from(s: SlotInfo) -> Self {
        SlotDto {
            index: s.index,
            file_name: s.file_name.clone(),
            present: s.path.is_some(),
            frames: s.frames,
            seconds: s.seconds(),
            format_ok: s.format.is_some_and(|f| reel::is_morphagene_format(&f)),
            markers: s.markers,
            problems: s.problems.iter().map(Problem::to_string).collect(),
        }
    }
}

fn backup_dir(app: &AppHandle, card: &Card) -> CmdResult<PathBuf> {
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
async fn list_cards() -> Vec<CardDto> {
    Card::detect(Path::new(VOLUMES))
        .into_iter()
        .map(|c| CardDto {
            root: c.root.display().to_string(),
            label: c.label(),
        })
        .collect()
}

#[tauri::command]
async fn list_slots(root: String) -> Vec<SlotDto> {
    Card::new(root)
        .slots()
        .into_iter()
        .map(SlotDto::from)
        .collect()
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
    card.restore(Path::new(&from), &backup_dir(&app, &card)?)
        .map_err(err)
}

// Reel editor

/// The reel as 48 kHz stereo floats, for drawing and playback.
/// Layout, little-endian: u32 marker count, the u64 audio id, the markers as
/// u32 frames, then every left sample followed by every right sample as f32.
#[tauri::command]
async fn load_reel(root: String, index: usize) -> CmdResult<Response> {
    check_slot(index)?;
    let file = ReelFile::open(&Card::new(root).slot_path(index)).map_err(err)?;
    let reel = Reel::from_file(&file).map_err(err)?;
    let mut out = Vec::with_capacity(12 + reel.markers.len() * 4 + reel.left.len() * 8);
    out.extend_from_slice(&(reel.markers.len() as u32).to_le_bytes());
    out.extend_from_slice(&file.audio_id().to_le_bytes());
    for m in &reel.markers {
        out.extend_from_slice(&m.to_le_bytes());
    }
    for s in reel.left.iter().chain(&reel.right) {
        out.extend_from_slice(&s.to_le_bytes());
    }
    Ok(Response::new(out))
}

/// Save new markers. A reel already in Morphagene format keeps its audio
/// and other chunks untouched; any other format is converted on save.
#[tauri::command]
async fn save_markers(
    app: AppHandle,
    root: String,
    index: usize,
    markers: Vec<u32>,
    audio_id: String,
) -> CmdResult<Saved> {
    check_slot(index)?;
    let card = Card::new(root);
    let path = card.slot_path(index);
    let mut file = ReelFile::open(&path).map_err(err)?;
    // A different card with the same name mounts at the same path, and
    // the reel can change on the card while it is open here. Either way,
    // these markers belong to other audio.
    if file.audio_id().to_string() != audio_id {
        return Err(format!(
            "{} has changed since it was opened, so the markers were not saved. \
             Is this the same card?",
            card::slot_file_name(index)
        ));
    }
    let format = file.format().map_err(err)?;
    let bytes = if reel::is_morphagene_format(&format) {
        let frames = file.frames().map_err(err)?;
        let markers = reel::clean_markers(markers, frames);
        check_limits(frames, markers.len())?;
        file.set_markers(&markers);
        file.to_bytes()
    } else {
        let mut reel = Reel::from_file(&file).map_err(err)?;
        reel.markers = reel::clean_markers(markers, reel.frames());
        check_limits(reel.frames(), reel.markers.len())?;
        reel.to_wav_bytes().map_err(err)?
    };
    card.write_slot(index, &bytes, &backup_dir(&app, &card)?)
        .map_err(err)?;
    // Converting the format changes the audio, so the editor needs the new id.
    let audio_id = ReelFile::parse(&bytes).map_err(err)?.audio_id().to_string();
    Ok(Saved {
        slot: card.slots().swap_remove(index).into(),
        audio_id,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Saved {
    slot: SlotDto,
    audio_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum SliceMode {
    Phrases,
    Gestures,
}

/// Propose markers for a reel. Nothing is written; the editor shows them as
/// unsaved changes.
#[tauri::command]
async fn auto_slice(
    root: String,
    index: usize,
    mode: SliceMode,
    min_seconds: f32,
    max_seconds: Option<f32>,
    max_markers: usize,
) -> CmdResult<Vec<u32>> {
    check_slot(index)?;
    let reel = Reel::open(&Card::new(root).slot_path(index)).map_err(err)?;
    let options = slice::Options {
        mode: match mode {
            SliceMode::Phrases => slice::Mode::Phrases,
            SliceMode::Gestures => slice::Mode::Gestures,
        },
        min_seconds,
        max_seconds,
        max_markers,
    };
    Ok(slice::auto_slice(&reel.left, &reel.right, options))
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
            load_reel,
            save_markers,
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
