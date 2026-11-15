use super::*;

#[test]
fn gaps_overlaps_and_window_edges_share_one_placement_rule() {
    let mut buffer = BlockBuffer::new(0, 2);
    buffer.append(2, &[1.0, 2.0, 3.0, 4.0], 6);
    buffer.append(3, &[9.0, 9.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0], 6);
    assert_eq!(buffer.end(), 6);
    assert_eq!(
        buffer.view(0..6).to_vec(0..6),
        vec![0.0, 0.0, 0.0, 0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]
    );
}

#[test]
fn streaming_drain_and_retained_views_produce_identical_blocks() {
    let mut streaming = BlockBuffer::new(0, 1);
    let mut retained = BlockBuffer::new(0, 1);
    let samples: Vec<_> = (0..2 * BLOCK_FRAMES + 7).map(|v| v as f32).collect();
    let mut emitted = Vec::new();
    for (i, chunk) in samples.chunks(997).enumerate() {
        streaming.append((i * 997) as u64, chunk, u64::MAX);
        retained.append((i * 997) as u64, chunk, u64::MAX);
        for (_, block) in streaming.take_complete() {
            emitted.extend_from_slice(&block);
        }
    }
    emitted.extend_from_slice(&streaming.take_tail().unwrap().1);
    assert_eq!(emitted, samples);
    let view = retained.view(0..samples.len() as u64);
    assert_eq!(view.to_vec(0..samples.len() as u64), samples);
    retained.drop_before(BLOCK_FRAMES);
    assert_eq!(retained.start(), BLOCK_FRAMES);
    assert_eq!(retained.held_samples(), BLOCK_FRAMES as usize + 7);
    assert_eq!(
        view.frame(0),
        Some(&[0.0][..]),
        "an outstanding view owns its blocks"
    );
}

#[test]
fn a_cancelled_decode_stops_at_the_first_delivered_chunk() {
    struct Directory(std::path::PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let path = std::env::temp_dir().join(format!(
        "vv-block-decoder-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&path).unwrap();
    let dir = Directory(path);
    let file = dir.0.join("source.wav");
    vv_media::test_support::ffmpeg(
        &["-f", "lavfi", "-i", "sine=sample_rate=48000:duration=2"],
        &file,
    );
    let mut chunks = 0;
    let formats = BlockDecoder::new(48000, 2)
        .decode(&file, &[0], 0, |slot, _, samples| {
            assert_eq!(slot, 0);
            assert_eq!(samples.len() % 2, 0);
            assert!(
                samples
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .all(|frame| frame[0] == frame[1])
            );
            chunks += 1;
            ControlFlow::Break(())
        })
        .unwrap();
    assert_eq!(chunks, 1);
    assert_eq!(formats.len(), 1);
    assert!(formats[0].is_some());
}
