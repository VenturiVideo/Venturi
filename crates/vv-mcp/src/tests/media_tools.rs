use super::*;
use crate::dispatch::tests::{create, error, import, ok, run_deferred, test_dir};
use crate::{ToolCall, dispatch};
use serde_json::Value;
use std::path::Path;

/// 2 s at 25 fps, 64x48, whose luma is 5 x the frame number.
fn numbered_frames(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("numbered.mp4");
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=64x48:r=25:d=2,format=yuv420p,geq=lum='N*5':cb=128:cr=128",
            "-c:v",
            "libx264",
            "-qp",
            "0",
            "-pix_fmt",
            "yuv420p",
        ],
        &path,
    );
    path
}

/// 1 s of silence, then 1 s of tone.
fn silence_then_tone(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("speech.wav");
    vv_media::test_support::ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "anullsrc=r=48000:cl=mono:d=1",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=1",
            "-filter_complex",
            "[0][1]concat=n=2:v=0:a=1",
        ],
        &path,
    );
    path
}

fn media_id(value: &Value) -> String {
    value["media"][0]["id"].as_str().unwrap().to_owned()
}

fn timeline_from(session: &mut Session, media: &str) -> String {
    let mut args = create("T");
    args.from_media = Some(media.into());
    ok(session, ToolCall::CreateTimeline(args))["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn insert(session: &mut Session, timeline: &str, media: &str, source_in: i64) {
    ok(
        session,
        ToolCall::InsertClip(InsertClipArgs {
            timeline_id: timeline.into(),
            if_revision: None,
            media_id: media.into(),
            at: 0,
            source_in: Some(source_in),
            source_out: None,
            video_track: None,
            audio_track: None,
            video: true,
            audio: true,
        }),
    );
}

fn decode_png(png: &[u8]) -> (u32, u32, Vec<u8>) {
    let mut reader = png::Decoder::new(std::io::Cursor::new(png))
        .read_info()
        .unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixels).unwrap();
    (info.width, info.height, pixels)
}

#[test]
fn render_frame_shows_exactly_the_asked_frame() {
    let dir = test_dir("render-frame");
    let path = numbered_frames(&dir);
    let mut session = Session::default();
    let media = media_id(&import(&mut session, &[&path]));
    let timeline = timeline_from(&mut session, &media);
    insert(&mut session, &timeline, &media, 3);

    let render = |frame| {
        ToolCall::RenderFrame(RenderFrameArgs {
            timeline_id: timeline.clone(),
            frame,
            max_width: Some(32),
        })
    };
    let output = run_deferred(&mut session, render(10)).unwrap();

    assert_eq!(output.value["width"], 32);
    assert_eq!(output.value["height"], 24);
    let (width, height, pixels) = decode_png(&output.image_png.unwrap());
    assert_eq!((width, height), (32, 24));
    // Timeline frame 10 = source frame 13: luma 65, limited range → ~57 RGB.
    let center = ((12 * 32 + 16) * 4) as usize;
    let red = pixels[center] as i32;
    assert!((red - 57).abs() <= 3, "red={red}");

    let length = session
        .project
        .timelines
        .values()
        .next()
        .unwrap()
        .total_frames();
    assert!(error(&mut session, render(length)).starts_with(&format!("frame {length} is outside")));
}

#[test]
fn audio_levels_of_a_media_and_of_the_timeline() {
    let dir = test_dir("audio-levels");
    let path = silence_then_tone(&dir);
    let mut session = Session::default();
    let media = media_id(&import(&mut session, &[&path]));
    let fps = session.project.media_pool.values().next().unwrap().meta.fps;
    let second = fps.as_f64().round() as i64;

    let levels = |media_id: Option<String>, timeline_id: Option<String>, window| {
        ToolCall::GetAudioLevels(AudioLevelsArgs {
            media_id,
            stream: None,
            timeline_id,
            start: 0,
            end: 2 * second,
            window,
        })
    };
    let result = run_deferred(
        &mut session,
        levels(Some(media.clone()), None, Some(second)),
    )
    .unwrap()
    .value;
    let rms: Vec<f64> = result["rms_db"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    assert_eq!(rms.len(), 2);
    assert_eq!(rms[0], -120.0);
    assert!(rms[1] > -30.0, "{rms:?}");

    let timeline = timeline_from(&mut session, &media);
    insert(&mut session, &timeline, &media, 0);
    let mixed = run_deferred(
        &mut session,
        levels(None, Some(timeline.clone()), Some(second)),
    )
    .unwrap()
    .value;
    assert_eq!(mixed["rms_db"][0], -120.0);
    assert!(mixed["rms_db"][1].as_f64().unwrap() > -30.0);

    assert_eq!(
        error(
            &mut session,
            levels(Some(media.clone()), Some(timeline), None)
        ),
        "pass either `media_id` or `timeline_id`"
    );
    let mut too_many = AudioLevelsArgs {
        media_id: Some(media),
        stream: None,
        timeline_id: None,
        start: 0,
        end: 30_000,
        window: Some(1),
    };
    assert!(
        error(&mut session, ToolCall::GetAudioLevels(too_many.clone())).contains("at most 20000")
    );
    too_many.stream = Some(1);
    too_many.end = 10;
    assert_eq!(
        error(&mut session, ToolCall::GetAudioLevels(too_many)),
        "the media has 1 audio streams"
    );
}

#[test]
fn timeline_audio_levels_wait_for_the_plugin_scan() {
    let dir = test_dir("levels-scan");
    let path = silence_then_tone(&dir);
    let mut session = Session::default();
    let media = media_id(&import(&mut session, &[&path]));
    let timeline = timeline_from(&mut session, &media);
    insert(&mut session, &timeline, &media, 0);
    let id = ids::timeline_id(&session.project, &timeline).unwrap();
    let mut effect = vv_core::AudioEffect::new(vv_core::AudioEffectKind::Vst3(
        vv_plugin::testing::fixture_effect_of("0123456789ABCDEF0123456789ABCDEF", &[]),
    ));
    effect.enabled = true;
    session.project.timelines[id].master.effects.push(effect);
    let fps = session.project.timelines[id].fps.as_f64().round() as i64;
    let levels = || {
        ToolCall::GetAudioLevels(AudioLevelsArgs {
            media_id: None,
            stream: None,
            timeline_id: Some(timeline.clone()),
            start: 0,
            end: 2 * fps,
            window: Some(fps),
        })
    };

    vv_plugin::set_scanning(true);
    let Dispatch::Handled(refused) = dispatch(&mut session, levels()) else {
        panic!("levels must be refused before rendering");
    };
    assert_eq!(
        refused.unwrap_err().0,
        "plugins are being scanned, retry shortly"
    );
    vv_plugin::set_scanning(false);
    let result = run_deferred(&mut session, levels()).unwrap().value;
    assert_eq!(result["rms_db"].as_array().unwrap().len(), 2);
}

#[test]
fn export_runs_in_the_background_and_reports_its_state() {
    let dir = test_dir("export");
    let mut session = Session::default();
    let timeline = ok(&mut session, ToolCall::CreateTimeline(create("T")))["id"]
        .as_str()
        .unwrap()
        .to_owned();
    ok(
        &mut session,
        ToolCall::AddSolidColor(AddSolidColorArgs {
            timeline_id: timeline.clone(),
            if_revision: None,
            at: 0,
            duration: Some(10),
            track: None,
            color: None,
        }),
    );
    let export = |path: std::path::PathBuf| {
        ToolCall::Export(ExportArgs {
            timeline_id: timeline.clone(),
            path: path.display().to_string(),
            range: None,
            scale_percent: Some(10),
            audio: false,
        })
    };
    assert!(error(&mut session, export(dir.join("nope/out.mp4"))).starts_with("folder "));

    let output = dir.join("out.mp4");
    let started = ok(&mut session, export(output.clone()));
    assert_eq!(started["total_frames"], 10);
    let job = JobArgs {
        job_id: started["job_id"].as_str().unwrap().into(),
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let status = loop {
        session.tick();
        let status = ok(&mut session, ToolCall::ExportStatus(job.clone()));
        if status["state"] != "running" {
            break status;
        }
        assert!(std::time::Instant::now() < deadline, "export never ended");
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert_eq!(status["state"], "done", "{status}");
    assert_eq!(status["current_frame"], 10);
    assert!(output.exists());
    assert!(error(&mut session, ToolCall::CancelExport(job)).ends_with("is not running"));
    assert_eq!(
        error(
            &mut session,
            ToolCall::ExportStatus(JobArgs {
                job_id: "999".into()
            })
        ),
        "no export \"999\""
    );
    assert!(matches!(
        dispatch(&mut session, ToolCall::GetProject),
        crate::Dispatch::Handled(Ok(_))
    ));
}
