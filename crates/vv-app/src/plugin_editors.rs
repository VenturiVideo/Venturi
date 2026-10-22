//! Open plugin editor windows, and what a gesture in one does to the project.
use std::collections::HashMap;
use std::sync::Arc;
use vv_core::{
    AudioEffect, AudioEffectKind, ChannelStrip, CommandLabel, EffectId, History, Id, MixerChannel,
    PluginState, Project, SetAudioEffect, Timeline, TimelineId, Vst3Effect,
};
use vv_plugin::{Editor, HostedPlugin};

use crate::mixer_panel::{channel_strip, plugin_key_of};
use crate::timeline_audio::TimelineAudio;

/// Strips the mixer numbers together: all tracks, or the master.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Group {
    Tracks,
    Master,
}

/// Like the mixer's live keys it has no track index, so removing another
/// track does not misroute it; the last part is the occurrence of that id in
/// the group, counted like the mixer's placements, so duplicates stay apart.
type Key = (TimelineId, Group, EffectId, u32);

/// Key of the editor's own instance, in a scope apart from the live keys.
fn aside_key(
    timeline_id: TimelineId,
    timeline: &Timeline,
    channel: MixerChannel,
    index: usize,
) -> u64 {
    let scope = vv_core::stable_hash(("editor", timeline_id.raw()));
    vv_audio::plugin_key_in(scope, timeline, channel, index).unwrap_or(0)
}

#[derive(Default)]
pub(crate) struct PluginEditors {
    open: HashMap<Key, Editor>,
    /// `History::generation` the instances were last compared with the project at.
    checked: u64,
    /// The pool was handed instances that must be taken back.
    holding: bool,
    /// Gestures of editors closed while an undo group was open.
    pending: Vec<(Key, Arc<dyn HostedPlugin>)>,
}

/// The live key each editor's instance stands for, so the pool reuses it
/// when the effect plays again instead of making an instance no window drives.
fn held_instances(
    project: &Project,
    editors: impl Iterator<Item = (Key, Arc<dyn HostedPlugin>)>,
) -> Vec<(u64, Arc<dyn HostedPlugin>)> {
    editors
        .filter_map(|(key, instance)| {
            let (channel, index, _) = find(project, key)?;
            Some((
                plugin_key_of(project.timelines.get(key.0)?, channel, index),
                instance,
            ))
        })
        .collect()
}

fn is_dropped<T: ?Sized>(dropped: &[Arc<dyn HostedPlugin>], instance: &Arc<T>) -> bool {
    let at = Arc::as_ptr(instance) as *const ();
    dropped.iter().any(|d| Arc::as_ptr(d) as *const () == at)
}

/// Brings an already open window to the front; false when there is none to raise.
fn raise_open<E>(
    editors: &mut HashMap<Key, E>,
    key: &Key,
    is_open: impl Fn(&E) -> bool,
    raise: impl Fn(&E),
) -> bool {
    match editors.get(key) {
        Some(editor) if is_open(editor) => {
            raise(editor);
            true
        }
        _ => false,
    }
}

/// The plugins of a group in mixer order, with their occurrence of that id.
fn placements(
    timeline: &Timeline,
    group: Group,
) -> impl Iterator<Item = (MixerChannel, usize, &AudioEffect, u32)> {
    let strips: Vec<(MixerChannel, &ChannelStrip)> = match group {
        Group::Tracks => timeline
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| (MixerChannel::Track(i), &t.mix))
            .collect(),
        Group::Master => vec![(MixerChannel::Master, &timeline.master)],
    };
    let mut met: HashMap<EffectId, u32> = HashMap::new();
    strips.into_iter().flat_map(move |(channel, strip)| {
        strip
            .effects
            .iter()
            .enumerate()
            .filter_map(|(index, effect)| Some((index, effect, plugin(effect)?.id)))
            .map(|(index, effect, id)| {
                let seen = met.entry(id).or_insert(0);
                let occurrence = *seen;
                *seen += 1;
                (channel, index, effect, occurrence)
            })
            .collect::<Vec<_>>()
    })
}

fn find(
    project: &Project,
    (timeline, group, id, occurrence): Key,
) -> Option<(MixerChannel, usize, &AudioEffect)> {
    placements(project.timelines.get(timeline)?, group)
        .find(|(_, _, effect, n)| *n == occurrence && plugin(effect).is_some_and(|v| v.id == id))
        .map(|(channel, index, effect, _)| (channel, index, effect))
}

fn key_at(timeline: &Timeline, id: TimelineId, channel: MixerChannel, index: usize) -> Option<Key> {
    let group = match channel {
        MixerChannel::Track(_) => Group::Tracks,
        MixerChannel::Master => Group::Master,
    };
    placements(timeline, group)
        .find(|(c, i, ..)| (*c, *i) == (channel, index))
        .and_then(|(_, _, effect, occurrence)| Some((id, group, plugin(effect)?.id, occurrence)))
}

fn plugin(effect: &AudioEffect) -> Option<&Vst3Effect> {
    match &effect.kind {
        AudioEffectKind::Vst3(v) => Some(v),
        _ => None,
    }
}

/// One undoable step for an edit gesture; the new content hash, or `None`
/// when the plugin's state did not change.
fn record_edit(
    project: &mut Project,
    history: &mut History,
    key: Key,
    component: PluginState,
    controller: PluginState,
) -> Option<u64> {
    let (channel, index, effect) = find(project, key)?;
    let old = plugin(effect)?;
    if old.component_state == component && old.controller_state == controller {
        return None;
    }
    // An unchanged half keeps the live effect's bytes instead of a second copy.
    let new = Vst3Effect {
        component_state: if old.component_state == component {
            old.component_state.clone()
        } else {
            component
        },
        controller_state: if old.controller_state == controller {
            old.controller_state.clone()
        } else {
            controller
        },
        ..old.clone()
    };
    let hash = new.content_hash();
    let edited = AudioEffect {
        kind: AudioEffectKind::Vst3(new),
        ..effect.clone()
    };
    let (timeline, ..) = key;
    let label = CommandLabel::EditAudioEffect;
    let command = SetAudioEffect::new(timeline, channel, index, edited, label);
    history.do_command(project, Box::new(command));
    Some(hash)
}

/// Records a gesture the instance still holds, and consumes it so it can never
/// become an out-of-order step later. While an undo group is open the gesture
/// stays pending: recorded now it would fold into that step. A gesture whose
/// state the plugin could not produce stays with the instance for a bounded
/// retry. Returns whether a gesture was consumed.
fn record_gesture(
    project: &mut Project,
    history: &mut History,
    key: Key,
    instance: &dyn HostedPlugin,
    group_open: bool,
) -> bool {
    instance.service_restart();
    if group_open {
        return false;
    }
    let Some((component, controller)) = instance.take_gesture() else {
        return false;
    };
    if let Some(hash) = record_edit(project, history, key, component, controller) {
        // The pool must not read the state back into the instance.
        instance.mark_state(hash);
    }
    true
}

fn drain_pending(
    pending: &mut Vec<(Key, Arc<dyn HostedPlugin>)>,
    project: &mut Project,
    history: &mut History,
    group_open: bool,
) {
    if group_open {
        return;
    }
    let mut ended = std::mem::take(pending);
    ended.sort_by_key(|(_, instance)| instance.gesture_seq());
    for (key, instance) in ended {
        record_gesture(project, history, key, &*instance, false);
    }
}

impl PluginEditors {
    /// Closes the editors `keep` rejects. A gesture that ended before the
    /// window went away (or inside its `removed`) is recorded while the
    /// effect can still be found.
    fn close_where(
        &mut self,
        project: &mut Project,
        history: &mut History,
        group_open: bool,
        keep: impl Fn(&Project, &Key, &Editor) -> bool,
    ) {
        let gone: Vec<Key> = self
            .open
            .iter()
            .filter(|(key, editor)| !keep(project, key, editor))
            .map(|(key, _)| *key)
            .collect();
        for key in gone {
            let Some(editor) = self.open.remove(&key) else {
                continue;
            };
            let instance = editor.instance().clone();
            drop(editor);
            self.pending.push((key, instance));
        }
        drain_pending(&mut self.pending, project, history, group_open);
    }

    /// `current` is the timeline the audio is playing.
    pub fn open(
        &mut self,
        ctx: &egui::Context,
        project: &Project,
        (timeline, channel, index): (TimelineId, MixerChannel, usize),
        current: Option<TimelineId>,
        audio: &mut TimelineAudio,
    ) {
        let Some(strip) = project
            .timelines
            .get(timeline)
            .and_then(|t| channel_strip(t, channel))
        else {
            return;
        };
        let Some(plugin) = strip.effects.get(index).and_then(plugin) else {
            return;
        };
        let Some(key) = key_at(&project.timelines[timeline], timeline, channel, index) else {
            return;
        };
        if raise_open(&mut self.open, &key, Editor::is_open, Editor::raise) {
            return;
        }
        self.open.remove(&key);
        let live_key = plugin_key_of(&project.timelines[timeline], channel, index);
        let live = (current == Some(timeline)).then_some(live_key);
        let aside = aside_key(timeline, &project.timelines[timeline], channel, index);
        let Some(instance) = audio.plugin_instance(live, aside, plugin) else {
            eprintln!("vv-plugin: {}: no instance for the editor", plugin.name);
            return;
        };
        let ctx = ctx.clone();
        let waker = std::sync::Arc::new(move || ctx.request_repaint());
        match Editor::open(&instance, &plugin.name, waker) {
            Ok(editor) => {
                self.open.insert(key, editor);
            }
            Err(err) => eprintln!("vv-plugin: {}: {err}", plugin.name),
        }
    }

    /// Windows of effects that are gone (removed, undone, another timeline)
    /// close; the others are pumped and their gestures recorded.
    pub fn update(
        &mut self,
        ctx: &egui::Context,
        project: &mut Project,
        history: &mut History,
        current: Option<TimelineId>,
        audio: Option<&mut TimelineAudio>,
        group_open: bool,
    ) {
        if self.open.is_empty() && !self.holding && self.pending.is_empty() {
            return;
        }
        self.close_where(project, history, group_open, |project, key, editor| {
            Some(key.0) == current && find(project, *key).is_some() && editor.is_open()
        });
        if let Some(audio) = audio {
            let held = held_instances(
                project,
                self.open
                    .iter()
                    .map(|(k, e)| (*k, e.instance().clone() as Arc<dyn HostedPlugin>)),
            );
            self.holding = !held.is_empty();
            audio.hold_plugins(held);
        }
        let generation = history.generation();
        let changed = generation != self.checked;
        let mut wake: Option<std::time::Duration> = None;
        let mut keys: Vec<Key> = self.open.keys().copied().collect();
        // HashMap order would record simultaneous gestures in a random order.
        keys.sort_by_key(|key| self.open[key].instance().gesture_seq());
        for key in keys {
            let Some(editor) = self.open.get_mut(&key) else {
                continue;
            };
            editor.idle();
            wake = match (wake, editor.next_wake()) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            let instance = editor.instance();
            // The sync below would overwrite a gesture still pending.
            let consumed = record_gesture(project, history, key, &**instance, group_open);
            if group_open || consumed {
                continue;
            }
            if changed
                && let Some(plugin) = find(project, key).and_then(|(_, _, e)| plugin(e))
                && plugin.content_hash() != instance.state_hash()
            {
                // An undo of an instance no mix is playing.
                instance.set_state(plugin);
            }
        }
        if !group_open {
            self.checked = history.generation();
        }
        if let Some(wake) = wake {
            // Edits, closing and resizing wake the app; only a plugin's own timers need this.
            ctx.request_repaint_after(wake);
        }
    }

    #[cfg(test)]
    pub fn defer_gesture(&mut self, key: Key, instance: Arc<dyn HostedPlugin>) {
        self.pending.push((key, instance));
    }

    /// Closes the editors driving one of `dropped`, keeping the others open.
    pub fn close_instances(
        &mut self,
        project: &mut Project,
        history: &mut History,
        dropped: &[Arc<dyn HostedPlugin>],
        group_open: bool,
    ) {
        self.close_where(project, history, group_open, |_, _, editor| {
            !is_dropped(dropped, editor.instance())
        });
    }
}

#[cfg(test)]
#[path = "tests/plugin_editors.rs"]
mod tests;
