//! A name and a note for each reel on a card, kept in `mgreel.json` at the
//! card root. The module loads only the reel files and `options.txt`, so the
//! names stay out of the WAV files it rewrites.

use crate::card::{slot_file_name, slot_index, SLOT_COUNT};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const NAMES_FILE: &str = "mgreel.json";
const VERSION: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReelLabel {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

impl ReelLabel {
    pub fn is_empty(&self) -> bool {
        self.name.is_empty() && self.note.is_empty()
    }
}

#[derive(Serialize, Deserialize)]
struct NamesFile {
    version: u32,
    /// Keyed by slot file name, such as `mg1.wav`.
    reels: BTreeMap<String, ReelLabel>,
}

/// The labels for all 32 slots, by slot index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Names(Vec<ReelLabel>);

impl Default for Names {
    fn default() -> Self {
        Names(vec![ReelLabel::default(); SLOT_COUNT])
    }
}

impl Names {
    pub fn parse(text: &str) -> Result<Names> {
        let file: NamesFile = serde_json::from_str(text).context("reading the reel names")?;
        if file.version > VERSION {
            bail!(
                "the reel names were saved by a newer version of the app (format {})",
                file.version
            );
        }
        let mut names = Names::default();
        for (key, label) in file.reels {
            // Keys that aren't slots can't belong to a reel, so they go.
            if let Some(i) = slot_index(&key) {
                names.0[i] = label;
            }
        }
        Ok(names)
    }

    pub fn to_json(&self) -> String {
        let reels = (0..SLOT_COUNT)
            .filter(|&i| !self.0[i].is_empty())
            .map(|i| (slot_file_name(i), self.0[i].clone()))
            .collect();
        let file = NamesFile {
            version: VERSION,
            reels,
        };
        let mut text = serde_json::to_string_pretty(&file).expect("names serialize");
        text.push('\n');
        text
    }

    pub fn get(&self, index: usize) -> &ReelLabel {
        &self.0[index]
    }

    /// Set a slot's label, trimmed.
    pub fn set(&mut self, index: usize, label: ReelLabel) {
        self.0[index] = ReelLabel {
            name: label.name.trim().to_string(),
            note: label.note.trim().to_string(),
        };
    }

    pub fn clear(&mut self, index: usize) {
        self.0[index] = ReelLabel::default();
    }

    pub fn is_empty(&self) -> bool {
        self.0.iter().all(ReelLabel::is_empty)
    }

    /// Follow a [`crate::card::Card::reorder`]: slot `i` gets the label of
    /// slot `order[i]`.
    pub fn reorder(&self, order: &[usize]) -> Names {
        Names(order.iter().map(|&src| self.0[src].clone()).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(name: &str) -> ReelLabel {
        ReelLabel {
            name: name.into(),
            note: String::new(),
        }
    }

    #[test]
    fn round_trips_and_skips_empty_slots() {
        let mut names = Names::default();
        names.set(0, label("  Harbour  "));
        names.set(
            9,
            ReelLabel {
                name: String::new(),
                note: "gulls".into(),
            },
        );
        let json = names.to_json();
        assert!(json.contains("\"mg1.wav\""));
        assert!(json.contains("\"mga.wav\""));
        assert!(!json.contains("mg2.wav"));
        let back = Names::parse(&json).unwrap();
        assert_eq!(back, names);
        assert_eq!(back.get(0).name, "Harbour");
    }

    #[test]
    fn reads_any_case_and_drops_unknown_keys() {
        let names = Names::parse(
            r#"{"version":1,"reels":{"MG2.WAV":{"name":"a"},"notes.txt":{"name":"b"}}}"#,
        )
        .unwrap();
        assert_eq!(names.get(1).name, "a");
        assert_eq!(names.to_json().matches("name").count(), 1);
    }

    #[test]
    fn refuses_newer_and_broken_files() {
        assert!(Names::parse(r#"{"version":2,"reels":{}}"#).is_err());
        assert!(Names::parse("{").is_err());
    }
}
