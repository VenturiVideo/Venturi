//! How the decoded frame of a clip is obtained: a cache filled in the
//! background (preview, `render_ahead.rs`) or synchronous decoding (export,
//! `export.rs`). The clip -> source frame mapping is a single one.

use std::sync::Arc;
use vv_core::{
    Clip, ClipSource, FrameIdx, MediaId, Project, Rgba, Timeline, TitleParams, Transform,
};
use vv_media::FrameYuv420;

use crate::export::ExportError;

/// `&mut self`: the export keeps the decoders open.
pub trait FrameProvider {
    /// `Ok(None)` = not available now (preview: not cached yet;
    /// export: past the end of the file). A decode error stays `Err`, so
    /// the export does not turn it into a silent black frame.
    fn frame_for(
        &mut self,
        project: &Project,
        clip: &Clip,
        timeline_frame: FrameIdx,
    ) -> Result<Option<Arc<FrameYuv420>>, ExportError>;

    /// The composed frame of `nested` (the nested timeline of a compound
    /// clip) at `local_frame`, as a GPU texture. `None` from the default
    /// implementation: whoever cannot compose on the GPU falls back on
    /// `frame_for`, i.e. on the frame composed and brought back to the CPU.
    /// Whoever implements it composes recursively with `track_layers_at`, passing itself.
    fn compound_texture(
        &mut self,
        _project: &Project,
        _nested: &Timeline,
        _local_frame: FrameIdx,
    ) -> Result<Option<vv_render::PooledTexture>, ExportError> {
        Ok(None)
    }

    /// The frame at `progress` of a Smooth Cut from `from` to `to`, as a GPU
    /// texture of `timeline_size`. `None` from the default implementation:
    /// the caller falls back on a dissolve.
    fn smooth_cut_texture(
        &mut self,
        _from: &OwnedLayer,
        _to: &OwnedLayer,
        _progress: f32,
        _timeline_size: (u32, u32),
    ) -> Option<vv_render::PooledTexture> {
        None
    }

    /// `fill` cut out by `matte` (see `stack_layers`), as a GPU texture of
    /// `timeline_size`. `None` from the default implementation: the caller
    /// shows `fill` as it is.
    fn matte_texture(
        &mut self,
        _fill: &[OwnedLayer],
        _matte: &[OwnedLayer],
        _mode: vv_core::MatteMode,
        _timeline_size: (u32, u32),
    ) -> Option<vv_render::PooledTexture> {
        None
    }
}

/// The nested timeline of `clip`, if it is a compound clip.
fn nested_timeline_of<'a>(project: &'a Project, clip: &Clip) -> Option<&'a Timeline> {
    let ClipSource::Media(media_id) = &clip.source else {
        return None;
    };
    let nested_id = project.media_pool.get(*media_id)?.compound?;
    project.timelines.get(nested_id)
}

/// `(media, source frame)` of `clip` at `timeline_frame`; `None` if it is not
/// a Media clip.
pub fn media_source_frame(clip: &Clip, timeline_frame: FrameIdx) -> Option<(MediaId, FrameIdx)> {
    let ClipSource::Media(media_id) = &clip.source else {
        return None;
    };
    Some((*media_id, clip.picture_frame_at(timeline_frame)))
}

/// *Native* resolution of the media, the unit of the crop even when decoding
/// the proxy. Generators: the timeline's; `(1, 1)` if the media is missing.
pub fn clip_source_size(project: &Project, clip: &Clip, timeline_size: (u32, u32)) -> (u32, u32) {
    match &clip.source {
        ClipSource::Media(id) => project
            .media_pool
            .get(*id)
            .map(|m| (m.meta.width, m.meta.height))
            .unwrap_or((1, 1)),
        ClipSource::SolidColor | ClipSource::Text | ClipSource::Adjustment => timeline_size,
    }
}

/// vv-render does not depend on vv-media: it takes raw byte planes.
pub fn as_render_yuv_frame(frame: &FrameYuv420) -> vv_render::YuvFrame<'_> {
    vv_render::YuvFrame {
        y: &frame.y,
        width: frame.width,
        height: frame.height,
        chroma: match &frame.chroma {
            vv_media::Chroma::Planar { u, v } => vv_render::YuvChroma::Planar { u, v },
            vv_media::Chroma::Interleaved(uv) => vv_render::YuvChroma::Interleaved(uv),
        },
        chroma_width: frame.chroma_width,
        chroma_height: frame.chroma_height,
        matrix: frame.matrix,
        full_range: frame.full_range,
        alpha: frame.alpha.as_deref().unwrap_or(&[255]),
    }
}

/// A compositing layer that owns what `vv_render::Layer` borrows.
pub struct OwnedLayer {
    pub content: OwnedContent,
    pub transform: Transform,
    pub opacity: f32,
    /// Only the active filters of `EffectStack::filters`, in their order.
    pub filters: Vec<vv_core::FilterValue>,
    pub blend: vv_core::BlendMode,
    /// Only the active masks of `EffectStack::masks` (`ClipMask::is_active`).
    pub masks: Vec<vv_core::MaskValue>,
}

pub enum OwnedContent {
    Video {
        frame: Arc<FrameYuv420>,
        /// Native resolution of the media (not of the proxy): the units of the crop.
        source_size: (u32, u32),
    },
    /// Compound clip composed on the GPU (see `FrameProvider::compound_texture`).
    Texture {
        texture: vv_render::PooledTexture,
        /// Resolution of the nested timeline: the units of the crop, like
        /// `Video`'s `source_size` (the texture can be smaller).
        source_size: (u32, u32),
    },
    Solid(Rgba),
    Text(TitleParams),
    Adjustment,
}

impl OwnedLayer {
    pub fn as_render(&self) -> vv_render::Layer<'_> {
        let content = match &self.content {
            OwnedContent::Video { frame, source_size } => vv_render::LayerContent::Video {
                frame: as_render_yuv_frame(frame),
                source_size: *source_size,
            },
            OwnedContent::Texture {
                texture,
                source_size,
            } => vv_render::LayerContent::Texture {
                texture,
                source_size: *source_size,
            },
            OwnedContent::Solid(color) => vv_render::LayerContent::Solid(*color),
            OwnedContent::Text(title) => vv_render::LayerContent::Text(title),
            OwnedContent::Adjustment => vv_render::LayerContent::Adjustment,
        };
        vv_render::Layer {
            content,
            transform: self.transform,
            opacity: self.opacity,
            filters: &self.filters,
            blend: self.blend,
            masks: &self.masks,
        }
    }

    /// `true` if composing it gives the same pixels as `other`. Frames are
    /// compared by identity: equal only while `other` holds its `Arc`. A
    /// compound clip's texture is composed anew every time, never equal.
    fn renders_same(&self, other: &OwnedLayer) -> bool {
        let same_content = match (&self.content, &other.content) {
            (
                OwnedContent::Video { frame, source_size },
                OwnedContent::Video {
                    frame: frame2,
                    source_size: source_size2,
                },
            ) => Arc::ptr_eq(frame, frame2) && source_size == source_size2,
            (OwnedContent::Solid(color), OwnedContent::Solid(color2)) => color == color2,
            (OwnedContent::Text(title), OwnedContent::Text(title2)) => title == title2,
            // What it adjusts is the rest of the stack, compared on its own.
            (OwnedContent::Adjustment, OwnedContent::Adjustment) => true,
            _ => false,
        };
        same_content
            && self.transform == other.transform
            && self.opacity == other.opacity
            && self.filters == other.filters
            && self.blend == other.blend
            && self.masks == other.masks
    }
}

/// `true` if the two stacks compose to the same pixels (see
/// `OwnedLayer::renders_same`).
pub fn renders_same(a: &[OwnedLayer], b: &[OwnedLayer]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.renders_same(b))
}

/// Adds to any provider the GPU composition of compound clips: the nested
/// timeline becomes a texture that stays on the card, instead of a composed
/// frame brought back to the CPU and converted to YUV again.
/// `inner` provides the real media, which the rest of the pipeline decodes
/// as always (including those inside the nested timelines).
pub struct GpuCompounds<'a> {
    inner: &'a mut dyn FrameProvider,
    compositor: &'a vv_render::Compositor,
    depth: u32,
    preview_divisor: u32,
}

impl<'a> GpuCompounds<'a> {
    pub fn new(inner: &'a mut dyn FrameProvider, compositor: &'a vv_render::Compositor) -> Self {
        Self {
            inner,
            compositor,
            depth: 0,
            preview_divisor: 1,
        }
    }

    /// Composes the compound clips at most at their resolution divided by
    /// `divisor` (see `OutputFrame::reduced`).
    pub fn with_preview_divisor(self, divisor: u32) -> Self {
        Self {
            preview_divisor: divisor,
            ..self
        }
    }
}

impl FrameProvider for GpuCompounds<'_> {
    fn frame_for(
        &mut self,
        project: &Project,
        clip: &Clip,
        timeline_frame: FrameIdx,
    ) -> Result<Option<Arc<FrameYuv420>>, ExportError> {
        self.inner.frame_for(project, clip, timeline_frame)
    }

    fn compound_texture(
        &mut self,
        project: &Project,
        nested: &Timeline,
        local_frame: FrameIdx,
    ) -> Result<Option<vv_render::PooledTexture>, ExportError> {
        if self.depth >= vv_core::MAX_COMPOUND_DEPTH {
            return Ok(None);
        }
        self.depth += 1;
        let layers = nested_layers(project, nested, local_frame, self);
        self.depth -= 1;
        let Some(layers) = layers? else {
            return Ok(None);
        };
        let render_layers: Vec<vv_render::Layer> =
            layers.iter().map(OwnedLayer::as_render).collect();
        let (width, height) = nested.resolution;
        // Transparent background: where `nested` has nothing to show, what is
        // below in the outer timeline must stay visible. And the texture does
        // not go back to the pool, because we hold it until the pass.
        Ok(Some(
            self.compositor.render_layers_to_owned_texture_transparent(
                &render_layers,
                vv_render::OutputFrame::exact(width, height).reduced(self.preview_divisor),
            ),
        ))
    }

    fn smooth_cut_texture(
        &mut self,
        from: &OwnedLayer,
        to: &OwnedLayer,
        progress: f32,
        (width, height): (u32, u32),
    ) -> Option<vv_render::PooledTexture> {
        Some(self.compositor.render_smooth_cut_to_owned_texture(
            &from.as_render(),
            &to.as_render(),
            progress,
            vv_render::OutputFrame::exact(width, height).reduced(self.preview_divisor),
        ))
    }

    fn matte_texture(
        &mut self,
        fill: &[OwnedLayer],
        matte: &[OwnedLayer],
        mode: vv_core::MatteMode,
        (width, height): (u32, u32),
    ) -> Option<vv_render::PooledTexture> {
        let fill: Vec<_> = fill.iter().map(OwnedLayer::as_render).collect();
        let matte: Vec<_> = matte.iter().map(OwnedLayer::as_render).collect();
        Some(self.compositor.render_matted_to_owned_texture(
            &fill,
            &matte,
            mode,
            vv_render::OutputFrame::exact(width, height).reduced(self.preview_divisor),
        ))
    }
}

/// The layers of `nested` at `local_frame`, or `None` if a clip covering that
/// frame does not have its content yet (a media being decoded): a half-composed
/// frame would be worse than no frame. No active clip is a valid case,
/// not a "not ready": it gives a transparent frame.
fn nested_layers(
    project: &Project,
    nested: &Timeline,
    local_frame: FrameIdx,
    provider: &mut dyn FrameProvider,
) -> Result<Option<Vec<OwnedLayer>>, ExportError> {
    let mut groups = Vec::new();
    for (track_index, clip) in nested.active_video_clips_at(local_frame) {
        let clip_layers = track_layers_at(
            project,
            nested,
            track_index,
            clip,
            local_frame,
            nested.resolution,
            provider,
        )?;
        if clip_layers.is_empty() {
            return Ok(None);
        }
        groups.push((clip.effects.matte, clip_layers));
    }
    Ok(Some(stack_layers(groups, provider, nested.resolution)))
}

/// The stack from the layers of each track, bottom to top, each with the
/// matte mode of its clip: a matte is not shown, it cuts out the track
/// below it instead. A matte with nothing below is dropped. An
/// adjustment below stands for the whole stack it adjusts.
pub fn stack_layers(
    groups: impl IntoIterator<Item = (Option<vv_core::MatteMode>, Vec<OwnedLayer>)>,
    provider: &mut dyn FrameProvider,
    timeline_size: (u32, u32),
) -> Vec<OwnedLayer> {
    let mut stack: Vec<Vec<OwnedLayer>> = Vec::new();
    for (matte, layers) in groups {
        let Some(mode) = matte else {
            stack.push(layers);
            continue;
        };
        let Some(mut fill) = stack.pop() else {
            continue;
        };
        // An adjustment replaces the stack with its adjusted version: that is
        // what the matte cuts out.
        if fill
            .iter()
            .any(|l| matches!(l.content, OwnedContent::Adjustment))
        {
            let adjusted: Vec<OwnedLayer> = stack.drain(..).flatten().chain(fill).collect();
            match provider.matte_texture(&adjusted, &layers, mode, timeline_size) {
                Some(texture) => stack.push(vec![OwnedLayer {
                    content: OwnedContent::Texture {
                        texture,
                        source_size: timeline_size,
                    },
                    transform: Transform::default(),
                    opacity: 1.0,
                    filters: Vec::new(),
                    blend: vv_core::BlendMode::Normal,
                    masks: Vec::new(),
                }]),
                None => stack.push(adjusted),
            }
            continue;
        }
        let Some(blend) = fill.last().map(|l| l.blend) else {
            continue;
        };
        // Composed alone, the fill blends onto the stack only once it is cut out.
        let blends: Vec<_> = fill
            .iter_mut()
            .map(|l| std::mem::take(&mut l.blend))
            .collect();
        let Some(texture) = provider.matte_texture(&fill, &layers, mode, timeline_size) else {
            for (layer, blend) in fill.iter_mut().zip(blends) {
                layer.blend = blend;
            }
            stack.push(fill);
            continue;
        };
        stack.push(vec![OwnedLayer {
            content: OwnedContent::Texture {
                texture,
                source_size: timeline_size,
            },
            transform: Transform::default(),
            opacity: 1.0,
            filters: Vec::new(),
            blend,
            masks: Vec::new(),
        }]);
    }
    stack.into_iter().flatten().collect()
}

/// The layer of `clip` at timeline frame `frame`, shared by preview
/// and export. `Ok(None)`: media frame not available.
pub fn clip_layer(
    project: &Project,
    clip: &Clip,
    frame: FrameIdx,
    timeline_size: (u32, u32),
    timeline_fps: vv_core::Rational,
    provider: &mut dyn FrameProvider,
) -> Result<Option<OwnedLayer>, ExportError> {
    let source_frame = clip.source_frame_at(frame);
    let transform = clip.effects.transform.value_at(source_frame);
    let push = clip.transition_offset_at(
        frame,
        (timeline_size.0 as f32, timeline_size.1 as f32),
        transform.zoom,
    );
    let opacity = clip.fade_multiplier_at(frame) * clip.transition_opacity_at(frame);
    let content = clip_content(project, clip, frame, provider)?;
    let mut layer = build_layer(
        project,
        clip,
        source_frame,
        (clip.source_offset as f64 + frame as f64 - clip.timeline_start as f64)
            / clip.rate().as_f64(),
        transform,
        opacity,
        content,
        timeline_size,
        timeline_fps,
    );
    if let Some(layer) = &mut layer {
        layer.transform.position[0] += push[0];
        layer.transform.position[1] += push[1];
    }
    Ok(layer)
}

/// What a clip shows at `timeline_frame`: nothing if it is not a Media clip,
/// the composed texture if it is a compound clip and the provider can compose
/// it on the GPU, otherwise the decoded frame (or composed and brought back to the CPU).
fn clip_content(
    project: &Project,
    clip: &Clip,
    timeline_frame: FrameIdx,
    provider: &mut dyn FrameProvider,
) -> Result<ClipContent, ExportError> {
    if !matches!(clip.source, ClipSource::Media(_)) {
        return Ok(ClipContent::None);
    }
    if let Some(nested) = nested_timeline_of(project, clip)
        && let Some(texture) =
            provider.compound_texture(project, nested, clip.picture_frame_at(timeline_frame))?
    {
        return Ok(ClipContent::Texture(texture));
    }
    Ok(provider
        .frame_for(project, clip, timeline_frame)?
        .map_or(ClipContent::None, ClipContent::Yuv))
}

/// The already obtained content of a clip, see `clip_content`.
enum ClipContent {
    None,
    Yuv(Arc<FrameYuv420>),
    Texture(vv_render::PooledTexture),
}

/// The part common to `clip_layer` and to the side of a crossing transition
/// (`crossing_side_layer`): given the source frame, the already computed
/// transform and the already decoded media frame (if `ClipSource::Media`),
/// it assembles the right `OwnedLayer` for the clip's source kind.
fn build_layer(
    project: &Project,
    clip: &Clip,
    source_frame: FrameIdx,
    source_position: f64,
    mut transform: Transform,
    opacity: f32,
    content: ClipContent,
    timeline_size: (u32, u32),
    timeline_fps: vv_core::Rational,
) -> Option<OwnedLayer> {
    let filters: Vec<vv_core::FilterValue> = clip
        .effects
        .filters
        .iter()
        .filter(|f| f.enabled && f.kind != vv_core::FilterKind::CameraShake)
        .map(|f| f.value_at(source_frame))
        .collect();
    let source_fps = match clip.source {
        ClipSource::Media(id) => project
            .media_pool
            .get(id)
            .map_or(timeline_fps.as_f64(), |m| m.meta.fps.as_f64()),
        _ => timeline_fps.as_f64(),
    };
    for filter in clip
        .effects
        .filters
        .iter()
        .filter(|f| f.enabled && f.kind == vv_core::FilterKind::CameraShake)
    {
        filter
            .shake
            .apply_at(&mut transform, source_position, source_fps, timeline_size);
    }
    let masks = clip
        .effects
        .masks
        .iter()
        .filter(|m| m.is_active())
        .map(|m| m.value_at(source_frame))
        .collect();
    let blend = clip.effects.blend_mode;
    // The clip opacity is multiplied by the one already carried by the
    // fades.
    let opacity = opacity * (transform.opacity / 100.0).clamp(0.0, 1.0);
    let content = match &clip.source {
        ClipSource::SolidColor => OwnedContent::Solid(
            clip.effects
                .color
                .as_ref()
                .map_or(Rgba::BLACK, |k| k.value_at(source_frame)),
        ),
        ClipSource::Text => OwnedContent::Text(clip.effects.title.as_ref()?.at(source_frame)),
        ClipSource::Adjustment => OwnedContent::Adjustment,
        ClipSource::Media(_) => {
            let source_size = clip_source_size(project, clip, timeline_size);
            match content {
                ClipContent::None => return None,
                ClipContent::Yuv(frame) => OwnedContent::Video { frame, source_size },
                ClipContent::Texture(texture) => OwnedContent::Texture {
                    texture,
                    source_size,
                },
            }
        }
    };
    Some(OwnedLayer {
        content,
        transform,
        opacity,
        filters,
        blend,
        masks,
    })
}

/// The timeline frame to take the content from: `timeline_frame`, or that of
/// the nearest source frame if it falls past the real edges of the media
/// (freeze instead of nothing). Used only by the crossing transitions,
/// where "past the end" is the norm — it is the clip lending its edge to the
/// transition — not an error to report as `clip_layer` does on export.
fn held_timeline_frame(project: &Project, clip: &Clip, timeline_frame: FrameIdx) -> FrameIdx {
    let ClipSource::Media(media_id) = &clip.source else {
        return timeline_frame;
    };
    let Some(media) = project.media_pool.get(*media_id) else {
        return timeline_frame;
    };
    let wanted = clip.picture_frame_at(timeline_frame);
    let clamped = wanted.clamp(0, (media.meta.duration_frames - 1).max(0));
    clip.timeline_frame_at(clamped)
}

/// One side of a crossing transition for a single clip: like `clip_layer`,
/// but the source frame clamps to the edges of the media instead of
/// disappearing, and the position offset is `CrossTransition`'s instead of
/// `Clip::transition_offset_at` (which concerns only the single edges,
/// `transition_in`/`transition_out`).
fn crossing_side_layer(
    project: &Project,
    clip: &Clip,
    frame: FrameIdx,
    timeline_size: (u32, u32),
    timeline_fps: vv_core::Rational,
    provider: &mut dyn FrameProvider,
    extra_offset: [f32; 2],
    extra_opacity: f32,
) -> Result<Option<OwnedLayer>, ExportError> {
    let source_frame = clip.source_frame_at(frame);
    let transform = clip.effects.transform.value_at(source_frame);
    let opacity = clip.fade_multiplier_at(frame) * extra_opacity;
    let content = clip_content(
        project,
        clip,
        held_timeline_frame(project, clip, frame),
        provider,
    )?;
    let mut layer = build_layer(
        project,
        clip,
        source_frame,
        (clip.source_offset as f64 + frame as f64 - clip.timeline_start as f64)
            / clip.rate().as_f64(),
        transform,
        opacity,
        content,
        timeline_size,
        timeline_fps,
    );
    if let Some(layer) = &mut layer {
        layer.transform.position[0] += extra_offset[0];
        layer.transform.position[1] += extra_offset[1];
    }
    Ok(layer)
}

/// The layers of an active crossing transition at `frame`: tail of `left`,
/// then head of `right` (in that order: `right` on top is what a dissolve
/// fades in).
fn crossing_layers(
    project: &Project,
    left: &Clip,
    right: &Clip,
    crossing: &vv_core::CrossTransition,
    frame: FrameIdx,
    timeline_size: (u32, u32),
    timeline_fps: vv_core::Rational,
    provider: &mut dyn FrameProvider,
) -> Result<Vec<OwnedLayer>, ExportError> {
    let progress = crossing.eased_progress_at(frame, left, right);
    if crossing.transition.kind == vv_core::TransitionKind::SmoothCut {
        return smooth_cut_layers(
            project,
            left,
            right,
            crossing,
            progress,
            timeline_size,
            timeline_fps,
            provider,
        );
    }
    let frame_size = (timeline_size.0 as f32, timeline_size.1 as f32);
    // The zoom of each clip at its own source frame: `offsets` needs it to
    // really clear the screen even if one of the two (or both) is zoomed
    // — see `push_clearance`. Recomputed here and again inside
    // `crossing_side_layer`: it costs very little (`Keyframed`), not worth
    // threading through as an extra parameter in several places.
    let left_zoom = left
        .effects
        .transform
        .value_at(left.source_frame_at(frame))
        .zoom;
    let right_zoom = right
        .effects
        .transform
        .value_at(right.source_frame_at(frame))
        .zoom;
    let (left_offset, right_offset) = crossing.offsets(progress, frame_size, left_zoom, right_zoom);
    let (left_opacity, right_opacity) = crossing.opacities(progress);
    let mut layers = Vec::with_capacity(2);
    layers.extend(crossing_side_layer(
        project,
        left,
        frame,
        timeline_size,
        timeline_fps,
        provider,
        left_offset,
        left_opacity,
    )?);
    layers.extend(crossing_side_layer(
        project,
        right,
        frame,
        timeline_size,
        timeline_fps,
        provider,
        right_offset,
        right_opacity,
    )?);
    Ok(layers)
}

/// A Smooth Cut morphs the last frame before its window into the first
/// after it, both held for the whole window. Nothing until both are
/// available; a dissolve between them where the provider cannot morph.
fn smooth_cut_layers(
    project: &Project,
    left: &Clip,
    right: &Clip,
    crossing: &vv_core::CrossTransition,
    progress: f32,
    timeline_size: (u32, u32),
    timeline_fps: vv_core::Rational,
    provider: &mut dyn FrameProvider,
) -> Result<Vec<OwnedLayer>, ExportError> {
    let window = crossing.window(left, right);
    let side = |clip: &Clip, frame: FrameIdx, provider: &mut dyn FrameProvider| {
        crossing_side_layer(
            project,
            clip,
            frame,
            timeline_size,
            timeline_fps,
            provider,
            [0.0; 2],
            1.0,
        )
    };
    let (Some(from), Some(mut to)) = (
        side(left, window.start, provider)?,
        side(right, window.end - 1, provider)?,
    ) else {
        return Ok(Vec::new());
    };
    if let Some(texture) = provider.smooth_cut_texture(&from, &to, progress, timeline_size) {
        return Ok(vec![OwnedLayer {
            content: OwnedContent::Texture {
                texture,
                source_size: timeline_size,
            },
            transform: Transform::default(),
            opacity: 1.0,
            filters: Vec::new(),
            blend: from.blend,
            masks: Vec::new(),
        }]);
    }
    to.opacity *= progress;
    Ok(vec![from, to])
}

/// The layers of `clip` (on track `track_index`) at timeline frame
/// `frame`: only one in the normal case, but two if `frame` falls in the
/// window of a valid crossing transition involving this clip —
/// the other half of the pair adds itself, pushed according to the
/// shared transition. The entry point to prefer over `clip_layer`
/// wherever a whole track is composed (preview and export), not just
/// an isolated clip.
pub fn track_layers_at(
    project: &Project,
    timeline: &Timeline,
    track_index: usize,
    clip: &Clip,
    frame: FrameIdx,
    timeline_size: (u32, u32),
    provider: &mut dyn FrameProvider,
) -> Result<Vec<OwnedLayer>, ExportError> {
    let track = &timeline.tracks[track_index];
    if let Some((left, right, crossing)) = track.crossing_at(frame)
        && (left.id == clip.id || right.id == clip.id)
    {
        return crossing_layers(
            project,
            left,
            right,
            crossing,
            frame,
            timeline_size,
            timeline.fps,
            provider,
        );
    }
    Ok(
        clip_layer(project, clip, frame, timeline_size, timeline.fps, provider)?
            .into_iter()
            .collect(),
    )
}

#[cfg(test)]
#[path = "tests/frame_provider.rs"]
mod tests;
