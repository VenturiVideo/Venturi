//! `SharedFrameCache`: cache of the decoded frames of every media in the
//! window, on a global budget, see plans/REFACTOR_PIPELINE.md §2.

use crate::decode::FrameYuv420;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use vv_core::{FrameIdx, MediaId};

/// How far `get_near` looks for a keyframe standing in: past the longest GOPs
/// cameras write (10 s at 60 fps is 600 frames).
const MAX_KEYFRAME_DISTANCE: FrameIdx = 1200;

/// How `get_near` found the frame it returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Served {
    Exact,
    /// Within the stride: fast playback shows no frame in between.
    Neighbour,
    /// A keyframe in place of a frame the decoder cannot reach in time.
    Keyframe,
}

/// Stretch of source frames wanted by the current window, with its
/// position on the timeline: used to measure the distance from the playhead.
#[derive(Debug, Clone, Copy)]
pub struct WantedRange {
    pub media_id: MediaId,
    pub source_start: FrameIdx,
    pub source_end: FrameIdx,
    /// Where `source_start` falls in timeline space.
    pub timeline_start: FrameIdx,
    /// The clip's `Clip::rate`: timeline frames per source frame.
    pub rate: vv_core::Rational,
}

impl WantedRange {
    fn contains(&self, idx: FrameIdx) -> bool {
        idx >= self.source_start && idx <= self.source_end
    }

    fn timeline_position_of(&self, idx: FrameIdx) -> FrameIdx {
        self.timeline_start + self.rate.scale_round(idx) - self.rate.scale_round(self.source_start)
    }

    /// A range of a compound clip's nested timeline placed on the timeline
    /// of `outer`, the compound clip's own range: positions in two
    /// different timelines cannot be compared with each other.
    pub fn seen_through(self, outer: &WantedRange) -> Self {
        Self {
            timeline_start: outer.timeline_position_of(self.timeline_start),
            rate: outer
                .rate
                .divided_by(vv_core::Rational::new(self.rate.den, self.rate.num)),
            ..self
        }
    }

    /// The part `source_start..=source_end` of this range.
    pub fn sub_range(&self, source_start: FrameIdx, source_end: FrameIdx) -> Self {
        Self {
            source_start,
            source_end,
            timeline_start: self.timeline_position_of(source_start),
            ..*self
        }
    }
}

struct SharedInner {
    entries: HashMap<MediaId, BTreeMap<FrameIdx, Arc<FrameYuv420>>>,
    bytes_used: usize,
    /// See `set_stride`; absent means 1.
    strides: HashMap<MediaId, FrameIdx>,
    /// See `set_keyframes_only`.
    keyframes_only: HashSet<MediaId>,
}

/// Cache of the frames of every media in the window, keyed by
/// `(MediaId, source frame)` with a budget in bytes (different resolutions
/// weigh differently). What to keep is decided by `reconcile` alone.
pub struct SharedFrameCache {
    inner: Mutex<SharedInner>,
}

impl Default for SharedFrameCache {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedFrameCache {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(SharedInner {
                entries: HashMap::new(),
                bytes_used: 0,
                strides: HashMap::new(),
                keyframes_only: Default::default(),
            }),
        }
    }

    pub fn get(&self, media_id: MediaId, idx: FrameIdx) -> Option<Arc<FrameYuv420>> {
        self.inner
            .lock()
            .unwrap()
            .entries
            .get(&media_id)?
            .get(&idx)
            .cloned()
    }

    /// `idx`, or else the nearest cached frame within the stride of the media
    /// and within `lo..=hi` (the clip's source frames, widened to `idx`: a
    /// transition shows frames past the trim), earlier first: with a stride
    /// the frames in between are not decoded at all. With `set_keyframes_only`,
    /// else the nearest earlier frame in the clip, or the nearest later one
    /// at its start, before the first keyframe inside it.
    pub fn get_near(
        &self,
        media_id: MediaId,
        idx: FrameIdx,
        lo: FrameIdx,
        hi: FrameIdx,
    ) -> Option<(Arc<FrameYuv420>, Served)> {
        let inner = self.inner.lock().unwrap();
        let frames = inner.entries.get(&media_id)?;
        let stride = inner.stride(media_id);
        let clip_lo = lo.min(idx);
        let clip_hi = hi.max(idx);
        if let Some(frame) = frames.get(&idx) {
            return Some((frame.clone(), Served::Exact));
        }
        if stride > 1 {
            let min_i = (idx - (stride - 1)).max(clip_lo);
            let max_i = (idx + (stride - 1)).min(clip_hi);
            let earlier = (min_i < idx)
                .then(|| frames.range(min_i..idx).next_back())
                .flatten();
            let later = (idx < max_i)
                .then(|| frames.range((idx + 1)..=max_i).next())
                .flatten();
            let neighbour = match (earlier, later) {
                (Some((&e_idx, e_frame)), Some((&l_idx, l_frame))) => {
                    if idx - e_idx <= l_idx - idx {
                        Some(e_frame)
                    } else {
                        Some(l_frame)
                    }
                }
                (Some((_, frame)), None) | (None, Some((_, frame))) => Some(frame),
                (None, None) => None,
            };
            if let Some(frame) = neighbour {
                return Some((frame.clone(), Served::Neighbour));
            }
        }
        if !inner.keyframes_only.contains(&media_id) {
            return None;
        }
        let min_kf = (idx - MAX_KEYFRAME_DISTANCE).max(clip_lo);
        if min_kf < idx
            && let Some((_, frame)) = frames.range(min_kf..idx).next_back()
        {
            return Some((frame.clone(), Served::Keyframe));
        }
        let max_kf = (idx + MAX_KEYFRAME_DISTANCE).min(clip_hi);
        if idx < max_kf
            && let Some((_, frame)) = frames.range((idx + 1)..=max_kf).next()
        {
            return Some((frame.clone(), Served::Keyframe));
        }
        None
    }

    /// The decoder cannot keep up with `media_id`: only keyframes are
    /// decoded, and `get_near` stands one in for any frame missing.
    pub fn set_keyframes_only(&self, media_id: MediaId, on: bool) {
        let mut inner = self.inner.lock().unwrap();
        if on {
            inner.keyframes_only.insert(media_id);
        } else {
            inner.keyframes_only.remove(&media_id);
        }
    }

    /// `set_keyframes_only` for exactly `media`, off for any other.
    pub fn set_keyframes_only_media(&self, media: HashSet<MediaId>) {
        self.inner.lock().unwrap().keyframes_only = media;
    }

    /// Some media has keyframes standing in for its frames.
    pub fn any_keyframes_only(&self) -> bool {
        !self.inner.lock().unwrap().keyframes_only.is_empty()
    }

    pub fn keyframes_only(&self, media_id: MediaId) -> bool {
        self.inner
            .lock()
            .unwrap()
            .keyframes_only
            .contains(&media_id)
    }

    /// Only the frames at multiples of `stride` are wanted for `media_id`
    /// (fast playback shows no others): `covers` and `cached_ranges` count the
    /// ones in between as there.
    pub fn set_stride(&self, media_id: MediaId, stride: FrameIdx) {
        let mut inner = self.inner.lock().unwrap();
        if stride > 1 {
            inner.strides.insert(media_id, stride);
        } else {
            inner.strides.remove(&media_id);
        }
    }

    pub fn stride(&self, media_id: MediaId) -> FrameIdx {
        self.inner.lock().unwrap().stride(media_id)
    }

    pub fn contains(&self, media_id: MediaId, idx: FrameIdx) -> bool {
        self.inner
            .lock()
            .unwrap()
            .entries
            .get(&media_id)
            .is_some_and(|frames| frames.contains_key(&idx))
    }

    /// Bytes used, on the real size of each frame.
    pub fn bytes_used(&self) -> usize {
        self.inner.lock().unwrap().bytes_used
    }

    /// Does not enforce the budget: the caller inserts in priority order and
    /// stops on its own.
    pub fn insert(&self, media_id: MediaId, idx: FrameIdx, frame: Arc<FrameYuv420>) {
        let mut inner = self.inner.lock().unwrap();
        let bytes = frame.byte_len();
        if let Some(old) = inner
            .entries
            .entry(media_id)
            .or_default()
            .insert(idx, frame)
        {
            inner.bytes_used -= old.byte_len();
        }
        inner.bytes_used += bytes;
    }

    /// Empties everything: for when the frames come from a wrong source (e.g.
    /// the proxy toggle changes), which `reconcile` would not be able to tell.
    pub fn clear(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.entries.clear();
        inner.bytes_used = 0;
    }

    /// Drops every frame of `media_id`: for when the id now names another
    /// file, which `reconcile` cannot tell.
    pub fn remove_media(&self, media_id: MediaId) {
        let mut inner = self.inner.lock().unwrap();
        inner.strides.remove(&media_id);
        inner.keyframes_only.remove(&media_id);
        if let Some(frames) = inner.entries.remove(&media_id) {
            inner.bytes_used -= frames.values().map(|f| f.byte_len()).sum::<usize>();
        }
    }

    /// The only eviction policy, run every cycle before the fill.
    /// Tier A: out goes whatever falls in no interval of `window`.
    /// Tier B: past `budget_bytes`, out go the frames farthest from the playhead
    /// in timeline frames. Never by recency: during the fill the frame at the
    /// playhead is the least recent one.
    pub fn reconcile(&self, playhead: FrameIdx, window: &[WantedRange], budget_bytes: usize) {
        let mut inner = self.inner.lock().unwrap();
        let inner = &mut *inner;

        // Tier A.
        let bytes_used = &mut inner.bytes_used;
        inner.entries.retain(|&media_id, frames| {
            frames.retain(|&idx, frame| {
                let keep = window
                    .iter()
                    .any(|w| w.media_id == media_id && w.contains(idx));
                if !keep {
                    *bytes_used -= frame.byte_len();
                }
                keep
            });
            !frames.is_empty()
        });

        // Tier B.
        if inner.bytes_used > budget_bytes {
            let mut scored: Vec<((MediaId, FrameIdx), (u8, FrameIdx))> = inner
                .entries
                .iter()
                .flat_map(|(&media_id, frames)| {
                    frames.keys().map(move |&idx| {
                        (
                            (media_id, idx),
                            eviction_rank(media_id, idx, playhead, window),
                        )
                    })
                })
                .collect();
            // Eviction works from the tail.
            scored.sort_unstable_by_key(|&(_, rank)| std::cmp::Reverse(rank));
            for ((media_id, idx), _) in scored {
                if inner.bytes_used <= budget_bytes {
                    break;
                }
                if let Some(frames) = inner.entries.get_mut(&media_id) {
                    if let Some(frame) = frames.remove(&idx) {
                        inner.bytes_used -= frame.byte_len();
                    }
                    if frames.is_empty() {
                        inner.entries.remove(&media_id);
                    }
                }
            }
        }
    }

    /// All the wanted frames `start..=end` of the media are cached: all of
    /// them, or with a stride its multiples (none in the range: covered).
    /// Stops at the first missing one, without building the intervals.
    pub fn covers(&self, media_id: MediaId, start: FrameIdx, end: FrameIdx) -> bool {
        self.inner.lock().unwrap().covers(media_id, start, end)
    }

    /// `covers` for a whole segment to show: one with no multiple of the
    /// stride in it (a freeze frame, a clip of a frame or two) still wants
    /// its first frame, or it would show nothing.
    pub fn covers_segment(&self, media_id: MediaId, start: FrameIdx, end: FrameIdx) -> bool {
        if start > end {
            return true;
        }
        let inner = self.inner.lock().unwrap();
        let stride = inner.stride(media_id);
        let has_multiple = start + (-start).rem_euclid(stride) <= end;
        inner.covers(media_id, start, end)
            && (has_multiple
                || inner
                    .entries
                    .get(&media_id)
                    .is_some_and(|frames| frames.contains_key(&start)))
    }

    /// For a media with `set_keyframes_only`, the stretches its cached frames
    /// stand in for, from one to the next: empty for any other.
    pub fn keyframe_ranges(&self, media_id: MediaId) -> Vec<(FrameIdx, FrameIdx)> {
        let inner = self.inner.lock().unwrap();
        if !inner.keyframes_only.contains(&media_id) {
            return Vec::new();
        }
        let Some(frames) = inner.entries.get(&media_id) else {
            return Vec::new();
        };
        let mut ranges: Vec<(FrameIdx, FrameIdx)> = Vec::new();
        for &idx in frames.keys() {
            match ranges.last_mut() {
                Some((_, end)) if idx <= *end + MAX_KEYFRAME_DISTANCE => *end = idx,
                _ => ranges.push((idx, idx)),
            }
        }
        ranges
    }

    /// Contiguous (inclusive) intervals currently cached for a media — for
    /// the "buffered" indicator in the UI. With a stride, the gaps it leaves
    /// do not split them.
    pub fn cached_ranges(&self, media_id: MediaId) -> Vec<(FrameIdx, FrameIdx)> {
        let inner = self.inner.lock().unwrap();
        let Some(frames) = inner.entries.get(&media_id) else {
            return Vec::new();
        };
        let stride = inner.stride(media_id);
        let mut ranges: Vec<(FrameIdx, FrameIdx)> = Vec::new();
        for &idx in frames.keys() {
            match ranges.last_mut() {
                Some((_, end)) if idx <= *end + stride => *end = idx,
                _ => ranges.push((idx, idx)),
            }
        }
        ranges
    }
}

impl SharedInner {
    fn stride(&self, media_id: MediaId) -> FrameIdx {
        self.strides.get(&media_id).copied().unwrap_or(1)
    }

    fn covers(&self, media_id: MediaId, start: FrameIdx, end: FrameIdx) -> bool {
        let stride = self.stride(media_id);
        let first_multiple = start + (-start).rem_euclid(stride);
        if first_multiple > end {
            return true;
        }
        let Some(frames) = self.entries.get(&media_id) else {
            return false;
        };
        if stride == 1 {
            frames.contains_key(&start)
                && frames.contains_key(&end)
                && frames.range(start..=end).count() == (end - start + 1) as usize
        } else {
            (first_multiple..=end)
                .step_by(stride as usize)
                .all(|idx| frames.contains_key(&idx))
        }
    }
}

/// Eviction order, greatest first: outside the window, then behind the
/// playhead, then ahead, farthest first within each (in timeline frames).
/// Behind goes before ahead: measured both ways alike, during playback the
/// frames just played would push out the ones about to be.
fn eviction_rank(
    media_id: MediaId,
    idx: FrameIdx,
    playhead: FrameIdx,
    window: &[WantedRange],
) -> (u8, FrameIdx) {
    window
        .iter()
        .filter(|w| w.media_id == media_id && w.contains(idx))
        .map(|w| {
            let offset = w.timeline_position_of(idx) - playhead;
            if offset < 0 {
                (1, -offset)
            } else {
                (0, offset)
            }
        })
        .min()
        .unwrap_or((2, FrameIdx::MAX))
}

#[cfg(test)]
#[path = "tests/cache.rs"]
mod tests;
