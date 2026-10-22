# AUDIO_LIVE_MIX — compounds mixed live, offline render and analyses in streaming

**Status:** chosen over [AUDIO_GRAPH.md](AUDIO_GRAPH.md) (2026-10-12),
which keeps the diagnosis (its §1) and the current state (its §1.3, §2).
Done: steps 0–4 implemented (§4). Step 5 (reverse) moved to
[REVERSE_SPEED.md](REVERSE_SPEED.md) §3: it needs that plan's model first.

**Goal:** remove the known limits of the preview audio and the memory of
the export without a generic node graph: the existing `MixSnapshot` stays
the one form the mix is evaluated in, and gains nesting, a streaming
offline render and content-keyed analyses.

---

## 1. Why a second plan

AUDIO_GRAPH puts the weight on a generic `Node` trait with demand
propagation, hashes and materialization. Looking at what each problem
actually needs:

| Problem | What solves it | Needs a generic graph? |
|---|---|---|
| Compound that normalizes plays silent over budget (AUDIO_GRAPH §1.3, limit 1) | the nested normalization measured like any other (§3.3) | no |
| Track/master normalization never measured over budget (limit 2) | an analysis that reads its input in streaming, not from the preview store | no |
| An edit inside a compound drops all its blocks (generation `content_hash`) | no compound blocks at all (§3.1) | no |
| Levels re-measured after eviction (`LevelAnalysis::key` from buffer addresses) | a content key | no |
| **Export memory** (not listed in AUDIO_GRAPH) | mixing in chunks, decoding by seeking (§3.2) | no |
| Reverse, pitch-corrected speed | per-clip read mapping, or a materialized input | no: two cases of the clip |

`MixSnapshot` already is a graph with a fixed topology: clips → track
insert chain → master. The cases that change what a clip reads are three
(speed, pitch-corrected speed, reverse), all properties of the clip. A
`match` in `collect_clips` covers them; a trait pays off only when node
kinds are many and keep growing.

### 1.1 Export memory today

`mix_audio_track` (`vv-session/src/export.rs`) decodes every window the
mix reaches into RAM at once (2 ch f32), mixes every compound whole, then
mixes the whole range into one `Vec`, and the encoder gets no audio until
that `Vec` is complete. On the reference project of AUDIO_GRAPH §1.1 the
output buffer alone is 29 min × 48 kHz × 2 ch × 4 B ≈ 670 MB; the decoded
windows of three streams are on the order of 2 GB more (estimate, to be
measured at step 1).

### 1.2 Decisions this plan rests on

- **Compounds are mixed live**, never cached as blocks. Deeply nested
  compounds are a video use case (complex compositions); several levels
  of nesting each with audio tracks and effects are not expected, so the
  callback cost of nested strips is not a concern worth a cache layer.
- **Normalization is mostly on the master**; a normalization inside a
  compound is rarer but must work. The master case drives the design of
  analyses (§3.3).
- **Exactness over approximation**: stateful effects (EQ, compressor)
  run sequentially wherever the audio is heard or exported, with no
  preroll approximation at block edges. It also saves memory: no second
  copy of the audio as compound blocks.
- **Third-party effects (plugins) will come.** They keep state, report a
  latency instead of asking for future input, may not be deterministic,
  and can have tails far longer than any preroll (reverbs). They only work
  run in order, which is what this plan does everywhere; rendering blocks
  out of order with a preroll (AUDIO_GRAPH §3.2) is where they would break.

---

## 2. What changes, in short

- Effects run through one `Effect` trait instead of a closed `match` on
  the kinds, the interface a plugin host will implement (§3.6).
- `MixClip` gets a nested snapshot as an alternative source to
  `SampleBlocks`; `MixState` becomes a tree. Compounds are mixed by the
  callback like the outer timeline.
- One **offline render** function: a timeline mixed start to end in
  chunks, with one `MixState` carried across chunks, its sources decoded
  by seeking and freed as the render moves on. Export, analyses and the
  pitch-corrected stretch all use it.
- **Analyses** are background jobs on the offline render, per segment,
  keyed by content, saved to disk. They never read the preview store, so
  the preview budget does not limit them.
- The preview store (`MixBufferCache`) keeps file blocks only, plus
  stretched (and later reversed-compound) clips and level values.

Removed: `CompoundCache`, `CompoundMix`, `CompoundBlock`,
`compound_blocks`, `COMPOUND_PREROLL_FRAMES`, `normalizes()` and the
"compound mixed whole" path, `cached_compound`/`store_compound`,
`WantedFrames`/`DecodedAudio`, `LevelAnalysis::key`.

---

## 3. Design

### 3.1 Compounds mixed live

```rust
/// Sketch.
pub enum ClipInput {
    Blocks(SampleBlocks),
    /// The compound's own timeline, in its own time: sample 0 is the
    /// nested timeline's 0.
    Nested(Arc<MixSnapshot>),
}
```

- `collect_clips` builds the nested snapshot recursively (it already
  recurses for `compound_audio`), threading `placement` so that the file
  blocks inside still know when they play in the outermost timeline
  (the preview store keeps prioritizing by distance to the playhead).
- `add_clip` on a `Nested` input renders the nested snapshot over the
  frames the clip reads into a scratch buffer, then applies the clip's
  gain and fades as today. Speed ≠ 1 (varispeed) renders
  `[floor(pos), ceil(pos_end) + 1)` and interpolates as `add_resampled`
  does; the frame shared by two consecutive calls is kept from the
  previous call, not rendered twice, so the nested state stays
  sequential.
- `MixState` gets one child state per nested use, preallocated with its
  scratch buffers in `MixState::new` (the callback does not allocate).
  The same compound used twice is two uses with two states: its effects
  run once per use, as they would on two real tracks. `continue_from`
  matches nested states by clip id, unique in the project, so an edit
  elsewhere, a move to another track or a track inserted above does not
  restart a compound's compressor; `MixClip` gains the clip id.
- Normalizations inside the compound resolve like those of the outer
  timeline (§3.3): the nested snapshot carries their gains as `Insert`s.
- Cycles: `MAX_COMPOUND_DEPTH` stays; past it the clip is silent, as
  today.
- Scrub, fast forward (`StretchedWindow`), meters and spectrum taps call
  `mix_range` on the outer snapshot and need nothing new; meters inside a
  compound are not shown (they are not today either).

Cost in the callback: the nested tracks with their inserts, only while
the compound is under the playhead. Measured on the reference project at
step 2, not ahead of it (see §1.2).

### 3.2 Offline render

```rust
/// Sketch. `out` gets chunks of interleaved frames in order.
pub fn render_offline(
    project: &Project,
    timeline: &Timeline,
    range: Range<u64>,
    stop_before: Option<InsertPos>,   // analyses: the chain up to the effect measured
    sink: impl FnMut(&[f32]) -> ControlFlow<()>,
) -> Result<(), RenderError>;
```

- Chunks of a few seconds. For each chunk a snapshot from
  `from_timeline_range` with a **streaming source**: one decoder per
  `(file, stream)` read, opened by seeking (`decode_audio_streams_from`)
  and carried on sequentially while the next chunk follows on, blocks
  freed once the render is past them. Resampled files, which cannot seek
  without a phase offset, are read from their start once per render
  (as `take_batch` does in the preview).
- One `MixState` across chunks: consecutive snapshots have the same
  tracks and inserts, so `continue_from` keeps every effect's memory and
  the result equals a single `mix_range` over the whole range. Tested as
  such.
- Memory: the chunk, plus the decoded blocks the open windows hold; not
  proportional to the timeline length.
- **Streaming source** (`vv-session/src/audio_render.rs`): a *reader* is
  one decode of a file (`decode_audio_streams_from`, the streams wanted
  near that position) on its own thread, its callback blocked on a bounded
  channel: a pull decoder with backpressure, without changing `vv-media`.
  Each chunk asks for its windows; a window of a stream a reader already
  holds or reaches shortly ahead reads from it, otherwise a new reader
  seeks. Readers keep what they decoded in 8192-frame blocks that the
  windows share (a coarser grid made every short window of an edited
  timeline hold up to two big blocks); after each chunk they drop the
  blocks behind the earliest frame still read, and close if nothing read
  them. On the last collect each file's readers close as soon as its
  windows are read: dozens of open decoders cost ~0.6 GB on the reference
  project.
- Until steps 2–4, at step 1: a compound clip is still mixed whole from
  whole windows of its sources (kept until the render no longer reads
  it), a pitch-corrected clip is stretched whole once, and a timeline that
  normalizes is collected once over the whole range (its levels need all
  of it) and only mixed and handed out in chunks.
- Export: `AudioInterleaver` receives chunks from the mix thread through
  a bounded channel instead of waiting for the whole mix, so audio and
  video advance together.

### 3.3 Analyses

A normalization needs one value from its input: the strip's signal up to
the effect before it. Today that input is mixed from the preview store and
only once all of it is there.

- **Job**: the level worker runs `render_offline` on the timeline with
  `stop_before` at the normalization, feeding `LevelMeter`. Constant
  memory, independent of the preview budget; it competes with the
  preview's decode worker for CPU and disk, not for the store.
- **Segments**: the measured range is split at the edit points of the
  strip's input (where the set of clips playing changes; a compound is
  one clip of its parent). Each segment is measured on its own and keyed
  by its content **in segment-local time**: the clips in it with their
  source window, speed, gain keyframes and fades relative to the segment
  start, the content key of their source (`MediaItem::content_hash` +
  stream, or the nested timeline's key for a compound), the track strips
  and the inserts before the measurement. A ripple delete shifts the
  segments after the cut without changing their keys: only the segments
  the edit touches are measured again.
- **Merging**: sample and true peak merge by `max`; integrated loudness
  keeps, per segment, the energies of its 100 ms gating segments
  (`Loudness` already stores exactly that) and concatenates them before
  gating, so the merge is exact when segment boundaries fall on the 100 ms
  grid and off by one gating step otherwise. Per-clip normalization
  (`SetLevel::Independent`, loudness per clip) is the same mechanism with
  one segment per clip.
- **State across segments**: with a compressor or EQ before the
  normalization, a segment is rendered with a preroll from the audio
  before it, and its key includes the content key of the segment before
  (not that segment's own predecessor), so an edit invalidates at most
  one extra segment. The measurement is then exact to within the
  compressor's convergence over the preroll; for a value that sets one
  gain, tested to 0.1 dB.
- **Disk cache**: segment results in `$XDG_CACHE_HOME/venturi/levels/`,
  keyed like the waveforms. The key uses a hasher stable across builds,
  not `DefaultHasher`. Reopening a project plays the master normalized
  from the first second.
- **While measuring**: the last value of the same slot keeps playing,
  as today; on a first measurement with nothing cached, unity gain.
- Nested normalizations are measured on the nested timeline the same way;
  their result enters the nested snapshot as an `Insert::Gain` or
  `ClipGains`.

### 3.4 Preview store after this plan

`MixBufferCache` keeps: file blocks (decoded by seeking, by distance to
the playhead, within the budget, as today), stretched clips, level values
(in memory, backed by the disk cache). The `Owner::Compound` case, the
compound mixing worker and the compound budget accounting go away.

### 3.5 Pitch-corrected speed and reverse

- **Pitch-corrected speed**: the stretch job's input becomes
  `render_offline` over the clip's source window (a file clip's window,
  or a compound's nested timeline), streamed into rubberband; the output
  is materialized as today. Export uses the same.
- **Reverse, file clip**: a read mapping, `[axis - b, axis - a)` read
  backwards in `add_clip`; nothing materialized, the store asks for the
  mirrored blocks.
- **Reverse, compound clip**: the nested mix must run forward for its
  effects, so it is materialized: `render_offline` of the window, stored
  reversed, like a stretched clip.

---

### 3.6 Effects, plugins and routing

The plugin host as built is in [VST3_HOST.md](VST3_HOST.md).

```rust
/// Sketch of step 0.
pub trait Effect: Send {
    /// In place, interleaved, a mix block at a time; no allocations.
    fn process(&mut self, samples: &mut [f32]) -> Option<BandActivity>;
    /// Silence in the filters and envelopes: the mix jumped.
    fn reset(&mut self);
    /// Carries on from `previous` (same kind, new parameters).
    fn take_memory_from(&mut self, previous: &dyn Effect);
    fn as_any(&self) -> &dyn std::any::Any;
}
```

- `Insert` stays the *parameters* of an effect (data: compared, hashed,
  cloned into snapshots); `Insert::effect` builds its `Box<dyn Effect>`,
  held by `MixState` and built on the UI thread with the snapshot.
- A plugin host adds `latency()` (compensated with a delay on the other
  tracks of the same sum) and `tail()` (how long it rings after its input
  stops, for the end of an export and for the analyses' preroll). Added
  with the first effect that has them, not before.
- **Routing** (buses, sends, side-chain for ducking) is a topology the
  snapshot does not express today (clips → track → master). When it comes
  it is a small DAG of strips evaluated in order in the same sequential
  mix, not a demand graph.
- **CPU headroom**: if live effects (nested ones included) get too heavy
  for the callback, the mix moves to a thread rendering a little ahead of
  the playhead into a ring buffer the callback reads ("anticipative" FX),
  which also allows independent tracks on several cores. Exact, and
  compatible with everything above.

## 4. Plan

Each step ships on its own, verified by unit tests, sample comparison with
the code it replaces and the runtime measurement of AUDIO_GRAPH §9.

0. **`Effect` trait** (§3.6) in place of `Processor`. No behaviour
   change: the mixer tests pass unchanged. *Done.*
1. **Offline render and export in chunks.** `render_offline` with the
   streaming source; `mix_audio_track` and `AudioInterleaver` on it;
   `WantedFrames`/`DecodedAudio` removed. Compounds still mixed whole
   inside it at this step. Acceptance: exports bit-identical to the
   current ones (the state is carried, so no tolerance); export peak
   memory on the reference project measured before and after.
   *Done.* The tests compare the chunked render with one `mix_range` over
   whole decoded files: equal sample for sample, compressor and EQ state
   across chunks included. Audio of the whole master timeline of the
   reference project, peak RSS, release: 2.19 → 1.54 GB as it is (its
   master normalizes, so it is collected whole until step 3), 2.19 →
   0.55 GB with that normalization off. 30–60% slower (6–8 s for 29
   minutes), negligible next to the video encode it runs alongside.
2. **Compounds live.** `ClipInput::Nested`, `MixState` tree, varispeed on
   nested input. Removes the compound block cache and the whole-compound
   path from preview and export. Acceptance: mix of every mixer test
   project sample-identical to a sequential `mix_range` of the flattened
   timeline; reference project preview memory below a5d3c48 (the
   compound blocks are gone); callback time measured with the chapter
   compounds playing.
   *Done.* `ClipInput::Nested`; `MixState` holds one `NestedState` per
   compound clip (its timeline's state and the last frames rendered, in a
   buffer sized for one mixer block so the callback never allocates).
   A pitch-corrected compound clip still stretches its whole mixdown
   (`compound_mixdown`, `cached_compound`) until step 4. The offline
   render caches levels by key, or a nested normalization was measured
   again in every chunk its compound played in. Tests: a live compound
   equals its mixdown (gain keyframes, fades, varispeed both ways); a mix
   split into many calls equals one; a new snapshot carries the nested
   compressor on; a chunked render of compounds equals one mix. Reference
   project, audio of the whole master timeline, peak RSS: 1.54 → 0.89 GB
   as it is, 0.55 → 0.33 GB with its master normalization off. The mix
   alone, in 1024-frame calls as the callback makes them, takes 1.1–1.4 s
   for the 29 minutes (~0.07% of real time). Not yet measured: the preview
   in the running app (AUDIO_GRAPH §9).
3. **Analyses on the offline render.** Level job on `render_offline`,
   segments, content keys, disk cache; `LevelAnalysis::key` and the
   `Readiness::Complete` gate on levels removed. Acceptance: levels equal
   today's within float tolerance (0.1 dB with a compressor before the
   normalization); a master normalization measured on the reference
   project at a 256 MB budget (today never); a ripple delete re-measures
   only the touched segments; reopening reads levels from disk.
   *Done*, with these choices:
   - `LevelAnalysis` (`vv-audio/src/levels.rs`) describes the measure
     (project, timeline, `Measured { channel, insert, clip }`, mode) and
     its segments, built from the model, not from decoded audio: no
     `Readiness` gate, no need for the timeline to be collected whole, so
     a normalizing timeline renders in chunks like any other.
     `MixSnapshot::measured` collects a timeline stopped before the
     measured effect (the `stop_before` of §3.2).
   - Segment keys: FNV-1a (`StableHasher`, pinned by a test), the clips
     playing with their position relative to the segment, their source
     (`content_hash` + stream, or the nested timeline's key) and every
     field that shapes them, the track's resolved chain and bus (master
     measures), the chain before the measure. With an effect that keeps a
     state, or a compound clip, before the measure: a 2 s preroll and the
     previous segment's content in the key.
   - Each segment is rendered with 0.25 s of warm-up (loudness filters,
     true-peak interpolation) and a few frames after; that context is not
     in the key, an approximation at the edges only. A part's loudness
     tail merges as one more 100 ms segment at its own power. Tested: merged
     parts within 0.1 dB of the whole, sample peak exact.
   - Preview: misses queue an owned analysis; `send_level_jobs` copies the
     project once per pass that has any, the level worker measures with
     the disk cache.
   Reference project, whole master timeline audio: peak RSS 0.89 → 0.13
   GB. Cold (no cached level): 20 s, 101 segments (the master's and the
   nested normalization of chapter "09-cc"); warm: 6 s, none; a clip's gain
   edited inside a chapter: 2 segments measured again.
   Then: neighbouring segments rendered in one run with a meter each (the
   93 segments of "09-cc": 6.4 → 0.65 s), the runs split across threads
   (all cores for an export, half in the preview), segments being measured
   shared between threads so that a compound met by two of them is
   rendered once. Cold export audio: 21.7 → 7.9 s (about 2.5 s of it the
   measure, the rest the export's own render); 280 MB peak.
4. **Stretch on the offline render**, compounds included. *Done.* A
   `StretchRequest` describes the input (a file's window, or a compound's
   timeline at its depth of nesting) with a content key; the preview's
   worker and the export render it from the files in chunks into a
   streaming `Stretcher` (`stretch_samples` is its whole-input case). The
   preview no longer holds a pitch-corrected clip's whole source window,
   and the whole-mixdown path of compounds is gone (`compound_audio`,
   `compound_mixdown`, `cached_compound`, `store_compound`). Offline
   renders, levels and stretches alike, run at the project format; a
   stretch is remixed to the device's channels. Tested: a chunked
   stretch of a file clip and of a compound with a compressor inside
   equals the whole input stretched at once, sample for sample.
5. **Reverse**: read mapping for file clips, materialized for compounds.
   Acceptance: a reversed clip equals the reversed samples of the forward
   clip, in preview and export. *Moved* to
   [REVERSE_SPEED.md](REVERSE_SPEED.md) §3 and its step 4: there is no
   reverse in the model yet (its step 1), and audio alone would have
   nothing to play.

---

## 5. Risks and open points

- **Callback cost** of nested strips: assumed small (§1.2); if a project
  proves otherwise, the fallback is materializing that compound with
  state checkpoints at block boundaries (exact when rendered in order),
  which this plan does not build up front.
- **Nested varispeed with stateful effects inside**: the nested render
  runs at the compound's own rate, so its effects see the same signal as
  in export; the shared interpolation frame between calls is the detail
  to get right.
- **Analysis cost**: a first master measurement reads every source once
  (4.7 GB on the reference project). Measure how long it takes; the disk
  cache makes it a one-time cost per content.
- **Segment keys** must cover everything before the measured effect,
  mute/solo included (`audible_tracks`); a missed field means a stale
  level. Tests enumerate the fields that must change the key.

## 6. Comparison with AUDIO_GRAPH

| | AUDIO_GRAPH | AUDIO_LIVE_MIX |
|---|---|---|
| Evaluated form | graph (or compiled to snapshot) | `MixSnapshot`, nested |
| Compounds | materialized blocks, preroll at edges | live, exact |
| Memory of compounds | sources + their mixed blocks | sources only |
| Export | step 6 | step 1 |
| Analyses | `Whole` stream nodes, hash per node | offline render per segment, local-time keys, disk cache |
| New feature | a new node | a case of the clip, or a materialized input |
| Size | rewrite of mixer, store, export (~3–4k lines touched) | mostly removal in store and mixer; new offline render and analysis job |
| Video reuse later | the model would fit | none |
| Plugins | live part sequential; inside compounds, blocks out of order with preroll | sequential everywhere |
