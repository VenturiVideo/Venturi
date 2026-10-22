use super::*;
use crate::{
    AudioEffect, AudioEffectKind, Clip, ClipId, CrossTransition, Ease, EffectId, FrameIdx, History,
    InsertClip, MediaMeta, PluginState, PushDirection, Rational, Track, TrackKind, Transition,
    TransitionKind, Vst3Effect,
};

fn clip_at(timeline_start: FrameIdx, len: FrameIdx, id: u64) -> Clip {
    Clip::from_source_range(
        ClipId(id),
        ClipSource::SolidColor,
        0,
        len,
        timeline_start,
        Rational::one(),
    )
}

fn absorb_all(
    project: &mut Project,
    other: Project,
    reuse: &HashMap<MediaId, MediaId>,
    folder: Option<FolderId>,
) -> Vec<TimelineId> {
    let mut add = AddEntities::new(CommandLabel::ImportOtio);
    let (timelines, _) = absorb(project, &mut add, other, reuse, folder);
    History::default().do_command(project, Box::new(add));
    timelines
}

fn project_with_timeline_item() -> (Project, MediaId) {
    let mut project = Project::default();
    let a = project.alloc_clip_id();
    let b = project.alloc_clip_id();
    let group = project.alloc_link_group_id();
    let mut clip_a = clip_at(0, 10, a.0);
    clip_a.linked_group = Some(group);
    let mut track = Track::new(TrackKind::Video);
    track.clips = vec![clip_a, clip_at(10, 10, b.0)];
    track.crossings = vec![CrossTransition {
        left_clip: a,
        right_clip: b,
        transition: Transition {
            kind: TransitionKind::Push,
            duration: 4,
            direction: PushDirection::Right,
            ease: Ease::None,
            curve: 0.0,
        },
    }];
    let timeline = project.timelines.insert(Timeline {
        name: "Timeline 1".into(),
        fps: Rational::new(30, 1),
        resolution: (64, 48),
        tracks: vec![track],
        markers: Vec::new(),
        master: Default::default(),
    });
    let media_id = project.media_pool.insert(MediaItem {
        path: "Timeline 1".into(),
        meta: MediaMeta {
            duration_frames: 0,
            fps: Rational::new(30, 1),
            width: 64,
            height: 48,
            has_video: true,
            has_audio: false,
            sample_rate: 48_000,
            channels: 2,
            audio_streams: 1,
            file: Default::default(),
        },
        content_hash: 0,
        compound: Some(timeline),
        folder: None,
    });
    project.sync_compound_meta(media_id);
    (project, media_id)
}

#[test]
fn duplicate_timeline_copies_clips_with_fresh_ids() {
    let (mut project, source) = project_with_timeline_item();
    let (copy, add) =
        duplicate_timeline(&mut project, source, "Timeline 1 copy".into()).expect("a timeline");
    History::default().do_command(&mut project, Box::new(add));

    let source_tl = &project.timelines[project.media_pool[source].compound.unwrap()];
    let copy_tl = &project.timelines[project.media_pool[copy].compound.unwrap()];
    assert_eq!(copy_tl.name, "Timeline 1 copy");
    assert_eq!(
        project.media_pool[copy].path,
        std::path::Path::new("Timeline 1 copy")
    );
    assert_eq!(project.media_pool[copy].meta.duration_frames, 20);

    let (src, dup) = (&source_tl.tracks[0], &copy_tl.tracks[0]);
    assert_eq!(dup.clips.len(), 2);
    for (s, d) in src.clips.iter().zip(&dup.clips) {
        assert_ne!(s.id, d.id);
        assert_eq!(s.timeline_start, d.timeline_start);
    }
    assert!(dup.clips[0].linked_group.is_some());
    assert_ne!(dup.clips[0].linked_group, src.clips[0].linked_group);
    assert_eq!(dup.crossings[0].left_clip, dup.clips[0].id);
    assert_eq!(dup.crossings[0].right_clip, dup.clips[1].id);
}

fn vst3_effect(id: EffectId) -> AudioEffect {
    AudioEffect::new(AudioEffectKind::Vst3(Vst3Effect {
        id,
        class_id: "0123456789ABCDEF0123456789ABCDEF".into(),
        name: "Plugin".into(),
        vendor: "Vendor".into(),
        version: "1.0".into(),
        component_state: PluginState::new(vec![1, 2, 3]),
        controller_state: PluginState::new(vec![4]),
    }))
}

fn effect_id(effect: &AudioEffect) -> EffectId {
    match &effect.kind {
        AudioEffectKind::Vst3(v) => v.id,
        _ => unreachable!(),
    }
}

#[test]
fn duplicate_timeline_gives_plugin_effects_fresh_ids() {
    let (mut project, source) = project_with_timeline_item();
    let timeline = project.media_pool[source].compound.unwrap();
    let track_effect = vst3_effect(EffectId::fresh());
    let master_effect = vst3_effect(EffectId::fresh());
    let tl = &mut project.timelines[timeline];
    tl.tracks[0].mix.effects.push(track_effect.clone());
    tl.master.effects.push(master_effect.clone());

    let (copy, add) = duplicate_timeline(&mut project, source, "copy".into()).expect("a timeline");
    History::default().do_command(&mut project, Box::new(add));
    let copy_tl = &project.timelines[project.media_pool[copy].compound.unwrap()];

    let ids = |effect: &AudioEffect| match &effect.kind {
        AudioEffectKind::Vst3(v) => (v.id, v.class_id.clone(), v.component_state.clone()),
        _ => unreachable!(),
    };
    let (src_track, dup_track) = (ids(&track_effect), ids(&copy_tl.tracks[0].mix.effects[0]));
    let (src_master, dup_master) = (ids(&master_effect), ids(&copy_tl.master.effects[0]));
    assert_ne!(src_track.0, dup_track.0);
    assert_ne!(src_master.0, dup_master.0);
    assert_eq!(src_track.1, dup_track.1);
    assert_eq!(src_track.2, dup_track.2);
    assert_eq!(src_master.1, dup_master.1);
    assert_eq!(src_master.2, dup_master.2);
}

#[test]
fn rename_timeline_renames_pool_item_and_timeline() {
    let (mut project, media_id) = project_with_timeline_item();
    let mut history = History::default();
    history.do_command(
        &mut project,
        Box::new(RenameTimeline::new(media_id, "Edit".into())),
    );
    assert_eq!(
        project.media_pool[media_id].path,
        std::path::Path::new("Edit")
    );
    let timeline = project.media_pool[media_id].compound.unwrap();
    assert_eq!(project.timelines[timeline].name, "Edit");

    history.undo(&mut project);
    assert_eq!(project.timelines[timeline].name, "Timeline 1");
    assert_eq!(
        project.media_pool[media_id].path,
        std::path::Path::new("Timeline 1")
    );
}

fn media_item(path: &str) -> MediaItem {
    let (project, timeline_item) = project_with_timeline_item();
    MediaItem {
        path: path.into(),
        compound: None,
        ..project.media_pool[timeline_item].clone()
    }
}

/// What an OTIO import produces: media and a timeline using them, with
/// ids that collide with the destination's.
fn imported_project() -> (Project, MediaId, MediaId) {
    let (mut project, timeline_item) = project_with_timeline_item();
    project.media_pool.remove(timeline_item);
    let a = project.media_pool.insert(media_item("/otio/a.mp4"));
    let b = project.media_pool.insert(media_item("/otio/b.mp4"));
    let timeline = project.timelines.values_mut().next().unwrap();
    timeline.tracks[0].clips[0].source = ClipSource::Media(a);
    timeline.tracks[0].clips[1].source = ClipSource::Media(b);
    (project, a, b)
}

#[test]
fn absorb_adds_media_to_the_folder_and_a_timeline_item_at_the_root() {
    let (mut project, existing_item) = project_with_timeline_item();
    let existing_tl = project.media_pool[existing_item].compound.unwrap();
    let existing_ids: Vec<ClipId> = project.timelines[existing_tl].tracks[0]
        .clips
        .iter()
        .map(|c| c.id)
        .collect();
    let folder = project.folders.insert(MediaFolder {
        name: "otio".into(),
        parent: None,
    });
    let (imported, ..) = imported_project();

    let timelines = absorb_all(&mut project, imported, &Default::default(), Some(folder));

    assert_eq!(timelines.len(), 1);
    assert_eq!(project.timelines.len(), 2, "the existing timeline is kept");
    let new_tl = &project.timelines[timelines[0]];
    let track = &new_tl.tracks[0];
    for clip in &track.clips {
        assert!(!existing_ids.contains(&clip.id), "clip ids stay unique");
        let ClipSource::Media(media) = clip.source else {
            panic!("media clip expected");
        };
        assert_eq!(project.media_pool[media].folder, Some(folder));
    }
    assert_eq!(track.crossings[0].left_clip, track.clips[0].id);
    let item = project
        .media_pool
        .values()
        .find(|m| m.compound == Some(timelines[0]))
        .expect("timeline item");
    assert_eq!(item.folder, None);
    assert_eq!(item.meta.duration_frames, 20);
}

#[test]
fn absorb_reuses_the_mapped_media() {
    let (mut project, _) = project_with_timeline_item();
    let existing = project.media_pool.insert(media_item("/mine/a.mp4"));
    let (imported, a, _) = imported_project();
    let pool_before = project.media_pool.len();

    let timelines = absorb_all(&mut project, imported, &[(a, existing)].into(), None);

    assert_eq!(
        project.media_pool.len(),
        pool_before + 2,
        "b.mp4 and the timeline item"
    );
    let clips = &project.timelines[timelines[0]].tracks[0].clips;
    assert!(matches!(clips[0].source, ClipSource::Media(m) if m == existing));
}

#[test]
fn absorb_gives_plugin_effects_fresh_ids_on_every_import() {
    let (mut payload, ..) = imported_project();
    let source = EffectId::fresh();
    let timeline = payload.timelines.values_mut().next().unwrap();
    timeline.tracks[0].mix.effects.push(vst3_effect(source));

    let (mut project, ..) = project_with_timeline_item();
    let first = absorb_all(&mut project, payload.clone(), &Default::default(), None);
    let second = absorb_all(&mut project, payload, &Default::default(), None);

    let id_in = |project: &Project, tl: TimelineId| {
        effect_id(&project.timelines[tl].tracks[0].mix.effects[0])
    };
    let (first, second) = (id_in(&project, first[0]), id_in(&project, second[0]));
    assert_ne!(first, source);
    assert_ne!(second, source);
    assert_ne!(first, second);
}

#[test]
fn deleting_a_folder_moves_its_content_to_the_parent() {
    let mut project = Project::default();
    let outer = project.folders.insert(MediaFolder {
        name: "outer".into(),
        parent: None,
    });
    let inner = project.folders.insert(MediaFolder {
        name: "inner".into(),
        parent: Some(outer),
    });
    let nested = project.folders.insert(MediaFolder {
        name: "nested".into(),
        parent: Some(inner),
    });
    let media = project.media_pool.insert(MediaItem {
        folder: Some(inner),
        ..media_item("/a.mp4")
    });

    let mut history = History::default();
    history.do_command(&mut project, Box::new(DeleteFolder::new(inner)));

    assert!(!project.folders.contains_key(inner));
    assert_eq!(project.folders[nested].parent, Some(outer));
    assert_eq!(project.media_pool[media].folder, Some(outer));

    history.undo(&mut project);
    assert_eq!(project.folders[inner].parent, Some(outer));
    assert_eq!(project.folders[nested].parent, Some(inner));
    assert_eq!(project.media_pool[media].folder, Some(inner));
}

#[test]
fn a_folder_cannot_move_into_itself_or_its_descendants() {
    let mut project = Project::default();
    let outer = project.folders.insert(MediaFolder {
        name: "outer".into(),
        parent: None,
    });
    let inner = project.folders.insert(MediaFolder {
        name: "inner".into(),
        parent: Some(outer),
    });

    assert!(!project.can_move_folder(outer, Some(outer)));
    assert!(!project.can_move_folder(outer, Some(inner)));
    assert!(project.can_move_folder(inner, None));

    let mut history = History::default();
    history.do_command(&mut project, Box::new(MoveFolder::new(inner, None)));
    assert!(project.can_move_folder(outer, Some(inner)));
    history.undo(&mut project);
    assert_eq!(project.folders[inner].parent, Some(outer));
}

#[test]
fn undoing_added_entities_removes_them_and_redo_restores_their_ids() {
    let mut project = Project::default();
    let mut history = History::default();
    let mut add = AddEntities::new(CommandLabel::ImportMedia);
    let folder = add.folder(
        &mut project,
        MediaFolder {
            name: "f".into(),
            parent: None,
        },
    );
    let media = add.media(
        &mut project,
        MediaItem {
            folder: Some(folder),
            ..media_item("/a.mp4")
        },
    );
    history.do_command(&mut project, Box::new(add));
    assert_eq!(project.media_pool[media].folder, Some(folder));

    history.undo(&mut project);
    assert!(project.media_pool.is_empty() && project.folders.is_empty());
    history.redo(&mut project);
    assert_eq!(project.media_pool[media].folder, Some(folder));
    assert_eq!(project.folders[folder].name, "f");
}

#[test]
fn a_clip_added_after_an_import_finds_its_media_after_undo_and_redo() {
    let (mut project, timeline_item) = project_with_timeline_item();
    let timeline = project.media_pool[timeline_item].compound.unwrap();
    let mut history = History::default();
    let mut add = AddEntities::new(CommandLabel::ImportMedia);
    let media = add.media(&mut project, media_item("/a.mp4"));
    history.do_command(&mut project, Box::new(add));
    let mut clip = clip_at(100, 10, project.alloc_clip_id().0);
    clip.source = ClipSource::Media(media);
    history.do_command(
        &mut project,
        Box::new(InsertClip {
            timeline,
            track_index: 0,
            clip,
        }),
    );

    history.undo(&mut project);
    history.undo(&mut project);
    history.redo(&mut project);
    history.redo(&mut project);

    let clip = project.timelines[timeline].tracks[0].clips.last().unwrap();
    assert!(matches!(clip.source, ClipSource::Media(id) if project.media_pool.contains_key(id)));
}

#[test]
fn set_media_folder_undo_restores_each_media_folder() {
    let mut project = Project::default();
    let folder = project.folders.insert(MediaFolder {
        name: "f".into(),
        parent: None,
    });
    let a = project.media_pool.insert(media_item("/a.mp4"));
    let b = project.media_pool.insert(MediaItem {
        folder: Some(folder),
        ..media_item("/b.mp4")
    });
    let mut history = History::default();

    history.do_command(
        &mut project,
        Box::new(SetMediaFolder::new(vec![a, b], None)),
    );
    assert_eq!(project.media_pool[b].folder, None);
    history.undo(&mut project);

    assert_eq!(project.media_pool[a].folder, None);
    assert_eq!(project.media_pool[b].folder, Some(folder));
}

#[test]
fn rename_folder_undo_restores_the_name() {
    let mut project = Project::default();
    let folder = project.folders.insert(MediaFolder {
        name: "old".into(),
        parent: None,
    });
    let mut history = History::default();

    history.do_command(
        &mut project,
        Box::new(RenameFolder::new(folder, "new".into())),
    );
    assert_eq!(project.folders[folder].name, "new");
    history.undo(&mut project);

    assert_eq!(project.folders[folder].name, "old");
}
