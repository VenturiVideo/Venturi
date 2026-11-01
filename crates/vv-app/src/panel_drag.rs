use egui::{Context, Event, Plugin, PointerButton, Pos2, RawInput, Ui, ViewportId};

use crate::timeline_ui::{FilterEntry, Generator, MediaDragSet, PresetDrag};

#[derive(Clone)]
enum Payload {
    Media(MediaDragSet),
    Generator(Generator),
    Filter(FilterEntry),
    Preset(PresetDrag),
    Kind(vv_core::TransitionKind),
    Transition(vv_core::Transition),
}

impl Payload {
    fn capture(ctx: &Context) -> Option<Self> {
        use egui::DragAndDrop as Dnd;
        Dnd::payload::<MediaDragSet>(ctx)
            .map(|p| Self::Media((*p).clone()))
            .or_else(|| Dnd::payload::<Generator>(ctx).map(|p| Self::Generator(*p)))
            .or_else(|| Dnd::payload::<FilterEntry>(ctx).map(|p| Self::Filter(*p)))
            .or_else(|| Dnd::payload::<PresetDrag>(ctx).map(|p| Self::Preset((*p).clone())))
            .or_else(|| Dnd::payload::<vv_core::TransitionKind>(ctx).map(|p| Self::Kind(*p)))
            .or_else(|| {
                Dnd::payload::<vv_core::Transition>(ctx).map(|p| Self::Transition((*p).clone()))
            })
    }

    fn restore(&self, ctx: &Context) {
        use egui::DragAndDrop as Dnd;
        match self {
            Self::Media(p) => Dnd::set_payload(ctx, p.clone()),
            Self::Generator(p) => Dnd::set_payload(ctx, *p),
            Self::Filter(p) => Dnd::set_payload(ctx, *p),
            Self::Preset(p) => Dnd::set_payload(ctx, p.clone()),
            Self::Kind(p) => Dnd::set_payload(ctx, *p),
            Self::Transition(p) => Dnd::set_payload(ctx, p.clone()),
        }
    }
}

struct Drag {
    source: ViewportId,
    payload: Payload,
    position: Pos2,
    modifiers: egui::Modifiers,
    released: bool,
    delivered: Option<ViewportId>,
}

#[derive(Default)]
pub(crate) struct NativeDrag {
    drag: Option<Drag>,
}

fn pixels_per_point(zoom: f32, raw: &RawInput, id: ViewportId) -> f32 {
    raw.viewports
        .get(&id)
        .and_then(|v| v.native_pixels_per_point)
        .unwrap_or(1.0)
        * zoom
}

fn desktop_position(zoom: f32, raw: &RawInput, point: Pos2) -> Option<Pos2> {
    let rect = raw.viewports.get(&raw.viewport_id)?.inner_rect?;
    Some((point + rect.min.to_vec2()) * pixels_per_point(zoom, raw, raw.viewport_id))
}

impl Plugin for NativeDrag {
    fn debug_name(&self) -> &'static str {
        "Native panel drag"
    }

    fn input_hook(&mut self, ctx: &Context, raw: &mut RawInput) {
        let zoom = ctx.zoom_factor();
        if raw.events.iter().any(|event| {
            matches!(
                event,
                Event::Key {
                    key: egui::Key::Escape,
                    pressed: true,
                    ..
                }
            )
        }) {
            self.drag = None;
            egui::DragAndDrop::clear_payload(ctx);
            return;
        }
        let Some(drag) = &mut self.drag else {
            return;
        };
        if raw
            .viewports
            .get(&drag.source)
            .is_none_or(|source| source.close_requested())
        {
            self.drag = None;
            egui::DragAndDrop::clear_payload(ctx);
            return;
        }
        if raw.viewport_id == drag.source && !drag.released {
            for event in &raw.events {
                match event {
                    Event::ModifiersChanged(modifiers) | Event::PointerButton { modifiers, .. } => {
                        drag.modifiers = *modifiers
                    }
                    _ => {}
                }
                let point = match event {
                    Event::PointerMoved(point) => Some(*point),
                    Event::PointerButton { pos, .. } => Some(*pos),
                    _ => None,
                };
                if let Some(point) = point.and_then(|p| desktop_position(zoom, raw, p)) {
                    drag.position = point;
                }
                if matches!(
                    event,
                    Event::PointerButton {
                        button: PointerButton::Primary,
                        pressed: false,
                        ..
                    }
                ) {
                    drag.released = true;
                }
            }
        }
        let inside_source = drag.source != ViewportId::ROOT
            && raw.viewports[&drag.source].inner_rect.is_some_and(|rect| {
                rect.contains(drag.position / pixels_per_point(zoom, raw, drag.source))
            });
        let mut targets = raw
            .viewports
            .iter()
            .filter(|(id, info)| {
                !inside_source
                    && **id != drag.source
                    && !info.close_requested()
                    && info.minimized != Some(true)
                    && info.occluded != Some(true)
            })
            .filter_map(|(id, info)| {
                let scale = pixels_per_point(zoom, raw, *id);
                let rect = info.inner_rect?;
                rect.contains(drag.position / scale)
                    .then_some((*id, rect, scale))
            })
            .collect::<Vec<_>>();
        if targets.iter().any(|(id, _, _)| *id != ViewportId::ROOT) {
            targets.retain(|(id, _, _)| *id != ViewportId::ROOT);
        }
        let target = (targets.len() == 1).then(|| targets[0]);
        let Some((target, rect, scale)) = target else {
            if drag.released {
                self.drag = None;
            }
            return;
        };
        ctx.request_repaint_of(target);
        if raw.viewport_id != target || drag.delivered.is_some() {
            return;
        }
        let point = drag.position / scale - rect.min.to_vec2();
        raw.events.push(Event::PointerMoved(point));
        // The source's native pointer grab receives release, even over another viewport.
        if drag.released {
            raw.events.push(Event::PointerButton {
                pos: point,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: drag.modifiers,
            });
            drag.delivered = Some(target);
        }
        drag.payload.restore(ctx);
    }

    fn on_end_pass(&mut self, ui: &mut Ui) {
        let id = ui.ctx().viewport_id();
        if self
            .drag
            .as_ref()
            .is_some_and(|drag| drag.delivered == Some(id))
        {
            self.drag = None;
            return;
        }
        if self.drag.is_some() {
            return;
        }
        let Some(payload) = Payload::capture(ui.ctx()) else {
            return;
        };
        if !ui.input(|i| i.pointer.primary_down()) {
            return;
        }
        let zoom = ui.ctx().zoom_factor();
        let position =
            ui.input(|input| desktop_position(zoom, &input.raw, input.pointer.interact_pos()?));
        if let Some(position) = position {
            self.drag = Some(Drag {
                source: id,
                payload,
                position,
                modifiers: ui.input(|i| i.modifiers),
                released: false,
                delivered: None,
            });
        }
    }
}

#[cfg(test)]
#[path = "tests/panel_drag.rs"]
mod tests;
