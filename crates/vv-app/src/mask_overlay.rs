//! Handles of the mask picked in the properties panel, drawn on the viewer
//! in place of the transform's. A mask lives in the layer space of its clip
//! (see `vv_core::mask`), so its points go through the clip's transform
//! like the transform handles do.
//!
//! Path masks: drag a vertex or its bezier handles (Alt breaks the
//! symmetry), click an edge to add a vertex, Alt+click a vertex to remove
//! it, double click to make it smooth or a corner. While drawing (pen),
//! each click adds a vertex, dragging pulls out its handles, and a click on
//! the first vertex, Enter or Esc closes the path.

use vv_core::{ClipMask, FrameIdx, MaskParam, MaskPath, MaskShape, PathPoint, Transform};

use crate::mask_panel::set_track_value;
use crate::viewer_overlay::{
    clip_to_frame, clockwise_turn, distance_to_segment, frame_to_clip, point_in_convex, rotate_cw,
};

const HANDLE_RADIUS: f32 = 4.5;
const GRAB: f32 = HANDLE_RADIUS + 4.0;
/// On-screen distance of the rotation knob past the top side.
const ROTATION_ARM: f32 = 30.0;
const ROTATION_SNAP: f32 = 15.0;
const ELLIPSE_SEGMENTS: usize = 64;
const EDGE_SAMPLES: usize = 16;
const MIN_SIZE: f32 = 1.0;
const MIN_PATH_POINTS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Handle {
    Move,
    Rotate,
    /// Corner of a rectangle/ellipse box: (±1, ±1).
    Corner(f32, f32),
    /// Side: (±1, 0) or (0, ±1).
    Side(f32, f32),
    Vertex(usize),
    TangentIn(usize),
    TangentOut(usize),
    /// Vertex just placed by the pen: dragging pulls out its handles.
    NewVertex(usize),
}

#[derive(Debug, Clone)]
struct MaskDrag {
    handle: Handle,
    /// Layer point under the pointer at the press.
    start_pointer: [f32; 2],
    center: [f32; 2],
    size: [f32; 2],
    rotation: f32,
    path: MaskPath,
    last_screen: egui::Pos2,
    turned: f32,
}

#[derive(Debug, Default)]
pub(crate) struct MaskOverlayState {
    drag: Option<MaskDrag>,
    /// Vertex whose bezier handles are shown.
    selected_vertex: Option<usize>,
    /// Pen tool active on a path mask.
    pub(crate) drawing: bool,
}

impl MaskOverlayState {
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }
}

fn add(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] + b[0], a[1] + b[1]]
}

fn sub(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] - b[0], a[1] - b[1]]
}

/// Mask-local point (relative to center and rotation) to layer and back.
#[derive(Clone, Copy)]
struct Frame {
    center: [f32; 2],
    rotation: f32,
}

impl Frame {
    fn to_layer(self, local: [f32; 2]) -> [f32; 2] {
        add(self.center, rotate_cw(local, self.rotation))
    }

    fn to_local(self, layer: [f32; 2]) -> [f32; 2] {
        rotate_cw(sub(layer, self.center), -self.rotation)
    }
}

/// Tangent handles of vertex `i` from its neighbours, for a smooth vertex.
fn auto_handles(points: &[PathPoint], i: usize) -> ([f32; 2], [f32; 2]) {
    let n = points.len();
    let prev = points[(i + n - 1) % n].point;
    let next = points[(i + 1) % n].point;
    let t = [(next[0] - prev[0]) / 6.0, (next[1] - prev[1]) / 6.0];
    ([-t[0], -t[1]], t)
}

/// Draws the handles of `mask` and handles their dragging, which can start
/// anywhere in `area`. Returns the edited mask if the user changed it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn show(
    ui: &egui::Ui,
    frame_rect: egui::Rect,
    area: egui::Rect,
    timeline_size: (u32, u32),
    transform: &Transform,
    mask: &ClipMask,
    frame: FrameIdx,
    state: &mut MaskOverlayState,
) -> Option<ClipMask> {
    let scale = frame_rect.width() / timeline_size.0.max(1) as f32;
    let center_px = frame_rect.center();
    let to_screen = |p: [f32; 2]| {
        let f = clip_to_frame(transform, p);
        center_px + egui::vec2(f[0] * scale, -f[1] * scale)
    };
    let from_screen = |pos: egui::Pos2| {
        frame_to_clip(
            transform,
            [
                (pos.x - center_px.x) / scale,
                -(pos.y - center_px.y) / scale,
            ],
        )
    };

    let value = mask.value_at(frame);
    let path = mask.path.value_at(frame);
    let mask_frame = Frame {
        center: value.center,
        rotation: value.rotation,
    };
    let is_path = mask.shape == MaskShape::Path;
    if !is_path {
        state.drawing = false;
    }
    let (w, h) = (value.size[0], value.size[1]);
    let box_point = |sx: f32, sy: f32| to_screen(mask_frame.to_layer([sx * w / 2.0, sy * h / 2.0]));
    let corners = [(-1.0, 1.0), (1.0, 1.0), (1.0, -1.0), (-1.0, -1.0)];
    let sides = [(0.0, 1.0), (1.0, 0.0), (0.0, -1.0), (-1.0, 0.0)];
    let center_screen = to_screen(value.center);
    let top = box_point(0.0, 1.0);
    let up = (top - center_screen).normalized();
    let up = if up.is_finite() { up } else { -egui::Vec2::Y };
    let knob = top + up * ROTATION_ARM;
    let vertices: Vec<egui::Pos2> = path
        .points
        .iter()
        .map(|p| to_screen(mask_frame.to_layer(p.point)))
        .collect();
    let handle_screen = |i: usize, offset: [f32; 2]| {
        to_screen(mask_frame.to_layer(add(path.points[i].point, offset)))
    };
    let outline: Vec<egui::Pos2> = match mask.shape {
        MaskShape::Rectangle => corners.iter().map(|&(sx, sy)| box_point(sx, sy)).collect(),
        MaskShape::Ellipse => (0..ELLIPSE_SEGMENTS)
            .map(|i| {
                let a = i as f32 / ELLIPSE_SEGMENTS as f32 * std::f32::consts::TAU;
                to_screen(mask_frame.to_layer([a.cos() * w / 2.0, a.sin() * h / 2.0]))
            })
            .collect(),
        MaskShape::Path => path
            .flatten()
            .into_iter()
            .map(|p| to_screen(mask_frame.to_layer(p)))
            .collect(),
    };
    let selected = state.selected_vertex.filter(|i| *i < path.points.len());

    let handle_at = |pos: egui::Pos2| -> Option<Handle> {
        if is_path {
            if let Some(i) = selected {
                let p = path.points[i];
                if pos.distance(handle_screen(i, p.in_handle)) <= GRAB {
                    return Some(Handle::TangentIn(i));
                }
                if pos.distance(handle_screen(i, p.out_handle)) <= GRAB {
                    return Some(Handle::TangentOut(i));
                }
            }
            if let Some(i) = vertices.iter().position(|v| pos.distance(*v) <= GRAB) {
                return Some(Handle::Vertex(i));
            }
            return polygon_contains(pos, &outline).then_some(Handle::Move);
        }
        if pos.distance(knob) <= GRAB {
            return Some(Handle::Rotate);
        }
        if let Some(&(sx, sy)) = corners
            .iter()
            .find(|&&(sx, sy)| pos.distance(box_point(sx, sy)) <= GRAB)
        {
            return Some(Handle::Corner(sx, sy));
        }
        if let Some(&(sx, sy)) = sides
            .iter()
            .find(|&&(sx, sy)| pos.distance(box_point(sx, sy)) <= GRAB)
        {
            return Some(Handle::Side(sx, sy));
        }
        let box_corners = corners.map(|(sx, sy)| box_point(sx, sy));
        point_in_convex(pos, &box_corners).then_some(Handle::Move)
    };

    let resp = ui.interact(
        area,
        ui.id().with("viewer_mask_overlay"),
        egui::Sense::click_and_drag(),
    );
    let (pressed, alt, shift, end_keys) = ui.input(|i| {
        (
            i.pointer.primary_pressed(),
            i.modifiers.alt,
            i.modifiers.shift,
            i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Escape),
        )
    });
    let mut new_path: Option<MaskPath> = None;
    let mut new_box: Option<([f32; 2], [f32; 2], f32)> = None;

    if state.drawing && end_keys {
        state.drawing = false;
    }

    if pressed
        && resp.contains_pointer()
        && let Some(press) = ui.input(|i| i.pointer.press_origin())
    {
        let layer = from_screen(press);
        let start = |handle| MaskDrag {
            handle,
            start_pointer: layer,
            center: value.center,
            size: value.size,
            rotation: value.rotation,
            path: path.clone(),
            last_screen: press,
            turned: 0.0,
        };
        state.drag = None;
        if state.drawing {
            if path.points.len() >= MIN_PATH_POINTS
                && vertices.first().is_some_and(|v| press.distance(*v) <= GRAB)
            {
                state.drawing = false;
            } else {
                let mut p = path.clone();
                p.points.push(PathPoint::corner(mask_frame.to_local(layer)));
                let index = p.points.len() - 1;
                state.selected_vertex = Some(index);
                state.drag = Some(MaskDrag {
                    path: p.clone(),
                    ..start(Handle::NewVertex(index))
                });
                new_path = Some(p);
            }
        } else {
            match handle_at(press) {
                Some(Handle::Vertex(i)) if alt => {
                    if path.points.len() > MIN_PATH_POINTS {
                        let mut p = path.clone();
                        p.points.remove(i);
                        state.selected_vertex = None;
                        new_path = Some(p);
                    }
                }
                Some(handle) => {
                    if let Handle::Vertex(i) = handle {
                        state.selected_vertex = Some(i);
                    }
                    state.drag = Some(start(handle));
                }
                None if is_path => {
                    if let Some(i) =
                        nearest_edge(press, &path, &|p| to_screen(mask_frame.to_layer(p)))
                    {
                        let mut p = path.clone();
                        p.points
                            .insert(i + 1, PathPoint::corner(mask_frame.to_local(layer)));
                        state.selected_vertex = Some(i + 1);
                        state.drag = Some(MaskDrag {
                            path: p.clone(),
                            ..start(Handle::Vertex(i + 1))
                        });
                        new_path = Some(p);
                    }
                }
                None => {}
            }
        }
    }

    if resp.double_clicked()
        && !state.drawing
        && let Some(pos) = resp.interact_pointer_pos()
        && let Some(Handle::Vertex(i)) = handle_at(pos)
    {
        let mut p = path.clone();
        let point = &mut p.points[i];
        if point.in_handle == [0.0; 2] && point.out_handle == [0.0; 2] {
            (point.in_handle, point.out_handle) = auto_handles(&path.points, i);
        } else {
            point.in_handle = [0.0; 2];
            point.out_handle = [0.0; 2];
        }
        new_path = Some(p);
    }

    let pointer_down = ui.input(|i| i.pointer.primary_down());
    if let Some(d) = state.drag.as_mut()
        && pointer_down
        && let Some(pos) = ui.input(|i| i.pointer.interact_pos())
        && pos != d.last_screen
    {
        let layer = from_screen(pos);
        let start_frame = Frame {
            center: d.center,
            rotation: d.rotation,
        };
        let local = start_frame.to_local(layer);
        let start_local = start_frame.to_local(d.start_pointer);
        match d.handle {
            Handle::Move => {
                let delta = sub(layer, d.start_pointer);
                new_box = Some((add(d.center, delta), d.size, d.rotation));
            }
            Handle::Rotate => {
                let pivot = to_screen(d.center);
                let from = d.last_screen - pivot;
                let to = pos - pivot;
                d.turned += clockwise_turn([from.x, -from.y], [to.x, -to.y]);
                let mut rotation = d.rotation + d.turned;
                if shift {
                    rotation = (rotation / ROTATION_SNAP).round() * ROTATION_SNAP;
                }
                new_box = Some((d.center, d.size, rotation));
            }
            Handle::Corner(sx, sy) | Handle::Side(sx, sy) => {
                let (w0, h0) = (d.size[0], d.size[1]);
                let opposite = [-sx * w0 / 2.0, -sy * h0 / 2.0];
                let mut size = d.size;
                let mut mid = [0.0, 0.0];
                if sx != 0.0 {
                    size[0] = (local[0] - opposite[0]).abs().max(MIN_SIZE);
                    mid[0] = (local[0] + opposite[0]) / 2.0;
                }
                if sy != 0.0 {
                    size[1] = (local[1] - opposite[1]).abs().max(MIN_SIZE);
                    mid[1] = (local[1] + opposite[1]) / 2.0;
                }
                if shift && sx != 0.0 && sy != 0.0 && w0 > 0.0 && h0 > 0.0 {
                    let k = (size[0] / w0).max(size[1] / h0);
                    size = [w0 * k, h0 * k];
                    let corner = [opposite[0] + sx * size[0], opposite[1] + sy * size[1]];
                    mid = [
                        (corner[0] + opposite[0]) / 2.0,
                        (corner[1] + opposite[1]) / 2.0,
                    ];
                }
                new_box = Some((start_frame.to_layer(mid), size, d.rotation));
            }
            Handle::Vertex(i) => {
                let mut p = d.path.clone();
                let delta = sub(local, start_local);
                p.points[i].point = add(d.path.points[i].point, delta);
                new_path = Some(p);
            }
            Handle::TangentIn(i) | Handle::TangentOut(i) | Handle::NewVertex(i) => {
                let mut p = d.path.clone();
                let point = &mut p.points[i];
                let offset = sub(local, point.point);
                let mirrored = [-offset[0], -offset[1]];
                match d.handle {
                    Handle::TangentIn(_) => {
                        point.in_handle = offset;
                        if !alt {
                            point.out_handle = mirrored;
                        }
                    }
                    _ => {
                        point.out_handle = offset;
                        if !alt {
                            point.in_handle = mirrored;
                        }
                    }
                }
                new_path = Some(p);
            }
        }
        d.last_screen = pos;
    }
    if !pointer_down {
        state.drag = None;
    }

    let hovered = state
        .drag
        .as_ref()
        .map(|d| d.handle)
        .or_else(|| resp.hover_pos().and_then(handle_at));
    if state.drawing && resp.contains_pointer() {
        ui.ctx()
            .output_mut(|o| o.cursor_icon = egui::CursorIcon::Crosshair);
    } else if let Some(handle) = hovered {
        ui.ctx().output_mut(|o| {
            o.cursor_icon = match handle {
                Handle::Move => egui::CursorIcon::Move,
                Handle::Rotate => egui::CursorIcon::Grab,
                Handle::Corner(sx, sy) if sx * sy > 0.0 => egui::CursorIcon::ResizeNeSw,
                Handle::Corner(..) => egui::CursorIcon::ResizeNwSe,
                Handle::Side(0.0, _) => egui::CursorIcon::ResizeRow,
                Handle::Side(..) => egui::CursorIcon::ResizeColumn,
                _ => egui::CursorIcon::PointingHand,
            }
        });
    }

    let painter = ui.painter().with_clip_rect(area);
    let accent = crate::theme::ACCENT;
    let line = egui::Stroke::new(1.5, accent);
    let thin = egui::Stroke::new(1.0, egui::Color32::from_rgb(225, 232, 245));
    let dot = |center: egui::Pos2, filled: bool| {
        painter.circle(
            center,
            HANDLE_RADIUS,
            if filled { accent } else { egui::Color32::WHITE },
            egui::Stroke::new(1.5, accent),
        );
    };
    if outline.len() >= 2 {
        let mut closed = outline.clone();
        if !state.drawing {
            closed.push(outline[0]);
        }
        painter.add(egui::Shape::line(closed, line));
    }
    if is_path {
        if let Some(i) = selected {
            let p = path.points[i];
            for offset in [p.in_handle, p.out_handle] {
                let h = handle_screen(i, offset);
                painter.line_segment([vertices[i], h], thin);
                painter.circle(h, HANDLE_RADIUS - 1.0, egui::Color32::WHITE, thin);
            }
        }
        for (i, v) in vertices.iter().enumerate() {
            dot(*v, Some(i) == selected);
        }
    } else {
        painter.line_segment([top, knob], thin);
        for &(sx, sy) in corners.iter().chain(&sides) {
            dot(box_point(sx, sy), false);
        }
        dot(knob, false);
    }

    let mut out = None;
    if let Some((center, size, rotation)) = new_box {
        let mut m = mask.clone();
        for (param, v) in [
            (MaskParam::CenterX, center[0]),
            (MaskParam::CenterY, center[1]),
            (MaskParam::Width, size[0]),
            (MaskParam::Height, size[1]),
            (MaskParam::Rotation, rotation),
        ] {
            if (mask.track(param).value_at(frame) - v).abs() > f32::EPSILON {
                set_track_value(m.track_mut(param), frame, v);
            }
        }
        out = Some(m);
    }
    if let Some(p) = new_path {
        let mut m = out.unwrap_or_else(|| mask.clone());
        set_track_value(&mut m.path, frame, p);
        out = Some(m);
    }
    out.filter(|m| m != mask)
}

/// Index of the path segment (from vertex `i` to `i + 1`) passing within
/// grab distance of `pos`, the nearest one.
fn nearest_edge(
    pos: egui::Pos2,
    path: &MaskPath,
    to_screen: &dyn Fn([f32; 2]) -> egui::Pos2,
) -> Option<usize> {
    let n = path.points.len();
    if n < 2 {
        return None;
    }
    let mut best: Option<(usize, f32)> = None;
    for i in 0..n {
        let samples: Vec<egui::Pos2> = (0..=EDGE_SAMPLES)
            .map(|s| to_screen(path.segment_point(i, s as f32 / EDGE_SAMPLES as f32)))
            .collect();
        let d = samples
            .windows(2)
            .map(|w| distance_to_segment(pos, w[0], w[1]))
            .fold(f32::INFINITY, f32::min);
        if d <= GRAB && best.is_none_or(|(_, bd)| d < bd) {
            best = Some((i, d));
        }
    }
    best.map(|(i, _)| i)
}

/// Even-odd rule: the path can be concave.
fn polygon_contains(p: egui::Pos2, polygon: &[egui::Pos2]) -> bool {
    let mut inside = false;
    for (i, a) in polygon.iter().enumerate() {
        let b = polygon[(i + 1) % polygon.len()];
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
    }
    inside
}

#[cfg(test)]
#[path = "tests/mask_overlay.rs"]
mod tests;
