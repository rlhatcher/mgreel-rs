//! Read, edit and write Make Noise Morphagene reels, the microSD card that
//! holds them, and the card's `options.txt`.
//!
//! Format facts come from the Morphagene manual: reels are 48 kHz 32-bit
//! float stereo WAVs up to 2.9 minutes long, named `mg1.wav` to `mgw.wav`,
//! with splice markers stored as cue points.

pub mod card;
pub mod edit;
pub mod history;
pub mod names;
pub mod options;
pub mod reel;
pub mod riff;
pub mod slice;

pub use card::{Card, SlotInfo};
pub use options::OptionsFile;
pub use reel::{BuildOptions, Clip, Peaks, Problem, Reel, ReelFile};
