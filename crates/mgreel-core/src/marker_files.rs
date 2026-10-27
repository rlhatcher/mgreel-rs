//! Markers made in other software, read so they can go onto a reel:
//! Audacity label files, REAPER marker and region exports, Ableton Live
//! sets, and the cue points and region ends in a WAV file.

use crate::reel::{clean_markers, ReelFile, SAMPLE_RATE};
use anyhow::{bail, Context, Result};
use flate2::read::GzDecoder;
use quick_xml::events::{BytesStart, Event};
use std::collections::{HashMap, HashSet};
use std::io::{BufReader, Read};
use std::path::Path;

/// Marker times read from a file, in seconds from its start.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MarkerTimes {
    pub seconds: Vec<f64>,
    /// Things that make the times less certain, to tell the user.
    pub warnings: Vec<String>,
}

/// The kinds of file [`read`] takes, for a file picker.
pub const EXTENSIONS: [&str; 5] = ["txt", "csv", "als", "wav", "wave"];

/// Read the markers in a file, choosing the reader by its extension.
pub fn read(path: &Path) -> Result<MarkerTimes> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let text =
        || std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()));
    let times = match ext.as_str() {
        "txt" => MarkerTimes {
            seconds: audacity(&text()?)?,
            warnings: Vec::new(),
        },
        "csv" => MarkerTimes {
            seconds: reaper(&text()?)?,
            warnings: Vec::new(),
        },
        "als" => {
            let file =
                std::fs::File::open(path).with_context(|| format!("reading {}", path.display()))?;
            ableton(file)?
        }
        "wav" | "wave" => MarkerTimes {
            seconds: wav(&ReelFile::open(path)?)?,
            warnings: Vec::new(),
        },
        _ => bail!(
            "markers can come from an Audacity label file (.txt), a REAPER marker export \
             (.csv), an Ableton Live set (.als) or a WAV file"
        ),
    };
    if times.seconds.is_empty() {
        bail!("{} has no markers", path.display());
    }
    Ok(times)
}

/// Times as markers on a reel `frames` long, where the reel's audio starts
/// `offset` seconds into the file's timeline. Returns the markers and how
/// many times fell before the audio or after its end.
pub fn to_markers(seconds: &[f64], offset: f64, frames: u32) -> (Vec<u32>, usize) {
    let mut outside = 0;
    let mut markers = Vec::new();
    for &t in seconds {
        let frame = ((t - offset) * SAMPLE_RATE as f64).round();
        if frame < 0.0 || frame >= frames as f64 {
            outside += 1;
        } else {
            // 0 is the start of the reel, a splice start already.
            markers.push(frame as u32);
        }
    }
    (clean_markers(markers, frames), outside)
}

/// A number in seconds. Some locales write a comma for the decimal point.
fn seconds(field: &str) -> Option<f64> {
    let t: f64 = field.trim().replace(',', ".").parse().ok()?;
    (t.is_finite() && t >= 0.0).then_some(t)
}

/// An Audacity label file: one label per line, start and end in seconds
/// and the label's text, split by tabs. A region's end is a marker too, so
/// the region becomes a splice of its own.
pub fn audacity(text: &str) -> Result<Vec<f64>> {
    let mut times = Vec::new();
    for (n, line) in text.lines().enumerate() {
        // Lines starting with a backslash hold a label's frequency range.
        if line.trim().is_empty() || line.starts_with('\\') {
            continue;
        }
        let mut fields = line.split('\t');
        let start = fields.next().and_then(seconds);
        let end = fields.next().and_then(seconds);
        let (Some(start), Some(end)) = (start, end) else {
            bail!("line {} isn't an Audacity label: {line}", n + 1);
        };
        times.push(start);
        if end > start {
            times.push(end);
        }
    }
    Ok(times)
}

/// Split one CSV line into fields, honouring quotes.
fn csv_fields(line: &str) -> Vec<String> {
    let mut fields = vec![String::new()];
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                fields.last_mut().unwrap().push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => fields.push(String::new()),
            _ => fields.last_mut().unwrap().push(c),
        }
    }
    fields
}

/// A REAPER time as written in a marker export: seconds, m:ss.sss or
/// h:mm:ss.sss. Measures and beats can't become seconds without the tempo,
/// so they are an error, and so are timecode frames.
fn reaper_time(field: &str) -> Result<Option<f64>> {
    let field = field.trim();
    if field.is_empty() {
        return Ok(None);
    }
    let fix = "In REAPER's Region/Marker Manager, show time as Minutes:Seconds or Seconds, \
               then export again.";
    let parts: Vec<&str> = field.split(':').collect();
    if parts.len() > 3 {
        bail!("{field} is timecode with frames. {fix}");
    }
    if field.matches('.').count() > 1 {
        bail!("{field} is in measures and beats. {fix}");
    }
    let mut total = 0.0;
    for (i, part) in parts.iter().enumerate() {
        let value = if i + 1 == parts.len() {
            seconds(part)
        } else {
            part.trim().parse::<u32>().ok().map(f64::from)
        };
        let Some(value) = value else {
            bail!("{field} isn't a time REAPER writes. {fix}");
        };
        total = total * 60.0 + value;
    }
    Ok(Some(total))
}

/// A REAPER marker and region export from the Region/Marker Manager, with
/// `Start` and `End` columns. A region's end is a marker too.
pub fn reaper(text: &str) -> Result<Vec<f64>> {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let header = csv_fields(lines.next().context("the file is empty")?);
    let column = |name: &str| {
        header
            .iter()
            .position(|h| h.trim().eq_ignore_ascii_case(name))
    };
    let (Some(start), end) = (column("Start"), column("End")) else {
        bail!("the file has no Start column, so it isn't a REAPER marker export");
    };
    let mut times = Vec::new();
    for line in lines {
        let fields = csv_fields(line);
        let field = |i: Option<usize>| i.and_then(|i| fields.get(i)).map_or("", String::as_str);
        let Some(at) = reaper_time(field(Some(start)))? else {
            continue;
        };
        times.push(at);
        if let Some(until) = reaper_time(field(end))? {
            if until > at {
                times.push(until);
            }
        }
    }
    Ok(times)
}

/// The value of an element's `Value` attribute, as a number.
fn value_of(e: &BytesStart) -> Option<f64> {
    let attr = e.try_get_attribute("Value").ok()??;
    attr.value.trim().parse().ok()
}

fn id_of(e: &BytesStart) -> Option<String> {
    let attr = e.try_get_attribute("Id").ok()??;
    Some(attr.value.into_owned())
}

/// The locators in an Ableton Live set, at the set's tempo. Live writes
/// locators in beats, so a set whose tempo changes gets a warning: the
/// times are right only up to the first change.
pub fn ableton(file: impl Read) -> Result<MarkerTimes> {
    let mut xml = quick_xml::Reader::from_reader(BufReader::new(GzDecoder::new(file)));
    let mut buf = Vec::new();
    // Element names from the root to the current element.
    let mut path: Vec<String> = Vec::new();
    let mut beats = Vec::new();
    let mut tempo = None;
    let mut tempo_target = None;
    // Automation envelopes: the parameter each one moves, and its values.
    let mut envelope: Option<(Option<String>, HashSet<u64>)> = None;
    let mut envelopes: HashMap<String, usize> = HashMap::new();
    loop {
        let event = xml
            .read_event_into(&mut buf)
            .context("reading the Live set; is it an .als file?")?;
        let (e, empty) = match &event {
            Event::Start(e) => (e, false),
            Event::Empty(e) => (e, true),
            Event::End(_) => {
                if path.pop().as_deref() == Some("AutomationEnvelope") {
                    if let Some((Some(target), values)) = envelope.take() {
                        envelopes.insert(target, values.len());
                    }
                }
                buf.clear();
                continue;
            }
            Event::Eof => break,
            _ => {
                buf.clear();
                continue;
            }
        };
        let name = e.name().into_inner().to_string();
        let parent = path.last().map(String::as_str);
        // Live 12 calls the master track the main track.
        let in_master = path.iter().any(|p| p == "MasterTrack" || p == "MainTrack");
        match (parent, name.as_str()) {
            (Some("Locator"), "Time") => beats.extend(value_of(e)),
            (Some("Tempo"), "Manual") if in_master => tempo = value_of(e),
            (Some("Tempo"), "AutomationTarget") if in_master => tempo_target = id_of(e),
            (_, "AutomationEnvelope") => envelope = Some((None, HashSet::new())),
            (Some("EnvelopeTarget"), "PointeeId") => {
                if let Some((target, _)) = &mut envelope {
                    *target = value_of(e).map(|v| v.to_string());
                }
            }
            (_, "FloatEvent") => {
                if let (Some((_, values)), Some(v)) = (&mut envelope, value_of(e)) {
                    values.insert(v.to_bits());
                }
            }
            _ => {}
        }
        if !empty {
            path.push(name);
        }
        buf.clear();
    }
    let Some(bpm) = tempo.filter(|t| *t > 0.0) else {
        bail!("couldn't find the Live set's tempo");
    };
    let mut warnings = Vec::new();
    let tempo_moves = tempo_target
        .and_then(|id| id.parse::<f64>().ok())
        .and_then(|id| envelopes.get(&id.to_string()))
        .is_some_and(|&values| values > 1);
    if tempo_moves {
        warnings.push(format!(
            "The set's tempo changes. These markers take it as {bpm} BPM throughout, so the \
             ones after the first change are off."
        ));
    }
    Ok(MarkerTimes {
        seconds: beats.iter().map(|b| b * 60.0 / bpm).collect(),
        warnings,
    })
}

/// A WAV file's cue points and region ends, in seconds.
pub fn wav(file: &ReelFile) -> Result<Vec<f64>> {
    let rate = file.format()?.sample_rate;
    if rate == 0 {
        bail!("the file's sample rate is 0");
    }
    Ok(file
        .source_markers()
        .into_iter()
        .map(|m| m as f64 / rate as f64)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use std::io::Write;

    #[test]
    fn audacity_labels_and_regions() {
        let text = "0.500000\t0.500000\tclap\n\
                    1.250000\t2.000000\tgulls\n\
                    \\\t100.0\t2000.0\n\
                    \n\
                    3,5\t3,5\tcomma locale\n";
        assert_eq!(audacity(text).unwrap(), vec![0.5, 1.25, 2.0, 3.5]);
        assert!(audacity("not a label\n").is_err());
    }

    #[test]
    fn reaper_exports() {
        let text = "#,Name,Start,End,Length\n\
                    M1,\"Intro, quiet\",0:01.500,,\n\
                    R1,Verse,1:02.000,1:04.250,0:02.250\n\
                    M2,Late,1:00:00.000,,\n\
                    M3,Seconds,7.25,,\n";
        assert_eq!(reaper(text).unwrap(), vec![1.5, 62.0, 64.25, 3600.0, 7.25]);
        let beats = "#,Name,Start,End,Length\nM1,One,1.1.00,,\n";
        let e = reaper(beats).unwrap_err().to_string();
        assert!(e.contains("measures and beats"), "{e}");
        assert!(reaper("#,Name\nM1,One\n").is_err());
    }

    fn gzip(xml: &str) -> Vec<u8> {
        let mut gz = GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(xml.as_bytes()).unwrap();
        gz.finish().unwrap()
    }

    fn live_set(main: &str, tempo_events: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<Ableton MajorVersion="5"><LiveSet>
  <Tracks><AudioTrack Id="1"><DeviceChain><Mixer>
    <Tempo><Manual Value="90" /><AutomationTarget Id="3" /></Tempo>
  </Mixer></DeviceChain></AudioTrack></Tracks>
  <{main}>
    <AutomationEnvelopes><Envelopes>
      <AutomationEnvelope Id="0">
        <EnvelopeTarget><PointeeId Value="8" /></EnvelopeTarget>
        <Automation><Events>{tempo_events}</Events></Automation>
      </AutomationEnvelope>
    </Envelopes></AutomationEnvelopes>
    <DeviceChain><Mixer>
      <Tempo><LomId Value="0" /><Manual Value="120" /><AutomationTarget Id="8"><LockEnvelope Value="0" /></AutomationTarget></Tempo>
    </Mixer></DeviceChain>
  </{main}>
  <Locators><Locators>
    <Locator Id="0"><LomId Value="0" /><Time Value="4" /><Name Value="Verse" /></Locator>
    <Locator Id="1"><LomId Value="0" /><Time Value="10" /><Name Value="Break" /></Locator>
  </Locators></Locators>
</LiveSet></Ableton>"#
        )
    }

    #[test]
    fn ableton_locators_at_the_master_tempo() {
        let steady = r#"<FloatEvent Id="1" Time="-63072000" Value="120" />"#;
        let set = gzip(&live_set("MasterTrack", steady));
        let times = ableton(&set[..]).unwrap();
        // 4 beats at 120 BPM is 2 s; a track's own Tempo isn't the set's.
        assert_eq!(times.seconds, vec![2.0, 5.0]);
        assert!(times.warnings.is_empty());

        let moving = r#"<FloatEvent Id="1" Time="-63072000" Value="120" /><FloatEvent Id="2" Time="16" Value="140" />"#;
        let set = gzip(&live_set("MainTrack", moving));
        let times = ableton(&set[..]).unwrap();
        assert_eq!(times.seconds, vec![2.0, 5.0]);
        assert_eq!(times.warnings.len(), 1);

        assert!(ableton(&b"not gzip"[..]).is_err());
    }

    #[test]
    fn to_markers_shifts_rounds_and_counts_what_falls_outside() {
        let (markers, outside) = to_markers(&[0.5, 1.0, 1.0, 2.0, 0.25], 0.5, 48_000);
        // 0.5 lands on the start; 0.25 comes before it and 2.0 after the end.
        assert_eq!(markers, vec![24_000]);
        assert_eq!(outside, 2);
    }
}
