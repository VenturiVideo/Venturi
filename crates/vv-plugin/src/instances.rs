use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use vv_audio::{PluginHandle, PluginRequest};
use vv_core::{PluginState, Vst3Effect};

use crate::{PluginInfo, ProcessMode, Vst3Instance};

pub type MakeInstance =
    Box<dyn FnMut(&Path, &Vst3Effect, u32) -> Option<Arc<dyn HostedPlugin>> + Send>;

/// What the pool needs beyond processing (Vst3Instance implements it).
pub trait HostedPlugin: vv_audio::PluginProcessor {
    fn set_state(&self, effect: &Vst3Effect);
    fn state_hash(&self) -> u64;
    fn mark_state(&self, hash: u64);
    /// The concrete instance, for the editor; `None` for a stand-in.
    fn into_vst3(self: Arc<Self>) -> Option<Arc<Vst3Instance>> {
        None
    }
    /// The bundle it was made from; `None` for a stand-in.
    fn bundle(&self) -> Option<&Path> {
        None
    }
    /// Acts on a restart the plugin asked for; called every pass on the UI thread.
    fn service_restart(&self) {}
    /// False once a re-activation was refused: the plugin plays dry.
    fn is_active(&self) -> bool {
        true
    }
    /// The full deactivate/activate cycle: clears what `reset` leaves, such
    /// as a reverb's tail.
    fn reactivate(&self) {}
    /// The state to record when an edit gesture ended since the last call.
    fn take_gesture(&self) -> Option<(PluginState, PluginState)> {
        None
    }
    /// When the last gesture ended, as a process-wide counter; 0 for none.
    fn gesture_seq(&self) -> u64 {
        0
    }
}

/// Bounds the log so a plugin with ever-changing messages cannot grow it.
const FAILURE_LOG_CAP: usize = 256;

/// The (key, reason) failures already printed since the last reset. Pools
/// are per worker, so one failure would otherwise print once per pool.
#[derive(Default)]
struct FailureLog {
    seen: HashSet<(String, String)>,
}

impl FailureLog {
    /// Whether `reason` for `key` is new; it is then remembered.
    fn first(&mut self, key: &str, reason: &str) -> bool {
        if self.seen.len() >= FAILURE_LOG_CAP {
            return false;
        }
        self.seen.insert((key.to_owned(), reason.to_owned()))
    }

    fn reset(&mut self) {
        self.seen.clear();
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.seen.len()
    }
}

static FAILURE_LOG: Mutex<Option<FailureLog>> = Mutex::new(None);

fn with_failure_log<T>(f: impl FnOnce(&mut FailureLog) -> T) -> T {
    let mut guard = FAILURE_LOG.lock().unwrap();
    f(guard.get_or_insert_with(FailureLog::default))
}

/// Whether `reason` for `key` has not been printed since the last reset.
pub(crate) fn first_failure(key: &str, reason: &str) -> bool {
    with_failure_log(|log| log.first(key, reason))
}

/// Lets every failure print again: called on a rescan and on a retry.
fn reset_failure_log() {
    with_failure_log(FailureLog::reset);
}

struct Slot {
    used_this_pass: bool,
    /// The `pass` it was last seen used at, set by `sweep_idle`.
    last_pass: u64,
    state: SlotState,
}

enum SlotState {
    Ready {
        instance: Arc<dyn HostedPlugin>,
        /// The class and bundle it was made from.
        origin: (String, PathBuf),
        /// Reactivated by its first `get` of an offline render.
        stale: bool,
    },
    /// Creation failed: not retried until the effect's state changes.
    Failed { hash: u64 },
}

/// Loads `effect`'s state unless the instance already holds it.
fn apply_state(instance: &dyn HostedPlugin, effect: &Vst3Effect, hash: u64) -> bool {
    let changed = instance.state_hash() != hash;
    if changed {
        instance.set_state(effect);
    }
    changed
}

/// Instances are torn down on drop, which calls into the plugin: `sweep` and
/// the drop run on the owner's thread (the UI thread for live). An offline
/// pool may move between a worker's threads, one at a time, and is dropped
/// on whichever holds it.
pub struct Instances {
    slots: HashMap<u64, Slot>,
    /// Scanned plugins by class id: the path each is made from and its version.
    known: HashMap<String, PluginInfo>,
    make: MakeInstance,
    /// Instances an editor window drives, by the key they stand for.
    held: HashMap<u64, Arc<dyn HostedPlugin>>,
    pass: u64,
}

impl Instances {
    pub fn realtime(known: Arc<[PluginInfo]>) -> Self {
        Self::new(known, ProcessMode::Realtime)
    }

    /// Offline pools are used and dropped on worker threads (export, level
    /// and stretch), not the UI thread VST3 names for the lifecycle calls: a
    /// plugin that needs the main thread may fail and then plays dry.
    pub fn offline(known: Arc<[PluginInfo]>) -> Self {
        Self::new(known, ProcessMode::Offline)
    }

    fn new(known: Arc<[PluginInfo]>, mode: ProcessMode) -> Self {
        let make = Box::new(move |path: &Path, effect: &Vst3Effect, sample_rate| {
            let made = Vst3Instance::new(path, effect, sample_rate, mode);
            match made {
                Ok(instance) => Some(instance as Arc<dyn HostedPlugin>),
                Err(err) => {
                    if first_failure(&effect.class_id, &err.to_string()) {
                        eprintln!("vv-plugin: {}: {err}", effect.name);
                    }
                    None
                }
            }
        });
        Self::with_maker(known, make)
    }

    pub fn with_maker(known: Arc<[PluginInfo]>, make: MakeInstance) -> Self {
        Self {
            slots: HashMap::new(),
            known: known
                .iter()
                .map(|i| (i.class_id.clone(), i.clone()))
                .collect(),
            make,
            held: HashMap::new(),
            pass: 0,
        }
    }

    pub fn get(&mut self, request: PluginRequest<'_>) -> Option<PluginHandle> {
        if !request.process {
            if let Some(slot) = self.slots.get_mut(&request.key) {
                slot.used_this_pass = true;
                // Its tails are from before the bypass.
                if let SlotState::Ready { stale, .. } = &mut slot.state {
                    *stale = true;
                }
            }
            return None;
        }
        let hash = request.effect.content_hash();
        if let Some(Slot {
            used_this_pass,
            state: SlotState::Ready {
                instance, stale, ..
            },
            ..
        }) = self.slots.get_mut(&request.key)
        {
            if *used_this_pass {
                // An analysis shares the pass with the live chain; they run
                // one after the other, never at once.
                return Some(instance.clone());
            }
            *used_this_pass = true;
            if std::mem::take(stale) {
                instance.reactivate();
            }
            apply_state(instance.as_ref(), request.effect, hash);
            // After set_state: a restart it raised applies now, before the latency is read.
            instance.service_restart();
            return Some(instance.clone());
        }
        let path = self.known.get(&request.effect.class_id)?.path.clone();
        let held = self
            .held
            .get(&request.key)
            .filter(|h| h.bundle().is_none_or(|b| b == path))
            .cloned();
        if let Some(slot) = self.slots.get_mut(&request.key) {
            slot.used_this_pass = true;
            if held.is_none()
                && matches!(slot.state, SlotState::Failed { hash: failed } if failed == hash)
            {
                return None;
            }
        }
        let adopted = held.is_some();
        let Some(instance) =
            held.or_else(|| (self.make)(&path, request.effect, request.sample_rate))
        else {
            self.slots.insert(
                request.key,
                Slot {
                    used_this_pass: true,
                    last_pass: 0,
                    state: SlotState::Failed { hash },
                },
            );
            return None;
        };
        if adopted && apply_state(instance.as_ref(), request.effect, hash) {
            instance.service_restart();
        }
        let handle = instance.clone();
        self.slots.insert(
            request.key,
            Slot {
                used_this_pass: true,
                last_pass: 0,
                state: SlotState::Ready {
                    instance,
                    origin: (request.effect.class_id.clone(), path),
                    stale: false,
                },
            },
        );
        Some(handle)
    }

    /// Replaces the held instances: when a key has none, `get` takes the held
    /// one instead of making another, so a window keeps driving what plays
    /// after the effect is disabled and enabled again.
    pub fn set_held(&mut self, held: Vec<(u64, Arc<dyn HostedPlugin>)>) {
        self.held = held.into_iter().collect();
    }

    /// Whether `effect` is a scanned plugin: what `get` needs to try it.
    pub fn knows(&self, effect: &Vst3Effect) -> bool {
        self.known.contains_key(&effect.class_id)
    }

    /// Fingerprint of the module this process runs for `effect`'s class;
    /// `None` if not installed. A new build is seen after a restart, the
    /// only time its code runs.
    pub fn build(&self, effect: &Vst3Effect) -> Option<u64> {
        let info = self.known.get(&effect.class_id)?;
        Some(crate::module::build(&info.path, &info.version))
    }

    /// Whether the last attempt to make the instance under `key` failed.
    pub fn failed(&self, key: u64) -> bool {
        match self.slots.get(&key).map(|slot| &slot.state) {
            Some(SlotState::Failed { .. }) => true,
            Some(SlotState::Ready { instance, .. }) => !instance.is_active(),
            _ => false,
        }
    }

    /// An offline pool starts a new render: each instance is reactivated
    /// by its first `get`, as it carries the previous render's tail.
    pub fn begin_render(&mut self) {
        for slot in self.slots.values_mut() {
            if let SlotState::Ready { stale, .. } = &mut slot.state {
                *stale = true;
            }
        }
    }

    /// Acts on pending restart requests without a snapshot pass.
    pub fn service_restarts(&self) {
        for slot in self.slots.values() {
            if let SlotState::Ready { instance, .. } = &slot.state {
                instance.service_restart();
            }
        }
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn begin_pass(&mut self) {
        self.pass += 1;
        for slot in self.slots.values_mut() {
            slot.used_this_pass = false;
        }
    }

    pub fn sweep(&mut self) {
        self.slots.retain(|_, slot| slot.used_this_pass);
    }

    /// `sweep` for an offline pool shared by renders: the slots the pass did
    /// not use are kept, the `keep` most recently used of them, so a render
    /// that comes back to a placement does not make it again. Kept ones are
    /// reactivated by their next `get` (their tails are old).
    pub fn sweep_idle(&mut self, keep: usize) {
        let pass = self.pass;
        if self.slots.len() <= keep {
            for slot in self.slots.values_mut() {
                match &mut slot.state {
                    SlotState::Ready { stale, .. } if !slot.used_this_pass => *stale = true,
                    _ => {}
                }
                if slot.used_this_pass {
                    slot.last_pass = pass;
                }
            }
            return;
        }
        let mut idle = Vec::new();
        for (key, slot) in &mut self.slots {
            if slot.used_this_pass {
                slot.last_pass = pass;
            } else {
                if let SlotState::Ready { stale, .. } = &mut slot.state {
                    *stale = true;
                }
                idle.push((std::cmp::Reverse(slot.last_pass), *key));
            }
        }
        if idle.len() > keep {
            idle.sort_unstable();
            for (_, key) in &idle[keep..] {
                self.slots.remove(key);
            }
        }
    }

    /// Adopts a new scan: instances of plugins still listed from the same
    /// bundle stay (the module is never reloaded, so they run the same code),
    /// the others are returned, and failures are forgotten so new plugins
    /// are tried.
    pub fn refresh(&mut self, known: Arc<[PluginInfo]>) -> Vec<Arc<dyn HostedPlugin>> {
        reset_failure_log();
        self.known = known
            .iter()
            .map(|i| (i.class_id.clone(), i.clone()))
            .collect();
        let known = &self.known;
        let mut dropped = Vec::new();
        self.slots.retain(|_, slot| match &slot.state {
            SlotState::Ready {
                instance,
                origin: (class_id, path),
                ..
            } => {
                let keep = known.get(class_id).is_some_and(|i| &i.path == path);
                if !keep {
                    dropped.push(instance.clone());
                }
                keep
            }
            SlotState::Failed { .. } => false,
        });
        // A held instance of a dropped slot or of a moved class must not be
        // adopted again: it runs the old bundle's code.
        let mut extra: Vec<Arc<dyn HostedPlugin>> = Vec::new();
        self.held.retain(|_, h| {
            let listed = dropped.iter().any(|d| Arc::ptr_eq(d, h));
            let moved = h
                .bundle()
                .is_some_and(|b| !known.values().any(|i| i.path == b));
            if moved && !listed {
                extra.push(h.clone());
            }
            !(listed || moved)
        });
        dropped.extend(extra);
        dropped
    }

    /// Forgets every failure so the next `get` tries again.
    pub fn clear_failures(&mut self) {
        reset_failure_log();
        self.slots
            .retain(|_, slot| matches!(slot.state, SlotState::Ready { .. }));
    }

    pub fn instance(&self, key: u64) -> Option<Arc<dyn HostedPlugin>> {
        match self.slots.get(&key) {
            Some(Slot {
                state: SlotState::Ready { instance, .. },
                ..
            }) => Some(instance.clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "tests/instances.rs"]
mod tests;
