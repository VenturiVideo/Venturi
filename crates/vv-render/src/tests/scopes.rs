use super::*;
use crate::Compositor;

fn scopes() -> (Compositor, Scopes) {
    let compositor = Compositor::new_headless();
    let scopes = Scopes::new(compositor.device().clone(), compositor.queue().clone());
    (compositor, scopes)
}

/// An `Rgba8Unorm` texture with `pixel(x, y)` everywhere.
fn source(scopes: &Scopes, w: u32, h: u32, pixel: impl Fn(u32, u32) -> [u8; 4]) -> wgpu::Texture {
    let texture = scopes.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let data: Vec<u8> = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .flat_map(|(x, y)| pixel(x, y))
        .collect();
    scopes.queue.write_texture(
        texture.as_image_copy(),
        &data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(w * 4),
            rows_per_image: Some(h),
        },
        texture.size(),
    );
    texture
}

fn read_buffer(scopes: &Scopes, buffer: &wgpu::Buffer, size: u64) -> Vec<u8> {
    let staging = scopes.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = scopes
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    encoder.copy_buffer_to_buffer(buffer, 0, &staging, 0, size);
    scopes.queue.submit(Some(encoder.finish()));
    let slice = staging.slice(..);
    slice.map_async(wgpu::MapMode::Read, |r| r.unwrap());
    scopes
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    let data = slice.get_mapped_range().unwrap().to_vec();
    staging.unmap();
    data
}

fn bins(scopes: &Scopes) -> Vec<u32> {
    bytemuck::cast_slice(&read_buffer(scopes, &scopes.bins, BIN_COUNT * 4)).to_vec()
}

/// The scope picture's pixels, as `[r, g, b, a]`.
fn picture(scopes: &Scopes, texture: &wgpu::Texture) -> Vec<[u8; 4]> {
    let (w, h) = (texture.width(), texture.height());
    let row =
        (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = scopes.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (row * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let mut encoder = scopes
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(h),
            },
        },
        texture.size(),
    );
    scopes.queue.submit(Some(encoder.finish()));
    let data = read_buffer(scopes, &buffer, (row * h) as u64);
    (0..h)
        .flat_map(|y| {
            let start = (y * row) as usize;
            data[start..start + (w * 4) as usize]
                .chunks(4)
                .map(|p| [p[0], p[1], p[2], p[3]])
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn a_flat_grey_puts_every_waveform_column_on_one_level() {
    let (_c, scopes) = scopes();
    let grey = source(&scopes, 64, 32, |_, _| [128, 128, 128, 255]);
    scopes.render(&grey, ScopeKind::Waveform, 0, (64, 256));
    let bins = bins(&scopes);
    for col in 0..64 {
        assert_eq!(bins[128 * 64 + col], 32, "column {col}");
    }
    assert_eq!(bins.iter().map(|b| *b as u64).sum::<u64>(), 64 * 32);
}

#[test]
fn a_horizontal_gradient_draws_a_waveform_diagonal() {
    let (_c, scopes) = scopes();
    let gradient = source(&scopes, 256, 8, |x, _| [x as u8, x as u8, x as u8, 255]);
    let texture = scopes.render(&gradient, ScopeKind::Waveform, 0, (256, 256));
    let bins = bins(&scopes);
    for col in [0usize, 100, 255] {
        assert_eq!(bins[col * 256 + col], 8, "level {col} in column {col}");
    }
    let pixels = picture(&scopes, &texture);
    let at = |x: usize, y: usize| pixels[y * 256 + x];
    assert!(at(200, 255 - 200)[1] > 100, "the trace is drawn");
    assert_eq!(at(200, 255 - 50), [0, 0, 0, 255], "nothing elsewhere");
}

#[test]
fn the_parade_splits_the_channels() {
    let (_c, scopes) = scopes();
    let red = source(&scopes, 32, 16, |_, _| [255, 0, 0, 255]);
    scopes.render(&red, ScopeKind::Parade, 0, (96, 256));
    let bins = bins(&scopes);
    let bins_w = 32;
    let count = |channel: usize, level: usize| {
        (0..bins_w)
            .map(|col| bins[(channel * 256 + level) * bins_w + col])
            .sum::<u32>()
    };
    assert_eq!(count(0, 255), 32 * 16);
    assert_eq!(count(1, 0), 32 * 16);
    assert_eq!(count(2, 0), 32 * 16);
}

#[test]
fn primaries_land_on_their_vectorscope_targets() {
    let (_c, scopes) = scopes();
    for (rgb, quadrant) in [
        ([255u8, 0, 0], (false, true)),
        ([0, 0, 255], (true, false)),
        ([0, 255, 0], (false, false)),
    ] {
        let flat = source(&scopes, 16, 16, |_, _| [rgb[0], rgb[1], rgb[2], 255]);
        scopes.render(&flat, ScopeKind::Vectorscope, 0, (256, 256));
        let bins = bins(&scopes);
        let index = bins.iter().position(|b| *b == 256).expect("one bin");
        let (cb, cr) = (index % 256, index / 256);
        assert_eq!((cb > 128, cr > 128), quadrant, "{rgb:?}: cb {cb} cr {cr}");
    }
}

#[test]
fn a_neutral_grey_sits_at_the_vectorscope_center() {
    let (_c, scopes) = scopes();
    let grey = source(&scopes, 16, 16, |_, _| [90, 90, 90, 255]);
    scopes.render(&grey, ScopeKind::Vectorscope, 0, (128, 128));
    let bins = bins(&scopes);
    assert_eq!(bins[128 * 256 + 128], 256);
}

#[test]
fn a_gradient_histogram_is_flat_and_counts_every_pixel() {
    let (_c, scopes) = scopes();
    let gradient = source(&scopes, 256, 4, |x, _| [x as u8, 0, 255, 255]);
    scopes.render(&gradient, ScopeKind::Histogram, 0, (256, 128));
    let bins = bins(&scopes);
    assert!(bins[..256].iter().all(|b| *b == 4), "red: flat");
    assert_eq!(bins[256], 256 * 4, "green: all at 0");
    assert_eq!(bins[2 * 256 + 255], 256 * 4, "blue: all at 255");
    assert_eq!(bins[3 * 256..4 * 256].iter().sum::<u32>(), 256 * 4, "luma");
    assert_eq!(bins[1024], 256 * 4, "tallest bin");
}

#[test]
fn a_large_source_is_sampled_down() {
    let (_c, scopes) = scopes();
    let large = source(&scopes, 2400, 10, |_, _| [128, 128, 128, 255]);
    scopes.render(&large, ScopeKind::Histogram, 0, (64, 64));
    let bins = bins(&scopes);
    assert_eq!(bins[3 * 256 + 128], 2400u32.div_ceil(3) * 10u32.div_ceil(3));
}
