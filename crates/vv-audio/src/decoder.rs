//! Shared file decoding and block storage for preview and offline readers.

use std::collections::VecDeque;
use std::ops::{ControlFlow, Range};
use std::path::Path;
use std::sync::Arc;

use crate::{BLOCK_FRAMES, SampleBlocks};

pub struct BlockDecoder {
    sample_rate: u32,
    channels: u16,
}

impl BlockDecoder {
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        Self {
            sample_rate,
            channels: channels.max(1),
        }
    }

    /// Slots follow `streams`; the callback controls cancellation and backpressure.
    pub fn decode(
        &self,
        path: &Path,
        streams: &[usize],
        from: u64,
        mut on_chunk: impl FnMut(usize, u64, &[f32]) -> ControlFlow<()>,
    ) -> Result<Vec<Option<(u32, u16)>>, vv_media::MediaError> {
        if streams.is_empty() {
            return Ok(Vec::new());
        }
        let mut remixed = Vec::new();
        vv_media::decode_audio_streams_from(
            path,
            streams,
            self.sample_rate,
            from,
            |slot, channels, at, samples| {
                remixed.clear();
                crate::mixer::remix_channels_into(samples, channels, self.channels, &mut remixed);
                on_chunk(slot, at, &remixed)
            },
        )
    }
}

/// Completed blocks are shared; only a view of the unfinished block is copied.
pub struct BlockBuffer {
    first: u64,
    blocks: VecDeque<Arc<Vec<f32>>>,
    filling: Vec<f32>,
    channels: u16,
}

impl BlockBuffer {
    pub fn new(from: u64, channels: u16) -> Self {
        let channels = channels.max(1);
        Self {
            first: from / BLOCK_FRAMES,
            blocks: VecDeque::new(),
            filling: Vec::with_capacity(BLOCK_FRAMES as usize * channels as usize),
            channels,
        }
    }

    pub fn start(&self) -> u64 {
        self.first * BLOCK_FRAMES
    }
    pub fn end(&self) -> u64 {
        (self.first + self.blocks.len() as u64) * BLOCK_FRAMES
            + self.filling.len() as u64 / u64::from(self.channels)
    }

    pub fn append(&mut self, at: u64, samples: &[f32], until: u64) {
        let mut cursor = self.end();
        crate::blocks::append_to_blocks(
            &mut self.filling,
            &mut cursor,
            at,
            samples,
            until,
            self.channels,
            BLOCK_FRAMES,
            |_, block| {
                self.blocks.push_back(Arc::new(block));
                true
            },
        );
    }

    pub fn view(&self, frames: Range<u64>) -> SampleBlocks {
        let end = frames.end.min(self.end());
        let first = (frames.start / BLOCK_FRAMES).max(self.first);
        let last = end.div_ceil(BLOCK_FRAMES).max(first);
        let full = self.first + self.blocks.len() as u64;
        let blocks = (first..last)
            .map(
                |index| match self.blocks.get((index - self.first) as usize) {
                    Some(block) => Some(block.clone()),
                    None => (index == full).then(|| Arc::new(self.filling.clone())),
                },
            )
            .collect();
        SampleBlocks::new(
            first * BLOCK_FRAMES,
            BLOCK_FRAMES,
            self.channels,
            blocks,
            end.max(first * BLOCK_FRAMES),
        )
    }

    pub fn drop_before(&mut self, frame: u64) {
        while self.first < frame / BLOCK_FRAMES && !self.blocks.is_empty() {
            self.blocks.pop_front();
            self.first += 1;
        }
    }

    pub fn take_complete(&mut self) -> Vec<(u64, Arc<Vec<f32>>)> {
        let mut result = Vec::with_capacity(self.blocks.len());
        while let Some(samples) = self.blocks.pop_front() {
            result.push((self.first, samples));
            self.first += 1;
        }
        result
    }

    pub fn take_tail(&mut self) -> Option<(u64, Arc<Vec<f32>>)> {
        if self.filling.is_empty() {
            return None;
        }
        Some((
            self.first + self.blocks.len() as u64,
            Arc::new(std::mem::take(&mut self.filling)),
        ))
    }

    pub fn held_samples(&self) -> usize {
        self.blocks.iter().map(|b| b.len()).sum::<usize>() + self.filling.len()
    }
}

#[cfg(test)]
#[path = "tests/decoder.rs"]
mod tests;
