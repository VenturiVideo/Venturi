//! The interface the mixer drives an effect with: the built-in ones today,
//! VST3 plugins through the same seam (see plans/VST3_HOST.md).

use std::any::Any;
use std::sync::Arc;

use vv_core::Vst3Effect;

use crate::dynamics::BandActivity;

/// An effect with a state, run in order a mix block at a time.
pub trait Effect: Send {
    /// In place, interleaved. No allocations: it runs in the realtime
    /// callback. `Some` for an effect with bands to meter.
    fn process(&mut self, samples: &mut [f32]) -> Option<BandActivity>;

    /// The mix jumped: start again from silence.
    fn reset(&mut self);

    /// Carries on from where `previous` was, if it is the same kind of
    /// effect: new parameters must not restart it.
    fn take_memory_from(&mut self, previous: &dyn Effect);

    fn as_any(&self) -> &dyn Any;
}

/// A hosted plugin, shared by the snapshots that play the same placement.
pub trait PluginProcessor: Send + Sync {
    /// In place, interleaved; never blocks: passes through when busy.
    /// `true` only if the block was processed (it is left untouched otherwise).
    fn process(&self, samples: &mut [f32], channels: usize) -> bool;
    fn reset(&self);
    fn latency(&self) -> u32;
}

pub type PluginHandle = Arc<dyn PluginProcessor>;

pub struct PluginRequest<'a> {
    /// Unique per place the effect plays in this pass: compound path, id, occurrence.
    pub key: u64,
    pub effect: &'a Vst3Effect,
    pub sample_rate: u32,
    /// `false` for a bypassed plugin: its live instance is kept, nothing is
    /// made or processed.
    pub process: bool,
}

/// Runs a plugin in a mix. The instance outlives the snapshots, so a new
/// one has nothing to take over.
pub(crate) struct PluginEffect {
    pub handle: PluginHandle,
    pub channels: usize,
}

impl Effect for PluginEffect {
    fn process(&mut self, samples: &mut [f32]) -> Option<BandActivity> {
        let processed = self.handle.process(samples, self.channels);
        // The plugin folded the channels into its stereo bus; the upmix back fills `c` from `c % 2`.
        if processed && self.channels > 2 {
            for frame in samples.chunks_exact_mut(self.channels) {
                for c in 2..frame.len() {
                    frame[c] = frame[c % 2];
                }
            }
        }
        None
    }

    fn reset(&mut self) {
        self.handle.reset();
    }

    fn take_memory_from(&mut self, _: &dyn Effect) {}

    fn as_any(&self) -> &dyn Any {
        self
    }
}
