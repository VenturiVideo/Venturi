use super::*;

#[test]
fn block_assembly_keeps_gaps_overlaps_and_trimming_independent_of_block_size() {
    for size in [2, 4, 16] {
        let mut filling = Vec::new();
        let mut cursor = 0;
        let mut blocks = Vec::new();
        for (at, samples) in [
            (2, &[1.0, 2.0, 3.0, 4.0][..]),
            (4, &[8.0, 9.0, 5.0, 6.0][..]),
            (10, &[7.0, 8.0, 9.0, 10.0][..]),
        ] {
            assert!(append_to_blocks(
                &mut filling,
                &mut cursor,
                at,
                samples,
                12,
                1,
                size,
                |index, block| {
                    blocks.push((index, block));
                    true
                }
            ));
        }
        let mut result: Vec<f32> = blocks
            .iter()
            .flat_map(|(_, block)| block.iter().copied())
            .collect();
        result.extend_from_slice(&filling);
        assert_eq!(cursor, 12);
        assert_eq!(
            result,
            [0.0, 0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 0.0, 0.0, 7.0, 8.0]
        );
        assert!(
            blocks
                .iter()
                .enumerate()
                .all(|(i, (index, _))| i as u64 == *index)
        );
    }
}

#[test]
fn block_assembly_stops_when_the_consumer_is_gone() {
    let mut filling = Vec::new();
    let mut cursor = 4;
    let mut calls = 0;
    assert!(!append_to_blocks(
        &mut filling,
        &mut cursor,
        4,
        &[1.0; 16],
        20,
        2,
        4,
        |index, block| {
            assert_eq!(index, 1);
            assert_eq!(block.len(), 8);
            calls += 1;
            false
        }
    ));
    assert_eq!(calls, 1);
    assert_eq!(cursor, 8);
}

fn block(values: Range<u32>) -> Option<Arc<Vec<f32>>> {
    Some(Arc::new(
        values.flat_map(|v| [v as f32, -(v as f32)]).collect(),
    ))
}

/// Stereo, blocks of 4 frames from frame 8, the second one missing, ending
/// at frame 18.
fn with_hole() -> SampleBlocks {
    SampleBlocks::new(8, 4, 2, vec![block(8..12), None, block(16..18)], 18)
}

#[test]
fn a_run_goes_to_the_end_of_its_block() {
    let blocks = with_hole();
    let Run::Samples(samples) = blocks.run(9) else {
        panic!("frame 9 is decoded");
    };
    assert_eq!(samples, [9.0, -9.0, 10.0, -10.0, 11.0, -11.0]);
    assert!(matches!(blocks.run(13), Run::Silence(3)));
    assert!(matches!(blocks.run(17), Run::Samples([17.0, -17.0])));
    assert!(matches!(blocks.run(18), Run::End));
    assert!(matches!(blocks.run(7), Run::End));
}

#[test]
fn a_frame_is_none_in_a_hole_and_outside() {
    let blocks = with_hole();
    assert_eq!(blocks.frame(16), Some(&[16.0, -16.0][..]));
    assert_eq!(blocks.frame(12), None);
    assert_eq!(blocks.frame(30), None);
}

#[test]
fn to_vec_fills_holes_and_the_tail_with_silence() {
    let samples = with_hole().to_vec(10..20);
    let expected: Vec<f32> = [10.0, 11.0, 0.0, 0.0, 0.0, 0.0, 16.0, 17.0]
        .iter()
        .flat_map(|&v: &f32| [v, if v == 0.0 { 0.0 } else { -v }])
        .collect();
    assert_eq!(samples, expected, "stops at `end`, frame 18");
}

#[test]
fn contiguous_is_one_block_from_its_start() {
    let blocks = SampleBlocks::contiguous(Arc::new(vec![1.0, 2.0, 3.0]), 5, 1);
    assert_eq!((blocks.start(), blocks.end()), (5, 8));
    assert_eq!(blocks.frame(7), Some(&[3.0][..]));
}

#[test]
fn holds_tells_whether_the_blocks_of_a_range_are_there() {
    let blocks = with_hole();
    assert!(blocks.holds(8..12));
    assert!(!blocks.holds(10..14));
    assert!(blocks.holds(16..30), "past the end nothing is missing");
}

/// No producer cuts a block mid-frame today; `to_vec` used to loop forever
/// on one.
#[test]
fn a_block_cut_mid_frame_reads_its_last_frame_as_silence() {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let cut = SampleBlocks::new(0, 2, 2, vec![Some(Arc::new(vec![1.0, 2.0, 3.0]))], 2);
        let _ = tx.send(cut.to_vec(0..2));
    });
    let read = rx.recv_timeout(std::time::Duration::from_secs(5));
    assert_eq!(read, Ok(vec![1.0, 2.0, 0.0, 0.0]));
}
