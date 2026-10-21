//! A stereo gain VST3 used as a host test fixture. The VST3 API is raw FFI, so
//! the trait impls are unsafe by nature; nothing here may panic across it.
use std::ffi::{CString, c_char, c_void};
use std::ptr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComRef, ComWrapper};

const PLUGIN_NAME: &str = "Venturi Test Gain";
const VENDOR: &str = "Venturi";
// ASCII "VVTESTGAN" then zeros and a trailing 1: hex 56565445535447414E00000000000001.
const CID: TUID = [
    0x56, 0x56, 0x54, 0x45, 0x53, 0x54, 0x47, 0x41, 0x4E, 0, 0, 0, 0, 0, 0, 1,
];
// Split variant: a processor-only component and its separate controller object.
const SPLIT_CID: TUID = [
    0x56, 0x56, 0x54, 0x45, 0x53, 0x54, 0x47, 0x41, 0x4E, 0, 0, 0, 0, 0, 0, 2,
];
const SPLIT_CONTROLLER_CID: TUID = [
    0x56, 0x56, 0x54, 0x45, 0x53, 0x54, 0x47, 0x41, 0x4E, 0, 0, 0, 0, 0, 0, 3,
];
// Single-component variant whose setProcessing returns kNotImplemented, like the SDK default.
const NOT_IMPL_CID: TUID = [
    0x56, 0x56, 0x54, 0x45, 0x53, 0x54, 0x47, 0x41, 0x4E, 0, 0, 0, 0, 0, 0, 4,
];
// Same, returning kResultFalse.
const FALSE_CID: TUID = [
    0x56, 0x56, 0x54, 0x45, 0x53, 0x54, 0x47, 0x41, 0x4E, 0, 0, 0, 0, 0, 0, 5,
];
// Single component whose state also reports how many distinct parameter ids (1..128) it received.
const COUNTING_CID: TUID = [
    0x56, 0x56, 0x54, 0x45, 0x53, 0x54, 0x47, 0x41, 0x4E, 0, 0, 0, 0, 0, 0, 6,
];
// Single component that writes its input peak to output parameter 1 every block.
const METER_CID: TUID = [
    0x56, 0x56, 0x54, 0x45, 0x53, 0x54, 0x47, 0x41, 0x4E, 0, 0, 0, 0, 0, 0, 7,
];
// Single component that reports a latency of 64 only once it was reactivated.
const LATENCY_CID: TUID = [
    0x56, 0x56, 0x54, 0x45, 0x53, 0x54, 0x47, 0x41, 0x4E, 0, 0, 0, 0, 0, 0, 8,
];
// The next three record their lifecycle calls in `lifecycle_log_path(cid)`:
// mono-only (rejects the stereo bus), rejecting setupProcessing, and well-behaved.
const MONO_CID: TUID = [
    0x56, 0x56, 0x54, 0x45, 0x53, 0x54, 0x47, 0x41, 0x4E, 0, 0, 0, 0, 0, 0, 9,
];
const SETUP_FAIL_CID: TUID = [
    0x56, 0x56, 0x54, 0x45, 0x53, 0x54, 0x47, 0x41, 0x4E, 0, 0, 0, 0, 0, 0, 10,
];
const LOGGED_CID: TUID = [
    0x56, 0x56, 0x54, 0x45, 0x53, 0x54, 0x47, 0x41, 0x4E, 0, 0, 0, 0, 0, 0, 11,
];
// Logged, and raises restartComponent(kLatencyChanged) from inside setActive(1).
const RESTART_CID: TUID = [
    0x56, 0x56, 0x54, 0x45, 0x53, 0x54, 0x47, 0x41, 0x4E, 0, 0, 0, 0, 0, 0, 12,
];
// Its component getState fails while `state_fail_marker()` exists.
const STATE_FAIL_CID: TUID = [
    0x56, 0x56, 0x54, 0x45, 0x53, 0x54, 0x47, 0x41, 0x4E, 0, 0, 0, 0, 0, 0, 13,
];
// Reports MANY_METERS parameters and writes all of them as outputs every block.
const MANY_METERS_CID: TUID = [
    0x56, 0x56, 0x54, 0x45, 0x53, 0x54, 0x47, 0x41, 0x4E, 0, 0, 0, 0, 0, 0, 14,
];
// Declares a sidechain input and an aux output next to the main buses; the first
// output sample reports what process received (see `report_buses`).
const AUX_CID: TUID = [
    0x56, 0x56, 0x54, 0x45, 0x53, 0x54, 0x47, 0x41, 0x4E, 0, 0, 0, 0, 0, 0, 15,
];
// Declares an aux pair but only accepts the main buses, whose default is mono.
const LEGACY_CID: TUID = [
    0x56, 0x56, 0x54, 0x45, 0x53, 0x54, 0x47, 0x41, 0x4E, 0, 0, 0, 0, 0, 0, 16,
];
// Raises restartComponent(kLatencyChanged) from inside process.
const PROCESS_RESTART_CID: TUID = [
    0x56, 0x56, 0x54, 0x45, 0x53, 0x54, 0x47, 0x41, 0x4E, 0, 0, 0, 0, 0, 0, 17,
];
// Logged, and refuses every activation after the first.
const REACTIVATE_FAIL_CID: TUID = [
    0x56, 0x56, 0x54, 0x45, 0x53, 0x54, 0x47, 0x41, 0x4E, 0, 0, 0, 0, 0, 0, 18,
];
// Reports a latency of 64 once reactivated, and raises restartComponent(kLatencyChanged)
// from inside setState.
const SET_STATE_RESTART_CID: TUID = [
    0x56, 0x56, 0x54, 0x45, 0x53, 0x54, 0x47, 0x41, 0x4E, 0, 0, 0, 0, 0, 0, 19,
];
// Split variant whose processor starts at 0.25 while its controller reports 0.5.
const SPLIT_PRESET_CID: TUID = [
    0x56, 0x56, 0x54, 0x45, 0x53, 0x54, 0x47, 0x41, 0x4E, 0, 0, 0, 0, 0, 0, 20,
];
// Calls performEdit(0, 0.25) from inside process.
const PROCESS_EDIT_CID: TUID = [
    0x56, 0x56, 0x54, 0x45, 0x53, 0x54, 0x47, 0x41, 0x4E, 0, 0, 0, 0, 0, 0, 21,
];
const MANY_METERS: u32 = 100;
const REACTIVATED_LATENCY: u32 = 64;
const DEFAULT_NORMALIZED: f64 = 0.5;

/// The plugin is loaded into the test process, so the pid matches the test's.
fn lifecycle_log_path(tag: u8) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "venturi-fixture-lifecycle-{}-{tag}.log",
        std::process::id()
    ))
}

fn state_fail_marker() -> std::path::PathBuf {
    let mut name = lifecycle_log_path(13).into_os_string();
    name.push(".fail");
    name.into()
}

fn copy_cstring(src: &str, dst: &mut [c_char]) {
    let bytes = CString::new(src).unwrap_or_default();
    let bytes = bytes.as_bytes_with_nul();
    let n = bytes.len().min(dst.len());
    for (d, s) in dst.iter_mut().zip(&bytes[..n]) {
        *d = *s as c_char;
    }
    if let Some(last) = dst.get_mut(n.saturating_sub(1)) {
        *last = 0;
    }
}

fn copy_wstring(src: &str, dst: &mut [TChar]) {
    let mut len = 0;
    for (d, s) in dst.iter_mut().zip(src.encode_utf16()) {
        *d = s as TChar;
        len += 1;
    }
    if let Some(end) = dst.get_mut(len.min(dst.len().saturating_sub(1))) {
        *end = 0;
    }
}

fn gain(normalized: f64) -> f32 {
    (normalized.clamp(0.0, 1.0) * 2.0) as f32
}

struct Gain {
    // Normalized value as f64 bits, shared by the processor and controller halves
    // (in the split variant each object has its own copy).
    normalized: AtomicU64,
    controller_cid: Option<TUID>,
    set_processing_result: tresult,
    counts_params: bool,
    writes_meter: bool,
    meter_count: u32,
    latency_on_reactivation: bool,
    log_tag: Option<u8>,
    mono_only: bool,
    rejects_setup: bool,
    restarts_on_activation: bool,
    restarts_in_process: bool,
    restarts_in_set_state: bool,
    edits_in_process: bool,
    fails_state: bool,
    fails_reactivation: bool,
    aux: bool,
    legacy: bool,
    stereo_set: AtomicBool,
    handler: Mutex<Option<ComPtr<IComponentHandler>>>,
    activations: AtomicU32,
    // Output parameter 1 as the controller last received it.
    meter: AtomicU64,
    seen: [AtomicU64; 2],
}

impl Gain {
    fn new(controller_cid: Option<TUID>) -> Self {
        Self {
            normalized: AtomicU64::new(DEFAULT_NORMALIZED.to_bits()),
            controller_cid,
            set_processing_result: kResultOk,
            counts_params: false,
            writes_meter: false,
            meter_count: 1,
            latency_on_reactivation: false,
            log_tag: None,
            mono_only: false,
            rejects_setup: false,
            restarts_on_activation: false,
            restarts_in_process: false,
            restarts_in_set_state: false,
            edits_in_process: false,
            fails_state: false,
            fails_reactivation: false,
            aux: false,
            legacy: false,
            stereo_set: AtomicBool::new(false),
            handler: Mutex::new(None),
            activations: AtomicU32::new(0),
            meter: AtomicU64::new(0),
            seen: [AtomicU64::new(0), AtomicU64::new(0)],
        }
    }

    fn buses(&self) -> i32 {
        1 + i32::from(self.aux)
    }

    fn meters(&self) -> u32 {
        if self.writes_meter {
            self.meter_count
        } else {
            0
        }
    }

    fn log(&self, call: &str) {
        use std::io::Write;
        let Some(tag) = self.log_tag else { return };
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(lifecycle_log_path(tag))
        {
            let _ = writeln!(file, "{call}");
        }
    }

    fn get(&self) -> f64 {
        f64::from_bits(self.normalized.load(Ordering::Relaxed))
    }

    fn set(&self, v: f64) {
        self.normalized
            .store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    unsafe fn read_state(&self, stream: *mut IBStream) -> tresult {
        let Some(stream) = (unsafe { ComRef::from_raw(stream) }) else {
            return kInvalidArgument;
        };
        let mut buf = [0u8; 4];
        let mut read = 0;
        let res = unsafe { stream.read(buf.as_mut_ptr().cast::<c_void>(), 4, &mut read) };
        if res != kResultOk || read != 4 {
            return kResultFalse;
        }
        self.set(f64::from(f32::from_le_bytes(buf)));
        kResultOk
    }
}

/// Touches the extra buses the way a sidechain plugin does, then overwrites the first
/// output sample with `numInputs + 10 * numOutputs + 100 * (all extra pointers usable)`.
unsafe fn report_buses(data: &ProcessData) {
    let mut usable = data.numInputs >= 2 && data.numOutputs >= 2;
    if usable {
        let (side, aux) = unsafe { (&*data.inputs.add(1), &*data.outputs.add(1)) };
        usable = side.numChannels == 2 && aux.numChannels == 2;
        for c in 0..2 {
            let (src, dst) = unsafe {
                (
                    side.__field0.channelBuffers32,
                    aux.__field0.channelBuffers32,
                )
            };
            if src.is_null() || dst.is_null() {
                usable = false;
                break;
            }
            let (src, dst) = unsafe { (*src.add(c), *dst.add(c)) };
            if src.is_null() || dst.is_null() {
                usable = false;
                break;
            }
            for n in 0..data.numSamples.max(0) as usize {
                unsafe { *dst.add(n) = *src.add(n) + 0.25 };
            }
        }
    }
    let report = data.numInputs + 10 * data.numOutputs + 100 * i32::from(usable);
    let out = unsafe { &*data.outputs };
    let first = unsafe { *out.__field0.channelBuffers32 };
    if data.numSamples > 0 && !first.is_null() {
        unsafe { *first = report as f32 };
    }
}

impl Gain {
    unsafe fn write_meter(&self, data: &ProcessData) {
        let Some(out) = (unsafe { ComRef::from_raw(data.outputParameterChanges) }) else {
            return;
        };
        if data.numInputs < 1 || data.inputs.is_null() {
            return;
        }
        let input = unsafe { &*data.inputs };
        let ins = unsafe { input.__field0.channelBuffers32 };
        if ins.is_null() || input.numChannels < 1 {
            return;
        }
        let src = unsafe { *ins };
        let mut peak = 0.0f32;
        for n in 0..data.numSamples.max(0) as usize {
            peak = peak.max(unsafe { *src.add(n) }.abs());
        }
        for id in 1..=self.meter_count {
            let mut index = 0;
            let queue = unsafe { out.addParameterData(&id, &mut index) };
            // Deliberately unchecked beyond the null test: the host must never hand back null.
            assert!(!queue.is_null(), "output queue for id {id} is null");
            if let Some(queue) = unsafe { ComRef::from_raw(queue) } {
                unsafe { queue.addPoint(0, f64::from(peak.min(1.0)), &mut index) };
            }
        }
    }
}

impl Class for Gain {
    type Interfaces = (IComponent, IAudioProcessor, IEditController);
}

impl IPluginBaseTrait for Gain {
    unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
        kResultOk
    }

    unsafe fn terminate(&self) -> tresult {
        self.log("terminate");
        kResultOk
    }
}

impl IComponentTrait for Gain {
    unsafe fn getControllerClassId(&self, class_id: *mut TUID) -> tresult {
        // Single-component effect: the host queries IEditController on the component.
        match self.controller_cid {
            Some(cid) if !class_id.is_null() => {
                unsafe { *class_id = cid };
                kResultOk
            }
            _ => kNotImplemented,
        }
    }

    unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
        kResultOk
    }

    unsafe fn getBusCount(&self, media_type: MediaType, _dir: BusDirection) -> i32 {
        if media_type == MediaTypes_::kAudio as MediaType {
            self.buses()
        } else {
            0
        }
    }

    unsafe fn getBusInfo(
        &self,
        media_type: MediaType,
        dir: BusDirection,
        index: i32,
        bus: *mut BusInfo,
    ) -> tresult {
        if media_type != MediaTypes_::kAudio as MediaType
            || !(0..self.buses()).contains(&index)
            || bus.is_null()
        {
            return kInvalidArgument;
        }
        let bus = unsafe { &mut *bus };
        bus.mediaType = MediaTypes_::kAudio as MediaType;
        bus.direction = dir;
        bus.channelCount = 2;
        copy_wstring(
            if dir == BusDirections_::kInput as BusDirection {
                "Input"
            } else {
                "Output"
            },
            &mut bus.name,
        );
        #[allow(clippy::unnecessary_cast)] // enum constant width differs per target
        let default_active = BusInfo_::BusFlags_::kDefaultActive as u32;
        if index == 0 {
            bus.busType = BusTypes_::kMain as BusType;
            bus.flags = default_active;
        } else {
            bus.busType = BusTypes_::kAux as BusType;
            bus.flags = 0;
        }
        kResultOk
    }

    unsafe fn getRoutingInfo(&self, _in: *mut RoutingInfo, _out: *mut RoutingInfo) -> tresult {
        kNotImplemented
    }

    unsafe fn activateBus(&self, _m: MediaType, _d: BusDirection, _i: i32, _s: TBool) -> tresult {
        kResultOk
    }

    unsafe fn setActive(&self, state: TBool) -> tresult {
        self.log(&format!("setActive({state})"));
        if state != 0 {
            let earlier = self.activations.fetch_add(1, Ordering::Relaxed);
            if self.fails_reactivation && earlier > 0 {
                return kResultFalse;
            }
            if self.restarts_on_activation
                && let Some(handler) = self.handler.lock().ok().and_then(|h| h.clone())
            {
                unsafe { handler.restartComponent(RestartFlags_::kLatencyChanged) };
            }
        }
        kResultOk
    }

    unsafe fn setState(&self, state: *mut IBStream) -> tresult {
        let result = unsafe { self.read_state(state) };
        if self.restarts_in_set_state
            && let Some(handler) = self.handler.lock().ok().and_then(|h| h.clone())
        {
            unsafe { handler.restartComponent(RestartFlags_::kLatencyChanged) };
        }
        result
    }

    unsafe fn getState(&self, state: *mut IBStream) -> tresult {
        if self.fails_state && state_fail_marker().exists() {
            return kInternalError;
        }
        let Some(stream) = (unsafe { ComRef::from_raw(state) }) else {
            return kInvalidArgument;
        };
        let mut bytes = Vec::from((self.get() as f32).to_le_bytes());
        if self.counts_params {
            let n: u32 = self
                .seen
                .iter()
                .map(|w| w.load(Ordering::Relaxed).count_ones())
                .sum();
            bytes.extend_from_slice(&n.to_le_bytes());
        }
        let len = bytes.len() as i32;
        let mut written = 0;
        let res = unsafe { stream.write(bytes.as_mut_ptr().cast::<c_void>(), len, &mut written) };
        if res == kResultOk && written == len {
            kResultOk
        } else {
            kResultFalse
        }
    }
}

impl IAudioProcessorTrait for Gain {
    unsafe fn setBusArrangements(
        &self,
        inputs: *mut SpeakerArrangement,
        num_ins: i32,
        outputs: *mut SpeakerArrangement,
        num_outs: i32,
    ) -> tresult {
        if self.legacy {
            let ok = num_ins == 1 && num_outs == 1 && !inputs.is_null() && !outputs.is_null();
            if !ok || unsafe { *inputs != SpeakerArr::kStereo || *outputs != SpeakerArr::kStereo } {
                return kResultFalse;
            }
            self.stereo_set.store(true, Ordering::Relaxed);
            return kResultTrue;
        }
        if num_ins != self.buses()
            || num_outs != self.buses()
            || inputs.is_null()
            || outputs.is_null()
        {
            return kResultFalse;
        }
        if self.mono_only {
            return kResultFalse;
        }
        let stereo = unsafe { *inputs == SpeakerArr::kStereo && *outputs == SpeakerArr::kStereo };
        if stereo { kResultTrue } else { kResultFalse }
    }

    unsafe fn getBusArrangement(
        &self,
        _dir: BusDirection,
        index: i32,
        arr: *mut SpeakerArrangement,
    ) -> tresult {
        if !(0..self.buses()).contains(&index) || arr.is_null() {
            return kInvalidArgument;
        }
        unsafe {
            *arr = if self.mono_only || (self.legacy && !self.stereo_set.load(Ordering::Relaxed)) {
                SpeakerArr::kMono
            } else {
                SpeakerArr::kStereo
            }
        };
        kResultOk
    }

    unsafe fn canProcessSampleSize(&self, size: i32) -> tresult {
        if size == SymbolicSampleSizes_::kSample32 as i32 {
            kResultOk
        } else {
            kNotImplemented
        }
    }

    unsafe fn getLatencySamples(&self) -> u32 {
        if self.latency_on_reactivation && self.activations.load(Ordering::Relaxed) >= 2 {
            REACTIVATED_LATENCY
        } else {
            0
        }
    }

    unsafe fn setupProcessing(&self, _setup: *mut ProcessSetup) -> tresult {
        if self.rejects_setup {
            return kResultFalse;
        }
        kResultOk
    }

    unsafe fn setProcessing(&self, state: TBool) -> tresult {
        self.log(&format!("setProcessing({state})"));
        self.set_processing_result
    }

    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        if self.fails_reactivation {
            self.log("process");
        }
        if data.is_null() {
            return kInvalidArgument;
        }
        let data = unsafe { &*data };
        if let Some(changes) = unsafe { ComRef::from_raw(data.inputParameterChanges) } {
            for i in 0..unsafe { changes.getParameterCount() } {
                let Some(queue) = (unsafe { ComRef::from_raw(changes.getParameterData(i)) }) else {
                    continue;
                };
                let points = unsafe { queue.getPointCount() };
                let id = unsafe { queue.getParameterId() };
                if (1..128).contains(&id) {
                    self.seen[id as usize / 64].fetch_or(1 << (id % 64), Ordering::Relaxed);
                }
                if id != 0 || points < 1 {
                    continue;
                }
                let (mut offset, mut value) = (0, 0.0);
                // The last point wins: no per-sample ramps in a test fixture.
                if unsafe { queue.getPoint(points - 1, &mut offset, &mut value) } == kResultTrue {
                    self.set(value);
                }
            }
        }

        if data.numInputs < 1
            || data.numOutputs < 1
            || data.inputs.is_null()
            || data.outputs.is_null()
        {
            return kResultOk;
        }
        let (input, output) = unsafe { (&*data.inputs, &*data.outputs) };
        let channels = input.numChannels.min(output.numChannels).max(0) as usize;
        let (ins, outs) = unsafe {
            (
                input.__field0.channelBuffers32,
                output.__field0.channelBuffers32,
            )
        };
        if ins.is_null() || outs.is_null() {
            return kResultOk;
        }
        let g = gain(self.get());
        if self.restarts_in_process
            && let Some(handler) = self.handler.lock().ok().and_then(|h| h.clone())
        {
            unsafe { handler.restartComponent(RestartFlags_::kLatencyChanged) };
        }
        if self.edits_in_process
            && let Some(handler) = self.handler.lock().ok().and_then(|h| h.clone())
        {
            unsafe { handler.performEdit(0, 0.25) };
        }
        if self.writes_meter {
            unsafe { self.write_meter(data) };
        }
        for c in 0..channels {
            // Raw pointers: hosts may process in place, where &/&mut slices would alias.
            let (src, dst) = unsafe { (*ins.add(c), *outs.add(c)) };
            if src.is_null() || dst.is_null() {
                continue;
            }
            for n in 0..data.numSamples.max(0) as usize {
                unsafe { *dst.add(n) = *src.add(n) * g };
            }
        }
        if self.aux {
            unsafe { report_buses(data) };
        }
        kResultOk
    }

    unsafe fn getTailSamples(&self) -> u32 {
        0
    }
}

impl IEditControllerTrait for Gain {
    unsafe fn setComponentState(&self, state: *mut IBStream) -> tresult {
        unsafe { self.read_state(state) }
    }

    unsafe fn setState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getState(&self, _state: *mut IBStream) -> tresult {
        kResultOk
    }

    unsafe fn getParameterCount(&self) -> i32 {
        1 + self.meters() as i32
    }

    unsafe fn getParameterInfo(&self, index: i32, info: *mut ParameterInfo) -> tresult {
        if index < 0 || index > self.meters() as i32 || info.is_null() {
            return kInvalidArgument;
        }
        let info = unsafe { &mut *info };
        let name = if index == 0 { "Gain" } else { "Meter" };
        info.id = index as u32;
        copy_wstring(name, &mut info.title);
        copy_wstring(name, &mut info.shortTitle);
        copy_wstring("", &mut info.units);
        info.stepCount = 0;
        info.defaultNormalizedValue = DEFAULT_NORMALIZED;
        info.unitId = 0;
        #[allow(clippy::unnecessary_cast)] // enum constant width differs per target
        let flags = if index == 0 {
            ParameterInfo_::ParameterFlags_::kCanAutomate as i32
        } else {
            ParameterInfo_::ParameterFlags_::kIsReadOnly as i32
        };
        info.flags = flags;
        kResultOk
    }

    unsafe fn getParamStringByValue(&self, id: u32, value: f64, string: *mut String128) -> tresult {
        if id != 0 || string.is_null() {
            return kInvalidArgument;
        }
        copy_wstring(&format!("{:.2}", gain(value)), unsafe { &mut *string });
        kResultOk
    }

    unsafe fn getParamValueByString(&self, _id: u32, _s: *mut TChar, _v: *mut f64) -> tresult {
        kNotImplemented
    }

    unsafe fn normalizedParamToPlain(&self, _id: u32, normalized: f64) -> f64 {
        f64::from(gain(normalized))
    }

    unsafe fn plainParamToNormalized(&self, _id: u32, plain: f64) -> f64 {
        (plain / 2.0).clamp(0.0, 1.0)
    }

    unsafe fn getParamNormalized(&self, id: u32) -> f64 {
        match id {
            0 => self.get(),
            1 => f64::from_bits(self.meter.load(Ordering::Relaxed)),
            _ => 0.0,
        }
    }

    unsafe fn setParamNormalized(&self, id: u32, value: f64) -> tresult {
        match id {
            0 => self.set(value),
            1 => self.meter.store(value.to_bits(), Ordering::Relaxed),
            _ => return kInvalidArgument,
        }
        kResultOk
    }

    unsafe fn setComponentHandler(&self, handler: *mut IComponentHandler) -> tresult {
        if let Ok(mut slot) = self.handler.lock() {
            *slot = unsafe { ComRef::from_raw(handler) }.map(|h| h.to_com_ptr());
        }
        kResultOk
    }

    unsafe fn createView(&self, _name: FIDString) -> *mut IPlugView {
        ptr::null_mut()
    }
}

struct Factory;

impl Class for Factory {
    type Interfaces = (IPluginFactory2,);
}

impl IPluginFactoryTrait for Factory {
    unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
        if info.is_null() {
            return kInvalidArgument;
        }
        let info = unsafe { &mut *info };
        copy_cstring(VENDOR, &mut info.vendor);
        copy_cstring("", &mut info.url);
        copy_cstring("", &mut info.email);
        info.flags = PFactoryInfo_::FactoryFlags_::kUnicode as i32;
        kResultOk
    }

    unsafe fn countClasses(&self) -> i32 {
        1
    }

    unsafe fn getClassInfo(&self, index: i32, info: *mut PClassInfo) -> tresult {
        if index != 0 || info.is_null() {
            return kInvalidArgument;
        }
        let info = unsafe { &mut *info };
        info.cid = CID;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as i32;
        copy_cstring("Audio Module Class", &mut info.category);
        copy_cstring(PLUGIN_NAME, &mut info.name);
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: FIDString,
        iid: FIDString,
        obj: *mut *mut c_void,
    ) -> tresult {
        if cid.is_null() || iid.is_null() || obj.is_null() {
            return kInvalidArgument;
        }
        let gain = match unsafe { *cid.cast::<TUID>() } {
            CID => Gain::new(None),
            NOT_IMPL_CID => Gain {
                set_processing_result: kNotImplemented,
                ..Gain::new(None)
            },
            FALSE_CID => Gain {
                set_processing_result: kResultFalse,
                ..Gain::new(None)
            },
            COUNTING_CID => Gain {
                counts_params: true,
                ..Gain::new(None)
            },
            METER_CID => Gain {
                writes_meter: true,
                ..Gain::new(None)
            },
            MANY_METERS_CID => Gain {
                writes_meter: true,
                meter_count: MANY_METERS,
                ..Gain::new(None)
            },
            LATENCY_CID => Gain {
                latency_on_reactivation: true,
                ..Gain::new(None)
            },
            MONO_CID => Gain {
                log_tag: Some(9),
                mono_only: true,
                ..Gain::new(None)
            },
            SETUP_FAIL_CID => Gain {
                log_tag: Some(10),
                rejects_setup: true,
                ..Gain::new(None)
            },
            LOGGED_CID => Gain {
                log_tag: Some(11),
                ..Gain::new(None)
            },
            RESTART_CID => Gain {
                log_tag: Some(12),
                restarts_on_activation: true,
                ..Gain::new(None)
            },
            AUX_CID => Gain {
                aux: true,
                ..Gain::new(None)
            },
            LEGACY_CID => Gain {
                aux: true,
                legacy: true,
                ..Gain::new(None)
            },
            PROCESS_RESTART_CID => Gain {
                restarts_in_process: true,
                latency_on_reactivation: true,
                ..Gain::new(None)
            },
            PROCESS_EDIT_CID => Gain {
                edits_in_process: true,
                ..Gain::new(None)
            },
            SET_STATE_RESTART_CID => Gain {
                restarts_in_set_state: true,
                latency_on_reactivation: true,
                ..Gain::new(None)
            },
            REACTIVATE_FAIL_CID => Gain {
                log_tag: Some(18),
                fails_reactivation: true,
                ..Gain::new(None)
            },
            STATE_FAIL_CID => Gain {
                fails_state: true,
                ..Gain::new(None)
            },
            SPLIT_CID => Gain::new(Some(SPLIT_CONTROLLER_CID)),
            SPLIT_PRESET_CID => Gain {
                normalized: AtomicU64::new(0.25f64.to_bits()),
                ..Gain::new(Some(SPLIT_CONTROLLER_CID))
            },
            SPLIT_CONTROLLER_CID => Gain::new(None),
            _ => return kInvalidArgument,
        };
        let Some(instance) = ComWrapper::new(gain).to_com_ptr::<FUnknown>() else {
            return kInternalError;
        };
        let raw = instance.as_ptr();
        // queryInterface adds the reference handed to the caller; ours drops with `instance`.
        unsafe { ((*(*raw).vtbl).queryInterface)(raw, iid.cast_mut().cast::<TUID>(), obj) }
    }
}

impl IPluginFactory2Trait for Factory {
    unsafe fn getClassInfo2(&self, index: i32, info: *mut PClassInfo2) -> tresult {
        if index != 0 || info.is_null() {
            return kInvalidArgument;
        }
        let info = unsafe { &mut *info };
        info.cid = CID;
        info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as i32;
        copy_cstring("Audio Module Class", &mut info.category);
        copy_cstring(PLUGIN_NAME, &mut info.name);
        info.classFlags = 0;
        copy_cstring("Fx", &mut info.subCategories);
        copy_cstring(VENDOR, &mut info.vendor);
        copy_cstring("1.0.0", &mut info.version);
        copy_cstring("VST 3.7", &mut info.sdkVersion);
        kResultOk
    }
}

#[unsafe(no_mangle)]
extern "C" fn GetPluginFactory() -> *mut IPluginFactory {
    match ComWrapper::new(Factory).to_com_ptr::<IPluginFactory>() {
        Some(factory) => factory.into_raw(),
        None => ptr::null_mut(),
    }
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
extern "C" fn bundleEntry(_bundle: *mut c_void) -> bool {
    true
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
extern "C" fn bundleExit() -> bool {
    true
}

#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
extern "C" fn ModuleEntry(_library_handle: *mut c_void) -> bool {
    true
}

#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
extern "C" fn ModuleExit() -> bool {
    true
}
