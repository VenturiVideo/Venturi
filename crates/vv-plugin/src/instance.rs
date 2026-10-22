//! A hosted VST3 effect, driven with the mixer's interleaved blocks.
use std::cell::UnsafeCell;
use std::ffi::c_char;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, TryLockError};

use vst3::Steinberg::Vst::{
    AudioBusBuffers, AudioBusBuffers__type0, IAudioProcessor, IAudioProcessorTrait, IComponent,
    IComponentHandler, IComponentTrait, IConnectionPoint, IConnectionPointTrait, IEditController,
    IEditControllerTrait, IParameterChanges, ParameterInfo, ParameterInfo_, ProcessContext,
    ProcessData, ProcessSetup, RestartFlags_, SpeakerArr, SpeakerArrangement,
};
use vst3::Steinberg::{
    FUnknown, IBStream, IPluginBaseTrait, TUID, kNotImplemented, kResultFalse, kResultOk,
    kResultTrue, tresult,
};
use vst3::{ComPtr, ComWrapper};
use vv_audio::{MIX_BLOCK_FRAMES, PluginProcessor};
use vv_core::{PluginState, Vst3Effect};

use crate::HostedPlugin;
use crate::host::{ComponentHandler, HostApp, MAX_PARAMS, MemStream, ParamChanges, Shared};
use crate::instances::first_failure;
use crate::module::{Module, module};

// The generated enum constants are u32 on some targets and i32 on others.
#[allow(clippy::unnecessary_cast)]
mod ids {
    use vst3::Steinberg::Vst::{
        BusDirections_, MediaTypes_, ProcessContext_, ProcessModes_, SymbolicSampleSizes_,
    };
    pub const INPUT: i32 = BusDirections_::kInput as i32;
    pub const OUTPUT: i32 = BusDirections_::kOutput as i32;
    pub const AUDIO: i32 = MediaTypes_::kAudio as i32;
    pub const REALTIME: i32 = ProcessModes_::kRealtime as i32;
    pub const OFFLINE: i32 = ProcessModes_::kOffline as i32;
    pub const TEMPO_VALID: u32 = ProcessContext_::StatesAndFlags_::kTempoValid as u32;
    pub const TIME_SIG_VALID: u32 = ProcessContext_::StatesAndFlags_::kTimeSigValid as u32;
    pub const SAMPLE32: i32 = SymbolicSampleSizes_::kSample32 as i32;
}

const MAX_STATE_FAILURES: u32 = 3;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProcessMode {
    Realtime,
    Offline,
}

impl ProcessMode {
    // One mapping for setup and every process call: VST3 requires them to agree.
    fn sdk(self) -> i32 {
        match self {
            ProcessMode::Realtime => ids::REALTIME,
            ProcessMode::Offline => ids::OFFLINE,
        }
    }
}

const MAX_BUSES: i32 = 16;

/// One `AudioBusBuffers` per declared bus of a direction, built once so that
/// process only hands out pointers. Bus 0 points at `Rt::inp`/`out`; the rest at `scratch`.
struct Buses {
    bufs: Vec<AudioBusBuffers>,
    // Boxed storage the entries of `bufs` point into; never resized after setup.
    tables: Vec<Vec<*mut f32>>,
    scratch: Vec<Vec<f32>>,
}

impl Buses {
    fn empty() -> Self {
        Self {
            bufs: Vec::new(),
            tables: Vec::new(),
            scratch: Vec::new(),
        }
    }

    fn push(&mut self, mut table: Vec<*mut f32>, silence: u64) {
        let buf = AudioBusBuffers {
            numChannels: table.len() as i32,
            silenceFlags: silence,
            __field0: AudioBusBuffers__type0 {
                channelBuffers32: table.as_mut_ptr(),
            },
        };
        self.tables.push(table);
        self.bufs.push(buf);
    }
}

/// Everything the audio thread touches; guarded by `Vst3Instance::busy`.
struct Rt {
    inp: [Vec<f32>; 2],
    out: [Vec<f32>; 2],
    ins: Buses,
    outs: Buses,
    changes: ComWrapper<ParamChanges>,
    changes_ptr: ComPtr<IParameterChanges>,
    outputs: ComWrapper<ParamChanges>,
    outputs_ptr: ComPtr<IParameterChanges>,
    context: ProcessContext,
}

pub struct Vst3Instance {
    component: ComPtr<IComponent>,
    processor: ComPtr<IAudioProcessor>,
    controller: Option<ComPtr<IEditController>>,
    // Only a separate controller object is initialized and terminated by us.
    separate_controller: bool,
    connections: Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)>,
    // Drop only undoes what succeeded: VST3 forbids deactivating a plugin never activated.
    active: AtomicBool,
    pub(crate) handler: ComPtr<IComponentHandler>,
    host: ComPtr<FUnknown>,
    shared: Arc<Shared>,
    rt: UnsafeCell<Rt>,
    busy: AtomicBool,
    mode: ProcessMode,
    bundle: PathBuf,
    state_hash: AtomicU64,
    latency: AtomicU32,
    class_id: String,
    // Consecutive gestures whose getState failed.
    state_failures: AtomicU32,
    // Read before any state is loaded: what an empty saved state means.
    defaults: (Vec<u8>, Vec<u8>),
    // Last: fields drop in declaration order, and the plugin objects above
    // must be released while the module's code is still loaded.
    _module: Arc<Module>,
}

// SAFETY: `rt` is only used by whoever holds `busy`; the COM pointers are to
// objects VST3 requires to be callable from the threads we use them on.
unsafe impl Send for Vst3Instance {}
unsafe impl Sync for Vst3Instance {}

/// Exclusive use of `Vst3Instance::rt`; releases `busy` on every exit, panics included.
struct AudioGuard<'a>(&'a AtomicBool);

impl<'a> AudioGuard<'a> {
    fn try_acquire(busy: &'a AtomicBool) -> Option<Self> {
        busy.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| Self(busy))
    }
}

impl Drop for AudioGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn parse_cid(hex: &str) -> Result<TUID, String> {
    let bad = || format!("bad class id {hex:?}");
    if hex.len() != 32 || !hex.is_ascii() {
        return Err(bad());
    }
    let mut cid: TUID = [0; 16];
    for (byte, pair) in cid.iter_mut().zip(hex.as_bytes().chunks(2)) {
        let pair = std::str::from_utf8(pair).map_err(|_| bad())?;
        *byte = u8::from_str_radix(pair, 16).map_err(|_| bad())? as c_char;
    }
    Ok(cid)
}

fn ok(res: i32, what: &str) -> Result<(), String> {
    (res == kResultOk)
        .then_some(())
        .ok_or_else(|| format!("{what} failed ({res})"))
}

impl Vst3Instance {
    /// On a non-realtime thread. Stereo in/out, blocks of at most 512 frames, f32.
    /// Drop it on the UI thread too: teardown calls into the plugin.
    pub fn new(
        info_path: &Path,
        effect: &Vst3Effect,
        sample_rate: u32,
        mode: ProcessMode,
    ) -> Result<Arc<Self>, String> {
        let module = module(info_path)?;
        let cid = parse_cid(&effect.class_id)?;
        let component = module
            .create::<IComponent>(&cid)
            .ok_or("the plugin could not create its component")?;
        let processor = component
            .cast::<IAudioProcessor>()
            .ok_or("the plugin has no audio processor")?;

        let shared = Arc::new(Shared::new());
        let host = ComWrapper::new(HostApp)
            .to_com_ptr::<FUnknown>()
            .ok_or("host context")?;
        let handler = ComWrapper::new(ComponentHandler(shared.clone()))
            .to_com_ptr::<IComponentHandler>()
            .ok_or("component handler")?;
        let changes = ParamChanges::input();
        let outputs = ParamChanges::output();
        let rt = Rt {
            inp: [vec![0.0; MIX_BLOCK_FRAMES], vec![0.0; MIX_BLOCK_FRAMES]],
            out: [vec![0.0; MIX_BLOCK_FRAMES], vec![0.0; MIX_BLOCK_FRAMES]],
            ins: Buses::empty(),
            outs: Buses::empty(),
            changes_ptr: changes.to_com_ptr().ok_or("parameter changes")?,
            outputs_ptr: outputs.to_com_ptr().ok_or("parameter changes")?,
            outputs,
            changes,
            // SAFETY: a plain C struct; zero is a valid empty state.
            context: unsafe { std::mem::zeroed() },
        };

        // SAFETY: a live component and a host context that outlives it.
        ok(unsafe { component.initialize(host.as_ptr()) }, "initialize")?;
        // From here Drop undoes whatever was done, error or not.
        let mut this = Self {
            bundle: info_path.to_path_buf(),
            _module: module.clone(),
            component,
            processor,
            controller: None,
            separate_controller: false,
            connections: None,
            active: AtomicBool::new(false),
            handler,
            host,
            shared,
            rt: UnsafeCell::new(rt),
            busy: AtomicBool::new(false),
            mode,
            state_hash: AtomicU64::new(effect.content_hash()),
            latency: AtomicU32::new(0),
            class_id: effect.class_id.clone(),
            state_failures: AtomicU32::new(0),
            defaults: (Vec::new(), Vec::new()),
        };
        this.attach_controller(&module)?;
        this.size_outputs()?;
        this.setup(sample_rate)?;
        this.defaults = this.read_state().unwrap_or_default();
        this.sync_controller(&this.defaults.0);
        if this.separate_controller {
            this.defaults.1 = this.read_state().map(|s| s.1).unwrap_or_default();
        }
        if !effect.component_state.bytes().is_empty() || !effect.controller_state.bytes().is_empty()
        {
            this.load_state(&effect.component_state, &effect.controller_state);
        }
        // SAFETY: setup is complete; these start the processing the mixer drives.
        unsafe {
            ok(this.component.setActive(1), "setActive")?;
            *this.active.get_mut() = true;
            // Informational: the SDK default returns kNotImplemented.
            this.processor.setProcessing(1);
        }
        this.refresh_latency_after_activation();
        Ok(Arc::new(this))
    }

    fn attach_controller(&mut self, module: &Module) -> Result<(), String> {
        let mut class_id: TUID = [0; 16];
        // SAFETY: out-parameter call on a live component.
        let separate = unsafe { self.component.getControllerClassId(&mut class_id) } == kResultOk
            && class_id.iter().any(|&b| b != 0);
        if !separate {
            self.controller = self.component.cast::<IEditController>();
        } else {
            let controller = module
                .create::<IEditController>(&class_id)
                .ok_or("the plugin could not create its controller")?;
            // SAFETY: a live controller and host context.
            ok(
                unsafe { controller.initialize(self.host.as_ptr()) },
                "controller initialize",
            )?;
            self.separate_controller = true;
            self.controller = Some(controller);
            let pair = self.component.cast::<IConnectionPoint>().zip(
                self.controller
                    .as_ref()
                    .and_then(|c| c.cast::<IConnectionPoint>()),
            );
            if let Some((a, b)) = pair {
                // SAFETY: both ends are live; each is told about the other.
                unsafe {
                    a.connect(b.as_ptr());
                    b.connect(a.as_ptr());
                }
                self.connections = Some((a, b));
            }
        }
        if let Some(controller) = &self.controller {
            // SAFETY: the handler outlives the controller's use of it (cleared in Drop).
            unsafe { controller.setComponentHandler(self.handler.as_ptr()) };
        }
        Ok(())
    }

    fn size_outputs(&mut self) -> Result<(), String> {
        let readonly = self.controller.as_ref().map_or(0, read_only_count);
        let capacity = readonly.max(MAX_PARAMS);
        let outputs = ParamChanges::output_with_capacity(capacity);
        let rt = self.rt.get_mut();
        rt.outputs_ptr = outputs.to_com_ptr().ok_or("parameter changes")?;
        rt.outputs = outputs;
        self.shared.reserve_outputs(capacity);
        Ok(())
    }

    fn setup(&self, sample_rate: u32) -> Result<(), String> {
        let mut arranged = [0 as SpeakerArrangement; 2];
        // SAFETY: calls on live interfaces with arrays of the lengths passed.
        unsafe {
            let counts = [ids::INPUT, ids::OUTPUT].map(|dir| {
                self.component
                    .getBusCount(ids::AUDIO, dir)
                    .clamp(1, MAX_BUSES)
            });
            // Bus 0 is the stereo main bus; the others keep the plugin's own arrangement.
            let mut layouts = [ids::INPUT, ids::OUTPUT].map(|dir| {
                let n = counts[usize::from(dir == ids::OUTPUT)];
                (0..n)
                    .map(|i| {
                        let mut arr = SpeakerArr::kStereo;
                        if i > 0 {
                            self.processor.getBusArrangement(dir, i, &mut arr);
                        }
                        arr
                    })
                    .collect::<Vec<_>>()
            });
            // A rejection is fine if the default arrangement is already stereo.
            let full = self.processor.setBusArrangements(
                layouts[0].as_mut_ptr(),
                counts[0],
                layouts[1].as_mut_ptr(),
                counts[1],
            );
            if full != kResultTrue {
                // Plugins that only take the main buses still get their stereo pair.
                let mut stereo = [SpeakerArr::kStereo];
                self.processor
                    .setBusArrangements(stereo.as_mut_ptr(), 1, stereo.as_mut_ptr(), 1);
            }
            let dirs = [ids::INPUT, ids::OUTPUT];
            for (arr, dir) in arranged.iter_mut().zip(dirs) {
                let res = self.processor.getBusArrangement(dir, 0, arr);
                if res != kResultOk || *arr != SpeakerArr::kStereo {
                    return Err("the plugin does not support a stereo bus".into());
                }
                // Not every plugin enables its buses by default.
                self.component.activateBus(ids::AUDIO, dir, 0, 1);
            }
            self.build_buses(counts);
            ok(
                self.processor.canProcessSampleSize(ids::SAMPLE32),
                "32-bit processing",
            )?;
            let mut setup = ProcessSetup {
                processMode: self.mode.sdk(),
                symbolicSampleSize: ids::SAMPLE32,
                maxSamplesPerBlock: MIX_BLOCK_FRAMES as i32,
                sampleRate: f64::from(sample_rate),
            };
            ok(
                self.processor.setupProcessing(&mut setup),
                "setupProcessing",
            )?;
            // Tempo-synced plugins need something sane; there is no transport to follow.
            let context = &mut (*self.rt.get()).context;
            context.sampleRate = f64::from(sample_rate);
            context.tempo = 120.0;
            context.timeSigNumerator = 4;
            context.timeSigDenominator = 4;
            context.state = ids::TEMPO_VALID | ids::TIME_SIG_VALID;
        }
        Ok(())
    }

    /// Every declared bus gets a buffer (VST3 sizes `ProcessData` by the declared
    /// buses); the extras are switched off and backed by silent/scratch memory.
    fn build_buses(&self, counts: [i32; 2]) {
        // SAFETY: setup runs before the instance is shared; nothing else touches `rt`.
        let rt = unsafe { &mut *self.rt.get() };
        let mut ins = Buses::empty();
        let mut outs = Buses::empty();
        ins.push(rt.inp.iter_mut().map(|c| c.as_mut_ptr()).collect(), 0);
        outs.push(rt.out.iter_mut().map(|c| c.as_mut_ptr()).collect(), 0);
        for (buses, dir, count) in [
            (&mut ins, ids::INPUT, counts[0]),
            (&mut outs, ids::OUTPUT, counts[1]),
        ] {
            for index in 1..count {
                // SAFETY: a live component; `info` is a plain C struct filled by the callee.
                let channels = unsafe {
                    let mut arr = SpeakerArr::kStereo;
                    self.processor.getBusArrangement(dir, index, &mut arr);
                    self.component.activateBus(ids::AUDIO, dir, index, 0);
                    (arr.count_ones() as usize).max(1)
                };
                let mut planes = vec![vec![0.0f32; MIX_BLOCK_FRAMES]; channels];
                let table = planes.iter_mut().map(|c| c.as_mut_ptr()).collect();
                buses.scratch.extend(planes);
                // Inputs are never written, so they stay silent.
                let silence = if dir == ids::INPUT { u64::MAX } else { 0 };
                buses.push(table, silence);
            }
        }
        rt.ins = ins;
        rt.outs = outs;
    }

    // A single object already took the state through its component half.
    fn sync_controller(&self, component_state: &[u8]) {
        let (Some(c), true) = (&self.controller, self.separate_controller) else {
            return;
        };
        if component_state.is_empty() {
            return;
        }
        if let Some(p) = MemStream::new(component_state.to_vec()).to_com_ptr::<IBStream>() {
            // SAFETY: the stream outlives the call; the controller copies what it needs.
            unsafe { c.setComponentState(p.as_ptr()) };
        }
    }

    // A plugin that rejects a state keeps its defaults: not worth failing the insert.
    fn load_state(&self, saved_component: &PluginState, saved_controller: &PluginState) {
        let stream = |bytes: &[u8]| MemStream::new(bytes.to_vec());
        // An empty half is the plugin's defaults, not "leave as is".
        let half = |saved: &PluginState, default: &[u8]| -> Vec<u8> {
            let bytes = saved.bytes();
            if bytes.is_empty() {
                default.to_vec()
            } else {
                bytes.to_vec()
            }
        };
        let component = &half(saved_component, &self.defaults.0);
        let controller = &half(saved_controller, &self.defaults.1);
        // SAFETY: the streams outlive each call; the plugin copies what it needs.
        unsafe {
            if !component.is_empty() {
                let s = stream(component);
                if let Some(p) = s.to_com_ptr::<IBStream>() {
                    self.component.setState(p.as_ptr());
                }
                self.sync_controller(component);
            }
            if !controller.is_empty() {
                let s = stream(controller);
                if let (Some(c), Some(p)) = (&self.controller, s.to_com_ptr::<IBStream>()) {
                    c.setState(p.as_ptr());
                }
            }
        }
    }

    fn try_lock_audio(&self) -> Option<AudioGuard<'_>> {
        AudioGuard::try_acquire(&self.busy)
    }

    fn lock_audio(&self) -> AudioGuard<'_> {
        loop {
            if let Some(guard) = self.try_lock_audio() {
                return guard;
            }
            std::thread::yield_now();
        }
    }

    /// Every restart flag raised while activating is dropped on purpose: the
    /// instance was just (re)activated with its current state, and keeping one
    /// pending would reactivate a running instance (or loop, if raised each time).
    fn refresh_latency_after_activation(&self) {
        self.shared.restart.store(0, Ordering::Release);
        self.refresh_latency();
    }

    /// Stops and deactivates (only what is still running), then activates again.
    /// A refused `setActive(1)` leaves the instance inactive: it plays dry and
    /// the next call retries.
    ///
    /// # Safety
    /// The caller holds the audio guard.
    unsafe fn cycle_activation(&self, between: impl FnOnce()) {
        // SAFETY: a live processor and component, per the caller.
        unsafe {
            let was_active = self.active.swap(false, Ordering::Relaxed);
            if was_active {
                self.processor.setProcessing(0);
                self.component.setActive(0);
            }
            between();
            if self.component.setActive(1) == kResultOk {
                // Edits queued while inactive were never delivered; the state carries them.
                // From an active plugin they are still due and flushed by the next block.
                if !was_active {
                    self.shared.clear_edits();
                }
                self.active.store(true, Ordering::Relaxed);
                self.processor.setProcessing(1);
            }
        }
    }

    fn refresh_latency(&self) {
        // SAFETY: a live processor.
        let samples = unsafe { self.processor.getLatencySamples() };
        self.latency.store(samples, Ordering::Relaxed);
    }

    /// Queued edits reach the processor first (a VST3 parameter flush), so the
    /// component state includes them even when no audio is running.
    /// `None` when the plugin failed to produce its state: an empty state is
    /// valid and means "defaults", so a failure must never pass for one.
    pub fn get_state(&self) -> Option<(PluginState, PluginState)> {
        self.flush_params();
        let (component, controller) = self.read_state()?;
        Some((PluginState::new(component), PluginState::new(controller)))
    }

    fn read_state(&self) -> Option<(Vec<u8>, Vec<u8>)> {
        let state = self.try_read_state();
        if state.is_none()
            && first_failure(&format!("getState:{}", self.class_id), "getState failed")
        {
            eprintln!("vv-plugin: {}: getState failed", self.class_id);
        }
        state
    }

    fn try_read_state(&self) -> Option<(Vec<u8>, Vec<u8>)> {
        let save = |write: &dyn Fn(*mut IBStream) -> tresult| {
            let stream = MemStream::new(Vec::new());
            let p = stream.to_com_ptr::<IBStream>()?;
            let result = write(p.as_ptr());
            Some((result, stream.take()))
        };
        // SAFETY: the stream is live for the call.
        let (result, component) = save(&|s| unsafe { self.component.getState(s) })?;
        if result != kResultOk {
            return None;
        }
        let Some(c) = &self.controller else {
            return Some((component, Vec::new()));
        };
        // SAFETY: as above.
        let (result, controller) = save(&|s| unsafe { c.getState(s) })?;
        // Plugins without controller state answer with one of these.
        match result {
            r if r == kResultOk => Some((component, controller)),
            r if r == kResultFalse || r == kNotImplemented => Some((component, Vec::new())),
            _ => None,
        }
    }

    fn flush_params(&self) {
        let audio = self.lock_audio();
        self.flush_params_locked(&audio);
    }

    fn flush_params_locked(&self, _audio: &AudioGuard<'_>) {
        if !self.active.load(Ordering::Relaxed) {
            return;
        }
        // SAFETY: the guard gives this thread exclusive use of `rt`.
        let rt = unsafe { &mut *self.rt.get() };
        // Bounded so a plugin that keeps queueing edits from inside `process` cannot spin us.
        let mut slices = None;
        loop {
            rt.changes.clear();
            {
                let mut params = self.shared.params.lock().unwrap_or_else(|e| e.into_inner());
                let left = slices.get_or_insert(params.len().div_ceil(MAX_PARAMS) + 1);
                if *left == 0 {
                    break;
                }
                *left -= 1;
                self.shared.fill(&rt.changes, Some(&mut params));
            }
            if !rt.changes.has_any() {
                break;
            }
            self.run_slice(rt, 0);
        }
        rt.changes.clear();
    }

    pub(crate) fn controller(&self) -> Option<&ComPtr<IEditController>> {
        self.controller.as_ref()
    }

    pub(crate) fn set_waker(&self, waker: Option<crate::host::Waker>) {
        self.shared.set_waker(waker);
    }

    /// An edit gesture ended since the last call.
    pub fn take_dirty(&self) -> bool {
        self.shared.dirty.swap(false, Ordering::AcqRel)
    }

    /// Audio thread: never blocks, a contended block is skipped.
    fn publish_outputs(&self, outputs: &ParamChanges) {
        let Ok(mut latest) = self.shared.outputs.try_lock() else {
            return;
        };
        let mut fresh = false;
        outputs.for_each_value(|id, value| {
            if let Some(slot) = latest.iter_mut().find(|p| p.0 == id) {
                if slot.1 != value {
                    *slot = (id, value, true);
                    fresh = true;
                }
            } else if latest.len() < latest.capacity() {
                latest.push((id, value, true));
                fresh = true;
            }
        });
        if fresh {
            self.shared.outputs_fresh.store(true, Ordering::Release);
        }
    }

    /// Hands the plugin's changed output parameters to its controller; UI thread only.
    pub(crate) fn forward_outputs(&self) {
        let Some(controller) = &self.controller else {
            return;
        };
        if !self.shared.outputs_fresh.load(Ordering::Acquire) {
            return;
        }
        let mut changed = Vec::new();
        {
            let mut latest = self
                .shared
                .outputs
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            self.shared.outputs_fresh.store(false, Ordering::Release);
            for slot in latest.iter_mut().filter(|p| p.2) {
                slot.2 = false;
                changed.push((slot.0, slot.1));
            }
        }
        for (id, value) in changed {
            // SAFETY: a live controller, called from the UI thread.
            unsafe { controller.setParamNormalized(id, value) };
        }
    }

    /// Output parameters changed since `forward_outputs` last ran.
    pub(crate) fn outputs_fresh(&self) -> bool {
        self.shared.outputs_fresh.load(Ordering::Acquire)
    }

    fn run_slice(&self, rt: &mut Rt, frames: usize) {
        let mut data = ProcessData {
            processMode: self.mode.sdk(),
            symbolicSampleSize: ids::SAMPLE32,
            numSamples: frames as i32,
            numInputs: rt.ins.bufs.len() as i32,
            numOutputs: rt.outs.bufs.len() as i32,
            inputs: rt.ins.bufs.as_mut_ptr(),
            outputs: rt.outs.bufs.as_mut_ptr(),
            inputParameterChanges: rt.changes_ptr.as_ptr(),
            outputParameterChanges: rt.outputs_ptr.as_ptr(),
            inputEvents: std::ptr::null_mut(),
            outputEvents: std::ptr::null_mut(),
            processContext: &mut rt.context,
        };
        rt.outputs.clear();
        let _scope = crate::host::AudioScope::enter();
        // SAFETY: buffers hold `frames` samples each; `data` lives for the call.
        let res = unsafe { self.processor.process(&mut data) };
        self.publish_outputs(&rt.outputs);
        if res != kResultOk {
            for c in 0..2 {
                rt.out[c][..frames].copy_from_slice(&rt.inp[c][..frames]);
            }
        }
    }
}

impl HostedPlugin for Vst3Instance {
    fn into_vst3(self: Arc<Self>) -> Option<Arc<Vst3Instance>> {
        Some(self)
    }

    fn bundle(&self) -> Option<&Path> {
        Some(&self.bundle)
    }

    /// Waits for the audio thread to leave `process`; UI thread only.
    fn set_state(&self, effect: &Vst3Effect) {
        let _audio = self.lock_audio();
        // A stale edit must not override the state just loaded.
        self.shared.clear_edits();
        self.load_state(&effect.component_state, &effect.controller_state);
        self.refresh_latency();
        self.mark_state(effect.content_hash());
    }

    fn state_hash(&self) -> u64 {
        self.state_hash.load(Ordering::Relaxed)
    }

    fn mark_state(&self, hash: u64) {
        self.state_hash.store(hash, Ordering::Relaxed);
    }

    /// The deactivate/activate cycle the spec requires; kIoChanged needs
    /// nothing more as the buses are fixed stereo.
    fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    fn service_restart(&self) {
        if self.shared.restart.load(Ordering::Acquire) == 0 {
            return;
        }
        let flags = self.shared.restart.swap(0, Ordering::AcqRel);
        let audio = self.lock_audio();
        // A failed read keeps the plugin's live state rather than reloading defaults.
        let saved = (flags & RestartFlags_::kReloadComponent != 0)
            .then(|| {
                self.flush_params_locked(&audio);
                self.read_state()
            })
            .flatten();
        // SAFETY: a live processor and component, from the UI thread; the busy
        // guard keeps the audio thread out while processing is stopped.
        unsafe {
            self.cycle_activation(|| {
                if let Some((component, controller)) = saved {
                    // The flush above delivered the queued edits; drop what arrived since.
                    self.shared.clear_edits();
                    self.load_state(&PluginState::new(component), &PluginState::new(controller));
                }
            });
        }
        self.refresh_latency_after_activation();
    }

    fn reactivate(&self) {
        let _audio = self.lock_audio();
        // SAFETY: as in `service_restart`.
        unsafe { self.cycle_activation(|| {}) };
        self.refresh_latency_after_activation();
    }

    fn take_gesture(&self) -> Option<(PluginState, PluginState)> {
        if !self.take_dirty() {
            return None;
        }
        let state = self.get_state();
        if state.is_some() {
            self.state_failures.store(0, Ordering::Relaxed);
        } else if self.state_failures.fetch_add(1, Ordering::Relaxed) + 1 < MAX_STATE_FAILURES {
            // Retried on the next poll; given up after a few so a broken plugin cannot loop.
            self.shared.dirty.store(true, Ordering::Release);
        } else {
            self.state_failures.store(0, Ordering::Relaxed);
        }
        state
    }

    fn gesture_seq(&self) -> u64 {
        self.shared.gesture_seq.load(Ordering::Acquire)
    }
}

impl PluginProcessor for Vst3Instance {
    fn process(&self, samples: &mut [f32], channels: usize) -> bool {
        if channels == 0 {
            return false;
        }
        let Some(_audio) = self.try_lock_audio() else {
            return false;
        };
        if !self.active.load(Ordering::Relaxed) {
            return false;
        }
        // SAFETY: `busy` gives this thread exclusive use of `rt`.
        let rt = unsafe { &mut *self.rt.get() };
        rt.changes.clear();
        match self.shared.params.try_lock() {
            Ok(mut params) => self.shared.fill(&rt.changes, Some(&mut params)),
            Err(TryLockError::Poisoned(e)) => {
                e.into_inner().clear();
                self.shared.fill(&rt.changes, None);
            }
            Err(TryLockError::WouldBlock) => self.shared.fill(&rt.changes, None),
        }
        let total = samples.len() / channels;
        for start in (0..total).step_by(MIX_BLOCK_FRAMES) {
            let frames = MIX_BLOCK_FRAMES.min(total - start);
            let block = &mut samples[start * channels..(start + frames) * channels];
            deinterleave(block, channels, &mut rt.inp, frames);
            self.run_slice(rt, frames);
            interleave(&rt.out, channels, block, frames);
            // Changes go to the first slice only.
            rt.changes.clear();
        }
        true
    }

    fn reset(&self) {
        let Some(_audio) = self.try_lock_audio() else {
            return;
        };
        if !self.active.load(Ordering::Relaxed) {
            return;
        }
        let _scope = crate::host::AudioScope::enter();
        // SAFETY: restarting processing is allowed from the audio thread.
        unsafe {
            self.processor.setProcessing(0);
            self.processor.setProcessing(1);
        }
    }

    fn latency(&self) -> u32 {
        self.latency.load(Ordering::Relaxed)
    }
}

impl Drop for Vst3Instance {
    fn drop(&mut self) {
        // SAFETY: tears down in reverse of setup on live interfaces.
        unsafe {
            if *self.active.get_mut() {
                self.processor.setProcessing(0);
                self.component.setActive(0);
            }
            if let Some(c) = &self.controller {
                c.setComponentHandler(std::ptr::null_mut());
            }
            if let Some((a, b)) = &self.connections {
                a.disconnect(b.as_ptr());
                b.disconnect(a.as_ptr());
            }
            if let Some(c) = self
                .controller
                .as_ref()
                .filter(|_| self.separate_controller)
            {
                c.terminate();
            }
            self.component.terminate();
        }
    }
}

/// Mono feeds both plugin inputs; more than two channels fold into the pair like `remix_channels_into` (`PluginEffect` re-derives the rest).
pub(crate) fn deinterleave(
    samples: &[f32],
    channels: usize,
    out: &mut [Vec<f32>; 2],
    frames: usize,
) {
    if channels > 2 {
        for (n, frame) in samples.chunks_exact(channels).take(frames).enumerate() {
            for (k, plane) in out.iter_mut().enumerate() {
                let (sum, count) = frame[k..]
                    .iter()
                    .step_by(2)
                    .fold((0.0, 0u32), |(sum, count), &v| (sum + v, count + 1));
                plane[n] = sum / count as f32;
            }
        }
        return;
    }
    let right = usize::from(channels > 1);
    for (n, frame) in samples.chunks_exact(channels).take(frames).enumerate() {
        out[0][n] = frame[0];
        out[1][n] = frame[right];
    }
}

pub(crate) fn interleave(inp: &[Vec<f32>; 2], channels: usize, samples: &mut [f32], frames: usize) {
    for (n, frame) in samples.chunks_exact_mut(channels).take(frames).enumerate() {
        if channels > 1 {
            frame[0] = inp[0][n];
            frame[1] = inp[1][n];
        } else {
            frame[0] = (inp[0][n] + inp[1][n]) * 0.5;
        }
    }
}

#[cfg(test)]
#[path = "tests/instance.rs"]
mod tests;

/// Output parameters are the read-only ones; asked once per parameter at setup.
fn read_only_count(controller: &ComPtr<IEditController>) -> usize {
    // SAFETY: a live controller, called before any processing starts.
    let total = unsafe { controller.getParameterCount() };
    (0..total)
        .filter(|&i| {
            // SAFETY: a plain C struct; zero is valid, and the controller fills it.
            let mut info: ParameterInfo = unsafe { std::mem::zeroed() };
            let res = unsafe { controller.getParameterInfo(i, &mut info) };
            res == kResultOk && info.flags & ParameterInfo_::ParameterFlags_::kIsReadOnly != 0
        })
        .count()
}
