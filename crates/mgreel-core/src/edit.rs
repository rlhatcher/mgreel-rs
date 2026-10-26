//! Editing a reel's audio splice by splice. Every edit replaces one stretch
//! of audio, and undo keeps only that stretch, before and after.

use crate::reel::{splice_ranges, SAMPLE_RATE};
use crate::slice::Biquad;
use anyhow::{bail, Result};
use std::ops::Range;

/// Undo history is trimmed from the oldest edit once it holds more samples
/// than this: 384 MB, about three edits that each rewrite a full reel.
const HISTORY_SAMPLES: usize = 96 * 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stereo {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl Stereo {
    pub fn new(left: Vec<f32>, right: Vec<f32>) -> Stereo {
        Stereo { left, right }
    }

    pub fn len(&self) -> usize {
        self.left.len()
    }

    pub fn is_empty(&self) -> bool {
        self.left.is_empty()
    }

    fn slice(&self, range: Range<usize>) -> Stereo {
        Stereo {
            left: self.left[range.clone()].to_vec(),
            right: self.right[range].to_vec(),
        }
    }

    fn extend(&mut self, other: &Stereo) {
        self.left.extend_from_slice(&other.left);
        self.right.extend_from_slice(&other.right);
    }

    fn channels(&mut self) -> [&mut Vec<f32>; 2] {
        [&mut self.left, &mut self.right]
    }

    fn peak(&self) -> f32 {
        self.left
            .iter()
            .chain(&self.right)
            .fold(0.0f32, |m, s| m.max(s.abs()))
    }

    fn rms(&self) -> f32 {
        let n = 2 * self.len();
        if n == 0 {
            return 0.0;
        }
        let sum: f64 = self
            .left
            .iter()
            .chain(&self.right)
            .map(|&s| (s as f64) * (s as f64))
            .sum();
        (sum / n as f64).sqrt() as f32
    }

    fn scale(&mut self, gain: f32) {
        for ch in self.channels() {
            ch.iter_mut().for_each(|s| *s *= gain);
        }
    }
}

/// Splices copied for pasting: their audio end to end, and each one's length.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Clipboard {
    pub audio: Stereo,
    pub lengths: Vec<usize>,
}

/// One edit: at `start`, `removed` was replaced by `inserted`.
#[derive(Debug, Clone)]
struct Change {
    start: usize,
    removed: Stereo,
    inserted: Stereo,
}

impl Change {
    fn samples(&self) -> usize {
        2 * (self.removed.len() + self.inserted.len())
    }
}

/// A replacement to make to a copy of the audio: at `start`, drop
/// `removed` frames and put `inserted` in their place.
#[derive(Debug, PartialEq)]
pub struct Span<'a> {
    pub start: usize,
    pub removed: usize,
    pub inserted: &'a Stereo,
}

/// What to do to the selected splices.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Op {
    Delete,
    /// Put a copy of the selection right after it.
    Duplicate,
    /// Move the selection to just before splice `n` (the splice count for
    /// the end of the reel).
    MoveTo(usize),
    /// Put the selected splices in a random order from this seed.
    Shuffle(u64),
    /// Play each splice backwards.
    Reverse,
    GainDb(f32),
    /// Scale each splice so its peak sits at this level in dBFS.
    NormalizeDb(f32),
    /// Bring each splice to the average loudness of the selection.
    MatchLevels,
    FadeInMs(f32),
    FadeOutMs(f32),
    /// Crossfade the end of each splice into its start, so it loops
    /// without a jump. The splice gets shorter by the fade length.
    SmoothLoopMs(f32),
    RemoveDc,
    HighPassHz(f32),
}

/// A reel's audio being edited, with its undo history.
pub struct Session {
    audio: Stereo,
    history: Vec<Change>,
    /// Edits dropped from the front of the history to save memory.
    base: usize,
    /// Edits applied, counting dropped ones: `base..=base + history.len()`.
    pos: usize,
    /// Samples the history may hold before it drops old edits.
    cap: usize,
}

impl Session {
    pub fn new(audio: Stereo) -> Session {
        Session {
            audio,
            history: Vec::new(),
            base: 0,
            pos: 0,
            cap: HISTORY_SAMPLES,
        }
    }

    pub fn audio(&self) -> &Stereo {
        &self.audio
    }

    pub fn frames(&self) -> usize {
        self.audio.len()
    }

    /// How many edits are applied. Undo and redo move this.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// The earliest position undo can still reach.
    pub fn oldest(&self) -> usize {
        self.base
    }

    /// The change made by the most recent edit, as a span to apply.
    pub fn last_span(&self) -> Option<Span<'_>> {
        let c = self.history.get(self.pos.checked_sub(self.base + 1)?)?;
        Some(Span {
            start: c.start,
            removed: c.removed.len(),
            inserted: &c.inserted,
        })
    }

    /// Splices `first..=last` as cut by `markers`, for pasting later.
    pub fn copy(&self, markers: &[u32], first: usize, last: usize) -> Result<Clipboard> {
        let bounds = self.bounds(markers);
        check_selection(&bounds, first, last)?;
        Ok(Clipboard {
            audio: self.audio.slice(bounds[first]..bounds[last + 1]),
            lengths: (first..=last).map(|i| bounds[i + 1] - bounds[i]).collect(),
        })
    }

    /// Insert the clipboard before splice `at` (the splice count for the
    /// end). Returns the new markers.
    pub fn paste(&mut self, markers: &[u32], at: usize, clip: &Clipboard) -> Result<Vec<u32>> {
        let bounds = self.bounds(markers);
        if at >= bounds.len() {
            bail!("no splice {at} to paste before");
        }
        if clip.audio.is_empty() {
            bail!("nothing to paste");
        }
        Ok(self.replace(&bounds, at, at, clip.audio.clone(), clip.lengths.clone()))
    }

    /// Apply `op` to splices `first..=last`. Returns the new markers. An
    /// edit that changes nothing leaves the history alone.
    pub fn apply(&mut self, markers: &[u32], first: usize, last: usize, op: Op) -> Result<Vec<u32>> {
        let bounds = self.bounds(markers);
        check_selection(&bounds, first, last)?;
        let count = bounds.len() - 1;
        let splice = |i: usize| self.audio.slice(bounds[i]..bounds[i + 1]);
        let selected = || (first..=last).map(splice);
        let (lo, hi, pieces): (usize, usize, Vec<Stereo>) = match op {
            Op::Delete => {
                if first == 0 && last == count - 1 {
                    bail!("a reel needs at least one splice");
                }
                (first, last + 1, Vec::new())
            }
            Op::Duplicate => (first, last + 1, selected().chain(selected()).collect()),
            Op::MoveTo(to) => {
                if to > count {
                    bail!("no splice {to} to move before");
                }
                if (first..=last + 1).contains(&to) {
                    return Ok(markers.to_vec());
                }
                if to < first {
                    (to, last + 1, selected().chain((to..first).map(splice)).collect())
                } else {
                    (first, to, (last + 1..to).map(splice).chain(selected()).collect())
                }
            }
            Op::Shuffle(seed) => {
                let mut order: Vec<usize> = (first..=last).collect();
                shuffle(&mut order, seed);
                (first, last + 1, order.into_iter().map(splice).collect())
            }
            Op::MatchLevels => {
                let pieces: Vec<Stereo> = selected().collect();
                (first, last + 1, match_levels(pieces))
            }
            Op::HighPassHz(hz) => {
                // One pass over the whole selection, so the filter state runs
                // on across splice boundaries instead of restarting at each.
                let mut whole = self.audio.slice(bounds[first]..bounds[last + 1]);
                high_pass(&mut whole, hz);
                let lengths = (first..=last).map(|i| bounds[i + 1] - bounds[i]).collect();
                return Ok(self.replace(&bounds, first, last + 1, whole, lengths));
            }
            _ => {
                let pieces = selected()
                    .map(|mut s| {
                        transform(&mut s, op);
                        s
                    })
                    .collect();
                (first, last + 1, pieces)
            }
        };
        let lengths = pieces.iter().map(Stereo::len).collect();
        let mut joined = Stereo::default();
        for p in &pieces {
            joined.extend(p);
        }
        Ok(self.replace(&bounds, lo, hi, joined, lengths))
    }

    /// Move to history position `target`, undoing or redoing edits. Returns
    /// the spans to apply, in order, to a copy of the audio.
    pub fn seek(&mut self, target: usize) -> Result<Vec<Span<'_>>> {
        if target < self.base {
            bail!("that edit is too old to undo: the history keeps about 380 MB");
        }
        if target > self.base + self.history.len() {
            bail!("nothing to redo");
        }
        let mut steps = Vec::new();
        while self.pos > target {
            self.pos -= 1;
            let c = &self.history[self.pos - self.base];
            replace(&mut self.audio, c.start, c.inserted.len(), &c.removed);
            steps.push((self.pos - self.base, true));
        }
        while self.pos < target {
            let c = &self.history[self.pos - self.base];
            replace(&mut self.audio, c.start, c.removed.len(), &c.inserted);
            steps.push((self.pos - self.base, false));
            self.pos += 1;
        }
        Ok(steps
            .into_iter()
            .map(|(i, undo)| {
                let c = &self.history[i];
                if undo {
                    Span {
                        start: c.start,
                        removed: c.inserted.len(),
                        inserted: &c.removed,
                    }
                } else {
                    Span {
                        start: c.start,
                        removed: c.removed.len(),
                        inserted: &c.inserted,
                    }
                }
            })
            .collect())
    }

    /// Splice boundaries: 0, each marker, then the end.
    fn bounds(&self, markers: &[u32]) -> Vec<usize> {
        let ranges = splice_ranges(markers, self.frames() as u32);
        let mut bounds: Vec<usize> = ranges.iter().map(|r| r.start as usize).collect();
        bounds.push(self.frames());
        bounds
    }

    /// Replace splices `lo..hi` with `audio`, cut into `lengths`. Records the
    /// change unless the audio and lengths come out the same.
    fn replace(
        &mut self,
        bounds: &[usize],
        lo: usize,
        hi: usize,
        audio: Stereo,
        lengths: Vec<usize>,
    ) -> Vec<u32> {
        let start = bounds[lo];
        let removed = self.audio.slice(start..bounds[hi]);
        let old_lengths: Vec<usize> = (lo..hi).map(|i| bounds[i + 1] - bounds[i]).collect();
        let mut all: Vec<usize> = (0..lo).map(|i| bounds[i + 1] - bounds[i]).collect();
        all.extend(lengths.iter().copied().filter(|&n| n > 0));
        all.extend((hi..bounds.len() - 1).map(|i| bounds[i + 1] - bounds[i]));
        if removed != audio || old_lengths != lengths {
            replace(&mut self.audio, start, removed.len(), &audio);
            self.history.truncate(self.pos - self.base);
            self.history.push(Change {
                start,
                removed,
                inserted: audio,
            });
            self.pos += 1;
            self.trim();
        }
        let mut markers = Vec::with_capacity(all.len());
        let mut at = 0;
        for n in &all[..all.len().saturating_sub(1)] {
            at += n;
            markers.push(at as u32);
        }
        markers
    }

    fn trim(&mut self) {
        let mut total: usize = self.history.iter().map(Change::samples).sum();
        // Always keep the latest edit, so it can be undone.
        while total > self.cap && self.history.len() > 1 {
            total -= self.history.remove(0).samples();
            self.base += 1;
        }
    }
}

fn check_selection(bounds: &[usize], first: usize, last: usize) -> Result<()> {
    let count = bounds.len() - 1;
    if first > last || last >= count {
        bail!("splices {} to {} aren't in this reel", first + 1, last + 1);
    }
    Ok(())
}

/// Replace `removed` frames at `start` with `inserted`.
fn replace(audio: &mut Stereo, start: usize, removed: usize, inserted: &Stereo) {
    let end = start + removed;
    audio
        .left
        .splice(start..end, inserted.left.iter().copied());
    audio
        .right
        .splice(start..end, inserted.right.iter().copied());
}

fn frames_for_ms(ms: f32) -> usize {
    (ms.max(0.0) * SAMPLE_RATE as f32 / 1000.0).round() as usize
}

fn db(gain_db: f32) -> f32 {
    10f32.powf(gain_db / 20.0)
}

fn transform(s: &mut Stereo, op: Op) {
    let len = s.len();
    match op {
        Op::Reverse => {
            for ch in s.channels() {
                ch.reverse();
            }
        }
        Op::GainDb(g) => s.scale(db(g)),
        Op::NormalizeDb(peak) => {
            let max = s.peak();
            if max > 0.0 {
                s.scale(db(peak) / max);
            }
        }
        Op::FadeInMs(ms) => {
            let n = frames_for_ms(ms).min(len);
            for ch in s.channels() {
                for (i, x) in ch.iter_mut().take(n).enumerate() {
                    *x *= raised_cosine((i as f32 + 0.5) / n as f32);
                }
            }
        }
        Op::FadeOutMs(ms) => {
            let n = frames_for_ms(ms).min(len);
            for ch in s.channels() {
                for (i, x) in ch.iter_mut().rev().take(n).enumerate() {
                    *x *= raised_cosine((i as f32 + 0.5) / n as f32);
                }
            }
        }
        Op::SmoothLoopMs(ms) => smooth_loop(s, frames_for_ms(ms)),
        Op::RemoveDc => {
            for ch in s.channels() {
                if ch.is_empty() {
                    continue;
                }
                let mean = (ch.iter().map(|&x| x as f64).sum::<f64>() / ch.len() as f64) as f32;
                ch.iter_mut().for_each(|x| *x -= mean);
            }
        }
        _ => {}
    }
}

/// 0 at t = 0, 1 at t = 1, with a smooth start and finish.
fn raised_cosine(t: f32) -> f32 {
    0.5 - 0.5 * (std::f32::consts::PI * t.clamp(0.0, 1.0)).cos()
}

/// Crossfade the last `n` frames into the first `n` and drop the first `n`.
/// The result ends just before where it starts, so looping it is smooth.
/// Equal-power, since splices are rarely correlated end to start.
fn smooth_loop(s: &mut Stereo, n: usize) {
    let len = s.len();
    let n = n.min(len / 3);
    if n < 2 {
        return;
    }
    for ch in s.channels() {
        let head: Vec<f32> = ch[..n].to_vec();
        for (k, &h) in head.iter().enumerate() {
            let theta = std::f32::consts::FRAC_PI_2 * (k as f32 + 0.5) / n as f32;
            let i = len - n + k;
            ch[i] = ch[i] * theta.cos() + h * theta.sin();
        }
        ch.drain(..n);
    }
}

/// Scale each piece to the average RMS in dB of the non-silent ones, then
/// pull down any piece that would clip.
fn match_levels(mut pieces: Vec<Stereo>) -> Vec<Stereo> {
    let levels: Vec<f32> = pieces.iter().map(Stereo::rms).collect();
    let heard: Vec<f32> = levels
        .iter()
        .filter(|&&r| r > 1e-6)
        .map(|r| 20.0 * r.log10())
        .collect();
    if heard.is_empty() {
        return pieces;
    }
    let target = db(heard.iter().sum::<f32>() / heard.len() as f32);
    for (p, &rms) in pieces.iter_mut().zip(&levels) {
        if rms <= 1e-6 {
            continue;
        }
        let peak = p.peak();
        let gain = (target / rms).min(0.99 / peak);
        p.scale(gain);
    }
    pieces
}

fn high_pass(s: &mut Stereo, hz: f32) {
    let hz = hz.clamp(5.0, 2000.0);
    for ch in s.channels() {
        let mut f = Biquad::high_pass(hz, SAMPLE_RATE as f32);
        ch.iter_mut().for_each(|x| *x = f.process(*x));
    }
}

/// Fisher-Yates with a xorshift generator, so a seed always gives the same
/// order. Never leaves two or more items in their starting order.
fn shuffle(items: &mut [usize], seed: u64) {
    let original = items.to_vec();
    let mut state = seed | 1;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..8 {
        for i in (1..items.len()).rev() {
            let j = (next() % (i as u64 + 1)) as usize;
            items.swap(i, j);
        }
        if items != original.as_slice() {
            return;
        }
    }
    items.rotate_left(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Splices of 100, 200 and 300 frames, each filled with its number.
    fn three() -> (Session, Vec<u32>) {
        let mut left = Vec::new();
        for (n, len) in [(1.0, 100), (2.0, 200), (3.0, 300)] {
            left.extend(std::iter::repeat_n(n, len));
        }
        let right = left.iter().map(|x| -x).collect();
        (Session::new(Stereo::new(left, right)), vec![100, 300])
    }

    /// The value each splice holds, in order.
    fn layout(s: &Session, markers: &[u32]) -> Vec<(f32, usize)> {
        let ranges = splice_ranges(markers, s.frames() as u32);
        ranges
            .iter()
            .map(|r| (s.audio().left[r.start as usize], (r.end - r.start) as usize))
            .collect()
    }

    #[test]
    fn delete_duplicate_and_move_keep_markers_with_their_audio() {
        let (mut s, m) = three();
        let m2 = s.apply(&m, 1, 1, Op::Delete).unwrap();
        assert_eq!(layout(&s, &m2), vec![(1.0, 100), (3.0, 300)]);

        let (mut s, m) = three();
        let m2 = s.apply(&m, 0, 1, Op::Duplicate).unwrap();
        assert_eq!(
            layout(&s, &m2),
            vec![(1.0, 100), (2.0, 200), (1.0, 100), (2.0, 200), (3.0, 300)]
        );

        let (mut s, m) = three();
        let m2 = s.apply(&m, 2, 2, Op::MoveTo(0)).unwrap();
        assert_eq!(layout(&s, &m2), vec![(3.0, 300), (1.0, 100), (2.0, 200)]);

        let (mut s, m) = three();
        let m2 = s.apply(&m, 0, 0, Op::MoveTo(3)).unwrap();
        assert_eq!(layout(&s, &m2), vec![(2.0, 200), (3.0, 300), (1.0, 100)]);
        assert_eq!(s.audio().right[0], -2.0);
    }

    #[test]
    fn moving_onto_itself_or_deleting_everything_changes_nothing() {
        let (mut s, m) = three();
        assert_eq!(s.apply(&m, 1, 1, Op::MoveTo(2)).unwrap(), m);
        assert_eq!(s.position(), 0);
        assert!(s.apply(&m, 0, 2, Op::Delete).is_err());
        assert!(s.apply(&m, 2, 3, Op::Reverse).is_err());
        assert_eq!(s.apply(&m, 0, 2, Op::GainDb(0.0)).unwrap(), m);
        assert_eq!(s.position(), 0);
    }

    #[test]
    fn copy_and_paste() {
        let (mut s, m) = three();
        let clip = s.copy(&m, 0, 0).unwrap();
        let m2 = s.paste(&m, 3, &clip).unwrap();
        assert_eq!(
            layout(&s, &m2),
            vec![(1.0, 100), (2.0, 200), (3.0, 300), (1.0, 100)]
        );
    }

    #[test]
    fn undo_and_redo_restore_audio_exactly() {
        let (mut s, m) = three();
        let original = s.audio().clone();
        let m1 = s.apply(&m, 1, 1, Op::Delete).unwrap();
        let after_delete = s.audio().clone();
        s.apply(&m1, 0, 1, Op::GainDb(-6.0)).unwrap();
        let after_gain = s.audio().clone();
        assert_eq!(s.position(), 2);

        // Apply the returned spans to a copy, as the app does.
        let mut copy = after_gain.clone();
        for span in s.seek(0).unwrap() {
            replace(&mut copy, span.start, span.removed, span.inserted);
        }
        assert_eq!(s.audio(), &original);
        assert_eq!(copy, original);

        for span in s.seek(2).unwrap() {
            replace(&mut copy, span.start, span.removed, span.inserted);
        }
        assert_eq!(copy, after_gain);
        s.seek(1).unwrap();
        assert_eq!(s.audio(), &after_delete);

        // A new edit after undo drops the redo.
        s.apply(&m1, 0, 0, Op::GainDb(-3.0)).unwrap();
        assert_eq!(s.position(), 2);
        assert!(s.seek(3).is_err());
    }

    #[test]
    fn smooth_loop_joins_end_to_start() {
        // A ramp jumps from 0.999 back to 0 when it loops.
        let ramp: Vec<f32> = (0..1000).map(|i| i as f32 / 1000.0).collect();
        let mut s = Stereo::new(ramp.clone(), ramp);
        smooth_loop(&mut s, 100);
        assert_eq!(s.len(), 900);
        let jump = (s.left[s.len() - 1] - s.left[0]).abs();
        assert!(jump < 0.01, "jump {jump}");
    }

    #[test]
    fn match_levels_evens_out_loudness() {
        let loud = Stereo::new([0.5, -0.5].repeat(50), [0.5, -0.5].repeat(50));
        let quiet = Stereo::new([0.05, -0.05].repeat(50), [0.05, -0.05].repeat(50));
        let out = match_levels(vec![loud, quiet]);
        let (a, b) = (out[0].rms(), out[1].rms());
        assert!((a - b).abs() < 1e-3, "{a} {b}");
    }

    #[test]
    fn fades_and_normalize() {
        let mut s = Stereo::new(vec![1.0; 480], vec![1.0; 480]);
        transform(&mut s, Op::FadeInMs(5.0));
        assert!(s.left[0] < 0.01 && s.left[239] > 0.99 && s.left[240] == 1.0);
        transform(&mut s, Op::FadeOutMs(5.0));
        assert!(s.left[479] < 0.01);
        transform(&mut s, Op::NormalizeDb(-6.0));
        assert!((s.peak() - db(-6.0)).abs() < 1e-4);
    }

    #[test]
    fn shuffle_always_changes_the_order() {
        for seed in 0..50 {
            let mut items = vec![0, 1];
            shuffle(&mut items, seed);
            assert_eq!(items, vec![1, 0]);
            let mut items: Vec<usize> = (0..6).collect();
            shuffle(&mut items, seed);
            assert_ne!(items, (0..6).collect::<Vec<_>>());
        }
    }

    #[test]
    fn history_drops_the_oldest_edit_past_the_cap() {
        let frames = 1000;
        let mut s = Session::new(Stereo::new(vec![0.5; frames], vec![0.5; frames]));
        // Each gain change stores 4000 samples; allow one and a half.
        s.cap = 6000;
        s.apply(&[], 0, 0, Op::GainDb(-1.0)).unwrap();
        s.apply(&[], 0, 0, Op::GainDb(-1.0)).unwrap();
        assert_eq!(s.position(), 2);
        assert_eq!(s.oldest(), 1);
        assert!(s.seek(0).is_err());
        assert!(s.seek(1).is_ok());
    }
}
