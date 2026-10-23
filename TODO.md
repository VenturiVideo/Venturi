# TODO

Open items from the review of the `audio-blocks` branch (2026-10-12).
Fixed: items 1–4 of that review (level-measure deadlock, decode priority,
compound fade-out, stale on-disk levels), timestamp gaps, seeking in
resampled files, stale stretch jobs, the shutdown wait, the fast
forward's thread per frame, an export that kept encoding after its audio
mix failed, the per-run test fixture dirs and the unclamped audio budget.

## Audio: correctness

- **Timestamps that overlap** (jump backwards): both readers keep the
  samples written first, so a seek landing inside the overlap keeps the
  other copy. Gaps are exact.
- **Timestamps that drift** from the sample count (no clean gap): a decode
  from the start jumps by 5 ms each time the drift reaches it, a seek
  places by timestamp: the two can differ by up to 5 ms.
- **Cached levels in memory.** A file overwritten during the session keeps
  its level until restart (the disk cache checks the files, the in-memory
  one does not). Same as the other derived caches, keyed by the
  import-time `content_hash`.
- **Pitch-corrected compound containing itself.** The stretch path drops
  `Readiness::Cyclic`: up to `MAX_COMPOUND_DEPTH` layered copies and one
  stretch job per depth instead of silence. Only from a hand-edited
  project: `Project::would_create_a_cycle` prevents building one.
- **`MixSnapshot::is_ready`** is true while a pitch-corrected clip's stretch
  is pending (the clip is skipped, not counted).
- **Failures cached forever.** A decode error marks the stream `missing`
  until it leaves the timeline; a failed level measure is `Ready(0.0)`, i.e. unity gain, with only an
  `eprintln`. A `recv` error in the export reader reads as end of file.
- **Weak seek anchor**: when none of the first `MAX_ORIGIN_PACKETS` packets
  gives two frames, the fallback anchor ignores priming and pre-skip (off by
  up to a frame). Only logged.

## Audio: performance

- **Fast forward windows are not cancelled**: one superseded by an edit
  or a seek still mixes and stretches its 8 s to the end before the next
  starts (one at a time since the drag fix).
- **Previewing a media** from the pool drops every decoded block of the
  timeline (`begin(true)` clears `asked`), despite the `begin_media_pass`
  doc.
- **Export reader with several streams** buffers the other streams to EOF
  when one is much shorter than its clip window (`Reader::read`).
- **On-disk level cache** (`$XDG_CACHE_HOME/venturi/levels`) is never
  pruned; the preview's in-memory parts grow for the session.

## Audio: budget

- [x] `rebalance` accounts for allocated sample bytes in cached blocks and
  stretched audio, evicting the furthest buffers across both caches first.

## Audio: cleanup

- Two block decoders (`mix_buffers::decode_blocks`, export
  `Reader`/`KeptStream`).
- Cancel-and-forget code copied in `sweep_unused` and `rebalance`.
- The seek's first read repeats the packet-to-slot lookup of
  `decode_streams`.

## Audio: tests

- Seek tests have no multi-stream or VBR MP3 case.
- `stretch_bench_window` is timing-based, so `#[ignore]`d: run it by hand
  with `--ignored` after touching the stretcher.

## Audio: behaviour to keep in mind

- A track's effect state passes to the next snapshot by track index
  (already on master): inserting, deleting or reordering a track while
  playing hands each track below the state of the one that was at its
  index, for a moment. Tracks have no stable id to match them by.
- After a seek into a compound clip its nested compressor starts from
  silence (master ran it from the compound's start).
- Split level-measure runs restart effect state with a 2 s preroll: the
  normalization gain can vary by ~0.1 dB with cache state and thread count.
- A cached segment's true peak at its edge was computed against the
  neighbour of that time, which is not in its key.
