//! `options.txt`, the firmware settings file at the card root.
//!
//! Each setting is a line like `omod 0 //Organize option: ...`. The manual
//! says to change only the number, so editing here swaps the value token and
//! leaves every other byte of the file alone.

use anyhow::{bail, Result};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    /// One of the listed integer choices.
    Choice(&'static [(i64, &'static str)]),
    /// Morph chord ratio: magnitude 0.0625 to 16, negative plays reversed.
    Ratio,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spec {
    pub key: &'static str,
    pub name: &'static str,
    pub kind: Kind,
    pub since_firmware: u32,
}

/// Options documented in the manual (firmware MG155 and MG203).
pub const SPECS: &[Spec] = &[
    Spec {
        key: "vsop",
        name: "Vari-Speed",
        kind: Kind::Choice(&[
            (0, "bidirectional classic"),
            (1, "bidirectional 1V/oct"),
            (2, "positive only, 1V/oct"),
        ]),
        since_firmware: 155,
    },
    Spec {
        key: "inop",
        name: "Input",
        kind: Kind::Choice(&[(0, "record SOS mix"), (1, "record input only")]),
        since_firmware: 155,
    },
    Spec {
        key: "pmin",
        name: "Phase/position modulation",
        kind: Kind::Choice(&[
            (0, "off"),
            (1, "right input modulates phase when left is unpatched"),
        ]),
        since_firmware: 155,
    },
    Spec {
        key: "omod",
        name: "Organize",
        kind: Kind::Choice(&[(0, "at end of gene"), (1, "immediately")]),
        since_firmware: 155,
    },
    Spec {
        key: "gnsm",
        name: "Gene smooth",
        kind: Kind::Choice(&[(0, "classic"), (1, "smooth gene window")]),
        since_firmware: 155,
    },
    Spec {
        key: "rsop",
        name: "Record",
        kind: Kind::Choice(&[
            (0, "REC+SPLICE records a new splice"),
            (1, "REC records a new splice"),
        ]),
        since_firmware: 155,
    },
    Spec {
        key: "pmod",
        name: "Play",
        kind: Kind::Choice(&[(0, "classic"), (1, "momentary"), (2, "trigger loop")]),
        since_firmware: 155,
    },
    Spec {
        key: "ckop",
        name: "Clock",
        kind: Kind::Choice(&[
            (0, "gene shift or time stretch by Morph"),
            (1, "gene shift only"),
            (2, "time stretch only"),
        ]),
        since_firmware: 155,
    },
    Spec {
        key: "cvop",
        name: "CV out",
        kind: Kind::Choice(&[(0, "envelope follower"), (1, "ramp at gene size")]),
        since_firmware: 155,
    },
    Spec {
        key: "mcr1",
        name: "Morph chord ratio, gene 2",
        kind: Kind::Ratio,
        since_firmware: 203,
    },
    Spec {
        key: "mcr2",
        name: "Morph chord ratio, gene 3",
        kind: Kind::Ratio,
        since_firmware: 203,
    },
    Spec {
        key: "mcr3",
        name: "Morph chord ratio, gene 4",
        kind: Kind::Ratio,
        since_firmware: 203,
    },
];

pub fn spec(key: &str) -> Option<&'static Spec> {
    SPECS.iter().find(|s| s.key == key)
}

/// Check a value against its spec and return it as the firmware writes it.
/// Keys without a spec (newer firmware) accept any single token.
pub fn validate(key: &str, value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() || value.contains(char::is_whitespace) {
        bail!("{key}: value must be a single token");
    }
    let Some(spec) = spec(key) else {
        return Ok(value.to_string());
    };
    match spec.kind {
        Kind::Choice(choices) => {
            let n: i64 = value
                .parse()
                .map_err(|_| anyhow::anyhow!("{key}: {value:?} is not a whole number"))?;
            if !choices.iter().any(|(c, _)| *c == n) {
                let allowed: Vec<String> = choices.iter().map(|(c, _)| c.to_string()).collect();
                bail!("{key}: {n} is not one of {}", allowed.join(", "));
            }
            Ok(n.to_string())
        }
        Kind::Ratio => {
            let r: f64 = value
                .parse()
                .map_err(|_| anyhow::anyhow!("{key}: {value:?} is not a number"))?;
            if !(0.0625..=16.0).contains(&r.abs()) {
                bail!("{key}: ratio magnitude must be between 0.0625 and 16");
            }
            Ok(format!("{r:.5}"))
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Line {
    Raw(String),
    Setting {
        key: String,
        /// Everything between the key and the value, usually one space.
        gap: String,
        value: String,
        /// The rest of the line, comment included.
        rest: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct OptionsFile {
    lines: Vec<Line>,
    newline: &'static str,
    trailing_newline: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub key: String,
    pub value: String,
    pub comment: String,
    pub spec: Option<&'static Spec>,
}

impl OptionsFile {
    pub fn parse(text: &str) -> OptionsFile {
        let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
        let trailing_newline = text.ends_with('\n');
        let body = text.strip_suffix(newline).unwrap_or(text);
        let lines = if text.is_empty() {
            Vec::new()
        } else {
            body.split(newline).map(parse_line).collect()
        };
        OptionsFile {
            lines,
            newline,
            trailing_newline,
        }
    }

    pub fn entries(&self) -> Vec<Entry> {
        self.lines
            .iter()
            .filter_map(|line| match line {
                Line::Setting {
                    key, value, rest, ..
                } => Some(Entry {
                    key: key.clone(),
                    value: value.clone(),
                    comment: rest.trim().trim_start_matches('/').trim().to_string(),
                    spec: spec(key),
                }),
                Line::Raw(_) => None,
            })
            .collect()
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.lines.iter().find_map(|line| match line {
            Line::Setting { key: k, value, .. } if k == key => Some(value.as_str()),
            _ => None,
        })
    }

    /// Change one setting's value. Only settings already in the file can be
    /// changed, because the Morphagene writes the lines and their comments.
    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        let value = validate(key, value)?;
        for line in &mut self.lines {
            if let Line::Setting {
                key: k, value: v, ..
            } = line
            {
                if k == key {
                    *v = value;
                    return Ok(());
                }
            }
        }
        bail!("{key} is not in this options.txt; load the card in a Morphagene with newer firmware to add it")
    }

    /// The firmware version from the `// firmware version 155` comment.
    pub fn firmware(&self) -> Option<u32> {
        self.lines.iter().find_map(|line| match line {
            Line::Raw(text) => text
                .trim_start_matches('/')
                .trim()
                .strip_prefix("firmware version")?
                .trim()
                .parse()
                .ok(),
            _ => None,
        })
    }

    pub fn to_text(&self) -> String {
        let mut out = self
            .lines
            .iter()
            .map(|line| match line {
                Line::Raw(text) => text.clone(),
                Line::Setting {
                    key,
                    gap,
                    value,
                    rest,
                } => format!("{key}{gap}{value}{rest}"),
            })
            .collect::<Vec<_>>()
            .join(self.newline);
        if self.trailing_newline {
            out.push_str(self.newline);
        }
        out
    }
}

/// A setting is a four-character lowercase key starting with a letter, then
/// whitespace, then a value token.
fn parse_line(text: &str) -> Line {
    let raw = || Line::Raw(text.to_string());
    let bytes = text.as_bytes();
    if bytes.len() < 6
        || !bytes[0].is_ascii_lowercase()
        || !bytes[1..4]
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        || !bytes[4].is_ascii_whitespace()
    {
        return raw();
    }
    let after_key = &text[4..];
    let value_start = after_key.len() - after_key.trim_start().len();
    let value_part = &after_key[value_start..];
    let value_len = value_part
        .find(|c: char| c.is_whitespace() || c == '/')
        .unwrap_or(value_part.len());
    if value_len == 0 {
        return raw();
    }
    Line::Setting {
        key: text[..4].to_string(),
        gap: after_key[..value_start].to_string(),
        value: value_part[..value_len].to_string(),
        rest: value_part[value_len..].to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // From the manual, with an MG203 ratio line added.
    const SAMPLE: &str = "6 46345 0\r\n//\r\n// firmware version 155\r\n//\r\n// 0 option is default\r\nvsop 0 ///Varispeed option: 0 bidirectional classic, 1 bidirectional 1 v/oct, 2 positive only - 1 v/oct.\r\ninop 0 //Input option: 0 record SOS mix, 1 record input only\r\nomod 0 //Organize option: 0 organize at end of gene, 1 organize immediately\r\nmcr1 2.00000 //Morph chord ratio 1\r\n";

    #[test]
    fn round_trips_byte_for_byte() {
        assert_eq!(OptionsFile::parse(SAMPLE).to_text(), SAMPLE);
        let lf = SAMPLE.replace("\r\n", "\n");
        assert_eq!(OptionsFile::parse(&lf).to_text(), lf);
        let no_trailing = lf.trim_end();
        assert_eq!(OptionsFile::parse(no_trailing).to_text(), no_trailing);
    }

    #[test]
    fn reads_entries_and_firmware() {
        let file = OptionsFile::parse(SAMPLE);
        let keys: Vec<_> = file.entries().into_iter().map(|e| e.key).collect();
        assert_eq!(keys, vec!["vsop", "inop", "omod", "mcr1"]);
        assert_eq!(file.get("omod"), Some("0"));
        assert_eq!(file.firmware(), Some(155));
        assert!(file.entries()[2].comment.starts_with("Organize option"));
    }

    #[test]
    fn set_changes_only_the_value() {
        let mut file = OptionsFile::parse(SAMPLE);
        file.set("omod", "1").unwrap();
        file.set("mcr1", "-1.5").unwrap();
        let expected = SAMPLE
            .replace("omod 0 //", "omod 1 //")
            .replace("mcr1 2.00000", "mcr1 -1.50000");
        assert_eq!(file.to_text(), expected);
    }

    #[test]
    fn set_rejects_bad_values() {
        let mut file = OptionsFile::parse(SAMPLE);
        assert!(file.set("omod", "2").is_err());
        assert!(file.set("vsop", "x").is_err());
        assert!(file.set("mcr1", "20").is_err());
        assert!(file.set("mcr1", "0.01").is_err());
        assert!(file.set("gnsm", "1").is_err(), "not in the file");
        assert_eq!(file.to_text(), SAMPLE);
    }
}
