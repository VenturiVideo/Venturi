use super::*;

/// YUV420 frame owned by the test (the planes of `YuvFrame` are
/// borrowed references): chroma dimensions computed as
/// `vv_media::FrameYuv420` would compute them, rounded up.
struct OwnedYuvFrame {
    width: u32,
    height: u32,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    chroma_width: u32,
    chroma_height: u32,
    matrix: ColorMatrix,
    full_range: bool,
}

impl OwnedYuvFrame {
    fn as_yuv_frame(&self) -> YuvFrame<'_> {
        YuvFrame {
            y: &self.y,
            width: self.width,
            height: self.height,
            chroma: YuvChroma::Planar {
                u: &self.u,
                v: &self.v,
            },
            chroma_width: self.chroma_width,
            chroma_height: self.chroma_height,
            matrix: self.matrix,
            full_range: self.full_range,
            alpha: OPAQUE,
        }
    }
}

/// Uniform frame: same Y/U/V on every pixel.
fn solid_frame(
    w: u32,
    h: u32,
    y: u8,
    u: u8,
    v: u8,
    matrix: ColorMatrix,
    full_range: bool,
) -> OwnedYuvFrame {
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    OwnedYuvFrame {
        width: w,
        height: h,
        y: vec![y; (w * h) as usize],
        u: vec![u; (cw * ch) as usize],
        v: vec![v; (cw * ch) as usize],
        chroma_width: cw,
        chroma_height: ch,
        matrix,
        full_range,
    }
}

/// 4x4 frame with four quadrants at different Y, neutral chroma
/// (U=V=128) and full range: with neutral chroma R=G=B=Y exactly
/// (see `yuv_to_rgb_reference`), useful to check *where* the
/// crop picks from by looking at the red channel alone, without the
/// color conversion adding another variable to the test.
fn quadrant_frame() -> OwnedYuvFrame {
    let w: u32 = 4;
    let h: u32 = 4;
    let mut y_plane = vec![0u8; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let level = match (x < w / 2, y < h / 2) {
                (true, true) => 40u8,    // top-left
                (false, true) => 100u8,  // top-right
                (true, false) => 160u8,  // bottom-left
                (false, false) => 220u8, // bottom-right
            };
            y_plane[(y * w + x) as usize] = level;
        }
    }
    let cw = w.div_ceil(2);
    let ch = h.div_ceil(2);
    OwnedYuvFrame {
        width: w,
        height: h,
        y: y_plane,
        u: vec![128; (cw * ch) as usize],
        v: vec![128; (cw * ch) as usize],
        chroma_width: cw,
        chroma_height: ch,
        matrix: ColorMatrix::Bt601,
        full_range: true,
    }
}

/// Even for a "neutral" chroma (128), 128/255 is not exactly
/// 0.5: a residue of a few levels in the 8 bits is expected
/// quantization of the YUV→RGB matrix (the same residue appears
/// in the f64 reference implementation, not just in the
/// f32 shader), not an error — hence a small tolerance instead
/// of an exact equality.
fn assert_close_rgba(got: [u8; 4], expected: [u8; 4]) {
    for i in 0..4 {
        assert!(
            (got[i] as i16 - expected[i] as i16).abs() <= 2,
            "got={got:?} expected={expected:?}"
        );
    }
}

/// Reference implementation (CPU, f64) of the same formula
/// used in the shader (`transform.wgsl`, `yuv_to_rgb`): it serves to
/// check that the computation on the GPU (f32) is effectively
/// that formula, not a silently different approximation
/// (plans/REFACTOR_PIPELINE.md §5, frame accuracy is non-negotiable).
fn yuv_to_rgb_reference(y: u8, u: u8, v: u8, matrix: ColorMatrix, full_range: bool) -> [u8; 3] {
    let (y_n, u_n, v_n) = if full_range {
        (
            y as f64 / 255.0,
            u as f64 / 255.0 - 0.5,
            v as f64 / 255.0 - 0.5,
        )
    } else {
        (
            (y as f64 - 16.0) / 219.0,
            (u as f64 - 128.0) / 224.0,
            (v as f64 - 128.0) / 224.0,
        )
    };
    let (kr, kb) = match matrix {
        ColorMatrix::Bt601 => (0.299, 0.114),
        ColorMatrix::Bt709 => (0.2126, 0.0722),
        ColorMatrix::Bt2020 => (0.2627, 0.0593),
    };
    let kg = 1.0 - kr - kb;
    let r = y_n + 2.0 * (1.0 - kr) * v_n;
    let b = y_n + 2.0 * (1.0 - kb) * u_n;
    let g = y_n - (2.0 * kr * (1.0 - kr) / kg) * v_n - (2.0 * kb * (1.0 - kb) / kg) * u_n;
    [r, g, b].map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8)
}

#[test]
fn identity_transform_passes_through_solid_color() {
    let compositor = Compositor::new_headless();
    let input = solid_frame(8, 8, 128, 128, 128, ColorMatrix::Bt601, true);
    let out = compositor.render_frame(
        &input.as_yuv_frame(),
        &Transform::default(),
        OutputFrame::exact(8, 8),
    );

    assert_eq!(out.len(), 8 * 8 * 4);
    // Neutral chroma (128) and full range: Y=128 maps to R=G=B=128 within
    // a quantization residue (see assert_close_rgba).
    for px in out.as_chunks::<4>().0 {
        assert_close_rgba(*px, [128, 128, 128, 255]);
    }
}

/// Checks the conversion formula itself (not just that "a
/// color gets through"): for every matrix/range, the GPU output must
/// match the same formula computed on the CPU, within a
/// small f32-vs-f64 rounding difference.
#[test]
fn yuv_to_rgb_matches_the_reference_formula_across_matrices_and_ranges() {
    let compositor = Compositor::new_headless();
    let cases = [
        (ColorMatrix::Bt601, false),
        (ColorMatrix::Bt601, true),
        (ColorMatrix::Bt709, false),
        (ColorMatrix::Bt709, true),
        (ColorMatrix::Bt2020, false),
        (ColorMatrix::Bt2020, true),
    ];
    // Non-degenerate Y/U/V (not all at half scale): really exercises
    // the matrix instead of reducing to a neutral grey.
    let (y, u, v) = (100u8, 90u8, 180u8);

    for (matrix, full_range) in cases {
        let input = solid_frame(2, 2, y, u, v, matrix, full_range);
        let out = compositor.render_frame(
            &input.as_yuv_frame(),
            &Transform::default(),
            OutputFrame::exact(2, 2),
        );
        let expected = yuv_to_rgb_reference(y, u, v, matrix, full_range);
        let got = &out[0..3];
        for i in 0..3 {
            assert!(
                (got[i] as i16 - expected[i] as i16).abs() <= 2,
                "matrix={matrix:?} full_range={full_range}: got={got:?} expected={expected:?}"
            );
        }
    }
}

/// The crop just cuts: what is left keeps falling where it was
/// in the frame, it is not recentered nor enlarged to fill it (where it was
/// cut the layer below shows, here the black of the clear).
#[test]
fn crop_cuts_without_moving_what_is_left() {
    let compositor = Compositor::new_headless();
    let input = quadrant_frame();

    let transform = Transform {
        crop: [0.0, 0.0, 2.0, 2.0], // away with the right half and the bottom half (2 px of 4)
        zoom: [1.0, 1.0],
        position: [0.0, 0.0],
        ..Transform::default()
    };
    let out = compositor.render_frame(
        &input.as_yuv_frame(),
        &transform,
        OutputFrame::exact(16, 16),
    );
    let pixel = |x: usize, y: usize| {
        let i = (y * 16 + x) * 4;
        [out[i], out[i + 1], out[i + 2], out[i + 3]]
    };

    // Neutral chroma: R matches the quadrant's Y exactly (40).
    assert_close_rgba(pixel(4, 4), [40, 40, 40, 255]);
    assert_eq!(pixel(12, 4), [0, 0, 0, 255], "top-right: cut");
    assert_eq!(pixel(4, 12), [0, 0, 0, 255], "bottom-left: cut");
    assert_eq!(pixel(12, 12), [0, 0, 0, 255], "bottom-right: cut");
}

#[test]
fn crop_to_bottom_right_quadrant_leaves_it_in_the_bottom_right() {
    let compositor = Compositor::new_headless();
    let input = quadrant_frame();

    let transform = Transform {
        crop: [2.0, 2.0, 0.0, 0.0],
        zoom: [1.0, 1.0],
        position: [0.0, 0.0],
        ..Transform::default()
    };
    let out = compositor.render_frame(
        &input.as_yuv_frame(),
        &transform,
        OutputFrame::exact(16, 16),
    );
    let pixel = |x: usize, y: usize| {
        let i = (y * 16 + x) * 4;
        [out[i], out[i + 1], out[i + 2], out[i + 3]]
    };

    assert_close_rgba(pixel(12, 12), [220, 220, 220, 255]);
    assert_eq!(pixel(4, 4), [0, 0, 0, 255], "top-left: cut");
}

#[test]
fn taller_source_in_wider_output_gets_black_side_bars() {
    let compositor = Compositor::new_headless();
    let input = solid_frame(8, 16, 235, 128, 128, ColorMatrix::Bt709, false);
    let out = compositor.render_frame(
        &input.as_yuv_frame(),
        &Transform::default(),
        OutputFrame::exact(32, 16),
    );

    let pixel = |x: usize, y: usize| {
        let i = (y * 32 + x) * 4;
        [out[i], out[i + 1], out[i + 2], out[i + 3]]
    };
    // 8:16 into 32:16 -> content 8 px wide, centered: columns 12..20.
    assert_eq!(pixel(0, 8), [0, 0, 0, 255]);
    assert_eq!(pixel(31, 8), [0, 0, 0, 255]);
    assert_close_rgba(pixel(16, 8), [255, 255, 255, 255]);
}

#[test]
fn matching_aspect_ratio_leaves_no_bars() {
    let compositor = Compositor::new_headless();
    let input = solid_frame(8, 16, 235, 128, 128, ColorMatrix::Bt709, false);
    let out = compositor.render_frame(
        &input.as_yuv_frame(),
        &Transform::default(),
        OutputFrame::exact(16, 32),
    );

    for corner in [0usize, 15, 16 * 31, 16 * 32 - 1] {
        let i = corner * 4;
        assert_close_rgba(
            [out[i], out[i + 1], out[i + 2], out[i + 3]],
            [255, 255, 255, 255],
        );
    }
}

/// The zoom enlarges the clip *relative to the output frame*: a 9:16
/// zoomed enough comes to cover a whole 16:9 frame, bars
/// included (case reported by the user).
#[test]
fn zoom_enlarges_the_clip_until_it_covers_the_whole_output_frame() {
    let compositor = Compositor::new_headless();
    let input = solid_frame(8, 16, 235, 128, 128, ColorMatrix::Bt709, false);

    let bars = compositor.render_frame(
        &input.as_yuv_frame(),
        &Transform::default(),
        OutputFrame::exact(32, 16),
    );
    assert_eq!(&bars[0..4], &[0, 0, 0, 255], "at zoom 1 the bars remain");

    let transform = Transform {
        crop: [0.0; 4],
        zoom: [5.0, 5.0], // > 32/16 : 8/16, i.e. the factor covering the width
        position: [0.0, 0.0],
        ..Transform::default()
    };
    let zoomed = compositor.render_frame(
        &input.as_yuv_frame(),
        &transform,
        OutputFrame::exact(32, 16),
    );
    for px in zoomed.as_chunks::<4>().0 {
        assert_close_rgba(*px, [255, 255, 255, 255]);
    }
}

/// The position moves the clip *inside* the frame, not the content
/// inside the clip.
#[test]
fn position_moves_the_clip_inside_the_output_frame() {
    let compositor = Compositor::new_headless();
    let input = solid_frame(4, 4, 235, 128, 128, ColorMatrix::Bt709, false);

    let transform = Transform {
        crop: [0.0; 4],
        zoom: [1.0, 1.0],
        position: [8.0, 0.0], // half a frame to the right (output 16x16)
        ..Transform::default()
    };
    let out = compositor.render_frame(
        &input.as_yuv_frame(),
        &transform,
        OutputFrame::exact(16, 16),
    );
    let pixel = |x: usize, y: usize| {
        let i = (y * 16 + x) * 4;
        [out[i], out[i + 1], out[i + 2], out[i + 3]]
    };

    assert_eq!(pixel(2, 8), [0, 0, 0, 255], "left half: clip moved out");
    assert_close_rgba(pixel(14, 8), [255, 255, 255, 255]);
}

/// 90° rotation: the top-left quadrant ends up at the top
/// right (clockwise rotation), and on a square output it is not deformed.
#[test]
fn rotation_turns_the_clip_clockwise_around_its_center() {
    let compositor = Compositor::new_headless();
    let input = quadrant_frame();
    let transform = Transform {
        rotation: 90.0,
        ..Transform::default()
    };
    let out = compositor.render_frame(
        &input.as_yuv_frame(),
        &transform,
        OutputFrame::exact(16, 16),
    );
    let pixel = |x: usize, y: usize| {
        let i = (y * 16 + x) * 4;
        [out[i], out[i + 1], out[i + 2], out[i + 3]]
    };

    assert_close_rgba(pixel(12, 4), [40, 40, 40, 255]);
    assert_close_rgba(pixel(4, 4), [160, 160, 160, 255]);
}

/// The anchor point moves the zoom pivot: zooming around
/// the top-left corner of the clip, that corner stays put.
#[test]
fn zoom_scales_around_the_anchor_point() {
    let compositor = Compositor::new_headless();
    let input = quadrant_frame();
    let transform = Transform {
        zoom: [2.0, 2.0],
        anchor: [-8.0, 8.0], // top-left corner of the clip (output 16x16)
        ..Transform::default()
    };
    let out = compositor.render_frame(
        &input.as_yuv_frame(),
        &transform,
        OutputFrame::exact(16, 16),
    );
    let pixel = |x: usize, y: usize| {
        let i = (y * 16 + x) * 4;
        [out[i], out[i + 1], out[i + 2], out[i + 3]]
    };

    // With the pivot on the top-left corner of the clip, the top-left
    // quadrant widens until it covers the whole frame by itself:
    // with the pivot at the center, at (10, 10) one would instead see the
    // bottom-right quadrant.
    assert_close_rgba(pixel(2, 2), [40, 40, 40, 255]);
    assert_close_rgba(pixel(10, 10), [40, 40, 40, 255]);
}

/// Position and anchor are in *timeline* pixels: the preview composes
/// at reduced resolution (proxy), but a clip moved by half a frame
/// stays moved by half a frame.
#[test]
fn position_is_in_timeline_pixels_whatever_the_output_resolution() {
    let compositor = Compositor::new_headless();
    let input = solid_frame(4, 4, 235, 128, 128, ColorMatrix::Bt709, false);
    let transform = Transform {
        position: [960.0, 0.0], // half a frame on a 1920x1080 timeline
        ..Transform::default()
    };
    // Output at 1/120 of the timeline: the clip must still start from
    // half the frame.
    let out = compositor.render_frame(
        &input.as_yuv_frame(),
        &transform,
        OutputFrame::scaled(16, 9, (1920, 1080)),
    );
    let pixel = |x: usize, y: usize| {
        let i = (y * 16 + x) * 4;
        [out[i], out[i + 1], out[i + 2], out[i + 3]]
    };

    assert_eq!(pixel(2, 4), [0, 0, 0, 255], "left half: empty");
    assert_close_rgba(pixel(13, 4), [255, 255, 255, 255]);
}

/// The crop is in media pixels at its native resolution: on a
/// proxy (smaller decoded frame) it cuts the same portion.
#[test]
fn crop_is_in_native_source_pixels_even_on_a_proxy_frame() {
    let compositor = Compositor::new_headless();
    // 4x4 decoded frame for a 1920x1080 native media.
    let input = quadrant_frame();
    let transform = Transform {
        crop: [0.0, 0.0, 960.0, 540.0], // away with the right half and the bottom half
        ..Transform::default()
    };
    let out = compositor.render_layers(
        &[Layer::new(
            LayerContent::Video {
                frame: input.as_yuv_frame(),
                source_size: (1920, 1080),
            },
            transform,
        )],
        OutputFrame::scaled(16, 16, (1920, 1080)),
    );
    let pixel = |x: usize, y: usize| {
        let i = (y * 16 + x) * 4;
        [out[i], out[i + 1], out[i + 2], out[i + 3]]
    };

    assert_close_rgba(pixel(4, 4), [40, 40, 40, 255]);
    assert_eq!(pixel(12, 12), [0, 0, 0, 255]);
}

/// The Y axis is an NLE's, not the uv one: positive = up.
#[test]
fn a_positive_y_position_lifts_the_clip() {
    let compositor = Compositor::new_headless();
    let input = quadrant_frame();
    let transform = Transform {
        position: [0.0, 8.0], // half a frame up (output 16x16)
        ..Transform::default()
    };
    let out = compositor.render_frame(
        &input.as_yuv_frame(),
        &transform,
        OutputFrame::exact(16, 16),
    );
    let pixel = |x: usize, y: usize| {
        let i = (y * 16 + x) * 4;
        [out[i], out[i + 1], out[i + 2], out[i + 3]]
    };

    // Raised by half a frame: at the top the bottom half of the clip remains, at the
    // bottom there is nothing left.
    assert_close_rgba(pixel(4, 4), [160, 160, 160, 255]);
    assert_eq!(pixel(4, 12), [0, 0, 0, 255]);
}

#[test]
fn flip_mirrors_the_clip_on_each_axis() {
    let compositor = Compositor::new_headless();
    let input = quadrant_frame();
    let transform = Transform {
        flip: [true, false],
        ..Transform::default()
    };
    let out = compositor.render_frame(
        &input.as_yuv_frame(),
        &transform,
        OutputFrame::exact(16, 16),
    );
    let pixel = |x: usize, y: usize| {
        let i = (y * 16 + x) * 4;
        [out[i], out[i + 1], out[i + 2], out[i + 3]]
    };

    assert_close_rgba(pixel(12, 4), [40, 40, 40, 255]);
    assert_close_rgba(pixel(4, 4), [100, 100, 100, 255]);
}

/// The softness acts on the alpha: at the crop edge the layer becomes
/// progressively transparent instead of cutting sharply. Negative = towards
/// the inside of the crop.
#[test]
fn negative_crop_softness_fades_inward_from_the_edge() {
    let compositor = Compositor::new_headless();
    let input = solid_frame(16, 16, 235, 128, 128, ColorMatrix::Bt709, false);
    let transform = Transform {
        crop: [0.0, 0.0, 8.0, 0.0], // away with the right half (8 px of 16)
        crop_softness: -1.6,
        ..Transform::default()
    };
    let out = compositor.render_frame(
        &input.as_yuv_frame(),
        &transform,
        OutputFrame::exact(16, 16),
    );
    let luma = |x: usize, y: usize| out[(y * 16 + x) * 4] as i32;

    // Black clear below: the closer to the crop edge, the darker.
    assert!(luma(4, 8) > 200, "far from the edge: full");
    assert!(
        luma(7, 8) < luma(6, 8) && luma(6, 8) < luma(4, 8),
        "gradient towards the edge: {} {} {}",
        luma(4, 8),
        luma(6, 8),
        luma(7, 8)
    );
    assert_eq!(luma(9, 8), 0, "past the crop it is cut, not feathered");
}

/// Positive softness: the ramp falls *past* the crop edge, so
/// it shows only where something was cut.
#[test]
fn positive_crop_softness_fades_outward_past_the_edge() {
    let compositor = Compositor::new_headless();
    let input = solid_frame(16, 16, 235, 128, 128, ColorMatrix::Bt709, false);
    let transform = Transform {
        crop: [0.0, 0.0, 8.0, 0.0],
        crop_softness: 1.6,
        ..Transform::default()
    };
    let out = compositor.render_frame(
        &input.as_yuv_frame(),
        &transform,
        OutputFrame::exact(16, 16),
    );
    let luma = |x: usize, y: usize| out[(y * 16 + x) * 4] as i32;

    assert!(
        luma(7, 8) > 200,
        "inside the crop it stays full up to the edge"
    );
    assert!(
        luma(8, 8) > 0 && luma(8, 8) < luma(7, 8),
        "just past the edge it fades instead of vanishing: {} {}",
        luma(7, 8),
        luma(8, 8)
    );
    assert_eq!(luma(12, 8), 0, "past the ramp nothing is left");
}

/// The case reported by the user: a 9:16 clip on top of a 16:9 one in a
/// 16:9 timeline — on the side bars the clip below must show,
/// not black.
#[test]
fn side_bars_of_the_top_layer_show_the_layer_below() {
    let compositor = Compositor::new_headless();
    // White below (16:8 like the output), black above (8:16, narrow).
    let below = solid_frame(32, 16, 235, 128, 128, ColorMatrix::Bt709, false);
    let above = solid_frame(8, 16, 16, 128, 128, ColorMatrix::Bt709, false);
    let out = compositor.render_layers(
        &[
            Layer::new(
                LayerContent::Video {
                    frame: below.as_yuv_frame(),
                    source_size: (below.width, below.height),
                },
                Transform::default(),
            ),
            Layer::new(
                LayerContent::Video {
                    frame: above.as_yuv_frame(),
                    source_size: (above.width, above.height),
                },
                Transform::default(),
            ),
        ],
        OutputFrame::exact(32, 16),
    );

    let pixel = |x: usize, y: usize| {
        let i = (y * 32 + x) * 4;
        [out[i], out[i + 1], out[i + 2], out[i + 3]]
    };
    assert_close_rgba(pixel(1, 8), [255, 255, 255, 255]);
    assert_close_rgba(pixel(30, 8), [255, 255, 255, 255]);
    assert_close_rgba(pixel(16, 8), [0, 0, 0, 255]);
}

#[test]
fn a_solid_layer_covers_everything_below_it() {
    let compositor = Compositor::new_headless();
    let below = solid_frame(8, 16, 235, 128, 128, ColorMatrix::Bt709, false);
    let out = compositor.render_layers(
        &[
            Layer::new(
                LayerContent::Video {
                    frame: below.as_yuv_frame(),
                    source_size: (below.width, below.height),
                },
                Transform::default(),
            ),
            Layer::new(LayerContent::Solid(RED), Transform::default()),
        ],
        OutputFrame::exact(16, 16),
    );
    assert!(
        out.as_chunks::<4>()
            .0
            .iter()
            .all(|px| px == &[255, 0, 0, 255])
    );
}

/// The black and white preset converts any kind of layer to luma, not just
/// video.
#[test]
fn black_and_white_flattens_a_solid_layer_to_its_luma() {
    let compositor = Compositor::new_headless();
    let out = compositor.render_layers(
        &[Layer {
            content: LayerContent::Solid(RED),
            transform: Transform::default(),
            opacity: 1.0,
            filters: BLACK_AND_WHITE,
            blend: BlendMode::Normal,
            masks: &[],
        }],
        OutputFrame::exact(4, 4),
    );
    let pixel = out.as_chunks::<4>().0[0];
    assert_eq!(pixel[0], pixel[1], "grey: R=G=B");
    assert_eq!(pixel[1], pixel[2]);
    assert!(
        pixel[0] > 0 && pixel[0] < 255,
        "luma of red, neither black nor white"
    );
}

#[test]
fn exposure_scales_the_color_by_stops_in_linear_light() {
    let compositor = Compositor::new_headless();
    let grey = vv_core::Rgba {
        r: 0.5,
        g: 0.5,
        b: 0.5,
        a: 1.0,
    };
    let render = |stops: f32| {
        let filters = [vv_core::FilterValue {
            amount: stops,
            ..vv_core::FilterValue::new(vv_core::FilterKind::Exposure)
        }];
        let out = compositor.render_layers(
            &[Layer {
                filters: &filters,
                ..Layer::new(LayerContent::Solid(grey), Transform::default())
            }],
            OutputFrame::exact(4, 4),
        );
        out.as_chunks::<4>().0[0][0]
    };
    let expected = |stops: f32| (0.5f32.powf(2.2) * stops.exp2()).powf(1.0 / 2.2) * 255.0;
    assert!((render(0.0) as f32 - 127.5).abs() <= 2.0);
    assert!((render(-1.0) as f32 - expected(-1.0)).abs() <= 2.0);
    assert!((render(1.0) as f32 - expected(1.0)).abs() <= 2.0);
    assert_eq!(render(5.0), 255, "clipped to white");
}

#[test]
fn a_text_layer_paints_its_color_only_where_the_glyphs_are() {
    let compositor = Compositor::new_headless();
    let title = vv_core::TitleParams {
        content: "II".into(),
        color: RED,
        size: 60.0,
        ..Default::default()
    };
    let out = compositor.render_layers(
        &[Layer::new(LayerContent::Text(&title), Transform::default())],
        OutputFrame::exact(160, 90),
    );
    let pixels = out.as_chunks::<4>().0;
    assert_eq!(pixels[0], [0, 0, 0, 255], "outside the text it stays black");
    assert!(
        pixels.iter().any(|px| px == &[255, 0, 0, 255]),
        "no text pixel"
    );
}

#[test]
fn a_text_shadow_darkens_the_layer_below() {
    let compositor = Compositor::new_headless();
    let title = vv_core::TitleParams {
        content: "II".into(),
        size: 60.0,
        shadow: vv_core::TitleShadow {
            enabled: true,
            offset: [10.0, -10.0],
            blur: 0.0,
            opacity: 100.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let out = compositor.render_layers(
        &[
            Layer::new(LayerContent::Solid(RED), Transform::default()),
            Layer::new(LayerContent::Text(&title), Transform::default()),
        ],
        OutputFrame::exact(160, 90),
    );
    let pixels = out.as_chunks::<4>().0;
    assert!(
        pixels.iter().any(|px| px == &[0, 0, 0, 255]),
        "no shadow pixel"
    );
}

const BLUE: vv_core::Rgba = vv_core::Rgba {
    r: 0.0,
    g: 0.0,
    b: 1.0,
    a: 1.0,
};

const WHITE: vv_core::Rgba = vv_core::Rgba {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
};

/// Composing into an intermediate (the nested timeline of a compound
/// clip) and reusing it as a `LayerContent::Texture` must give the same pixels
/// as composing its layers directly: alpha-over is associative,
/// and the round-trip through the texture must not introduce
/// differences.
#[test]
fn a_texture_layer_composites_like_the_layers_it_was_made_of() {
    let compositor = Compositor::new_headless();
    let output = OutputFrame::exact(16, 16);
    // Zoom < 1: around the red it stays uncovered, i.e. transparent
    // in the intermediate — it is the part that must let the blue show.
    let inner = Transform {
        zoom: [0.5, 0.5],
        ..Transform::default()
    };
    let below = || Layer::new(LayerContent::Solid(BLUE), Transform::default());
    let above = || Layer::new(LayerContent::Solid(RED), inner);

    let nested = compositor.render_layers_to_owned_texture_transparent(&[above()], output);
    let via_texture = compositor.render_layers(
        &[
            below(),
            Layer::new(
                LayerContent::Texture {
                    texture: &nested,
                    source_size: (16, 16),
                },
                Transform::default(),
            ),
        ],
        output,
    );
    let direct = compositor.render_layers(&[below(), above()], output);

    for (i, (a, b)) in via_texture.iter().zip(direct.iter()).enumerate() {
        assert!(
            (*a as i16 - *b as i16).abs() <= 2,
            "byte {i}: via texture {a}, direct {b}"
        );
    }
}

/// An intermediate is composed onto a transparent background with
/// `ALPHA_BLENDING`, so its color is already multiplied by the
/// alpha: reusing it as a layer without dividing it out would attenuate it a
/// second time (alpha squared on the edges and on the fades).
#[test]
fn a_semitransparent_texture_layer_is_not_faded_twice() {
    let compositor = Compositor::new_headless();
    let output = OutputFrame::exact(8, 8);
    let nested = compositor.render_layers_to_owned_texture_transparent(
        &[Layer {
            content: LayerContent::Solid(RED),
            transform: Transform::default(),
            opacity: 0.5,
            filters: &[],
            blend: BlendMode::Normal,
            masks: &[],
        }],
        output,
    );
    let out = compositor.render_layers(
        &[
            Layer::new(LayerContent::Solid(WHITE), Transform::default()),
            Layer::new(
                LayerContent::Texture {
                    texture: &nested,
                    source_size: (8, 8),
                },
                Transform::default(),
            ),
        ],
        output,
    );
    // Red at 50% over white. With the double multiplication the
    // red would drop to ~191.
    assert_close_rgba(out.as_chunks::<4>().0[0], [255, 128, 128, 255]);
}

/// The intermediate goes back to its pool as soon as whoever uses it lets it go,
/// and the next render finds it again instead of allocating.
#[test]
fn an_owned_texture_returns_to_the_pool_when_dropped() {
    let compositor = Compositor::new_headless();
    let output = OutputFrame::exact(8, 8);

    let texture = compositor.render_layers_to_owned_texture_transparent(&[], output);
    assert!(
        compositor.scratch.lock().unwrap().is_empty(),
        "in use, not in the pool"
    );
    drop(texture);
    assert_eq!(compositor.scratch.lock().unwrap().len(), 1);

    let _reused = compositor.render_layers_to_owned_texture_transparent(&[], output);
    assert!(
        compositor.scratch.lock().unwrap().is_empty(),
        "taken from the pool, not allocated"
    );
}

/// `render_layers_to_owned_texture_transparent` does not put the texture
/// back into the pool: a later render of the same size must not
/// draw over it, or the retained nested frame would be corrupted.
#[test]
fn an_owned_texture_is_not_recycled_by_the_next_render() {
    let compositor = Compositor::new_headless();
    let output = OutputFrame::exact(8, 8);
    let nested = compositor.render_layers_to_owned_texture_transparent(
        &[Layer::new(LayerContent::Solid(RED), Transform::default())],
        output,
    );
    compositor.render_layers(
        &[Layer::new(LayerContent::Solid(BLUE), Transform::default())],
        output,
    );
    let out = compositor.render_layers(
        &[Layer::new(
            LayerContent::Texture {
                texture: &nested,
                source_size: (8, 8),
            },
            Transform::default(),
        )],
        output,
    );
    assert_eq!(out.as_chunks::<4>().0[0], [255, 0, 0, 255]);
}

const RED: vv_core::Rgba = vv_core::Rgba {
    r: 1.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};

#[test]
fn a_solid_layer_is_cropped_and_moved_like_a_video_layer() {
    let compositor = Compositor::new_headless();
    // Crop in timeline pixels: right half cut, then moved
    // a quarter to the right.
    let out = compositor.render_layers(
        &[Layer::new(
            LayerContent::Solid(RED),
            Transform {
                crop: [0.0, 0.0, 8.0, 0.0],
                position: [4.0, 0.0],
                ..Transform::default()
            },
        )],
        OutputFrame::scaled(8, 4, (16, 8)),
    );
    let px = |x: usize, y: usize| &out[(y * 8 + x) * 4..(y * 8 + x) * 4 + 4];
    assert_eq!(px(1, 2), &[0, 0, 0, 255], "the left stays uncovered");
    assert_eq!(px(3, 2), &[255, 0, 0, 255]);
    assert_eq!(px(6, 2), &[0, 0, 0, 255], "past the crop");
}

/// The layer opacity (clip fades) attenuates the alpha it composes with
/// onto the layer below, both for video and for a solid color.
#[test]
fn layer_opacity_blends_with_what_is_below() {
    let compositor = Compositor::new_headless();
    let below = solid_frame(4, 4, 235, 128, 128, ColorMatrix::Bt709, false); // white
    let out = compositor.render_layers(
        &[
            Layer::new(
                LayerContent::Video {
                    frame: below.as_yuv_frame(),
                    source_size: (below.width, below.height),
                },
                Transform::default(),
            ),
            Layer {
                content: LayerContent::Solid(RED),
                transform: Transform::default(),
                opacity: 0.5,
                filters: &[],
                blend: BlendMode::Normal,
                masks: &[],
            },
        ],
        OutputFrame::exact(4, 4),
    );
    let px = |out: &[u8], x: usize, y: usize| -> [u8; 4] {
        out[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4]
            .try_into()
            .unwrap()
    };
    assert_close_rgba(px(&out, 2, 2), [255, 127, 127, 255]); // 50% red over white = pink

    // Opacity 0: the layer above is not visible at all.
    let out = compositor.render_layers(
        &[
            Layer::new(
                LayerContent::Video {
                    frame: below.as_yuv_frame(),
                    source_size: (below.width, below.height),
                },
                Transform::default(),
            ),
            Layer {
                content: LayerContent::Solid(RED),
                transform: Transform::default(),
                opacity: 0.0,
                filters: &[],
                blend: BlendMode::Normal,
                masks: &[],
            },
        ],
        OutputFrame::exact(4, 4),
    );
    assert_close_rgba(px(&out, 2, 2), [255, 255, 255, 255]);
}

#[test]
fn render_layers_i420_packs_dense_planes_for_odd_sizes() {
    let compositor = Compositor::new_headless();
    let out = compositor.render_layers_i420(
        &[Layer::new(LayerContent::Solid(RED), Transform::default())],
        OutputFrame::exact(5, 3),
    );
    // Chroma 3x2; 15 + 6 + 6 = 27 bytes, not a multiple of 4.
    let mut expected = vec![63u8; 15];
    expected.extend([102; 6]);
    expected.extend([240; 6]);
    assert_eq!(out.len(), expected.len());
    assert!(
        out.iter().zip(&expected).all(|(a, b)| a.abs_diff(*b) <= 1),
        "{out:?}"
    );
}

#[test]
fn render_layers_i420_reuses_its_buffers_across_frames_and_sizes() {
    let compositor = Compositor::new_headless();
    let solid = |color| [Layer::new(LayerContent::Solid(color), Transform::default())];
    let fresh = |color, w, h| {
        Compositor::new_headless().render_layers_i420(&solid(color), OutputFrame::exact(w, h))
    };
    let blue = vv_core::Rgba::from([0.0, 0.0, 1.0, 1.0]);
    for (color, w, h) in [(RED, 5, 3), (blue, 5, 3), (RED, 8, 6), (RED, 5, 3)] {
        let out = compositor.render_layers_i420(&solid(color), OutputFrame::exact(w, h));
        assert_eq!(out, fresh(color, w, h), "{w}x{h}");
    }
}

#[test]
fn no_layers_renders_a_black_frame() {
    let compositor = Compositor::new_headless();
    let out = compositor.render_layers(&[], OutputFrame::exact(4, 4));
    assert!(
        out.as_chunks::<4>()
            .0
            .iter()
            .all(|px| px == &[0, 0, 0, 255])
    );
}

/// `render_layers_rgba_transparent` is what composes the nested
/// timeline of a compound clip: the areas with nothing above must
/// stay transparent (alpha 0), not black as for the final video —
/// otherwise they would cover what is below when the compound clip
/// becomes a layer elsewhere in turn.
#[test]
fn no_layers_renders_fully_transparent_with_the_transparent_variant() {
    let compositor = Compositor::new_headless();
    let out = compositor.render_layers_rgba_transparent(&[], OutputFrame::exact(4, 4));
    assert!(out.as_chunks::<4>().0.iter().all(|px| px == &[0, 0, 0, 0]));
}

#[test]
fn render_layers_rgba_transparent_leaves_uncovered_areas_transparent_not_black() {
    let compositor = Compositor::new_headless();
    // Crop in timeline pixels: right half cut away.
    let out = compositor.render_layers_rgba_transparent(
        &[Layer::new(
            LayerContent::Solid(RED),
            Transform {
                crop: [0.0, 0.0, 8.0, 0.0],
                ..Transform::default()
            },
        )],
        OutputFrame::exact(16, 8),
    );
    let px = |x: usize, y: usize| &out[(y * 16 + x) * 4..(y * 16 + x) * 4 + 4];
    assert_eq!(
        px(2, 4),
        &[255, 0, 0, 255],
        "covered by the layer: opaque red"
    );
    assert_eq!(
        px(12, 4),
        &[0, 0, 0, 0],
        "uncovered: transparent, not black"
    );
}

/// The mechanism by which an already composed compound clip comes back as a
/// layer elsewhere: `YuvFrame::alpha` carries the real per-pixel coverage,
/// not just the uniform `opacity` multiplier — where it is 0 it must
/// let what is below show, exactly as a hole in the nested timeline
/// it comes from would.
#[test]
fn a_videos_own_alpha_plane_lets_the_layer_below_show_through() {
    let compositor = Compositor::new_headless();
    let frame = solid_frame(4, 4, 255, 128, 128, ColorMatrix::Bt709, true); // white
    // Left opaque, right transparent.
    let alpha: Vec<u8> = (0..16u32)
        .map(|i| if i % 4 < 2 { 255 } else { 0 })
        .collect();
    let out = compositor.render_layers(
        &[Layer::new(
            LayerContent::Video {
                frame: YuvFrame {
                    alpha: &alpha,
                    ..frame.as_yuv_frame()
                },
                source_size: (4, 4),
            },
            Transform::default(),
        )],
        OutputFrame::exact(4, 4),
    );
    let px = |x: usize, y: usize| &out[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4];
    assert_close_rgba(px(0, 0).try_into().unwrap(), [255, 255, 255, 255]);
    assert_eq!(
        px(3, 0),
        &[0, 0, 0, 255],
        "right: alpha 0 in the plane lets the black below show"
    );
}

#[test]
fn fit_output_size_wraps_the_source_in_the_requested_aspect() {
    assert_eq!(fit_output_size((540, 960), (1920, 1080)), (1707, 960));
    assert_eq!(fit_output_size((1920, 1080), (1080, 1920)), (1920, 3413));
    assert_eq!(fit_output_size((1280, 720), (1920, 1080)), (1280, 720));
}

#[test]
fn output_size_can_differ_from_input_size() {
    let compositor = Compositor::new_headless();
    let input = solid_frame(4, 4, 1, 2, 3, ColorMatrix::Bt601, true);
    let out = compositor.render_frame(
        &input.as_yuv_frame(),
        &Transform::default(),
        OutputFrame::exact(37, 21),
    );
    assert_eq!(out.len(), 37 * 21 * 4);
}

/// Reads back a texture created by this same `Compositor`
/// (same device/queue) — it is not part of the public API, it only serves
/// to check in the test that the zero-copy path produces exactly
/// the same bytes as the readback path: a performance-only
/// change must not alter a single pixel of what is
/// shown (plans/REFACTOR_PIPELINE.md §5, frame accuracy is
/// non-negotiable).
fn read_back(compositor: &Compositor, texture: &wgpu::Texture, w: u32, h: u32) -> Vec<u8> {
    let unpadded_bytes_per_row = w * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(align) * align;

    let buffer = compositor.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("test readback buffer"),
        size: (padded_bytes_per_row * h) as wgpu::BufferAddress,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = compositor
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    compositor.queue.submit(Some(encoder.finish()));

    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    compositor
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    rx.recv().unwrap().unwrap();

    let mut out = Vec::with_capacity((unpadded_bytes_per_row * h) as usize);
    {
        let data = slice.get_mapped_range().unwrap();
        for row in 0..h {
            let start = (row * padded_bytes_per_row) as usize;
            out.extend_from_slice(&data[start..start + unpadded_bytes_per_row as usize]);
        }
    }
    buffer.unmap();
    out
}

#[test]
fn render_frame_to_texture_produces_the_same_pixels_as_render_frame() {
    let compositor = Compositor::new_headless();
    let input = quadrant_frame();
    let transform = Transform {
        crop: [0.0, 0.0, 0.5, 0.5],
        zoom: [1.0, 1.0],
        position: [0.0, 0.0],
        ..Transform::default()
    };

    let via_readback = compositor.render_frame(
        &input.as_yuv_frame(),
        &transform,
        OutputFrame::exact(16, 16),
    );
    let texture = compositor.render_frame_to_texture(
        &input.as_yuv_frame(),
        &transform,
        OutputFrame::exact(16, 16),
    );
    let via_texture = read_back(&compositor, &texture, 16, 16);

    assert_eq!(
        via_readback, via_texture,
        "the zero-copy path must produce exactly the same pixels as the readback path"
    );
}

/// The compositing methods other than Normal read the already composed
/// stack and apply their formula to it, channel by channel.
#[test]
fn a_blend_mode_combines_the_layer_with_what_is_below() {
    let compositor = Compositor::new_headless();
    let output = OutputFrame::exact(8, 8);
    let grey = vv_core::Rgba {
        r: 0.5,
        g: 0.5,
        b: 0.5,
        a: 1.0,
    };
    let blended = |mode| {
        let out = compositor.render_layers(
            &[
                Layer::new(LayerContent::Solid(grey), Transform::default()),
                Layer {
                    content: LayerContent::Solid(grey),
                    transform: Transform::default(),
                    opacity: 1.0,
                    filters: &[],
                    blend: mode,
                    masks: &[],
                },
            ],
            output,
        );
        out.as_chunks::<4>().0[0]
    };
    assert_close_rgba(blended(BlendMode::Normal), [128, 128, 128, 255]);
    assert_close_rgba(blended(BlendMode::Multiply), [64, 64, 64, 255]);
    assert_close_rgba(blended(BlendMode::Screen), [191, 191, 191, 255]);
    assert_close_rgba(blended(BlendMode::Add), [255, 255, 255, 255]);
    assert_close_rgba(blended(BlendMode::Difference), [0, 0, 0, 255]);
    assert_close_rgba(blended(BlendMode::Subtract), [0, 0, 0, 255]);
}

/// A blended layer does not erase the background where it does not cover: outside the
/// crop what was below remains (the REPLACE pipeline would write
/// zeros if the shader did not recompose the backdrop).
#[test]
fn a_blended_layer_leaves_the_backdrop_where_it_does_not_cover() {
    let compositor = Compositor::new_headless();
    let output = OutputFrame::exact(16, 16);
    let out = compositor.render_layers(
        &[
            Layer::new(LayerContent::Solid(BLUE), Transform::default()),
            Layer {
                content: LayerContent::Solid(WHITE),
                // Away with the right half: the blue must remain there.
                transform: Transform {
                    crop: [0.0, 0.0, 8.0, 0.0],
                    ..Transform::default()
                },
                opacity: 1.0,
                filters: &[],
                blend: BlendMode::Screen,
                masks: &[],
            },
        ],
        output,
    );
    let pixels = out.as_chunks::<4>().0;
    assert_close_rgba(pixels[0], [255, 255, 255, 255]);
    assert_close_rgba(pixels[12], [0, 0, 255, 255]);
}

/// The first layer of a stack can be blended: the clear must happen
/// first, or the backdrop it reads would be the previous frame.
#[test]
fn the_first_layer_can_be_blended_over_the_clear() {
    let compositor = Compositor::new_headless();
    let output = OutputFrame::exact(8, 8);
    let out = compositor.render_layers(
        &[Layer {
            content: LayerContent::Solid(RED),
            transform: Transform::default(),
            opacity: 1.0,
            filters: &[],
            blend: BlendMode::Screen,
            masks: &[],
        }],
        output,
    );
    // Screen over black (the clear) leaves the color as it is.
    assert_close_rgba(out.as_chunks::<4>().0[0], [255, 0, 0, 255]);
}

fn adjustment(transform: Transform, opacity: f32, filters: &[vv_core::FilterValue]) -> Layer<'_> {
    Layer {
        content: LayerContent::Adjustment,
        transform,
        opacity,
        filters,
        blend: BlendMode::Normal,
        masks: &[],
    }
}

const BLACK_AND_WHITE: &[vv_core::FilterValue] = &[vv_core::FilterValue {
    grade: vv_core::GradePreset::BlackAndWhite.value(),
    ..vv_core::FilterValue::new(vv_core::FilterKind::ColorCorrection)
}];

#[test]
fn an_adjustment_filters_the_layers_below_but_not_those_above() {
    let compositor = Compositor::new_headless();
    let out = compositor.render_layers(
        &[
            Layer::new(LayerContent::Solid(RED), Transform::default()),
            adjustment(Transform::default(), 1.0, BLACK_AND_WHITE),
            Layer::new(
                LayerContent::Solid(BLUE),
                Transform {
                    crop: [8.0, 0.0, 0.0, 0.0],
                    ..Transform::default()
                },
            ),
        ],
        OutputFrame::exact(16, 16),
    );
    let pixels = out.as_chunks::<4>().0;
    let left = pixels[16 * 8 + 2];
    assert_eq!(left[0], left[1], "grey: R=G=B");
    assert_eq!(left[1], left[2]);
    assert!(left[0] > 0 && left[0] < 255);
    assert_close_rgba(pixels[16 * 8 + 12], [0, 0, 255, 255]);
}

#[test]
fn an_adjustment_opacity_mixes_the_processed_stack_with_the_original() {
    let compositor = Compositor::new_headless();
    let render = |opacity| {
        let out = compositor.render_layers(
            &[
                Layer::new(LayerContent::Solid(RED), Transform::default()),
                adjustment(Transform::default(), opacity, BLACK_AND_WHITE),
            ],
            OutputFrame::exact(4, 4),
        );
        out.as_chunks::<4>().0[5]
    };
    let grey = render(1.0);
    let half = render(0.5);
    assert_close_rgba(render(0.0), [255, 0, 0, 255]);
    assert_close_rgba(
        half,
        [
            ((255 + grey[0] as u32) / 2) as u8,
            grey[1] / 2,
            grey[2] / 2,
            255,
        ],
    );
}

/// What the transform uncovers is the timeline background, not the
/// unprocessed stack.
#[test]
fn an_adjustment_zoomed_out_shows_black_around_the_stack() {
    let compositor = Compositor::new_headless();
    let out = compositor.render_layers(
        &[
            Layer::new(LayerContent::Solid(RED), Transform::default()),
            adjustment(
                Transform {
                    zoom: [0.5, 0.5],
                    ..Transform::default()
                },
                1.0,
                &[],
            ),
        ],
        OutputFrame::exact(16, 16),
    );
    let pixels = out.as_chunks::<4>().0;
    assert_close_rgba(pixels[0], [0, 0, 0, 255]);
    assert_close_rgba(pixels[16 * 8 + 8], [255, 0, 0, 255]);
}

#[test]
fn an_adjustment_crop_leaves_black_where_it_cuts() {
    let compositor = Compositor::new_headless();
    let out = compositor.render_layers(
        &[
            Layer::new(LayerContent::Solid(RED), Transform::default()),
            adjustment(
                Transform {
                    crop: [0.0, 0.0, 8.0, 0.0],
                    ..Transform::default()
                },
                1.0,
                &[],
            ),
        ],
        OutputFrame::scaled(8, 8, (16, 16)),
    );
    let pixels = out.as_chunks::<4>().0;
    assert_close_rgba(pixels[8 * 4 + 1], [255, 0, 0, 255]);
    assert_close_rgba(pixels[8 * 4 + 6], [0, 0, 0, 255]);
}

#[test]
fn an_adjustment_moves_the_whole_stack_below() {
    let compositor = Compositor::new_headless();
    let out = compositor.render_layers(
        &[
            Layer::new(
                LayerContent::Solid(RED),
                Transform {
                    crop: [0.0, 0.0, 8.0, 0.0],
                    ..Transform::default()
                },
            ),
            adjustment(
                Transform {
                    position: [8.0, 0.0],
                    ..Transform::default()
                },
                1.0,
                &[],
            ),
        ],
        OutputFrame::exact(16, 16),
    );
    let pixels = out.as_chunks::<4>().0;
    assert_close_rgba(pixels[16 * 8 + 4], [0, 0, 0, 255]);
    assert_close_rgba(pixels[16 * 8 + 12], [255, 0, 0, 255]);
}

#[test]
fn an_adjustment_blend_mode_combines_the_processed_stack_with_the_original() {
    let compositor = Compositor::new_headless();
    let grey = vv_core::Rgba {
        r: 0.5,
        g: 0.5,
        b: 0.5,
        a: 1.0,
    };
    let out = compositor.render_layers(
        &[
            Layer::new(LayerContent::Solid(grey), Transform::default()),
            Layer {
                blend: BlendMode::Multiply,
                ..adjustment(Transform::default(), 1.0, &[])
            },
        ],
        OutputFrame::exact(4, 4),
    );
    assert_close_rgba(out.as_chunks::<4>().0[0], [64, 64, 64, 255]);
}

/// Inside a compound clip there may be nothing below: the adjustment must not
/// turn the transparency into black.
#[test]
fn an_adjustment_over_nothing_stays_transparent_in_a_transparent_render() {
    let compositor = Compositor::new_headless();
    let out = compositor.render_layers_rgba_transparent(
        &[adjustment(
            Transform {
                zoom: [0.5, 0.5],
                ..Transform::default()
            },
            1.0,
            BLACK_AND_WHITE,
        )],
        OutputFrame::exact(8, 8),
    );
    assert!(out.iter().all(|&b| b == 0));
}

/// NV12 samples U/V from one `Rg8` texture: same filtering, same pixels as
/// the planar layout, also when scaling interpolates the chroma.
#[test]
fn an_interleaved_chroma_renders_like_the_planar_one() {
    let compositor = Compositor::new_headless();
    let (w, h, cw, ch) = (16u32, 12u32, 8u32, 6u32);
    let y: Vec<u8> = (0..w * h).map(|i| (i * 7 % 256) as u8).collect();
    let u: Vec<u8> = (0..cw * ch).map(|i| (40 + i * 5) as u8).collect();
    let v: Vec<u8> = (0..cw * ch).map(|i| (220 - i * 3) as u8).collect();
    let uv: Vec<u8> = u.iter().zip(&v).flat_map(|(u, v)| [*u, *v]).collect();
    let frame = |chroma| YuvFrame {
        y: &y,
        width: w,
        height: h,
        chroma,
        chroma_width: cw,
        chroma_height: ch,
        matrix: ColorMatrix::Bt709,
        full_range: false,
        alpha: OPAQUE,
    };
    let render = |chroma| {
        compositor.render_frame(
            &frame(chroma),
            &Transform::default(),
            OutputFrame::exact(37, 29),
        )
    };
    let planar = render(YuvChroma::Planar { u: &u, v: &v });
    let interleaved = render(YuvChroma::Interleaved(&uv));
    assert!(planar == interleaved);
}

fn blur(kind: vv_core::FilterKind, radius: f32) -> vv_core::FilterValue {
    vv_core::FilterValue {
        radius,
        ..vv_core::FilterValue::new(kind)
    }
}

/// 16x4, black on the left half and white on the right one (neutral chroma,
/// full range: R=G=B=Y).
fn split_frame() -> OwnedYuvFrame {
    let mut frame = solid_frame(16, 4, 0, 128, 128, ColorMatrix::Bt709, true);
    for row in frame.y.chunks_mut(16) {
        row[8..].fill(255);
    }
    frame
}

fn render_blurred(filters: &[vv_core::FilterValue]) -> Vec<u8> {
    let frame = split_frame();
    let out = Compositor::new_headless().render_layers(
        &[Layer {
            filters,
            ..Layer::new(
                LayerContent::Video {
                    frame: frame.as_yuv_frame(),
                    source_size: (16, 4),
                },
                Transform::default(),
            )
        }],
        OutputFrame::exact(16, 4),
    );
    out.as_chunks::<4>().0[16..32]
        .iter()
        .map(|px| px[0])
        .collect()
}

#[test]
fn filter_chain_splits_the_per_pixel_filters_at_the_blurs() {
    use vv_core::FilterKind::*;
    let filters = [
        vv_core::FilterValue::new(ColorCorrection),
        blur(BoxBlur, 3.0),
        vv_core::FilterValue::new(ColorCorrection),
        blur(GaussianBlur, 0.0),
        blur(GaussianBlur, 2.0),
    ];
    let chain = FilterChain::new(&filters, false);
    assert_eq!(chain.leading, [vv_core::FilterValue::new(ColorCorrection)]);
    assert_eq!(
        chain.blurs,
        [
            (
                Blur {
                    gaussian: false,
                    radius: 3.0,
                    direction: vv_core::BlurDirection::Both,
                },
                vec![vv_core::FilterValue::new(ColorCorrection)]
            ),
            (
                Blur {
                    gaussian: true,
                    radius: 2.0,
                    direction: vv_core::BlurDirection::Both,
                },
                vec![]
            ),
        ],
        "a zero radius is no blur"
    );

    let uniform = FilterChain::new(&filters, true);
    assert_eq!(
        uniform.leading,
        [vv_core::FilterValue::new(ColorCorrection); 2]
    );
    assert!(uniform.blurs.is_empty(), "nothing to blur on a solid color");
}

#[test]
fn box_blur_averages_the_pixels_within_the_radius() {
    let row = render_blurred(&[blur(vv_core::FilterKind::BoxBlur, 2.0)]);
    assert!(
        row[0] <= 2,
        "the edges repeat, nothing comes from outside: {row:?}"
    );
    assert!(row[15] >= 253, "{row:?}");
    assert!(
        row[5] <= 2,
        "farther than the radius from the edge: {row:?}"
    );
    // Pixel 7 averages 5..=9: three black, two white.
    assert!(row[7].abs_diff(102) <= 2, "{row:?}");
    assert!(row[8].abs_diff(153) <= 2, "{row:?}");
}

#[test]
fn a_blur_spreads_only_along_its_direction() {
    let along = |direction| {
        render_blurred(&[vv_core::FilterValue {
            direction,
            ..blur(vv_core::FilterKind::BoxBlur, 2.0)
        }])
    };
    assert!(along(vv_core::BlurDirection::Horizontal)[7].abs_diff(102) <= 2);
    let vertical = along(vv_core::BlurDirection::Vertical);
    assert!(
        vertical[7] <= 2 && vertical[8] >= 253,
        "the edge is vertical: {vertical:?}"
    );
}

#[test]
fn gaussian_blur_is_a_symmetric_ramp_across_the_edge() {
    let row = render_blurred(&[blur(vv_core::FilterKind::GaussianBlur, 4.0)]);
    assert!(
        row.windows(2).all(|w| w[0] <= w[1].saturating_add(1)),
        "{row:?}"
    );
    assert!(row[7] > 0 && row[7] < 128, "{row:?}");
    assert!((row[7] as i32 + row[8] as i32 - 255).abs() <= 2, "{row:?}");
}

#[test]
fn blur_radius_is_in_timeline_pixels_whatever_the_output_resolution() {
    let frame = split_frame();
    let render = |output| {
        let filters = [blur(vv_core::FilterKind::BoxBlur, 2.0)];
        let out = Compositor::new_headless().render_layers(
            &[Layer {
                filters: &filters,
                ..Layer::new(
                    LayerContent::Video {
                        frame: frame.as_yuv_frame(),
                        source_size: (16, 4),
                    },
                    Transform::default(),
                )
            }],
            output,
        );
        out.as_chunks::<4>()
            .0
            .iter()
            .map(|px| px[0])
            .collect::<Vec<_>>()
    };
    let full = render(OutputFrame::exact(32, 8));
    let half = render(OutputFrame::scaled(16, 4, (32, 8)));
    // Same position on screen: pixel 7 at half resolution, 14-15 at full.
    let full_at_7 = (full[32 * 4 + 14] as i32 + full[32 * 4 + 15] as i32) / 2;
    assert!(
        (half[16 * 2 + 7] as i32 - full_at_7).abs() <= 12,
        "{half:?} {full:?}"
    );
}

#[test]
fn an_adjustment_blurs_the_stack_below() {
    let compositor = Compositor::new_headless();
    let filters = [blur(vv_core::FilterKind::BoxBlur, 2.0)];
    let out = compositor.render_layers(
        &[
            Layer::new(
                LayerContent::Solid(WHITE),
                Transform {
                    crop: [8.0, 0.0, 0.0, 0.0],
                    ..Transform::default()
                },
            ),
            adjustment(Transform::default(), 1.0, &filters),
        ],
        OutputFrame::exact(16, 4),
    );
    let row: Vec<u8> = out.as_chunks::<4>().0[16..32]
        .iter()
        .map(|px| px[0])
        .collect();
    assert!(row[0] <= 2 && row[15] >= 253, "{row:?}");
    assert!(row[7] > 50 && row[8] < 205, "{row:?}");
}

fn rect_mask(center: [f32; 2], size: [f32; 2]) -> vv_core::MaskValue {
    vv_core::MaskValue {
        shape: vv_core::MaskShape::Rectangle,
        invert: false,
        mode: vv_core::MaskMode::Add,
        center,
        size,
        rotation: 0.0,
        roundness: 0.0,
        feather: 0.0,
        expansion: 0.0,
        opacity: 1.0,
        polygon: Vec::new(),
    }
}

/// A red solid over the black clear, masked; 32x16 so that pixel `(x, y)`
/// is the layer point `(x + 0.5 - 16, 8 - y - 0.5)`.
fn render_masked_red(masks: &[vv_core::MaskValue], transform: Transform) -> Vec<[u8; 4]> {
    let compositor = Compositor::new_headless();
    let out = compositor.render_layers(
        &[Layer {
            masks,
            ..Layer::new(LayerContent::Solid(RED), transform)
        }],
        OutputFrame::exact(32, 16),
    );
    out.as_chunks::<4>().0.to_vec()
}

fn at(pixels: &[[u8; 4]], x: usize, y: usize) -> [u8; 4] {
    pixels[y * 32 + x]
}

#[test]
fn a_rectangle_mask_shows_the_layer_only_inside_it() {
    let pixels = render_masked_red(&[rect_mask([0.0, 0.0], [16.0, 8.0])], Transform::default());
    assert_close_rgba(at(&pixels, 16, 8), [255, 0, 0, 255]);
    assert_close_rgba(at(&pixels, 9, 5), [255, 0, 0, 255]);
    assert_close_rgba(at(&pixels, 6, 8), [0, 0, 0, 255]);
    assert_close_rgba(at(&pixels, 16, 2), [0, 0, 0, 255]);
}

#[test]
fn the_mask_center_is_in_layer_pixels_with_y_up() {
    let pixels = render_masked_red(&[rect_mask([8.0, 4.0], [4.0, 4.0])], Transform::default());
    assert_close_rgba(at(&pixels, 24, 4), [255, 0, 0, 255]);
    assert_close_rgba(at(&pixels, 24, 12), [0, 0, 0, 255]);
    assert_close_rgba(at(&pixels, 8, 4), [0, 0, 0, 255]);
}

#[test]
fn the_mask_moves_with_the_layer() {
    let moved = Transform {
        position: [8.0, 0.0],
        ..Transform::default()
    };
    let pixels = render_masked_red(&[rect_mask([0.0, 0.0], [4.0, 4.0])], moved);
    assert_close_rgba(at(&pixels, 24, 8), [255, 0, 0, 255]);
    assert_close_rgba(at(&pixels, 16, 8), [0, 0, 0, 255]);
}

#[test]
fn an_inverted_mask_shows_the_layer_outside_it() {
    let mask = vv_core::MaskValue {
        invert: true,
        ..rect_mask([0.0, 0.0], [16.0, 8.0])
    };
    let pixels = render_masked_red(&[mask], Transform::default());
    assert_close_rgba(at(&pixels, 16, 8), [0, 0, 0, 255]);
    assert_close_rgba(at(&pixels, 2, 1), [255, 0, 0, 255]);
}

#[test]
fn an_ellipse_mask_leaves_out_the_corners_of_its_box() {
    let mask = vv_core::MaskValue {
        shape: vv_core::MaskShape::Ellipse,
        ..rect_mask([0.0, 0.0], [32.0, 16.0])
    };
    let pixels = render_masked_red(&[mask], Transform::default());
    assert_close_rgba(at(&pixels, 16, 8), [255, 0, 0, 255]);
    assert_close_rgba(at(&pixels, 2, 8), [255, 0, 0, 255]);
    assert_close_rgba(at(&pixels, 1, 1), [0, 0, 0, 255]);
}

#[test]
fn a_rotated_rectangle_mask_turns_clockwise() {
    let mask = vv_core::MaskValue {
        rotation: 90.0,
        ..rect_mask([0.0, 0.0], [24.0, 4.0])
    };
    let pixels = render_masked_red(&[mask], Transform::default());
    assert_close_rgba(at(&pixels, 16, 2), [255, 0, 0, 255]);
    assert_close_rgba(at(&pixels, 6, 8), [0, 0, 0, 255]);
}

#[test]
fn a_path_mask_fills_its_polygon() {
    // Triangle pointing up, base along the bottom half.
    let mask = vv_core::MaskValue {
        shape: vv_core::MaskShape::Path,
        polygon: vec![[-12.0, -6.0], [12.0, -6.0], [0.0, 6.0]],
        ..rect_mask([0.0, 0.0], [0.0, 0.0])
    };
    let pixels = render_masked_red(&[mask], Transform::default());
    assert_close_rgba(at(&pixels, 16, 10), [255, 0, 0, 255]);
    assert_close_rgba(at(&pixels, 6, 3), [0, 0, 0, 255]);
    assert_close_rgba(at(&pixels, 26, 3), [0, 0, 0, 255]);
}

#[test]
fn feather_ramps_the_coverage_across_the_outline() {
    let mask = vv_core::MaskValue {
        feather: 8.0,
        ..rect_mask([0.0, 0.0], [16.0, 64.0])
    };
    let pixels = render_masked_red(&[mask], Transform::default());
    let red = |x| at(&pixels, x, 8)[0] as i32;
    // Outline at x = 8 (pixel 24, center 24.5 → distance 0.5).
    assert!(
        (red(24) - 128).abs() < 40,
        "about half on the edge: {}",
        red(24)
    );
    assert!(red(18) > 250 && red(30) < 5);
    assert!(red(22) > red(24) && red(24) > red(26));
}

#[test]
fn a_subtract_mask_cuts_a_hole_in_the_one_above() {
    let hole = vv_core::MaskValue {
        mode: vv_core::MaskMode::Subtract,
        ..rect_mask([0.0, 0.0], [8.0, 4.0])
    };
    let pixels = render_masked_red(
        &[rect_mask([0.0, 0.0], [24.0, 12.0]), hole],
        Transform::default(),
    );
    assert_close_rgba(at(&pixels, 16, 8), [0, 0, 0, 255]);
    assert_close_rgba(at(&pixels, 8, 8), [255, 0, 0, 255]);
    assert_close_rgba(at(&pixels, 1, 1), [0, 0, 0, 255]);
}

#[test]
fn an_intersect_mask_keeps_only_the_overlap() {
    let second = vv_core::MaskValue {
        mode: vv_core::MaskMode::Intersect,
        ..rect_mask([8.0, 0.0], [16.0, 16.0])
    };
    let pixels = render_masked_red(
        &[rect_mask([0.0, 0.0], [16.0, 16.0]), second],
        Transform::default(),
    );
    assert_close_rgba(at(&pixels, 20, 8), [255, 0, 0, 255]);
    assert_close_rgba(at(&pixels, 12, 8), [0, 0, 0, 255]);
    assert_close_rgba(at(&pixels, 28, 8), [0, 0, 0, 255]);
}

#[test]
fn a_masked_adjustment_processes_only_inside_the_mask() {
    let compositor = Compositor::new_headless();
    let darken = [vv_core::FilterValue {
        amount: -5.0,
        ..vv_core::FilterValue::new(vv_core::FilterKind::Exposure)
    }];
    let render = |invert: bool| {
        let masks = [vv_core::MaskValue {
            invert,
            ..rect_mask([0.0, 0.0], [16.0, 8.0])
        }];
        let out = compositor.render_layers(
            &[
                Layer::new(LayerContent::Solid(WHITE), Transform::default()),
                Layer {
                    masks: &masks,
                    ..adjustment(Transform::default(), 1.0, &darken)
                },
            ],
            OutputFrame::exact(32, 16),
        );
        out.as_chunks::<4>().0.to_vec()
    };
    let inside = render(false);
    assert!(at(&inside, 16, 8)[0] < 128, "darkened inside");
    assert_close_rgba(at(&inside, 2, 1), [255, 255, 255, 255]);
    let outside = render(true);
    assert_close_rgba(at(&outside, 16, 8), [255, 255, 255, 255]);
    assert!(at(&outside, 2, 1)[0] < 128, "darkened outside");
}

#[test]
fn a_masked_adjustment_blurs_only_inside_the_mask() {
    let compositor = Compositor::new_headless();
    let blur = [vv_core::FilterValue {
        radius: 6.0,
        ..vv_core::FilterValue::new(vv_core::FilterKind::BoxBlur)
    }];
    let masks = [rect_mask([-8.0, 0.0], [16.0, 16.0])];
    // Left half red, right half blue: the blur would mix them at the seam.
    let out = compositor.render_layers(
        &[
            Layer::new(LayerContent::Solid(BLUE), Transform::default()),
            Layer::new(
                LayerContent::Solid(RED),
                Transform {
                    crop: [0.0, 0.0, 16.0, 0.0],
                    ..Transform::default()
                },
            ),
            Layer {
                masks: &masks,
                ..adjustment(Transform::default(), 1.0, &blur)
            },
        ],
        OutputFrame::exact(32, 16),
    );
    let pixels = out.as_chunks::<4>().0;
    let left_of_seam = at(pixels, 14, 8);
    assert!(left_of_seam[2] > 30, "blurred inside: {left_of_seam:?}");
    assert_close_rgba(at(pixels, 17, 8), [0, 0, 255, 255]);
}

fn exposure(stops: f32) -> [vv_core::FilterValue; 1] {
    [vv_core::FilterValue {
        amount: stops,
        ..vv_core::FilterValue::new(vv_core::FilterKind::Exposure)
    }]
}

/// 256x4, Y = x on every row (neutral chroma, full range).
fn gradient_frame() -> OwnedYuvFrame {
    let mut frame = solid_frame(256, 4, 0, 128, 128, ColorMatrix::Bt709, true);
    for row in frame.y.chunks_mut(256) {
        for (x, y) in row.iter_mut().enumerate() {
            *y = x as u8;
        }
    }
    frame
}

/// The green channel of the first row of the gradient, under
/// Exposure `down` then an adjustment with Exposure `up`.
fn render_pushed_gradient(compositor: &Compositor, down: f32, up: f32) -> Vec<u8> {
    let frame = gradient_frame();
    let (down, up) = (exposure(down), exposure(up));
    let out = compositor.render_layers(
        &[
            Layer {
                filters: &down,
                ..Layer::new(
                    LayerContent::Video {
                        frame: frame.as_yuv_frame(),
                        source_size: (256, 4),
                    },
                    Transform::default(),
                )
            },
            adjustment(Transform::default(), 1.0, &up),
        ],
        OutputFrame::exact(256, 4),
    );
    out.as_chunks::<4>().0[..256]
        .iter()
        .map(|px| px[1])
        .collect()
}

/// Exposure −6 then +6 squeezes the gradient into about 40 levels in
/// between: with 8-bit intermediates it would come back banded, each pixel
/// up to 3 levels off.
#[test]
fn a_chain_of_passes_keeps_the_precision_of_the_gradient() {
    let compositor = Compositor::new_headless();
    let reference = render_pushed_gradient(&compositor, 0.0, 0.0);
    let pushed = render_pushed_gradient(&compositor, -6.0, 6.0);
    let levels = |row: &[u8]| row.iter().collect::<std::collections::BTreeSet<_>>().len();
    assert!(
        levels(&pushed) + 10 >= levels(&reference),
        "{} levels out of {}",
        levels(&pushed),
        levels(&reference)
    );
    let worst = reference
        .iter()
        .zip(&pushed)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap();
    assert!(worst <= 2, "{reference:?}\n{pushed:?}");
}

#[test]
fn the_dither_is_the_same_on_every_render() {
    let compositor = Compositor::new_headless();
    assert_eq!(
        render_pushed_gradient(&compositor, -6.0, 6.0),
        render_pushed_gradient(&compositor, -6.0, 6.0)
    );
    let frame = gradient_frame();
    let i420 = || {
        compositor.render_layers_i420(
            &[Layer::new(
                LayerContent::Video {
                    frame: frame.as_yuv_frame(),
                    source_size: (256, 4),
                },
                Transform::default(),
            )],
            OutputFrame::exact(256, 4),
        )
    };
    assert_eq!(i420(), i420());
}

/// Between two levels the dither spreads the pixels over both, in the
/// proportion that keeps the mean; on a level it leaves them alone.
#[test]
fn the_dither_keeps_the_mean_of_a_flat_color() {
    let compositor = Compositor::new_headless();
    // Exact in f16, but not on an 8-bit level; then one on a level.
    for v in [0.5f32, 0.25, 0.3125, 0.78125, 64.0 / 255.0] {
        let level = v * 255.0;
        let out = compositor.render_layers(
            &[Layer::new(
                LayerContent::Solid(vv_core::Rgba::from([v, v, v, 1.0])),
                Transform::default(),
            )],
            OutputFrame::exact(64, 64),
        );
        let pixels = out.as_chunks::<4>().0;
        let mean = pixels.iter().map(|px| px[0] as f32).sum::<f32>() / pixels.len() as f32;
        assert!((mean - level).abs() < 0.05, "{level}: mean {mean}");
        assert!(
            pixels.iter().all(|px| (px[0] as f32 - level).abs() <= 1.5),
            "{level}: no wider than the triangular noise"
        );
        assert!(
            pixels.iter().all(|px| px[3] == 255),
            "alpha is not dithered"
        );
    }
}

#[test]
fn float_work_needs_rendering_blending_and_filtering() {
    use wgpu::{TextureFormatFeatureFlags as Flags, TextureUsages as Usages};
    let features = |allowed_usages, flags| wgpu::TextureFormatFeatures {
        allowed_usages,
        flags,
    };
    let all = Flags::BLENDABLE | Flags::FILTERABLE;
    let usages = Usages::RENDER_ATTACHMENT | Usages::TEXTURE_BINDING;
    assert!(float_work_supported(features(usages, all)));
    assert!(!float_work_supported(features(
        Usages::TEXTURE_BINDING,
        all
    )));
    assert!(!float_work_supported(features(usages, Flags::FILTERABLE)));
    assert!(!float_work_supported(features(usages, Flags::BLENDABLE)));
}

#[test]
fn the_work_format_follows_the_precision_and_falls_back_to_8_bits() {
    use ProcessingPrecision::*;
    assert_eq!(work_format(High, true), PRECISE_WORK_FORMAT);
    assert_eq!(work_format(High, false), OUTPUT_FORMAT);
    assert_eq!(work_format(Standard, true), OUTPUT_FORMAT);
}

#[test]
fn standard_precision_bands_the_chain_that_high_keeps_smooth() {
    let mut compositor = Compositor::new_headless_with_precision(ProcessingPrecision::Standard);
    let reference = render_pushed_gradient(&compositor, 0.0, 0.0);
    let levels = |row: &[u8]| row.iter().collect::<std::collections::BTreeSet<_>>().len();
    assert!(levels(&render_pushed_gradient(&compositor, -6.0, 6.0)) < levels(&reference) / 2);

    compositor.set_precision(ProcessingPrecision::High);
    let reference = render_pushed_gradient(&compositor, 0.0, 0.0);
    assert!(levels(&render_pushed_gradient(&compositor, -6.0, 6.0)) + 10 >= levels(&reference));
}

/// The basic paths in 8 bits too: clear, solid, blend, adjustment, I420.
#[test]
fn standard_precision_composes_like_high() {
    let mut compositor = Compositor::new_headless_with_precision(ProcessingPrecision::Standard);
    let layers = [
        Layer::new(LayerContent::Solid(RED), Transform::default()),
        Layer {
            blend: BlendMode::Screen,
            ..Layer::new(
                LayerContent::Solid(BLUE),
                Transform {
                    crop: [8.0, 0.0, 0.0, 0.0],
                    ..Transform::default()
                },
            )
        },
        adjustment(Transform::default(), 0.5, BLACK_AND_WHITE),
    ];
    let output = OutputFrame::exact(16, 16);
    let standard = (
        compositor.render_layers(&layers, output),
        compositor.render_layers_i420(&layers, output),
    );
    compositor.set_precision(ProcessingPrecision::High);
    let high = (
        compositor.render_layers(&layers, output),
        compositor.render_layers_i420(&layers, output),
    );
    for (a, b) in [(&standard.0, &high.0), (&standard.1, &high.1)] {
        assert_eq!(a.len(), b.len());
        assert!(
            a.iter().zip(b).all(|(a, b)| a.abs_diff(*b) <= 2),
            "{a:?}\n{b:?}"
        );
    }
}

fn render_graded(compositor: &Compositor, color: [f32; 3], grade: vv_core::GradeValue) -> [u8; 4] {
    let filters = [vv_core::FilterValue {
        grade,
        ..vv_core::FilterValue::new(vv_core::FilterKind::ColorCorrection)
    }];
    let out = compositor.render_layers(
        &[Layer {
            filters: &filters,
            ..Layer::new(
                LayerContent::Solid(vv_core::Rgba::from([color[0], color[1], color[2], 1.0])),
                Transform::default(),
            )
        }],
        OutputFrame::exact(4, 4),
    );
    out.as_chunks::<4>().0[0]
}

fn grade_with(params: &[(vv_core::GradeParam, f32)]) -> vv_core::GradeValue {
    let mut grade = vv_core::GradeValue::NEUTRAL;
    for (param, value) in params {
        grade.set(*param, *value);
    }
    grade
}

#[test]
fn the_color_correction_matches_its_reference_in_both_precisions() {
    use vv_core::GradeParam::*;
    let grades = [
        grade_with(&[]),
        grade_with(&[(ShadowsX, 0.7), (ShadowsY, -0.3), (ShadowsLuma, 0.2)]),
        grade_with(&[(MidtonesY, 0.8), (MidtonesSat, 1.6), (LowRange, 0.2)]),
        grade_with(&[
            (HighlightsX, -0.5),
            (HighlightsLuma, -0.3),
            (HighRange, 0.8),
        ]),
        grade_with(&[(OffsetX, 0.3), (OffsetLuma, -0.1), (Saturation, 0.4)]),
        // The range edges: no midtones, the full span, crossed ranges.
        grade_with(&[
            (LowRange, 0.5),
            (HighRange, 0.5),
            (ShadowsLuma, 0.2),
            (HighlightsX, 0.5),
        ]),
        grade_with(&[
            (LowRange, 0.0),
            (HighRange, 1.0),
            (MidtonesLuma, 0.2),
            (ShadowsX, 0.5),
        ]),
        grade_with(&[
            (LowRange, 0.7),
            (HighRange, 0.3),
            (ShadowsY, 0.6),
            (HighlightsLuma, -0.2),
        ]),
    ];
    let colors = [
        [0.05, 0.08, 0.1],
        [0.5, 0.25, 0.75],
        [0.9, 0.85, 0.7],
        [0.3, 0.6, 0.2],
        [0.5, 0.5, 0.5],
        [0.7, 0.7, 0.7],
    ];
    for precision in [
        vv_core::ProcessingPrecision::Standard,
        vv_core::ProcessingPrecision::High,
    ] {
        let compositor = Compositor::new_headless_with_precision(precision);
        for grade in &grades {
            for color in colors {
                let got = render_graded(&compositor, color, *grade);
                let expected =
                    vv_core::apply_grade(color, grade).map(|c| (c * 255.0).round() as u8);
                assert!(
                    (0..3).all(|i| got[i].abs_diff(expected[i]) <= 2),
                    "{precision:?} {color:?} {grade:?}: {got:?} vs {expected:?}"
                );
            }
        }
    }
}

#[test]
fn a_neutral_color_correction_changes_nothing() {
    let compositor = Compositor::new_headless();
    for color in [[0.0, 0.0, 0.0], [1.0, 1.0, 1.0], [0.2, 0.5, 0.9]] {
        let got = render_graded(&compositor, color, vv_core::GradeValue::NEUTRAL);
        let expected = color.map(|c| (c * 255.0).round() as u8);
        assert!((0..3).all(|i| got[i].abs_diff(expected[i]) <= 1), "{got:?}");
    }
}

#[test]
fn the_shadows_wheel_lifts_the_shadows_and_leaves_the_highlights() {
    use vv_core::GradeParam::ShadowsLuma;
    let compositor = Compositor::new_headless();
    let lift = grade_with(&[(ShadowsLuma, 0.3)]);
    let dark = [0.1, 0.1, 0.1];
    let bright = [0.9, 0.9, 0.9];
    assert!(render_graded(&compositor, dark, lift)[1] > 26 + 30);
    assert!(render_graded(&compositor, bright, lift)[1].abs_diff(230) <= 1);
}

/// The range weights sum to 1: the same luminance on the three ranges is the
/// same as on the offset wheel.
#[test]
fn the_three_ranges_together_act_like_the_offset() {
    use vv_core::GradeParam::*;
    let compositor = Compositor::new_headless();
    let ranges = grade_with(&[
        (ShadowsLuma, 0.1),
        (MidtonesLuma, 0.1),
        (HighlightsLuma, 0.1),
    ]);
    let offset = grade_with(&[(OffsetLuma, 0.1)]);
    for color in [[0.1, 0.2, 0.15], [0.45, 0.5, 0.4], [0.7, 0.75, 0.8]] {
        let a = render_graded(&compositor, color, ranges);
        let b = render_graded(&compositor, color, offset);
        assert!((0..3).all(|i| a[i].abs_diff(b[i]) <= 1), "{a:?} vs {b:?}");
    }
}

#[test]
fn a_wheel_push_changes_the_hue_but_not_the_luma() {
    use vv_core::GradeParam::{MidtonesX, MidtonesY};
    let compositor = Compositor::new_headless();
    let grey = [0.5, 0.5, 0.5];
    let pushed = render_graded(
        &compositor,
        grey,
        grade_with(&[(MidtonesX, -0.3), (MidtonesY, 0.9)]),
    );
    let luma = vv_core::luma([0, 1, 2].map(|i| pushed[i] as f32));
    assert!((luma - 127.5).abs() <= 1.5, "{pushed:?}");
    assert!(pushed[0] > pushed[2] + 20, "towards red: {pushed:?}");
}

/// A blue cast over a whole gradient: once balanced, every range's mean
/// color is grey.
#[test]
fn auto_balance_removes_a_cast_from_the_render() {
    let compositor = Compositor::new_headless();
    let mut frame = gradient_frame();
    frame.u.fill(140);
    frame.v.fill(122);
    let render = |grade: vv_core::GradeValue| {
        let filters = [vv_core::FilterValue {
            grade,
            ..vv_core::FilterValue::new(vv_core::FilterKind::ColorCorrection)
        }];
        let out = compositor.render_layers(
            &[Layer {
                filters: &filters,
                ..Layer::new(
                    LayerContent::Video {
                        frame: frame.as_yuv_frame(),
                        source_size: (256, 4),
                    },
                    Transform::default(),
                )
            }],
            OutputFrame::exact(256, 4),
        );
        out.as_chunks::<4>()
            .0
            .iter()
            .map(|p| [p[0], p[1], p[2]].map(|c| c as f32 / 255.0))
            .collect::<Vec<_>>()
    };
    // Mean |Cb| + |Cr| of the unclipped pixels.
    let cast = |pixels: &[[f32; 3]]| {
        let chroma: Vec<f32> = pixels
            .iter()
            .filter(|p| p.iter().all(|c| *c > 0.02 && *c < 0.98))
            .map(|p| {
                let [cb, cr] = vv_core::cb_cr(*p);
                cb.abs() + cr.abs()
            })
            .collect();
        chroma.iter().sum::<f32>() / chroma.len() as f32
    };
    let before = render(vv_core::GradeValue::NEUTRAL);
    let mut grade = vv_core::auto_balance(&vv_core::GradeValue::NEUTRAL, before.iter().copied());
    grade = vv_core::auto_balance(&grade, render(grade).into_iter());
    let after = render(grade);
    assert!(cast(&before) > 0.04, "a visible cast: {}", cast(&before));
    assert!(
        cast(&after) < cast(&before) * 0.25,
        "{} -> {}",
        cast(&before),
        cast(&after)
    );
}
