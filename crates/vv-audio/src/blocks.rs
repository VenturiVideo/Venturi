//! Interleaved audio held in equal blocks, some of which may be missing:
//! a long file is decoded, published and freed a block at a time, never
//! copied whole.

use std::ops::Range;
use std::sync::Arc;

/// The block size of the preview's audio: about 1.4 s at 48 kHz.
pub const BLOCK_FRAMES: u64 = 1 << 16;

/// Appends a placed chunk on a fixed block grid, filling gaps with silence
/// and keeping the first samples in overlaps. The caller owns the partial block.
pub fn append_to_blocks(
    filling: &mut Vec<f32>,
    cursor: &mut u64,
    at: u64,
    samples: &[f32],
    until: u64,
    channels: u16,
    block_frames: u64,
    mut emit: impl FnMut(u64, Vec<f32>) -> bool,
) -> bool {
    let ch = channels.max(1) as u64;
    let block_frames = block_frames.max(1);
    let end = (at + samples.len() as u64 / ch).min(until);
    while *cursor < end {
        let block_end = (*cursor / block_frames + 1) * block_frames;
        let next = end.min(block_end).min(if *cursor < at { at } else { end });
        let count = ((next - *cursor) * ch) as usize;
        if *cursor < at {
            filling.resize(filling.len() + count, 0.0);
        } else {
            let offset = ((*cursor - at) * ch) as usize;
            filling.extend_from_slice(&samples[offset..offset + count]);
        }
        *cursor = next;
        if next == block_end {
            let block =
                std::mem::replace(filling, Vec::with_capacity((block_frames * ch) as usize));
            if !emit(next / block_frames - 1, block) {
                return false;
            }
        }
    }
    true
}

/// A run of frames read from `SampleBlocks::run`.
pub enum Run<'a> {
    Samples(&'a [f32]),
    /// A block not there: this many frames of silence.
    Silence(u64),
    End,
}

#[derive(Clone)]
pub struct SampleBlocks {
    /// Frame of the first sample of `blocks[0]`.
    start: u64,
    block_frames: u64,
    channels: usize,
    /// Each one `block_frames` long, except the one at `end`.
    blocks: Vec<Option<Arc<Vec<f32>>>>,
    /// Where the audio ends: the end of the file, or of the blocks asked for.
    end: u64,
}

impl SampleBlocks {
    pub fn new(
        start: u64,
        block_frames: u64,
        channels: u16,
        blocks: Vec<Option<Arc<Vec<f32>>>>,
        end: u64,
    ) -> Self {
        Self {
            start,
            block_frames: block_frames.max(1),
            channels: channels.max(1) as usize,
            blocks,
            end: end.max(start),
        }
    }

    /// One block holding all of `samples`, starting at frame `start`.
    pub fn contiguous(samples: Arc<Vec<f32>>, start: u64, channels: u16) -> Self {
        let frames = (samples.len() / channels.max(1) as usize) as u64;
        Self::new(start, frames, channels, vec![Some(samples)], start + frames)
    }

    pub fn start(&self) -> u64 {
        self.start
    }

    pub fn end(&self) -> u64 {
        self.end
    }

    pub fn is_empty(&self) -> bool {
        self.end == self.start
    }

    /// The samples from `frame` to the end of its block.
    pub fn run(&self, frame: u64) -> Run<'_> {
        if frame < self.start || frame >= self.end {
            return Run::End;
        }
        let offset = frame - self.start;
        let index = (offset / self.block_frames) as usize;
        let block_end = (self.start + (index as u64 + 1) * self.block_frames).min(self.end);
        match self.blocks.get(index) {
            Some(Some(block)) => {
                let from = (offset % self.block_frames) as usize * self.channels;
                let to = (from + (block_end - frame) as usize * self.channels).min(block.len());
                match block.get(from..to) {
                    Some(samples) if !samples.is_empty() => Run::Samples(samples),
                    _ => Run::Silence(block_end - frame),
                }
            }
            _ => Run::Silence(block_end - frame),
        }
    }

    /// The samples of one frame; `None` outside the audio or in a missing block.
    pub fn frame(&self, frame: u64) -> Option<&[f32]> {
        match self.run(frame) {
            Run::Samples(samples) => samples.get(..self.channels),
            Run::Silence(_) | Run::End => None,
        }
    }

    /// Frames `range` in one buffer, missing blocks as silence, up to `end`.
    pub fn to_vec(&self, range: Range<u64>) -> Vec<f32> {
        let to = range.end.min(self.end);
        let mut out = Vec::with_capacity(to.saturating_sub(range.start) as usize * self.channels);
        let mut frame = range.start;
        while frame < to {
            let frames = match self.run(frame) {
                Run::Samples(samples) if samples.len() >= self.channels => {
                    let frames = ((samples.len() / self.channels) as u64).min(to - frame);
                    out.extend_from_slice(&samples[..frames as usize * self.channels]);
                    frames
                }
                // Less than a frame left in a block cut mid-frame: one frame
                // of silence, or the loop would stall.
                Run::Samples(_) => {
                    out.resize(out.len() + self.channels, 0.0);
                    1
                }
                Run::Silence(frames) => {
                    let frames = frames.min(to - frame);
                    out.resize(out.len() + frames as usize * self.channels, 0.0);
                    frames
                }
                Run::End => {
                    let frames = to - frame;
                    out.resize(out.len() + frames as usize * self.channels, 0.0);
                    frames
                }
            };
            frame += frames;
        }
        out
    }

    /// Every block holding the frames `range` is there; past `end` nothing
    /// is missing.
    pub fn holds(&self, range: Range<u64>) -> bool {
        let (from, to) = (range.start.max(self.start), range.end.min(self.end));
        if from >= to {
            return true;
        }
        let first = ((from - self.start) / self.block_frames) as usize;
        let last = ((to - 1 - self.start) / self.block_frames) as usize;
        (first..=last).all(|index| self.blocks.get(index).is_some_and(Option::is_some))
    }
}

#[cfg(test)]
#[path = "tests/blocks.rs"]
mod tests;
