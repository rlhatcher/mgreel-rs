//! Automatic splice markers for any source: field recordings, found sound,
//! voices, machines, music. Nothing here assumes pitch, tempo or beats.
//!
//! Markers go in the quiet moments between sounds rather than on attacks,
//! so a splice never starts or ends in the middle of a sound event.
//!
//! The audio is reduced to four band envelopes per 10 ms frame: below
//! 250 Hz, 250 Hz-2 kHz, 2-6 kHz and above 6 kHz.
//!
//! - Phrases: a marker wherever the sound changes character, measured as the
//!   difference in band balance and activity between the few seconds before
//!   and after each point.
//! - Gestures: a marker before each event that rises out of the running
//!   level of any band: a thump, a scrape, a voice, a hiss.
//!
//! Either way, a longest-splice setting then cuts any splice still too long
//! at its quietest point.

use crate::reel::{clean_markers, MAX_MARKERS, SAMPLE_RATE};

/// 10 ms at 48 kHz.
const HOP: usize = 480;
const FRAMES_PER_SECOND: f32 = SAMPLE_RATE as f32 / HOP as f32;
/// Treat anything quieter as silence.
const FLOOR_DB: f32 = -80.0;
/// Band edges in Hz.
const EDGES: [f32; 3] = [250.0, 2000.0, 6000.0];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Phrases,
    Gestures,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    pub mode: Mode,
    /// Shortest splice to create, in seconds.
    pub min_seconds: f32,
    /// Longest splice to leave, in seconds. Raised to twice `min_seconds`
    /// if lower, since a splice can't be cut into two shorter than that.
    pub max_seconds: Option<f32>,
    /// Most markers to place. Capped at the Morphagene's 299.
    pub max_markers: usize,
}

impl Options {
    pub fn phrases() -> Options {
        Options {
            mode: Mode::Phrases,
            min_seconds: 4.0,
            max_seconds: Some(15.0),
            max_markers: MAX_MARKERS,
        }
    }

    pub fn gestures() -> Options {
        Options {
            mode: Mode::Gestures,
            min_seconds: 0.5,
            max_seconds: Some(5.0),
            max_markers: MAX_MARKERS,
        }
    }
}

/// Sorted marker positions in frames.
/// Remove markers until no splice is shorter than `min` frames, as far as
/// locked markers allow. The shortest splice goes first, joining whichever
/// neighbour is shorter. A short splice between two locked markers stays.
pub fn merge_short(markers: &[u32], frames: u32, min: u32, locked: &[u32]) -> Vec<u32> {
    let mut markers = clean_markers(markers.to_vec(), frames);
    loop {
        // Splice i runs from bounds[i] to bounds[i + 1]. Marker k is
        // bounds[k + 1], so splice i starts at marker i - 1 and ends at i.
        let mut bounds = vec![0];
        bounds.extend(&markers);
        bounds.push(frames);
        let len = |i: usize| bounds[i + 1] - bounds[i];
        let count = bounds.len() - 1;
        // The shortest short splice that can join a neighbour, and the
        // marker that goes for it.
        let mut best: Option<(u32, usize)> = None;
        for i in (0..count).filter(|&i| len(i) < min) {
            let before = (i > 0 && !locked.contains(&bounds[i])).then(|| (len(i - 1), i - 1));
            let after =
                (i + 1 < count && !locked.contains(&bounds[i + 1])).then(|| (len(i + 1), i));
            let marker = match (before, after) {
                (Some(b), Some(a)) => {
                    if a.0 < b.0 {
                        a.1
                    } else {
                        b.1
                    }
                }
                (Some(b), None) => b.1,
                (None, Some(a)) => a.1,
                (None, None) => continue,
            };
            if best.is_none_or(|(shortest, _)| len(i) < shortest) {
                best = Some((len(i), marker));
            }
        }
        match best {
            Some((_, marker)) => {
                markers.remove(marker);
            }
            None => return markers,
        }
    }
}

pub fn auto_slice(left: &[f32], right: &[f32], options: Options) -> Vec<u32> {
    let mono: Vec<f32> = left.iter().zip(right).map(|(l, r)| (l + r) * 0.5).collect();
    let frames = mono.len();
    let min_gap = ((options.min_seconds.max(0.05) * FRAMES_PER_SECOND) as usize).max(1);
    let max_markers = options.max_markers.min(MAX_MARKERS);
    let bands = Bands::analyse(&mono);
    let n = bands.total.len();
    if n < 2 * min_gap {
        return Vec::new();
    }
    let quiet = smooth(&bands.total, 5);
    let mut cuts = match options.mode {
        Mode::Phrases => phrases(&bands, &quiet, min_gap, max_markers),
        Mode::Gestures => gestures(&bands, &quiet, min_gap, max_markers),
    };
    if let Some(max_seconds) = options.max_seconds {
        let max_gap = ((max_seconds * FRAMES_PER_SECOND) as usize).max(2 * min_gap);
        split_long(&mut cuts, &quiet, min_gap, max_gap, max_markers);
    }
    let mut markers: Vec<u32> = cuts
        .into_iter()
        .map(|f| zero_crossing(&mono, f * HOP + HOP / 2) as u32)
        .filter(|&m| m > 0 && (m as usize) < frames)
        .collect();
    markers.sort_unstable();
    markers.dedup();
    markers
}

/// Like `auto_slice`, but `keep` markers stay where they are. Each stretch
/// between them is sliced on its own, and the marker budget left after the
/// kept ones is shared out by length.
pub fn auto_slice_keeping(left: &[f32], right: &[f32], options: Options, keep: &[u32]) -> Vec<u32> {
    let frames = left.len().min(right.len());
    let mut keep: Vec<u32> = keep
        .iter()
        .copied()
        .filter(|&k| k > 0 && (k as usize) < frames)
        .collect();
    keep.sort_unstable();
    keep.dedup();
    if keep.is_empty() {
        return auto_slice(left, right, options);
    }
    let budget = options
        .max_markers
        .min(MAX_MARKERS)
        .saturating_sub(keep.len());
    let bounds: Vec<usize> = std::iter::once(0)
        .chain(keep.iter().map(|&k| k as usize))
        .chain(std::iter::once(frames))
        .collect();
    let mut markers = keep.clone();
    for pair in bounds.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let share = budget * (end - start) / frames;
        if share == 0 {
            continue;
        }
        let part = Options {
            max_markers: share,
            ..options
        };
        markers.extend(
            auto_slice(&left[start..end], &right[start..end], part)
                .into_iter()
                .map(|m| m + start as u32),
        );
    }
    markers.sort_unstable();
    markers.dedup();
    markers
}

/// Per-frame levels in dB.
struct Bands {
    bands: Vec<Vec<f32>>,
    total: Vec<f32>,
}

impl Bands {
    fn analyse(mono: &[f32]) -> Bands {
        let fs = SAMPLE_RATE as f32;
        let mut bands = vec![frame_db(&filtered(
            mono,
            &mut [Biquad::low_pass(EDGES[0], fs)],
        ))];
        for pair in EDGES.windows(2) {
            let mut chain = [
                Biquad::high_pass(pair[0], fs),
                Biquad::low_pass(pair[1], fs),
            ];
            bands.push(frame_db(&filtered(mono, &mut chain)));
        }
        bands.push(frame_db(&filtered(
            mono,
            &mut [Biquad::high_pass(EDGES[2], fs)],
        )));
        Bands {
            bands,
            total: frame_db(mono),
        }
    }
}

fn phrases(bands: &Bands, quiet: &[f32], min_gap: usize, max_markers: usize) -> Vec<usize> {
    let n = quiet.len();
    // Compare windows of up to 3 s, but never longer than a phrase.
    let w = min_gap.min(3 * FRAMES_PER_SECOND as usize).max(10);
    if n <= 2 * w {
        return Vec::new();
    }
    // Balance: smoothed band levels. Activity: how much each band moves,
    // from its raw frames.
    let balance: Vec<Prefix> = bands
        .bands
        .iter()
        .map(|b| Prefix::new(&smooth(b, 10)))
        .collect();
    let activity: Vec<Prefix> = bands.bands.iter().map(|b| Prefix::new(b)).collect();
    let k = bands.bands.len() as f32;

    let mut novelty = vec![0.0f32; n];
    for (t, nov) in novelty.iter_mut().enumerate().take(n - w).skip(w) {
        let (a, b, c) = (t - w, t, t + w);
        let shift: f32 = balance
            .iter()
            .map(|s| (s.mean(a, b) - s.mean(b, c)).powi(2))
            .sum::<f32>()
            .sqrt();
        let busy: f32 = activity
            .iter()
            .map(|s| (s.std(a, b) - s.std(b, c)).abs())
            .sum::<f32>()
            / k;
        *nov = shift + busy;
    }

    let (mean, std) = mean_std(&novelty[w..n - w]);
    let threshold = mean + 0.5 * std;
    let candidates: Vec<(usize, f32)> = local_maxima(&novelty, min_gap / 2)
        .into_iter()
        .filter(|&i| novelty[i] > threshold)
        .map(|i| (i, novelty[i]))
        .collect();

    // Snap each change to the quietest moment within 0.75 s.
    let reach = (0.75 * FRAMES_PER_SECOND) as usize;
    let snapped = candidates
        .into_iter()
        .map(|(i, score)| {
            (
                argmin(quiet, i.saturating_sub(reach), (i + reach).min(n)),
                score,
            )
        })
        .collect();
    pick(snapped, min_gap, n, max_markers)
}

fn gestures(bands: &Bands, quiet: &[f32], min_gap: usize, max_markers: usize) -> Vec<usize> {
    let n = quiet.len();
    // Salience: how far the most prominent band stands above its own level
    // over the surrounding 1.5 s, so an event counts whatever its register.
    let width = (1.5 * FRAMES_PER_SECOND) as usize;
    let mut salience = vec![f32::NEG_INFINITY; n];
    for band in &bands.bands {
        let env = smooth(band, 3);
        let baseline = running_median(&env, width);
        for i in 0..n {
            if env[i] > FLOOR_DB {
                salience[i] = salience[i].max(env[i] - baseline[i]);
            }
        }
    }
    for s in &mut salience {
        *s = s.max(0.0);
    }

    let peaks: Vec<usize> = local_maxima(&salience, 15)
        .into_iter()
        .filter(|&i| salience[i] >= 3.0)
        .collect();

    // Peaks belong to the same event unless salience dips at least 3 dB
    // below both between them. Each event: (first peak, last peak, strength).
    let mut events: Vec<(usize, usize, f32)> = Vec::new();
    for &p in &peaks {
        match events.last_mut() {
            Some(last)
                if {
                    let dip = salience[last.1..p]
                        .iter()
                        .copied()
                        .fold(f32::INFINITY, f32::min);
                    dip > salience[last.1].min(salience[p]) - 3.0
                } =>
            {
                last.1 = p;
                last.2 = last.2.max(salience[p]);
            }
            _ => events.push((p, p, salience[p])),
        }
    }

    // Cut in the quietest moment between the previous event and this one,
    // looking back at most 1 s.
    let back = FRAMES_PER_SECOND as usize;
    let mut cuts = Vec::with_capacity(events.len());
    for (k, &(first, _, strength)) in events.iter().enumerate() {
        let prev_end = if k > 0 { events[k - 1].1 } else { 0 };
        let lo = first.saturating_sub(back).max(prev_end);
        let hi = first.saturating_sub(5).max(lo + 1);
        cuts.push((argmin(quiet, lo, hi), strength));
    }
    pick(cuts, min_gap, n, max_markers)
}

/// Keep the strongest candidates that stay `min_gap` apart from each other
/// and from both ends.
fn pick(mut candidates: Vec<(usize, f32)>, min_gap: usize, n: usize, max: usize) -> Vec<usize> {
    candidates.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut kept: Vec<usize> = Vec::new();
    for (i, _) in candidates {
        if kept.len() >= max {
            break;
        }
        if i >= min_gap && i + min_gap <= n && kept.iter().all(|&k| k.abs_diff(i) >= min_gap) {
            kept.push(i);
        }
    }
    kept.sort_unstable();
    kept
}

/// Cut the longest splice at its quietest point until none is longer than
/// `max_gap` or the marker cap is reached. Each cut leaves the left piece
/// within `max_gap` and both pieces at least `min_gap` long.
fn split_long(
    cuts: &mut Vec<usize>,
    quiet: &[f32],
    min_gap: usize,
    max_gap: usize,
    max_markers: usize,
) {
    let n = quiet.len();
    while cuts.len() < max_markers {
        let bounds: Vec<usize> = std::iter::once(0)
            .chain(cuts.iter().copied())
            .chain(std::iter::once(n))
            .collect();
        let Some((a, b)) = bounds
            .windows(2)
            .map(|w| (w[0], w[1]))
            .max_by_key(|(a, b)| b - a)
        else {
            break;
        };
        if b - a <= max_gap {
            break;
        }
        let lo = (a + min_gap).max(b.saturating_sub(max_gap));
        let hi = (b - min_gap).min(a + max_gap);
        let at = if lo < hi {
            argmin(quiet, lo, hi + 1)
        } else {
            (a + b) / 2
        };
        let pos = cuts.partition_point(|&c| c < at);
        cuts.insert(pos, at);
    }
}

/// The nearest sample at or before `at`, within 2 ms, where the signal
/// changes sign, so the splice starts without a click.
fn zero_crossing(mono: &[f32], at: usize) -> usize {
    let at = at.min(mono.len().saturating_sub(1));
    let lo = at.saturating_sub(96).max(1);
    (lo..=at)
        .rev()
        .find(|&i| mono[i - 1].is_sign_negative() != mono[i].is_sign_negative())
        .unwrap_or(at)
}

/// RBJ cookbook biquad, Q = 1/sqrt(2).
pub(crate) struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    x: [f32; 2],
    y: [f32; 2],
}

impl Biquad {
    fn new(fc: f32, fs: f32, high: bool) -> Biquad {
        let w0 = std::f32::consts::TAU * fc / fs;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / std::f32::consts::SQRT_2;
        let a0 = 1.0 + alpha;
        let b = if high {
            [(1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0]
        } else {
            [(1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0]
        };
        Biquad {
            b: b.map(|v| v / a0),
            a: [-2.0 * cos / a0, (1.0 - alpha) / a0],
            x: [0.0; 2],
            y: [0.0; 2],
        }
    }

    fn low_pass(fc: f32, fs: f32) -> Biquad {
        Biquad::new(fc, fs, false)
    }

    pub(crate) fn high_pass(fc: f32, fs: f32) -> Biquad {
        Biquad::new(fc, fs, true)
    }

    pub(crate) fn process(&mut self, x: f32) -> f32 {
        let y = self.b[0] * x + self.b[1] * self.x[0] + self.b[2] * self.x[1]
            - self.a[0] * self.y[0]
            - self.a[1] * self.y[1];
        self.x = [x, self.x[0]];
        self.y = [y, self.y[0]];
        y
    }
}

fn filtered(input: &[f32], chain: &mut [Biquad]) -> Vec<f32> {
    input
        .iter()
        .map(|&s| chain.iter_mut().fold(s, |v, f| f.process(v)))
        .collect()
}

fn frame_db(signal: &[f32]) -> Vec<f32> {
    signal
        .chunks(HOP)
        .map(|c| {
            let ms = c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32;
            (10.0 * (ms + 1e-12).log10()).max(FLOOR_DB)
        })
        .collect()
}

/// Centred moving average over 2r+1 frames.
fn smooth(v: &[f32], r: usize) -> Vec<f32> {
    let p = Prefix::new(v);
    (0..v.len())
        .map(|i| p.mean(i.saturating_sub(r), (i + r + 1).min(v.len())))
        .collect()
}

fn running_median(v: &[f32], width: usize) -> Vec<f32> {
    let half = width / 2;
    let mut window = Vec::with_capacity(width + 1);
    (0..v.len())
        .map(|i| {
            window.clear();
            window.extend_from_slice(&v[i.saturating_sub(half)..(i + half + 1).min(v.len())]);
            let mid = window.len() / 2;
            *window.select_nth_unstable_by(mid, f32::total_cmp).1
        })
        .collect()
}

/// Frames that are the largest within `r` frames either side.
fn local_maxima(v: &[f32], r: usize) -> Vec<usize> {
    (0..v.len())
        .filter(|&i| {
            let lo = i.saturating_sub(r);
            let hi = (i + r + 1).min(v.len());
            v[lo..hi].iter().all(|&x| x <= v[i]) && v[lo..i].iter().all(|&x| x < v[i])
        })
        .collect()
}

fn argmin(v: &[f32], lo: usize, hi: usize) -> usize {
    (lo..hi.max(lo + 1).min(v.len()))
        .min_by(|&a, &b| v[a].total_cmp(&v[b]))
        .unwrap_or(lo)
}

fn mean_std(v: &[f32]) -> (f32, f32) {
    let n = v.len().max(1) as f32;
    let mean = v.iter().sum::<f32>() / n;
    let var = v.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / n;
    (mean, var.sqrt())
}

/// Prefix sums for O(1) window mean and standard deviation.
struct Prefix {
    sum: Vec<f64>,
    sq: Vec<f64>,
}

impl Prefix {
    fn new(v: &[f32]) -> Prefix {
        let mut sum = vec![0.0; v.len() + 1];
        let mut sq = vec![0.0; v.len() + 1];
        for (i, &x) in v.iter().enumerate() {
            sum[i + 1] = sum[i] + x as f64;
            sq[i + 1] = sq[i] + (x as f64) * (x as f64);
        }
        Prefix { sum, sq }
    }

    fn mean(&self, a: usize, b: usize) -> f32 {
        ((self.sum[b] - self.sum[a]) / (b - a).max(1) as f64) as f32
    }

    fn std(&self, a: usize, b: usize) -> f32 {
        let n = (b - a).max(1) as f64;
        let mean = (self.sum[b] - self.sum[a]) / n;
        (((self.sq[b] - self.sq[a]) / n - mean * mean).max(0.0)).sqrt() as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_short_joins_the_shorter_neighbour_and_keeps_locks() {
        // Splices of 10, 490, 5, 395 and 100 frames.
        let markers = [10, 500, 505, 900];
        // The 5-frame splice joins the 395 after it, then the 10 joins the 490.
        assert_eq!(merge_short(&markers, 1000, 50, &[]), vec![500, 900]);
        // A locked marker at 10 keeps the first splice short.
        assert_eq!(merge_short(&markers, 1000, 50, &[10]), vec![10, 500, 900]);
        // Locks on both sides of the 5-frame splice keep it too.
        assert_eq!(
            merge_short(&markers, 1000, 50, &[500, 505]),
            vec![500, 505, 900]
        );
        assert_eq!(merge_short(&markers, 1000, 1, &[]), markers.to_vec());
    }

    /// Deterministic white noise.
    fn noise(len: usize, seed: &mut u32) -> Vec<f32> {
        (0..len)
            .map(|_| {
                *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (*seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
            })
            .collect()
    }

    fn secs(s: f32) -> usize {
        (s * SAMPLE_RATE as f32) as usize
    }

    #[test]
    fn keeping_markers_slices_around_them() {
        let mut seed = 7;
        let mut x: Vec<f32> = noise(secs(12.0), &mut seed)
            .iter()
            .map(|s| s * 0.02)
            .collect();
        for k in 1..7 {
            let b = secs(k as f32 * 1.5);
            for (i, s) in noise(secs(0.4), &mut seed).into_iter().enumerate() {
                x[b + i] += s * 0.5;
            }
        }
        let options = Options {
            max_seconds: None,
            ..Options::gestures()
        };
        let keep = secs(5.0) as u32;
        let markers = auto_slice_keeping(&x, &x, options, &[keep]);
        assert!(markers.contains(&keep), "{markers:?}");
        assert!(markers.windows(2).all(|w| w[0] < w[1]));
        // The kept marker sits between two bursts, so the bursts still
        // each get a cut before them.
        assert!(markers.len() >= 6, "{markers:?}");
        // A budget of one leaves only the kept marker.
        let tight = Options {
            max_markers: 1,
            ..options
        };
        assert_eq!(auto_slice_keeping(&x, &x, tight, &[keep]), vec![keep]);
    }

    #[test]
    fn gestures_cut_in_the_gaps_between_bursts() {
        // A quiet crowd bed with a loud 0.4 s burst every 1.5 s.
        let mut seed = 1;
        let mut x: Vec<f32> = noise(secs(12.0), &mut seed)
            .iter()
            .map(|s| s * 0.02)
            .collect();
        let bursts: Vec<usize> = (1..7).map(|k| secs(k as f32 * 1.5)).collect();
        for &b in &bursts {
            for (i, s) in noise(secs(0.4), &mut seed).into_iter().enumerate() {
                x[b + i] += s * 0.5;
            }
        }
        let options = Options {
            max_seconds: None,
            ..Options::gestures()
        };
        let markers = auto_slice(&x, &x, options);
        assert_eq!(markers.len(), bursts.len(), "{markers:?}");
        for (m, b) in markers.iter().zip(&bursts) {
            let m = *m as usize;
            assert!(
                m < *b && m > b - secs(1.1),
                "marker {m} should sit in the gap before {b}"
            );
        }
    }

    #[test]
    fn gestures_respect_min_length_and_cap() {
        let mut seed = 7;
        let mut x: Vec<f32> = noise(secs(10.0), &mut seed)
            .iter()
            .map(|s| s * 0.02)
            .collect();
        for k in 0..30 {
            let b = secs(0.3 * k as f32 + 0.5);
            for i in 0..secs(0.1) {
                x[b + i] += 0.5 * x[b + i].signum();
            }
        }
        let options = Options {
            min_seconds: 1.0,
            max_markers: 5,
            ..Options::gestures()
        };
        let markers = auto_slice(&x, &x, options);
        assert!(markers.len() <= 5);
        assert!(markers.windows(2).all(|w| w[1] - w[0] >= SAMPLE_RATE));
    }

    #[test]
    fn phrases_find_a_change_of_texture() {
        // 10 s of low rumble, then 10 s of bright hiss, at similar levels.
        let mut seed = 3;
        let fs = SAMPLE_RATE as f32;
        let mut low = Biquad::low_pass(200.0, fs);
        let mut high = Biquad::high_pass(5000.0, fs);
        let mut x: Vec<f32> = noise(secs(10.0), &mut seed)
            .into_iter()
            .map(|s| low.process(s))
            .collect();
        x.extend(
            noise(secs(10.0), &mut seed)
                .into_iter()
                .map(|s| high.process(s)),
        );
        let options = Options {
            max_seconds: None,
            ..Options::phrases()
        };
        let markers = auto_slice(&x, &x, options);
        assert_eq!(markers.len(), 1, "{markers:?}");
        assert!(
            markers[0].abs_diff(secs(10.0) as u32) < secs(0.8) as u32,
            "{markers:?}"
        );
    }

    #[test]
    fn longest_splice_cuts_at_the_quietest_point() {
        // 30 s of steady noise with a near-silent dip at 12 s and nothing
        // else to find.
        let mut seed = 5;
        let mut x: Vec<f32> = noise(secs(30.0), &mut seed)
            .iter()
            .map(|s| s * 0.3)
            .collect();
        for s in &mut x[secs(11.9)..secs(12.1)] {
            *s *= 0.01;
        }
        // Steady noise has no events, so only the longest-splice rule cuts.
        let options = Options {
            min_seconds: 1.0,
            max_seconds: Some(20.0),
            ..Options::gestures()
        };
        let markers = auto_slice(&x, &x, options);
        assert!(!markers.is_empty());
        let bounds: Vec<u32> = std::iter::once(0)
            .chain(markers.iter().copied())
            .chain(std::iter::once(x.len() as u32))
            .collect();
        assert!(
            bounds.windows(2).all(|w| w[1] - w[0] <= secs(20.0) as u32),
            "{markers:?}"
        );
        assert!(
            markers
                .iter()
                .any(|&m| m.abs_diff(secs(12.0) as u32) < secs(0.2) as u32),
            "{markers:?}"
        );
    }

    #[test]
    fn longest_splice_fills_a_reel_with_no_events() {
        let x = vec![0.0f32; secs(20.0)];
        let options = Options {
            min_seconds: 1.0,
            max_seconds: Some(3.0),
            ..Options::gestures()
        };
        let markers = auto_slice(&x, &x, options);
        let bounds: Vec<u32> = std::iter::once(0)
            .chain(markers.iter().copied())
            .chain(std::iter::once(x.len() as u32))
            .collect();
        assert!(
            bounds
                .windows(2)
                .all(|w| w[1] - w[0] <= secs(3.0) as u32 + 96
                    && w[1] - w[0] >= secs(1.0) as u32 - 96),
            "{markers:?}"
        );
    }

    #[test]
    fn gestures_hear_events_in_any_register() {
        // A low thump at 2 s and a high hiss at 5 s over a quiet bed.
        let mut seed = 11;
        let fs = SAMPLE_RATE as f32;
        let mut x: Vec<f32> = noise(secs(8.0), &mut seed)
            .iter()
            .map(|s| s * 0.01)
            .collect();
        let mut low = Biquad::low_pass(120.0, fs);
        for (i, s) in noise(secs(0.3), &mut seed).into_iter().enumerate() {
            x[secs(2.0) + i] += low.process(s) * 4.0;
        }
        let mut high = Biquad::high_pass(8000.0, fs);
        for (i, s) in noise(secs(0.3), &mut seed).into_iter().enumerate() {
            x[secs(5.0) + i] += high.process(s) * 0.5;
        }
        let options = Options {
            max_seconds: None,
            ..Options::gestures()
        };
        let markers = auto_slice(&x, &x, options);
        assert_eq!(markers.len(), 2, "{markers:?}");
        assert!(
            markers[0] < secs(2.0) as u32
                && markers[1] < secs(5.0) as u32
                && markers[1] > secs(2.3) as u32,
            "{markers:?}"
        );
    }

    #[test]
    fn silence_and_short_input_give_no_markers() {
        let none = Options {
            max_seconds: None,
            ..Options::gestures()
        };
        assert!(auto_slice(&[0.0; 48_000], &[0.0; 48_000], none).is_empty());
        assert!(auto_slice(&[0.1; 100], &[0.1; 100], Options::phrases()).is_empty());
    }

    #[test]
    fn markers_land_on_zero_crossings() {
        let x: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.05).sin()).collect();
        let z = zero_crossing(&x, 700);
        assert!((700 - 96..=700).contains(&z));
        assert_ne!(x[z - 1].is_sign_negative(), x[z].is_sign_negative());
    }
}
