//! Audio track mixing (gain, resample, time-stretch) and `cpal` output
//! (see ARCHITECTURE.md § Audio pipeline). The mixer position is the
//! playback clock.

pub mod blocks;
pub mod decoder;
pub mod dynamics;
pub mod effect;
pub mod eq;
mod filter;
pub mod levels;
pub mod loudness;
pub mod mixer;
pub mod recorder;
pub mod spectrum;
pub mod stretch;
pub mod stretched;

pub use blocks::{BLOCK_FRAMES, SampleBlocks};
pub use effect::{PluginHandle, PluginProcessor, PluginRequest};
pub use levels::{LevelAnalysis, LevelSegment, Measured};
pub use mixer::{
    AnalysisSlot, AudioSource, BandMeter, ClipAudio, ClipInput, LevelReading, MIX_BLOCK_FRAMES,
    MixSnapshot, Mixer, MixerState, StretchedWindow, StripKeys, mix_range_without_plugins,
    plugin_key, plugin_key_in, plugin_keys_in,
};
pub use stretch::{STRETCH_CHUNK_FRAMES, Stretcher, stretch_samples};
pub use stretched::{StretchInput, StretchRequest};
