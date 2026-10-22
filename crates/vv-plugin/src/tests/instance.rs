use std::sync::Arc;

use vst3::Steinberg::Vst::{IComponentHandlerTrait, RestartFlags_};
use vv_audio::PluginProcessor;
use vv_core::Vst3Effect;

use super::*;
use crate::testing::{fixture_bundle, fixture_effect, fixture_effect_of, fixture_instance};

struct CountingAlloc;

thread_local! {
    static ALLOCS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// SAFETY: forwards to the system allocator, only counting on this thread.
unsafe impl std::alloc::GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        unsafe { std::alloc::System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        unsafe { std::alloc::System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new: usize) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        unsafe { std::alloc::System.realloc(ptr, layout, new) }
    }
}

#[global_allocator]
static COUNTING: CountingAlloc = CountingAlloc;

#[test]
fn process_does_not_allocate() {
    let instance = fixture(0.25f32.to_le_bytes().to_vec());
    let mut audio = ramp(1300, 2);
    instance.process(&mut audio, 2);
    // SAFETY: the handler is a live COM object owned by the instance.
    unsafe { instance.handler.performEdit(0, 0.25) };
    for id in 1..101 {
        // SAFETY: as above.
        unsafe { instance.handler.performEdit(id, 0.5) };
    }
    let mut mono = ramp(700, 1);
    let before = ALLOCS.with(|c| c.get());
    for _ in 0..8 {
        instance.process(&mut audio, 2);
    }
    instance.process(&mut mono, 1);
    instance.reset();
    assert_eq!(ALLOCS.with(|c| c.get()) - before, 0);
}

#[test]
fn set_state_discards_pending_edits() {
    let instance = fixture(Vec::new());
    // SAFETY: the handler is a live COM object owned by the instance.
    unsafe { instance.handler.performEdit(0, 0.25) };
    instance.set_state(&effect(Vec::new()));
    let input = ramp(64, 2);
    assert_scaled(&run(&instance, &input), &input, 1.0);
}

fn ramp(frames: usize, channels: usize) -> Vec<f32> {
    (0..frames * channels)
        .map(|i| i as f32 * 0.001 + 0.1)
        .collect()
}

fn scratch() -> [Vec<f32>; 2] {
    let block = vv_audio::MIX_BLOCK_FRAMES;
    [vec![9.0; block], vec![9.0; block]]
}

#[test]
fn deinterleave_round_trip_stereo() {
    let input = ramp(5, 2);
    let mut planar = scratch();
    deinterleave(&input, 2, &mut planar, 5);
    assert_eq!(planar[0][..5], [0.1, 0.102, 0.104, 0.106, 0.108]);
    assert_eq!(planar[1][0], 0.101);
    let mut out = vec![0.0; 10];
    interleave(&planar, 2, &mut out, 5);
    assert_eq!(out, input);
}

#[test]
fn interleave_maps_mono_and_surround() {
    let mono = ramp(4, 1);
    let mut planar = scratch();
    deinterleave(&mono, 1, &mut planar, 4);
    assert_eq!(planar[0][..4], planar[1][..4]);
    planar[0][..4].fill(0.5);
    planar[1][..4].fill(0.7);
    let mut out = mono.clone();
    interleave(&planar, 1, &mut out, 4);
    assert_eq!(out, [0.6; 4], "mono takes the mean of the two outputs");

    let surround = ramp(3, 6);
    deinterleave(&surround, 6, &mut planar, 3);
    let frame = &surround[6..12];
    assert_eq!(planar[0][1], (frame[0] + frame[2] + frame[4]) / 3.0);
    assert_eq!(planar[1][1], (frame[1] + frame[3] + frame[5]) / 3.0);
    planar[0][..3].fill(-1.0);
    planar[1][..3].fill(-2.0);
    let mut out = surround.clone();
    interleave(&planar, 6, &mut out, 3);
    for frame in out.chunks(6).zip(surround.chunks(6)) {
        assert_eq!(frame.0[..2], [-1.0, -2.0]);
        assert_eq!(frame.0[2..], frame.1[2..], "channels 2-5 are untouched");
    }
}

fn effect(state: Vec<u8>) -> Vst3Effect {
    fixture_effect(&state)
}

fn effect_of(class_id: &str, state: Vec<u8>) -> Vst3Effect {
    fixture_effect_of(class_id, &state)
}

fn fixture(state: Vec<u8>) -> Arc<Vst3Instance> {
    fixture_instance(&effect(state))
}

fn run(instance: &Vst3Instance, input: &[f32]) -> Vec<f32> {
    let mut out = input.to_vec();
    instance.process(&mut out, 2);
    out
}

fn assert_scaled(out: &[f32], input: &[f32], gain: f32) {
    for (o, i) in out.iter().zip(input) {
        assert!((o - i * gain).abs() < 1e-6, "{o} vs {i} * {gain}");
    }
}

#[test]
fn fixture_gain_applies() {
    let instance = fixture(0.25f32.to_le_bytes().to_vec());
    let input = ramp(512, 2);
    assert_scaled(&run(&instance, &input), &input, 0.5);
    // Longer than one plugin block: processed in slices.
    let input = ramp(1300, 2);
    assert_scaled(&run(&instance, &input), &input, 0.5);
}

#[test]
fn fixture_default_state_is_unity() {
    let instance = fixture(Vec::new());
    let input = ramp(100, 2);
    assert_scaled(&run(&instance, &input), &input, 1.0);
}

#[test]
fn fixture_state_round_trips() {
    let bytes = 0.25f32.to_le_bytes().to_vec();
    let instance = fixture(Vec::new());
    instance.set_state(&effect(bytes.clone()));
    let (component, _) = instance.get_state().expect("state");
    assert_eq!(component.bytes(), bytes);
    let input = ramp(64, 2);
    assert_scaled(&run(&instance, &input), &input, 0.5);
}

#[test]
fn fixture_param_change_reaches_processor() {
    let instance = fixture(Vec::new());
    let input = ramp(256, 2);
    assert_scaled(&run(&instance, &input), &input, 1.0);
    // SAFETY: the handler is a live COM object owned by the instance.
    unsafe {
        instance.handler.beginEdit(0);
        instance.handler.performEdit(0, 0.25);
        instance.handler.endEdit(0);
    }
    assert!(instance.take_dirty());
    assert!(!instance.take_dirty());
    assert_scaled(&run(&instance, &input), &input, 0.5);
    assert_scaled(&run(&instance, &input), &input, 0.5);
}

fn split_fixture() -> Arc<Vst3Instance> {
    fixture_instance(&effect_of("56565445535447414E00000000000002", Vec::new()))
}

#[test]
fn a_fresh_split_controller_mirrors_the_processor_defaults() {
    let instance = fixture_instance(&effect_of("56565445535447414E00000000000014", Vec::new()));
    assert!(instance.separate_controller);
    let controller = instance.controller().expect("controller");
    // SAFETY: a live controller.
    assert_eq!(unsafe { controller.getParamNormalized(0) }, 0.25);
    let (component, _) = instance.get_state().expect("state");
    assert_eq!(component.bytes(), 0.25f32.to_le_bytes());
}

#[test]
fn plugin_whose_set_processing_fails_still_loads_and_processes() {
    for class_id in [
        "56565445535447414E00000000000004", // kNotImplemented
        "56565445535447414E00000000000005", // kResultFalse
    ] {
        let effect = effect_of(class_id, Vec::new());
        let instance = Vst3Instance::new(&fixture_bundle(), &effect, 48_000, ProcessMode::Realtime)
            .expect("setProcessing's result is informational");
        instance.reset();
        let input = ramp(64, 2);
        assert_scaled(&run(&instance, &input), &input, 1.0);
    }
}

#[test]
fn get_state_flushes_pending_edits_to_a_split_processor() {
    let instance = split_fixture();
    assert!(instance.separate_controller);
    // SAFETY: the handler is a live COM object owned by the instance.
    unsafe {
        instance.handler.beginEdit(0);
        instance.handler.performEdit(0, 0.25);
        instance.handler.endEdit(0);
    }
    assert!(instance.take_dirty());
    let (component, _) = instance.get_state().expect("state");
    assert_eq!(component.bytes(), 0.25f32.to_le_bytes());
    let input = ramp(64, 2);
    assert_scaled(&run(&instance, &input), &input, 0.5);
}

#[test]
fn get_state_flush_does_not_replay_edits_on_the_next_process() {
    let instance = split_fixture();
    // SAFETY: the handler is a live COM object owned by the instance.
    unsafe { instance.handler.performEdit(0, 0.25) };
    instance.get_state();
    assert!(instance.shared.params.lock().unwrap().is_empty());
}

#[test]
fn busy_instance_passes_audio_through() {
    let instance = fixture(0.25f32.to_le_bytes().to_vec());
    instance
        .busy
        .store(true, std::sync::atomic::Ordering::Release);
    let input = ramp(64, 2);
    assert_eq!(run(&instance, &input), input);
    instance
        .busy
        .store(false, std::sync::atomic::Ordering::Release);
}

#[test]
fn process_reports_whether_the_block_was_processed() {
    let instance = fixture(Vec::new());
    let mut block = ramp(32, 6);
    assert!(instance.process(&mut block, 6));
    instance
        .busy
        .store(true, std::sync::atomic::Ordering::Release);
    assert!(!instance.process(&mut block, 6));
    instance
        .busy
        .store(false, std::sync::atomic::Ordering::Release);
    instance
        .active
        .store(false, std::sync::atomic::Ordering::Relaxed);
    let before = block.clone();
    assert!(!instance.process(&mut block, 6));
    assert_eq!(block, before, "a skipped block is left untouched");
    assert!(!instance.process(&mut block, 0));
    // Back to the real state, so Drop deactivates before terminate.
    instance
        .active
        .store(true, std::sync::atomic::Ordering::Relaxed);
}

#[test]
fn mono_and_surround_run_through_the_plugin() {
    let instance = fixture(0.25f32.to_le_bytes().to_vec());
    let mut mono = ramp(32, 1);
    let want: Vec<f32> = mono.iter().map(|s| s * 0.5).collect();
    instance.process(&mut mono, 1);
    assert_eq!(mono.len(), want.len());
    for (a, b) in mono.iter().zip(&want) {
        assert!((a - b).abs() < 1e-6);
    }
    let surround = ramp(32, 6);
    let mut out = surround.clone();
    instance.process(&mut out, 6);
    for (o, i) in out.chunks(6).zip(surround.chunks(6)) {
        let left = (i[0] + i[2] + i[4]) / 3.0;
        let right = (i[1] + i[3] + i[5]) / 3.0;
        assert!((o[0] - left * 0.5).abs() < 1e-6);
        assert!((o[1] - right * 0.5).abs() < 1e-6);
        assert_eq!(o[2..], i[2..], "PluginEffect re-derives these");
    }
}

#[test]
fn unknown_class_is_an_error() {
    let mut bad = effect(Vec::new());
    bad.class_id = "00000000000000000000000000000000".into();
    let r = Vst3Instance::new(&fixture_bundle(), &bad, 48_000, ProcessMode::Offline);
    assert!(r.is_err());
    bad.class_id = "zz".into();
    assert!(Vst3Instance::new(&fixture_bundle(), &bad, 48_000, ProcessMode::Offline).is_err());
}

#[cfg(target_os = "macos")]
mod real_plugins {
    use super::*;
    use crate::scan;
    use std::path::PathBuf;

    fn installed(name: &str) -> Option<(PathBuf, Vst3Effect)> {
        let dir = PathBuf::from("/Library/Audio/Plug-Ins/VST3");
        let info = scan(&[dir]).into_iter().find(|p| p.name.contains(name))?;
        let mut e = effect(Vec::new());
        e.class_id = info.class_id;
        e.name = info.name;
        Some((info.path, e))
    }

    fn noise(frames: usize) -> Vec<f32> {
        let mut x = 0x1234_5678u32;
        (0..frames * 2)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x as f32 / u32::MAX as f32 - 0.5) * 0.5
            })
            .collect()
    }

    #[test]
    #[ignore = "needs Wider installed"]
    fn wider_processes_noise_with_finite_output() {
        let (path, e) = installed("Wider").expect("Wider installed");
        let inst = Vst3Instance::new(&path, &e, 48_000, ProcessMode::Realtime).unwrap();
        let mut audio = noise(48_000);
        for block in audio.chunks_mut(512 * 2) {
            inst.process(block, 2);
        }
        assert!(audio.iter().all(|s| s.is_finite()));
    }

    // TDR writes a per-process instance counter into its state.
    fn mask_instance(mut state: Vec<u8>) -> Vec<u8> {
        let key = b"instance=\"";
        if let Some(at) = state.windows(key.len()).position(|w| w == key) {
            let end = (at + key.len() + 16).min(state.len());
            state[at + key.len()..end].fill(0);
        }
        state
    }

    #[test]
    #[ignore = "needs TDR Nova installed"]
    fn state_round_trips() {
        let (path, e) = installed("TDR Nova").expect("TDR Nova installed");
        let a = Vst3Instance::new(&path, &e, 48_000, ProcessMode::Realtime).unwrap();
        let (c1, k1) = a.get_state().expect("state");
        assert!(!c1.bytes().is_empty());
        let mut saved = e.clone();
        saved.component_state = c1.clone();
        saved.controller_state = k1.clone();
        let b = Vst3Instance::new(&path, &saved, 48_000, ProcessMode::Realtime).unwrap();
        let (c2, k2) = b.get_state().expect("state");
        assert_eq!(
            mask_instance(c1.bytes().to_vec()),
            mask_instance(c2.bytes().to_vec())
        );
        assert_eq!(k1, k2);
    }

    #[test]
    #[ignore = "needs TDR Nova installed"]
    fn nova_default_is_near_transparent() {
        let (path, e) = installed("TDR Nova").expect("TDR Nova installed");
        let inst = Vst3Instance::new(&path, &e, 48_000, ProcessMode::Realtime).unwrap();
        let latency = inst.latency() as usize;
        let input = noise(48_000);
        let mut out = input.clone();
        for block in out.chunks_mut(512 * 2) {
            inst.process(block, 2);
        }
        // Compare after the plugin's own delay, skipping the settle time.
        let skip = 8192 * 2;
        let (mut err, mut sig) = (0.0f64, 0.0f64);
        for i in skip..input.len() - latency * 2 {
            let d = f64::from(out[i + latency * 2] - input[i]);
            err += d * d;
            sig += f64::from(input[i]).powi(2);
        }
        let db = 10.0 * (err / sig).log10();
        assert!(db < -90.0, "residual {db} dB");
    }
}

#[test]
fn set_state_with_an_empty_effect_restores_the_defaults() {
    let instance = fixture(Vec::new());
    let (defaults, _) = instance.get_state().expect("state");
    instance.set_state(&effect(0.25f32.to_le_bytes().to_vec()));
    let input = ramp(64, 2);
    assert_scaled(&run(&instance, &input), &input, 0.5);
    instance.set_state(&effect(Vec::new()));
    assert_scaled(&run(&instance, &input), &input, 1.0);
    assert_eq!(
        instance.get_state().expect("state").0.bytes(),
        defaults.bytes()
    );
}

#[test]
fn set_state_with_an_empty_effect_restores_the_defaults_of_a_split_plugin() {
    let instance = split_fixture();
    let (defaults, _) = instance.get_state().expect("state");
    instance.set_state(&effect_of(
        "56565445535447414E00000000000002",
        0.25f32.to_le_bytes().to_vec(),
    ));
    instance.set_state(&effect_of("56565445535447414E00000000000002", Vec::new()));
    let input = ramp(64, 2);
    assert_scaled(&run(&instance, &input), &input, 1.0);
    assert_eq!(
        instance.get_state().expect("state").0.bytes(),
        defaults.bytes()
    );
}

const COUNTING_CLASS: &str = "56565445535447414E00000000000006";

fn counting_fixture() -> Arc<Vst3Instance> {
    fixture_instance(&effect_of(COUNTING_CLASS, Vec::new()))
}

fn edit_ids(instance: &Vst3Instance, ids: std::ops::Range<u32>) {
    for id in ids {
        // SAFETY: the handler is a live COM object owned by the instance.
        unsafe { instance.handler.performEdit(id, 0.5) };
    }
}

fn received(instance: &Vst3Instance) -> u32 {
    let (component, _) = instance.get_state().expect("state");
    u32::from_le_bytes(component.bytes()[4..8].try_into().unwrap())
}

#[test]
fn more_edits_than_one_block_holds_all_reach_the_processor_while_paused() {
    let instance = counting_fixture();
    edit_ids(&instance, 1..101);
    assert_eq!(received(&instance), 100);
    assert!(instance.shared.params.lock().unwrap().is_empty());
}

#[test]
fn more_edits_than_one_block_holds_are_delivered_over_consecutive_blocks() {
    let instance = counting_fixture();
    edit_ids(&instance, 1..101);
    let mut audio = ramp(64, 2);
    instance.process(&mut audio, 2);
    assert_eq!(instance.shared.params.lock().unwrap().len(), 36);
    instance.process(&mut audio, 2);
    assert!(instance.shared.params.lock().unwrap().is_empty());
    assert_eq!(received(&instance), 100);
}

const METER_CLASS: &str = "56565445535447414E00000000000007";

fn meter_fixture() -> Arc<Vst3Instance> {
    fixture_instance(&effect_of(METER_CLASS, Vec::new()))
}

fn meter_input(peak: f32) -> Vec<f32> {
    let mut audio = vec![0.0; 128];
    audio[10] = peak;
    audio
}

fn controller_value(instance: &Vst3Instance, id: u32) -> f64 {
    // SAFETY: a live controller owned by the instance.
    unsafe { instance.controller().unwrap().getParamNormalized(id) }
}

const MANY_METERS_CLASS: &str = "56565445535447414E0000000000000E";

#[test]
fn every_declared_output_parameter_is_published() {
    let instance = fixture_instance(&effect_of(MANY_METERS_CLASS, Vec::new()));
    let mut audio = meter_input(0.5);
    instance.process(&mut audio, 1);
    let outputs = instance.shared.outputs.lock().unwrap().clone();
    let mut ids: Vec<u32> = outputs.iter().map(|o| o.0).collect();
    ids.sort_unstable();
    assert_eq!(ids, (1..=100).collect::<Vec<u32>>());
    instance.forward_outputs();
    assert_eq!(controller_value(&instance, 1), 0.5);
    assert!(!instance.outputs_fresh());
}

#[test]
fn process_publishes_the_latest_output_parameter_value() {
    let instance = meter_fixture();
    let mut audio = meter_input(0.5);
    instance.process(&mut audio, 1);
    audio = meter_input(0.25);
    instance.process(&mut audio, 1);
    let outputs = instance.shared.outputs.lock().unwrap().clone();
    assert_eq!(outputs, vec![(1, 0.25, true)]);
}

#[test]
fn output_parameters_reach_the_controller() {
    let instance = meter_fixture();
    assert_eq!(controller_value(&instance, 1), 0.0);
    let mut audio = meter_input(0.5);
    instance.process(&mut audio, 1);
    assert_eq!(controller_value(&instance, 1), 0.0);
    instance.forward_outputs();
    assert_eq!(controller_value(&instance, 1), 0.5);
    assert!(!instance.outputs_fresh());
}

#[test]
fn an_unchanged_output_value_is_not_fresh_again() {
    let instance = meter_fixture();
    let mut audio = meter_input(0.5);
    instance.process(&mut audio, 1);
    assert!(instance.outputs_fresh());
    instance.forward_outputs();
    assert!(!instance.outputs_fresh());
    audio = meter_input(0.5);
    instance.process(&mut audio, 1);
    assert!(!instance.outputs_fresh());
    audio = meter_input(0.75);
    instance.process(&mut audio, 1);
    assert!(instance.outputs_fresh());
    instance.forward_outputs();
    assert_eq!(controller_value(&instance, 1), 0.75);
    assert!(!instance.outputs_fresh());
}

#[test]
fn process_with_output_parameters_does_not_allocate() {
    for instance in [
        meter_fixture(),
        fixture_instance(&effect_of(MANY_METERS_CLASS, Vec::new())),
    ] {
        no_allocation_with_outputs(&instance);
    }
}

fn no_allocation_with_outputs(instance: &Vst3Instance) {
    let mut audio = ramp(1300, 2);
    instance.process(&mut audio, 2);
    instance.forward_outputs();
    let before = ALLOCS.with(|c| c.get());
    for _ in 0..8 {
        instance.process(&mut audio, 2);
    }
    assert_eq!(ALLOCS.with(|c| c.get()) - before, 0);
}

const PROCESS_RESTART_CLASS: &str = "56565445535447414E00000000000011";
const LATENCY_CLASS: &str = "56565445535447414E00000000000008";

fn latency_fixture(state: Vec<u8>) -> Arc<Vst3Instance> {
    fixture_instance(&effect_of(LATENCY_CLASS, state))
}

fn request_restart(instance: &Vst3Instance, flags: i32) {
    // SAFETY: the handler is a live COM object owned by the instance.
    unsafe { instance.handler.restartComponent(flags) };
}

#[test]
fn a_latency_restart_reactivates_the_plugin_and_rereads_its_latency() {
    let instance = latency_fixture(Vec::new());
    assert_eq!(instance.latency(), 0);
    request_restart(&instance, RestartFlags_::kLatencyChanged);
    instance.service_restart();
    assert_eq!(instance.latency(), 64);
}

#[test]
fn servicing_without_a_request_does_not_reactivate() {
    let instance = latency_fixture(Vec::new());
    instance.service_restart();
    assert_eq!(instance.latency(), 0);
    request_restart(&instance, RestartFlags_::kLatencyChanged);
    instance.service_restart();
    instance.service_restart();
    assert_eq!(instance.latency(), 64);
}

#[test]
fn a_reload_restart_keeps_the_state_and_keeps_processing() {
    let instance = latency_fixture(0.25f32.to_le_bytes().to_vec());
    request_restart(
        &instance,
        RestartFlags_::kReloadComponent | RestartFlags_::kIoChanged,
    );
    instance.service_restart();
    let (component, _) = instance.get_state().expect("state");
    assert_eq!(component.bytes(), 0.25f32.to_le_bytes());
    assert_eq!(instance.latency(), 64);
    let input = ramp(256, 2);
    assert_scaled(&run(&instance, &input), &input, 0.5);
}

#[test]
fn gestures_are_stamped_in_the_order_they_end_across_instances() {
    let (a, b) = (split_fixture(), split_fixture());
    assert_eq!(a.gesture_seq(), 0);
    // SAFETY: the handlers are live COM objects owned by the instances.
    unsafe {
        b.handler.endEdit(0);
        a.handler.endEdit(0);
    }
    assert!(0 < b.gesture_seq() && b.gesture_seq() < a.gesture_seq());
}

#[test]
fn a_reload_restart_keeps_an_edit_queued_before_it() {
    let instance = split_fixture();
    // SAFETY: the handler is a live COM object owned by the instance.
    unsafe {
        instance.handler.beginEdit(0);
        instance.handler.performEdit(0, 0.25);
        instance.handler.endEdit(0);
    }
    request_restart(&instance, RestartFlags_::kReloadComponent);
    instance.service_restart();
    let (component, _) = instance.take_gesture().expect("the gesture is kept");
    assert_eq!(component.bytes(), 0.25f32.to_le_bytes());
    let input = ramp(64, 2);
    assert_scaled(&run(&instance, &input), &input, 0.5);
}

#[test]
fn a_reload_restart_keeps_more_queued_edits_than_one_block_holds() {
    let instance = counting_fixture();
    edit_ids(&instance, 1..101);
    request_restart(&instance, RestartFlags_::kReloadComponent);
    instance.service_restart();
    assert!(instance.shared.params.lock().unwrap().is_empty());
    assert_eq!(received(&instance), 100);
}

#[test]
fn a_restart_of_an_active_plugin_keeps_its_queued_edits() {
    let instance = counting_fixture();
    edit_ids(&instance, 1..11);
    request_restart(&instance, RestartFlags_::kLatencyChanged);
    instance.service_restart();
    run(&instance, &ramp(64, 2));
    assert_eq!(received(&instance), 10);
}

// Mirrors the fixture's log path: it is loaded into this process.
fn lifecycle_log_path(tag: u8) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "venturi-fixture-lifecycle-{}-{tag}.log",
        std::process::id()
    ))
}

/// Clears the log on creation and removes it on drop, also when a test panics.
struct FixtureLog(std::path::PathBuf);

impl FixtureLog {
    fn new(tag: u8) -> Self {
        Self::at(lifecycle_log_path(tag))
    }

    fn at(path: std::path::PathBuf) -> Self {
        let _ = std::fs::remove_file(&path);
        Self(path)
    }

    fn calls(&self) -> Vec<String> {
        read_log(&self.0)
    }
}

impl Drop for FixtureLog {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn fixture_log_is_removed_on_drop() {
    let path = std::env::temp_dir().join(format!("venturi-log-guard-{}", std::process::id()));
    let log = FixtureLog::at(path.clone());
    std::fs::write(&path, "call\n").unwrap();
    assert_eq!(log.calls(), ["call"]);
    drop(log);
    assert!(!path.exists());
}

fn lifecycle_build(class: &str) -> Result<Arc<Vst3Instance>, String> {
    Vst3Instance::new(
        &fixture_bundle(),
        &effect_of(class, Vec::new()),
        48_000,
        ProcessMode::Realtime,
    )
}

fn read_log(path: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_plugin_that_rejects_the_stereo_bus_is_never_deactivated() {
    let log = FixtureLog::new(9);
    let result = lifecycle_build("56565445535447414E00000000000009");
    assert!(result.is_err());
    assert_eq!(log.calls(), ["terminate"]);
}

#[test]
fn a_plugin_that_rejects_setup_is_never_deactivated() {
    let log = FixtureLog::new(10);
    let result = lifecycle_build("56565445535447414E0000000000000A");
    assert!(result.is_err());
    assert_eq!(log.calls(), ["terminate"]);
}

#[test]
fn a_built_instance_is_torn_down_in_reverse_order() {
    let log = FixtureLog::new(11);
    let instance = lifecycle_build("56565445535447414E0000000000000B").unwrap();
    assert_eq!(log.calls(), ["setActive(1)", "setProcessing(1)"]);
    instance.reactivate();
    let calls = log.calls();
    assert_eq!(
        calls[2..],
        [
            "setProcessing(0)",
            "setActive(0)",
            "setActive(1)",
            "setProcessing(1)"
        ]
    );
    drop(instance);
    let calls = log.calls();
    assert_eq!(
        calls[6..],
        ["setProcessing(0)", "setActive(0)", "terminate"]
    );
}

#[test]
fn dropping_instances_leaves_the_module_usable() {
    for _ in 0..3 {
        let instance = fixture(Vec::new());
        assert_scaled(&run(&instance, &ramp(64, 2)), &ramp(64, 2), 1.0);
    }
    let again = fixture(Vec::new());
    assert_scaled(&run(&again, &ramp(64, 2)), &ramp(64, 2), 1.0);
}

#[test]
fn the_audio_guard_releases_busy_on_drop() {
    let busy = AtomicBool::new(false);
    let guard = AudioGuard::try_acquire(&busy).expect("free flag");
    assert!(busy.load(Ordering::Relaxed));
    assert!(AudioGuard::try_acquire(&busy).is_none());
    drop(guard);
    assert!(!busy.load(Ordering::Relaxed));
}

#[test]
fn the_audio_guard_releases_busy_on_early_return() {
    fn guarded(busy: &AtomicBool) -> Option<u32> {
        let _guard = AudioGuard::try_acquire(busy)?;
        None?;
        Some(1)
    }
    let busy = AtomicBool::new(false);
    assert_eq!(guarded(&busy), None);
    assert!(!busy.load(Ordering::Relaxed));
}

#[test]
fn the_audio_guard_releases_busy_on_panic() {
    let busy = AtomicBool::new(false);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = AudioGuard::try_acquire(&busy).unwrap();
        panic!("plugin blew up");
    }));
    assert!(result.is_err());
    assert!(!busy.load(Ordering::Relaxed));
    assert!(AudioGuard::try_acquire(&busy).is_some());
}

#[test]
fn process_mode_maps_to_the_sdk_constants() {
    use vst3::Steinberg::Vst::ProcessModes_;
    assert_eq!(ProcessMode::Realtime.sdk(), ProcessModes_::kRealtime as i32);
    assert_eq!(ProcessMode::Offline.sdk(), ProcessModes_::kOffline as i32);
}

#[test]
fn new_and_set_state_record_the_hash_of_the_loaded_state() {
    let first = effect(0.25f32.to_le_bytes().to_vec());
    let instance =
        Vst3Instance::new(&fixture_bundle(), &first, 48_000, ProcessMode::Realtime).unwrap();
    assert_eq!(instance.state_hash(), first.content_hash());

    let second = effect(0.75f32.to_le_bytes().to_vec());
    instance.set_state(&second);
    assert_eq!(instance.state_hash(), second.content_hash());
}

#[test]
fn a_restart_raised_while_activating_is_not_serviced_again() {
    let log = FixtureLog::new(12);
    let instance = lifecycle_build("56565445535447414E0000000000000C").unwrap();
    instance.service_restart();
    assert_eq!(log.calls(), ["setActive(1)", "setProcessing(1)"]);
}

#[test]
fn a_plugin_raising_a_restart_on_every_activation_is_not_reactivated_in_a_loop() {
    let log = FixtureLog::new(12);
    let instance = lifecycle_build("56565445535447414E0000000000000C").unwrap();
    instance.reactivate();
    instance.service_restart();
    let before = log.calls().len();
    instance.service_restart();
    assert_eq!(log.calls().len(), before);
}

const STATE_FAIL_CLASS: &str = "56565445535447414E0000000000000D";

/// While alive, the `STATE_FAIL_CLASS` fixture's getState fails; the file is
/// the one the fixture checks (same process, same temp dir); keep the path
/// in step with `state_fail_marker()` in the fixture.
struct StateFailure(std::path::PathBuf);

impl StateFailure {
    fn start() -> Self {
        let mut path = lifecycle_log_path(13).into_os_string();
        path.push(".fail");
        let path = std::path::PathBuf::from(path);
        std::fs::write(&path, b"").expect("marker");
        Self(path)
    }
}

impl Drop for StateFailure {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn state_fail_fixture(state: Vec<u8>) -> Arc<Vst3Instance> {
    fixture_instance(&effect_of(STATE_FAIL_CLASS, state))
}

#[test]
fn a_reload_restart_with_a_failing_get_state_keeps_the_live_state() {
    let instance = state_fail_fixture(0.25f32.to_le_bytes().to_vec());
    let failure = StateFailure::start();
    request_restart(&instance, RestartFlags_::kReloadComponent);
    instance.service_restart();
    drop(failure);
    let (component, _) = instance.get_state().expect("state");
    assert_eq!(component.bytes(), 0.25f32.to_le_bytes());
}

#[test]
fn get_state_reports_a_failing_plugin_and_recovers() {
    let instance = state_fail_fixture(0.25f32.to_le_bytes().to_vec());
    {
        let _failure = StateFailure::start();
        assert!(instance.get_state().is_none());
    }
    assert!(instance.get_state().is_some());
}

fn edited_state_fail_fixture() -> Arc<Vst3Instance> {
    let instance = state_fail_fixture(Vec::new());
    // SAFETY: the handler is a live COM object owned by the instance.
    unsafe {
        instance.handler.beginEdit(0);
        instance.handler.performEdit(0, 0.25);
        instance.handler.endEdit(0);
    }
    instance
}

#[test]
fn a_gesture_is_retried_once_get_state_works_again() {
    let instance = edited_state_fail_fixture();
    let failure = StateFailure::start();
    assert!(instance.take_gesture().is_none());
    drop(failure);
    let (component, _) = instance.take_gesture().expect("retried");
    assert_eq!(component.bytes(), 0.25f32.to_le_bytes());
}

#[test]
fn a_gesture_is_given_up_after_repeated_get_state_failures() {
    let instance = edited_state_fail_fixture();
    let failure = StateFailure::start();
    for _ in 0..3 {
        assert!(instance.take_gesture().is_none());
    }
    drop(failure);
    assert!(instance.take_gesture().is_none(), "no more retries");
}

#[test]
fn a_valid_empty_controller_state_is_a_state() {
    let instance = fixture(Vec::new());
    let (_, controller) = instance.get_state().expect("state");
    assert!(controller.bytes().is_empty());
}

const AUX_CLASS: &str = "56565445535447414E0000000000000F";

fn aux_fixture() -> Arc<Vst3Instance> {
    fixture_instance(&effect_of(AUX_CLASS, Vec::new()))
}

#[test]
fn every_declared_audio_bus_gets_a_buffer() {
    let out = run(&aux_fixture(), &ramp(64, 2));
    // numInputs 2 + 10 * numOutputs 2 + 100 for usable extra-bus channel pointers.
    assert_eq!(out[0], 122.0);
}

#[test]
fn processing_with_extra_buses_does_not_allocate() {
    let instance = aux_fixture();
    let mut audio = ramp(1300, 2);
    instance.process(&mut audio, 2);
    let before = ALLOCS.with(|c| c.get());
    for _ in 0..8 {
        instance.process(&mut audio, 2);
    }
    assert_eq!(ALLOCS.with(|c| c.get()) - before, 0);
}

#[test]
fn a_plugin_that_only_accepts_its_main_buses_still_loads() {
    let instance = fixture_instance(&effect_of("56565445535447414E00000000000010", Vec::new()));
    assert_eq!(run(&instance, &ramp(64, 2))[0], 122.0);
}

// The only test that installs the process-wide waker (a OnceLock); it counts
// per thread so parallel tests cannot disturb it.
#[test]
fn a_restart_wakes_the_process_waker_except_from_the_audio_thread() {
    use std::cell::Cell;
    thread_local!(static WOKEN: Cell<usize> = const { Cell::new(0) });
    crate::set_restart_waker(Arc::new(|| WOKEN.set(WOKEN.get() + 1)));
    let instance = latency_fixture(Vec::new());
    request_restart(&instance, RestartFlags_::kLatencyChanged);
    assert_eq!(WOKEN.get(), 1);

    let instance = fixture_instance(&effect_of(PROCESS_RESTART_CLASS, Vec::new()));
    let mut samples = [0.0f32; 128];
    instance.process(&mut samples, 2);
    assert_eq!(WOKEN.get(), 1, "the audio thread must not call the waker");
    assert_eq!(instance.latency(), 0);
    instance.service_restart();
    assert_eq!(instance.latency(), 64);
}

const REACTIVATE_FAIL_CLASS: &str = "56565445535447414E00000000000012";

fn reactivate_fail_fixture() -> Arc<Vst3Instance> {
    fixture_instance(&effect_of(
        REACTIVATE_FAIL_CLASS,
        0.25f32.to_le_bytes().to_vec(),
    ))
}

fn is_dry(instance: &Vst3Instance) -> bool {
    let input = ramp(256, 2);
    run(instance, &input) == input
}

#[test]
fn a_failed_reactivation_does_not_restart_processing_and_plays_dry() {
    let log = FixtureLog::new(18);
    let instance = reactivate_fail_fixture();
    assert!(!is_dry(&instance));
    instance.reactivate();
    assert_eq!(
        log.calls()
            .into_iter()
            .filter(|c| c != "process")
            .collect::<Vec<_>>(),
        [
            "setActive(1)",
            "setProcessing(1)",
            "setProcessing(0)",
            "setActive(0)",
            "setActive(1)"
        ]
    );
    assert!(is_dry(&instance));
}

#[test]
fn a_failed_restart_reload_plays_dry() {
    let instance = reactivate_fail_fixture();
    request_restart(&instance, RestartFlags_::kReloadComponent);
    instance.service_restart();
    assert!(is_dry(&instance));
}

#[test]
fn a_failed_reactivation_is_not_deactivated_again_on_drop() {
    let log = FixtureLog::new(18);
    let instance = reactivate_fail_fixture();
    instance.reactivate();
    let before = log.calls().len();
    drop(instance);
    assert_eq!(log.calls()[before..], ["terminate"]);
}

#[test]
fn an_inactive_instance_retries_activation_without_a_second_deactivate() {
    let log = FixtureLog::new(18);
    let instance = reactivate_fail_fixture();
    instance.reactivate();
    instance.reactivate();
    assert_eq!(log.calls()[5..], ["setActive(1)"]);
}

#[test]
fn an_inactive_instance_is_not_processed_to_read_its_state_or_flush_edits() {
    let log = FixtureLog::new(18);
    let instance = reactivate_fail_fixture();
    instance.reactivate();
    let before = log.calls().len();
    // SAFETY: the handler is a live COM object owned by the instance.
    unsafe {
        instance.handler.beginEdit(0);
        instance.handler.performEdit(0, 0.25);
        instance.handler.endEdit(0);
    }
    assert!(instance.get_state().is_some());
    request_restart(&instance, RestartFlags_::kReloadComponent);
    instance.service_restart();
    assert!(!log.calls()[before..].contains(&"process".to_owned()));
    assert!(!instance.is_active());
}

const PROCESS_EDIT_CLASS: &str = "56565445535447414E00000000000015";

#[test]
fn a_plugin_edit_from_process_never_blocks_on_the_ui_and_is_applied_later() {
    let instance = fixture_instance(&effect_of(PROCESS_EDIT_CLASS, Vec::new()));
    let input = ramp(64, 2);
    let held = instance.shared.params.lock().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = {
        let (instance, input) = (instance.clone(), input.clone());
        std::thread::spawn(move || tx.send(run(&instance, &input)).unwrap())
    };
    let first = rx.recv_timeout(std::time::Duration::from_secs(2));
    drop(held);
    worker.join().unwrap();
    let first = first.expect("process blocked on the params lock");
    assert_scaled(&first, &input, 1.0);
    // The edit made while the UI held the lock must reach the plugin on the next block.
    assert_scaled(&run(&instance, &input), &input, 0.5);
}
