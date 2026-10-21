use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use vv_audio::{AudioSource, ClipAudio, MixSnapshot, PluginHandle, PluginProcessor, PluginRequest};
use vv_core::Vst3Effect;

use super::*;
use crate::scan;
use crate::testing::{
    FIXTURE_CLASS_ID, fixture_dir, fixture_effect, fixture_effect_of, fixture_info,
    fixture_instance,
};

const CLASS: &str = FIXTURE_CLASS_ID;

#[derive(Default)]
struct Counts {
    made: AtomicUsize,
    loads: AtomicUsize,
    dropped: AtomicUsize,
    reactivated: AtomicUsize,
    processed: AtomicUsize,
    inactive: std::sync::atomic::AtomicBool,
}

struct Fake {
    bundle: Option<PathBuf>,
    hash: AtomicU64,
    counts: Arc<Counts>,
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.counts.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

impl PluginProcessor for Fake {
    fn process(&self, samples: &mut [f32], _: usize) -> bool {
        self.counts.processed.fetch_add(1, Ordering::SeqCst);
        samples.iter_mut().for_each(|s| *s *= 0.5);
        true
    }
    fn reset(&self) {}
    fn latency(&self) -> u32 {
        0
    }
}

impl HostedPlugin for Fake {
    fn bundle(&self) -> Option<&std::path::Path> {
        self.bundle.as_deref()
    }
    fn is_active(&self) -> bool {
        !self.counts.inactive.load(Ordering::SeqCst)
    }
    fn reactivate(&self) {
        self.counts.reactivated.fetch_add(1, Ordering::SeqCst);
    }
    fn set_state(&self, effect: &Vst3Effect) {
        self.counts.loads.fetch_add(1, Ordering::SeqCst);
        self.hash.store(effect.content_hash(), Ordering::SeqCst);
    }
    fn state_hash(&self) -> u64 {
        self.hash.load(Ordering::SeqCst)
    }
    fn mark_state(&self, hash: u64) {
        self.hash.store(hash, Ordering::SeqCst);
    }
}

fn info(class_id: &str) -> PluginInfo {
    PluginInfo {
        class_id: class_id.into(),
        ..fixture_info()
    }
}

fn pool(counts: &Arc<Counts>) -> Instances {
    let counts = counts.clone();
    Instances::with_maker(
        vec![info(CLASS)].into(),
        Box::new(move |path, effect, _| {
            counts.made.fetch_add(1, Ordering::SeqCst);
            Some(Arc::new(Fake {
                bundle: Some(path.to_path_buf()),
                hash: AtomicU64::new(effect.content_hash()),
                counts: counts.clone(),
            }))
        }),
    )
}

fn effect(class_id: &str, state: &[u8]) -> Vst3Effect {
    fixture_effect_of(class_id, state)
}

fn request(key: u64, effect: &Vst3Effect) -> PluginRequest<'_> {
    PluginRequest {
        key,
        effect,
        sample_rate: 48_000,
        process: true,
    }
}

#[test]
fn same_key_reuses_instance() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    let e = effect(CLASS, &[1]);
    pool.begin_pass();
    let first = pool.get(request(7, &e)).unwrap();
    pool.begin_pass();
    let second = pool.get(request(7, &e)).unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(counts.made.load(Ordering::SeqCst), 1);
    assert_eq!(counts.loads.load(Ordering::SeqCst), 0);
}

#[test]
fn changed_state_reloads_without_new_instance() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    pool.begin_pass();
    let first = pool.get(request(7, &effect(CLASS, &[1]))).unwrap();
    pool.begin_pass();
    let second = pool.get(request(7, &effect(CLASS, &[2]))).unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(counts.made.load(Ordering::SeqCst), 1);
    assert_eq!(counts.loads.load(Ordering::SeqCst), 1);
    pool.begin_pass();
    pool.get(request(7, &effect(CLASS, &[2]))).unwrap();
    assert_eq!(
        counts.loads.load(Ordering::SeqCst),
        1,
        "same state is not reloaded"
    );
}

#[test]
fn unknown_class_is_none() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    pool.begin_pass();
    assert!(
        pool.get(request(7, &effect("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB", &[])))
            .is_none()
    );
    assert_eq!(counts.made.load(Ordering::SeqCst), 0);
}

#[test]
fn sweep_drops_unused() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    let e = effect(CLASS, &[]);
    pool.begin_pass();
    pool.get(request(1, &e)).unwrap();
    pool.get(request(2, &e)).unwrap();
    pool.begin_pass();
    pool.get(request(1, &e)).unwrap();
    pool.sweep();
    assert!(pool.instance(1).is_some());
    assert!(pool.instance(2).is_none());
    assert_eq!(counts.dropped.load(Ordering::SeqCst), 1);
}

#[test]
fn sweep_idle_keeps_the_most_recent_unused_and_reactivates_them() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    let e = effect(CLASS, &[]);
    for key in 1..=3 {
        pool.begin_pass();
        pool.get(request(key, &e)).unwrap();
        pool.sweep_idle(1);
    }
    pool.begin_pass();
    pool.get(request(4, &e)).unwrap();
    pool.sweep_idle(1);
    assert!(pool.instance(4).is_some());
    assert!(pool.instance(3).is_some(), "the latest idle one is kept");
    assert!(pool.instance(1).is_none() && pool.instance(2).is_none());
    assert_eq!(counts.dropped.load(Ordering::SeqCst), 2);
    let reactivated = counts.reactivated.load(Ordering::SeqCst);
    pool.begin_pass();
    pool.get(request(3, &e)).unwrap();
    assert_eq!(counts.reactivated.load(Ordering::SeqCst), reactivated + 1);
    assert_eq!(counts.made.load(Ordering::SeqCst), 4);
}

#[test]
fn same_key_twice_in_one_pass_gets_two_instances() {
    // Keys are unique per placement, so two keys for one effect are two instances.
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    let e = effect(CLASS, &[]);
    pool.begin_pass();
    let a = pool.get(request(1, &e)).unwrap();
    let b = pool.get(request(2, &e)).unwrap();
    assert!(!Arc::ptr_eq(&a, &b));
    assert_eq!(counts.made.load(Ordering::SeqCst), 2);
}

#[test]
fn second_get_of_a_key_in_one_pass_is_the_same_instance() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    let e = effect(CLASS, &[1]);
    pool.begin_pass();
    let first = pool.get(request(1, &e)).unwrap();
    let again = pool.get(request(1, &e)).unwrap();
    assert!(Arc::ptr_eq(&first, &again));
    assert_eq!(counts.made.load(Ordering::SeqCst), 1);
    assert_eq!(counts.loads.load(Ordering::SeqCst), 0);
    assert_eq!(counts.reactivated.load(Ordering::SeqCst), 0);
    pool.sweep();
    assert!(pool.instance(1).is_some());
}

/// A source over the pool that keeps the default `level`, which measures
/// through `self` before the live chain asks for the same keys.
struct PoolSource(Instances);

impl AudioSource for PoolSource {
    fn file(&mut self, _: &std::path::Path, _: usize) -> ClipAudio {
        ClipAudio::Ready(vv_audio::SampleBlocks::contiguous(
            Arc::new(vec![0.5; 10_000]),
            0,
            1,
        ))
    }

    fn plugin(&mut self, request: PluginRequest<'_>) -> Option<PluginHandle> {
        self.0.get(request)
    }
}

#[test]
fn default_level_does_not_leave_the_live_chain_dry() {
    use vv_core::*;
    let mut project = Project::default();
    let media = project.media_pool.insert(MediaItem {
        path: PathBuf::from("a.wav"),
        meta: MediaMeta {
            duration_frames: 1000,
            fps: Rational::new(10, 1),
            width: 0,
            height: 0,
            has_video: false,
            has_audio: true,
            sample_rate: 100,
            channels: 1,
            audio_streams: 1,
            file: Default::default(),
        },
        content_hash: 7,
        compound: None,
        folder: None,
    });
    let clip = Clip::from_source_range(
        ClipId(1),
        ClipSource::Media(media),
        0,
        5,
        0,
        Rational::one(),
    );
    let mut track = Track {
        clips: vec![clip],
        ..Track::new(TrackKind::Audio)
    };
    let plugin = effect(CLASS, &[]);
    track.mix.effects = vec![
        AudioEffect::new(AudioEffectKind::Vst3(plugin)),
        AudioEffect::new(AudioEffectKind::Normalize {
            target_db: 0.0,
            mode: NormalizeMode::SamplePeak,
            set_level: SetLevel::Relative,
        }),
    ];
    let timeline = Timeline {
        name: "t".into(),
        fps: Rational::new(10, 1),
        resolution: (4, 2),
        tracks: vec![track],
        markers: Vec::new(),
        master: Default::default(),
    };
    let counts = Arc::new(Counts::default());
    let mut source = PoolSource(pool(&counts));
    source.0.begin_pass();
    let snapshot = MixSnapshot::from_timeline(&project, &timeline, 100, 1, &mut source);
    counts.processed.store(0, Ordering::SeqCst);
    let mut out = vec![0.0; 40];
    vv_audio::mixer::mix_range(&snapshot, 0, &mut out);
    assert!(
        counts.processed.load(Ordering::SeqCst) > 0,
        "the live chain played dry"
    );
    // The gain is measured after the plugin: 0.5 * 0.5 normalized to 1.0.
    assert!(out.iter().all(|s| (s - 1.0).abs() < 1e-4), "{out:?}");
}

#[test]
fn fixture_instance_reused_across_passes() {
    let infos = scan(&[fixture_dir()]);
    let class_id = infos.first().expect("fixture is scanned").class_id.clone();
    let e = effect(&class_id, &0.25f32.to_le_bytes());
    let mut pool = Instances::realtime(infos.into());
    let mut handles = Vec::new();
    for _ in 0..2 {
        pool.begin_pass();
        handles.push(pool.get(request(1, &e)).expect("fixture instance"));
        pool.sweep();
    }
    assert!(Arc::ptr_eq(&handles[0], &handles[1]));
}

fn bypassed(key: u64, effect: &Vst3Effect) -> PluginRequest<'_> {
    PluginRequest {
        process: false,
        ..request(key, effect)
    }
}

#[test]
fn a_bypassed_request_keeps_the_instance_alive_without_processing_it() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    let e = effect(CLASS, &[1]);
    pool.begin_pass();
    let first = pool.get(request(1, &e)).unwrap();
    pool.sweep();
    for _ in 0..3 {
        pool.begin_pass();
        assert!(pool.get(bypassed(1, &e)).is_none());
        pool.sweep();
    }
    pool.begin_pass();
    let back = pool.get(request(1, &e)).unwrap();
    assert!(Arc::ptr_eq(&first, &back));
    assert_eq!(counts.made.load(Ordering::SeqCst), 1);
    assert_eq!(counts.dropped.load(Ordering::SeqCst), 0);
}

#[test]
fn a_re_enabled_instance_is_reactivated_once_to_clear_its_tails() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    let e = effect(CLASS, &[1]);
    pool.begin_pass();
    pool.get(request(1, &e)).unwrap();
    pool.sweep();
    pool.begin_pass();
    pool.get(bypassed(1, &e));
    pool.sweep();
    assert_eq!(counts.reactivated.load(Ordering::SeqCst), 0);
    for _ in 0..3 {
        pool.begin_pass();
        pool.get(request(1, &e)).unwrap();
        pool.sweep();
    }
    assert_eq!(counts.reactivated.load(Ordering::SeqCst), 1);
}

#[test]
fn a_bypassed_request_keeps_a_failed_slot_without_retrying() {
    let counts = Arc::new(Counts::default());
    let mut pool = failing_pool(&counts);
    let e = effect(CLASS, &[1]);
    pool.begin_pass();
    assert!(pool.get(request(1, &e)).is_none());
    pool.sweep();
    for _ in 0..3 {
        pool.begin_pass();
        assert!(pool.get(bypassed(1, &e)).is_none());
        pool.sweep();
    }
    assert!(pool.failed(1));
    assert_eq!(counts.made.load(Ordering::SeqCst), 1);
}

#[test]
fn a_bypassed_request_never_makes_an_instance() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    let e = effect(CLASS, &[1]);
    pool.begin_pass();
    assert!(pool.get(bypassed(1, &e)).is_none());
    pool.sweep();
    assert_eq!(counts.made.load(Ordering::SeqCst), 0);
    assert!(pool.instance(1).is_none());
}

#[test]
fn a_state_changed_while_bypassed_is_loaded_when_it_plays_again() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    pool.begin_pass();
    pool.get(request(1, &effect(CLASS, &[1]))).unwrap();
    pool.sweep();
    let changed = effect(CLASS, &[2]);
    pool.begin_pass();
    pool.get(bypassed(1, &changed));
    pool.sweep();
    assert_eq!(counts.loads.load(Ordering::SeqCst), 0);
    pool.begin_pass();
    pool.get(request(1, &changed)).unwrap();
    assert_eq!(counts.loads.load(Ordering::SeqCst), 1);
    assert_eq!(counts.made.load(Ordering::SeqCst), 1);
}

#[test]
fn a_bypassed_fixture_plugin_is_the_same_instance_when_enabled_again() {
    let infos = scan(&[fixture_dir()]);
    let class_id = infos.first().expect("fixture is scanned").class_id.clone();
    let e = effect(&class_id, &0.25f32.to_le_bytes());
    let mut pool = Instances::realtime(infos.into());
    pool.begin_pass();
    let first = pool.get(request(1, &e)).expect("fixture instance");
    pool.sweep();
    pool.begin_pass();
    assert!(pool.get(bypassed(1, &e)).is_none());
    pool.sweep();
    assert!(pool.instance(1).is_some());
    pool.begin_pass();
    let back = pool.get(request(1, &e)).expect("fixture instance");
    assert!(Arc::ptr_eq(&first, &back));
}

fn failing_pool(counts: &Arc<Counts>) -> Instances {
    let counts = counts.clone();
    Instances::with_maker(
        vec![info(CLASS)].into(),
        Box::new(move |_, _, _| {
            counts.made.fetch_add(1, Ordering::SeqCst);
            None
        }),
    )
}

#[test]
fn failed_instance_is_not_recreated_every_pass() {
    let counts = Arc::new(Counts::default());
    let mut pool = failing_pool(&counts);
    let e = effect(CLASS, &[1]);
    for _ in 0..3 {
        pool.begin_pass();
        assert!(pool.get(request(7, &e)).is_none());
        pool.sweep();
    }
    assert_eq!(counts.made.load(Ordering::SeqCst), 1);
    pool.begin_pass();
    assert!(pool.get(request(7, &effect(CLASS, &[2]))).is_none());
    assert_eq!(
        counts.made.load(Ordering::SeqCst),
        2,
        "a new state is tried again"
    );
}

#[test]
fn failure_is_forgotten_once_its_key_is_not_asked() {
    let counts = Arc::new(Counts::default());
    let mut pool = failing_pool(&counts);
    let e = effect(CLASS, &[1]);
    pool.begin_pass();
    assert!(pool.get(request(7, &e)).is_none());
    pool.sweep();
    pool.begin_pass();
    pool.sweep();
    pool.begin_pass();
    assert!(pool.get(request(7, &e)).is_none());
    assert_eq!(counts.made.load(Ordering::SeqCst), 2);
}

#[test]
fn a_failed_key_is_reported_until_it_stops_being_asked() {
    let counts = Arc::new(Counts::default());
    let mut pool = failing_pool(&counts);
    let e = effect(CLASS, &[1]);
    assert!(!pool.failed(7));
    pool.begin_pass();
    assert!(pool.get(request(7, &e)).is_none());
    assert!(pool.failed(7));
    assert!(!pool.failed(8), "another key");
    pool.sweep();
    assert!(pool.failed(7), "still asked");
    pool.begin_pass();
    pool.sweep();
    assert!(!pool.failed(7));
}

#[test]
fn a_failed_key_adopts_the_held_instance_made_aside() {
    let counts = Arc::new(Counts::default());
    let e = effect(CLASS, &[1]);
    let mut made_aside = pool(&counts);
    made_aside.begin_pass();
    made_aside.get(request(99, &e)).unwrap();
    let aside = made_aside.instance(99).unwrap();

    let mut pool = failing_pool(&counts);
    pool.begin_pass();
    assert!(pool.get(request(1, &e)).is_none());
    assert!(pool.failed(1));
    pool.set_held(vec![(1, aside.clone())]);
    pool.sweep();
    pool.begin_pass();
    let played = pool.get(request(1, &e)).expect("the held instance");
    assert!(same(&played, &aside));
    assert!(same(&pool.instance(1).unwrap(), &aside));
    assert!(!pool.failed(1));
    assert_eq!(counts.made.load(Ordering::SeqCst), 2, "no new make");
}

#[test]
fn a_failed_key_does_not_adopt_a_held_instance_from_another_bundle() {
    let counts = Arc::new(Counts::default());
    let e = effect(CLASS, &[1]);
    let mut pool = failing_pool(&counts);
    pool.begin_pass();
    assert!(pool.get(request(1, &e)).is_none());
    let moved: Arc<dyn HostedPlugin> = Arc::new(Fake {
        bundle: Some(PathBuf::from("/elsewhere/Moved.vst3")),
        hash: AtomicU64::new(e.content_hash()),
        counts: counts.clone(),
    });
    pool.set_held(vec![(1, moved)]);
    pool.sweep();
    pool.begin_pass();
    assert!(pool.get(request(1, &e)).is_none());
    assert!(pool.instance(1).is_none());
}

fn same<A: ?Sized, B: ?Sized>(a: &Arc<A>, b: &Arc<B>) -> bool {
    Arc::as_ptr(a).cast::<()>() == Arc::as_ptr(b).cast::<()>()
}

#[test]
fn a_held_instance_survives_a_disable_and_is_the_one_played_again() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    let e = effect(CLASS, &[1]);
    pool.begin_pass();
    pool.get(request(1, &e)).unwrap();
    let editor = pool.instance(1).unwrap();
    pool.set_held(vec![(1, editor.clone())]);
    pool.sweep();
    pool.begin_pass();
    pool.sweep();
    assert!(pool.instance(1).is_none(), "disabled: not in the mix");
    pool.begin_pass();
    let played = pool.get(request(1, &e)).unwrap();
    assert!(same(&played, &editor));
    assert_eq!(counts.made.load(Ordering::SeqCst), 1);
}

#[test]
fn a_held_instance_is_adopted_by_a_key_made_after_it() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    let e = effect(CLASS, &[1]);
    pool.begin_pass();
    pool.get(request(99, &e)).unwrap();
    let aside = pool.instance(99).unwrap();
    pool.set_held(vec![(1, aside.clone())]);
    pool.sweep();
    pool.begin_pass();
    let played = pool.get(request(1, &e)).unwrap();
    assert!(same(&played, &aside));
    assert!(Arc::ptr_eq(&pool.instance(1).unwrap(), &aside));
    assert_eq!(counts.made.load(Ordering::SeqCst), 1);
}

#[test]
fn a_held_instance_with_stale_state_is_reloaded_when_adopted() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    pool.begin_pass();
    pool.get(request(99, &effect(CLASS, &[1]))).unwrap();
    let aside = pool.instance(99).unwrap();
    pool.set_held(vec![(1, aside)]);
    pool.begin_pass();
    pool.get(request(1, &effect(CLASS, &[2]))).unwrap();
    assert_eq!(counts.loads.load(Ordering::SeqCst), 1);
}

#[test]
fn released_instances_are_no_longer_adopted() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    let e = effect(CLASS, &[1]);
    pool.begin_pass();
    pool.get(request(99, &e)).unwrap();
    let aside = pool.instance(99).unwrap();
    pool.set_held(vec![(1, aside)]);
    pool.set_held(Vec::new());
    pool.begin_pass();
    pool.get(request(1, &e)).unwrap();
    assert_eq!(counts.made.load(Ordering::SeqCst), 2);
}

#[test]
fn a_restart_request_is_serviced_by_the_pool_without_an_editor() {
    use vst3::Steinberg::Vst::{IComponentHandlerTrait, RestartFlags_};
    let class_id = "56565445535447414E00000000000008";
    let e = effect(class_id, &[]);
    let instance = fixture_instance(&e);
    let shared = instance.clone();
    let mut pool = Instances::with_maker(
        vec![info(class_id)].into(),
        Box::new(move |_, _, _| Some(shared.clone() as Arc<dyn HostedPlugin>)),
    );
    pool.begin_pass();
    pool.get(request(1, &e)).expect("instance");
    pool.sweep();
    assert_eq!(instance.latency(), 0);
    // SAFETY: the handler is a live COM object owned by the instance.
    unsafe {
        instance
            .handler
            .restartComponent(RestartFlags_::kLatencyChanged)
    };
    pool.begin_pass();
    pool.get(request(1, &e)).expect("instance");
    assert_eq!(instance.latency(), 64);
}

#[test]
fn a_restart_raised_by_set_state_is_applied_in_the_same_pass() {
    let class_id = "56565445535447414E00000000000013";
    let e = effect(class_id, &[]);
    let instance = fixture_instance(&e);
    let shared = instance.clone();
    let mut pool = Instances::with_maker(
        vec![info(class_id)].into(),
        Box::new(move |_, _, _| Some(shared.clone() as Arc<dyn HostedPlugin>)),
    );
    pool.begin_pass();
    pool.get(request(1, &e)).expect("instance");
    pool.sweep();
    assert_eq!(instance.latency(), 0);
    let changed = effect(class_id, &0.25f32.to_le_bytes());
    pool.begin_pass();
    pool.get(request(1, &changed)).expect("instance");
    assert_eq!(instance.latency(), 64);
    pool.service_restarts();
    assert_eq!(instance.latency(), 64);
}

#[test]
fn service_restarts_applies_a_request_without_a_pass() {
    use vst3::Steinberg::Vst::{IComponentHandlerTrait, RestartFlags_};
    let class_id = "56565445535447414E00000000000008";
    let e = effect(class_id, &[]);
    let instance = fixture_instance(&e);
    let shared = instance.clone();
    let mut pool = Instances::with_maker(
        vec![info(class_id)].into(),
        Box::new(move |_, _, _| Some(shared.clone() as Arc<dyn HostedPlugin>)),
    );
    pool.begin_pass();
    pool.get(request(1, &e)).expect("instance");
    // SAFETY: the handler is a live COM object owned by the instance.
    unsafe {
        instance
            .handler
            .restartComponent(RestartFlags_::kLatencyChanged)
    };
    pool.service_restarts();
    assert_eq!(instance.latency(), 64);
}

#[test]
fn a_new_render_reactivates_each_instance_once_not_per_pass() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    let e = effect(CLASS, &[1]);
    for render in 0..3 {
        pool.begin_render();
        for _ in 0..4 {
            pool.begin_pass();
            pool.get(request(7, &e)).unwrap();
            pool.sweep();
        }
        // A new instance starts clean: the first render reactivates nothing.
        assert_eq!(counts.reactivated.load(Ordering::SeqCst), render);
    }
    assert_eq!(counts.made.load(Ordering::SeqCst), 1);
}

#[test]
fn a_failing_class_is_reported_once_per_reason() {
    let mut log = FailureLog::default();
    assert!(log.first("test-class-a", "offline"));
    assert!(!log.first("test-class-a", "offline"));
    assert!(log.first("test-class-a", "main thread only"));
    assert!(!log.first("test-class-a", "main thread only"));
    assert!(log.first("test-class-b", "offline"));
}

#[test]
fn a_reset_reports_the_same_failures_again() {
    let mut log = FailureLog::default();
    assert!(log.first("test-class-a", "offline"));
    log.reset();
    assert!(log.first("test-class-a", "offline"));
}

#[test]
fn failures_past_the_bound_are_not_reported_and_not_stored() {
    let mut log = FailureLog::default();
    for i in 0..FAILURE_LOG_CAP {
        assert!(log.first("test-class", &i.to_string()));
    }
    assert!(!log.first("test-class", "one more"));
    assert_eq!(log.len(), FAILURE_LOG_CAP);
}

#[test]
fn clearing_failures_retries_the_same_state() {
    let counts = Arc::new(Counts::default());
    let mut pool = failing_pool(&counts);
    let e = effect(CLASS, &[1]);
    pool.begin_pass();
    assert!(pool.get(request(7, &e)).is_none());
    pool.clear_failures();
    assert!(!pool.failed(7));
    assert!(pool.get(request(7, &e)).is_none());
    assert_eq!(counts.made.load(Ordering::SeqCst), 2);
}

#[test]
fn make_gets_the_path_the_class_was_scanned_from() {
    let made = Arc::new(std::sync::Mutex::new(Vec::<PathBuf>::new()));
    let seen = made.clone();
    let mut pool = Instances::with_maker(
        vec![info(CLASS)].into(),
        Box::new(move |path, _, _| {
            seen.lock().unwrap().push(path.to_path_buf());
            None
        }),
    );
    let e = effect(CLASS, &[]);
    pool.begin_pass();
    assert!(pool.get(request(1, &e)).is_none());
    let unknown = effect("UNKNOWN", &[]);
    assert!(pool.get(request(2, &unknown)).is_none());
    assert_eq!(*made.lock().unwrap(), vec![PathBuf::from("/fake.vst3")]);
}

fn at(class_id: &str, path: &str) -> PluginInfo {
    PluginInfo {
        path: PathBuf::from(path),
        ..info(class_id)
    }
}

#[test]
fn a_refresh_keeps_the_instances_of_plugins_still_listed_from_the_same_bundle() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    let e = effect(CLASS, &[1]);
    pool.begin_pass();
    let first = pool.get(request(7, &e)).unwrap();
    let other = at("OTHER", "/other.vst3");
    let updated = PluginInfo {
        version: "2".into(),
        ..info(CLASS)
    };
    let dropped = pool.refresh(vec![updated, other].into());
    assert!(dropped.is_empty());
    pool.begin_pass();
    let again = pool.get(request(7, &e)).unwrap();
    assert!(same(&first, &again));
    assert_eq!(counts.made.load(Ordering::SeqCst), 1);
    assert_eq!(counts.dropped.load(Ordering::SeqCst), 0);
}

#[test]
fn a_refresh_drops_only_the_instances_whose_plugin_vanished_or_moved() {
    let counts = Arc::new(Counts::default());
    let mut pool = Instances::with_maker(
        vec![
            info(CLASS),
            at("GONE", "/gone.vst3"),
            at("MOVED", "/a.vst3"),
        ]
        .into(),
        {
            let counts = counts.clone();
            Box::new(move |path, effect, _| {
                counts.made.fetch_add(1, Ordering::SeqCst);
                Some(Arc::new(Fake {
                    bundle: Some(path.to_path_buf()),
                    hash: AtomicU64::new(effect.content_hash()),
                    counts: counts.clone(),
                }))
            })
        },
    );
    let (kept, gone, moved) = (
        effect(CLASS, &[]),
        effect("GONE", &[]),
        effect("MOVED", &[]),
    );
    pool.begin_pass();
    let kept_instance = pool.get(request(1, &kept)).unwrap();
    let gone_instance = pool.get(request(2, &gone)).unwrap();
    let moved_instance = pool.get(request(3, &moved)).unwrap();
    let dropped = pool.refresh(vec![info(CLASS), at("MOVED", "/b.vst3")].into());
    assert_eq!(dropped.len(), 2);
    assert!(dropped.iter().any(|d| same(d, &gone_instance)));
    assert!(dropped.iter().any(|d| same(d, &moved_instance)));
    assert!(pool.instance(1).is_some_and(|i| same(&i, &kept_instance)));
    assert!(pool.instance(2).is_none() && pool.instance(3).is_none());
    pool.begin_pass();
    pool.get(request(3, &moved)).unwrap();
    assert_eq!(counts.made.load(Ordering::SeqCst), 4);
}

#[test]
fn a_refresh_lets_a_failed_plugin_be_made_once_it_is_listed_again() {
    let counts = Arc::new(Counts::default());
    let mut pool = failing_pool(&counts);
    let e = effect(CLASS, &[1]);
    pool.begin_pass();
    assert!(pool.get(request(7, &e)).is_none());
    assert!(pool.failed(7));
    pool.refresh(vec![info(CLASS)].into());
    assert!(!pool.failed(7));
    pool.begin_pass();
    assert!(pool.get(request(7, &e)).is_none());
    assert_eq!(counts.made.load(Ordering::SeqCst), 2);
}

#[test]
fn a_refresh_lets_a_class_installed_since_be_made() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    let late = effect("LATE", &[]);
    pool.begin_pass();
    assert!(pool.get(request(1, &late)).is_none());
    pool.refresh(vec![info(CLASS), at("LATE", "/late.vst3")].into());
    assert!(pool.knows(&late));
    pool.begin_pass();
    assert!(pool.get(request(1, &late)).is_some());
}

#[test]
fn a_dropped_instance_that_is_held_is_not_adopted_again_from_the_new_bundle() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    let e = effect(CLASS, &[1]);
    pool.begin_pass();
    pool.get(request(7, &e)).unwrap();
    let old: Arc<dyn HostedPlugin> = pool.instance(7).unwrap();
    pool.set_held(vec![(7, old.clone())]);
    let dropped = pool.refresh(vec![at(CLASS, "/moved.vst3")].into());
    assert!(dropped.iter().any(|d| same(d, &old)));
    pool.begin_pass();
    let fresh = pool.get(request(7, &e)).unwrap();
    assert!(!same(&fresh, &old));
    assert_eq!(counts.made.load(Ordering::SeqCst), 2);
}

#[test]
fn a_held_instance_of_a_disabled_effect_is_dropped_when_its_bundle_is_unlisted() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    let e = effect(CLASS, &[1]);
    pool.begin_pass();
    pool.get(request(7, &e)).unwrap();
    let held: Arc<dyn HostedPlugin> = pool.instance(7).unwrap();
    pool.begin_pass();
    pool.get(bypassed(7, &e));
    pool.sweep();
    pool.set_held(vec![(7, held.clone())]);
    let dropped = pool.refresh(vec![at(CLASS, "/moved.vst3")].into());
    assert!(dropped.iter().any(|d| same(d, &held)));
    assert_eq!(dropped.len(), 1);
}

fn module_file(dir: &std::path::Path, body: &[u8]) -> PathBuf {
    let path = dir.join("Fake.vst3");
    std::fs::write(&path, body).unwrap();
    path
}

fn build_in(path: &std::path::Path, version: &str, class_id: &str) -> Option<u64> {
    let info = PluginInfo {
        path: path.to_path_buf(),
        version: version.into(),
        ..info(CLASS)
    };
    let pool = Instances::with_maker(
        vec![info].into(),
        Box::new(|_: &std::path::Path, _: &Vst3Effect, _: u32| None),
    );
    pool.build(&effect(class_id, &[]))
}

#[test]
fn the_build_is_the_one_of_the_code_this_process_runs() {
    let dir = std::env::temp_dir().join(format!("vv-build-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = module_file(&dir, b"one");
    let first = build_in(&path, "1.0", CLASS);
    assert!(first.is_some());
    module_file(&dir, b"a longer, updated module");
    assert_eq!(first, build_in(&path, "1.0", CLASS));
    let other = std::fs::create_dir_all(dir.join("b")).map(|_| module_file(&dir.join("b"), b"x"));
    assert_ne!(first, build_in(&other.unwrap(), "1.0", CLASS));
    assert_eq!(
        None,
        build_in(&path, "1.0", "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB")
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_ready_slot_whose_instance_is_inactive_is_reported_failed_until_it_recovers() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    pool.begin_pass();
    pool.get(request(7, &effect(CLASS, &[1]))).unwrap();
    assert!(!pool.failed(7));
    counts.inactive.store(true, Ordering::SeqCst);
    assert!(pool.failed(7));
    counts.inactive.store(false, Ordering::SeqCst);
    assert!(!pool.failed(7));
}

#[test]
fn a_lowercase_project_class_id_finds_the_scanned_plugin() {
    let counts = Arc::default();
    let pool = pool(&counts);
    let mut json = serde_json::to_value(fixture_effect(&[])).unwrap();
    json["class_id"] = FIXTURE_CLASS_ID.to_ascii_lowercase().into();
    let loaded: Vst3Effect = serde_json::from_value(json).unwrap();
    assert!(pool.knows(&loaded));
    assert!(pool.build(&loaded).is_some());
}

#[test]
fn a_cached_lowercase_list_and_a_lowercase_project_still_match() {
    let cached = format!(
        r#"{{"class_id": "{}", "name": "n", "vendor": "v", "version": "1", "path": "/fake.vst3"}}"#,
        FIXTURE_CLASS_ID.to_ascii_lowercase()
    );
    let info: PluginInfo = serde_json::from_str(&cached).unwrap();
    let pool = Instances::with_maker(
        vec![info].into(),
        Box::new(|_: &std::path::Path, _: &Vst3Effect, _: u32| None),
    );
    let mut json = serde_json::to_value(fixture_effect(&[])).unwrap();
    json["class_id"] = FIXTURE_CLASS_ID.to_ascii_lowercase().into();
    let loaded: Vst3Effect = serde_json::from_value(json).unwrap();
    assert!(pool.knows(&loaded));
}
