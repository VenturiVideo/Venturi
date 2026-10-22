use vst3::{ComPtr, ComRef};

use super::*;

#[test]
fn stream_seek_past_end_reads_nothing_and_negative_is_refused() {
    use vst3::Steinberg::{IBStream, IBStreamTrait};
    let stream = MemStream::new(vec![1, 2, 3]);
    let s = stream.as_com_ref::<IBStream>().unwrap();
    let mut pos = 0;
    let mut buf = [0u8; 4];
    let mut read = -1;
    // SAFETY: valid out-pointers on a live stream.
    unsafe {
        assert_eq!(s.seek(10, 0, &mut pos), kResultOk);
        assert_eq!(pos, 10);
        assert_eq!(s.read(buf.as_mut_ptr().cast(), 4, &mut read), kResultOk);
        assert_eq!(read, 0);
        assert_ne!(s.seek(-1, 0, &mut pos), kResultOk);
        assert_eq!(s.seek(0, 1, &mut pos), kResultOk);
        assert_eq!(pos, 10);
    }
}

#[test]
fn stream_seek_forward_then_write_zero_fills_the_gap() {
    use vst3::Steinberg::{IBStream, IBStreamTrait};
    let stream = MemStream::new(Vec::new());
    let s = stream.as_com_ref::<IBStream>().unwrap();
    let body = [7u8, 8, 9];
    let mut written = 0;
    // SAFETY: valid buffers and out-pointers on a live stream.
    unsafe {
        assert_eq!(s.seek(4, 1, std::ptr::null_mut()), kResultOk);
        assert_eq!(
            s.write(body.as_ptr().cast_mut().cast(), 3, &mut written),
            kResultOk
        );
        assert_eq!(written, 3);
    }
    assert_eq!(stream.take(), [0, 0, 0, 0, 7, 8, 9]);
}

#[test]
fn stream_header_reserved_by_seeking_forward_keeps_the_body() {
    use vst3::Steinberg::{IBStream, IBStreamTrait};
    let stream = MemStream::new(Vec::new());
    let s = stream.as_com_ref::<IBStream>().unwrap();
    let body = [1u8, 2, 3, 4, 5];
    let size = (body.len() as u32).to_le_bytes();
    // SAFETY: valid buffers and out-pointers on a live stream.
    unsafe {
        assert_eq!(s.seek(4, 1, std::ptr::null_mut()), kResultOk);
        assert_eq!(
            s.write(body.as_ptr().cast_mut().cast(), 5, std::ptr::null_mut()),
            kResultOk
        );
        assert_eq!(s.seek(0, 0, std::ptr::null_mut()), kResultOk);
        assert_eq!(
            s.write(size.as_ptr().cast_mut().cast(), 4, std::ptr::null_mut()),
            kResultOk
        );
    }
    assert_eq!(stream.take(), [5, 0, 0, 0, 1, 2, 3, 4, 5]);
}

#[test]
fn host_creates_messages_with_round_tripping_attributes() {
    use std::ffi::{CString, c_void};
    use vst3::Steinberg::Vst::{
        IAttributeListTrait, IHostApplication, IHostApplicationTrait, IMessage, IMessage_iid,
        IMessageTrait,
    };
    let host = ComWrapper::new(crate::host::HostApp);
    let app = host.as_com_ref::<IHostApplication>().unwrap();
    let mut cid = IMessage_iid;
    let mut obj: *mut c_void = std::ptr::null_mut();
    // SAFETY: valid ids and out-pointers; the object is released by the ComPtr.
    unsafe {
        assert_eq!(app.createInstance(&mut cid, &mut cid, &mut obj), kResultOk);
        let msg = ComPtr::from_raw(obj.cast::<IMessage>()).unwrap();
        let id = CString::new("hello").unwrap();
        msg.setMessageID(id.as_ptr());
        assert_eq!(std::ffi::CStr::from_ptr(msg.getMessageID()), id.as_c_str());
        let attrs = ComRef::from_raw(msg.getAttributes()).unwrap();
        let key = |s: &str| CString::new(s).unwrap();
        attrs.setInt(key("i").as_ptr(), -7);
        attrs.setFloat(key("f").as_ptr(), 2.5);
        let text: Vec<u16> = "word\0".encode_utf16().collect();
        attrs.setString(key("s").as_ptr(), text.as_ptr());
        attrs.setBinary(key("b").as_ptr(), [9u8, 8, 7].as_ptr().cast(), 3);

        let (mut i, mut f) = (0i64, 0.0f64);
        assert_eq!(attrs.getInt(key("i").as_ptr(), &mut i), kResultOk);
        assert_eq!(attrs.getFloat(key("f").as_ptr(), &mut f), kResultOk);
        assert_eq!((i, f), (-7, 2.5));
        let mut out = [0u16; 16];
        assert_eq!(
            attrs.getString(key("s").as_ptr(), out.as_mut_ptr(), 32),
            kResultOk
        );
        assert_eq!(out[..5], text[..]);
        let (mut data, mut len): (*const c_void, u32) = (std::ptr::null(), 0);
        assert_eq!(
            attrs.getBinary(key("b").as_ptr(), &mut data, &mut len),
            kResultOk
        );
        assert_eq!(
            std::slice::from_raw_parts(data.cast::<u8>(), len as usize),
            [9, 8, 7]
        );
        assert_ne!(attrs.getInt(key("missing").as_ptr(), &mut i), kResultOk);
    }
}

#[test]
fn stream_write_far_past_the_end_is_refused_without_allocating() {
    use vst3::Steinberg::{IBStream, IBStreamTrait};
    let stream = MemStream::new(Vec::new());
    let s = stream.as_com_ref::<IBStream>().unwrap();
    let body = [1u8];
    // SAFETY: valid buffers and out-pointers on a live stream.
    unsafe {
        assert_eq!(s.seek(i64::MAX - 1, 0, std::ptr::null_mut()), kResultOk);
        assert_ne!(
            s.write(body.as_ptr().cast_mut().cast(), 1, std::ptr::null_mut()),
            kResultOk
        );
    }
    assert!(stream.take().is_empty());
}

#[test]
fn missed_edits_coalesce_by_id_and_drain_once() {
    let missed = Missed::new();
    assert!(missed.record(3, 0.1, 1));
    assert!(missed.record(4, 0.2, 2));
    assert!(missed.record(3, 0.9, 3));
    let mut got = Vec::new();
    missed.drain(|id, v, s| got.push((id, v, s)));
    assert_eq!(got, [(3, 0.9, 3), (4, 0.2, 2)]);
    missed.drain(|_, _, _| panic!("drained twice"));
}

#[test]
fn a_full_missed_table_refuses_new_ids_but_updates_known_ones() {
    let missed = Missed::new();
    for id in 0..MAX_PARAMS as u32 {
        assert!(missed.record(id, 0.5, 1));
    }
    assert!(!missed.record(1000, 0.5, 2));
    assert!(missed.record(7, 0.6, 3));
}

fn values(changes: &ParamChanges) -> Vec<(ParamID, ParamValue)> {
    let mut got = Vec::new();
    changes.for_each_value(|id, v| got.push((id, v)));
    got
}

#[test]
fn an_audio_thread_edit_never_waits_for_the_params_lock() {
    let shared = std::sync::Arc::new(Shared::new());
    let handler = ComponentHandler(shared.clone());
    let held = shared.params.lock().unwrap();
    let _scope = AudioScope::enter();
    // SAFETY: plain calls on a live handler.
    unsafe { handler.performEdit(5, 0.75) };
    drop(_scope);
    drop(held);
    assert!(shared.params.lock().unwrap().is_empty());
    let changes = ParamChanges::input();
    shared.fill(&changes, None);
    assert_eq!(values(&changes), [(5, 0.75)]);
}

#[test]
fn an_edit_outside_the_audio_thread_is_queued_as_before() {
    let shared = std::sync::Arc::new(Shared::new());
    let handler = ComponentHandler(shared.clone());
    // SAFETY: plain call on a live handler.
    unsafe { handler.performEdit(5, 0.75) };
    let params = shared.params.lock().unwrap();
    assert_eq!((params[0].0, params[0].1), (5, 0.75));
}

#[test]
fn an_audio_thread_edit_does_not_grow_a_full_params_queue() {
    let shared = std::sync::Arc::new(Shared::new());
    let handler = ComponentHandler(shared.clone());
    let cap = {
        let mut params = shared.params.lock().unwrap();
        let cap = params.capacity();
        for id in 0..cap as u32 {
            params.push((id, 0.0, 0));
        }
        cap
    };
    let _scope = AudioScope::enter();
    // SAFETY: plain call on a live handler.
    unsafe { handler.performEdit(10_000, 0.5) };
    assert_eq!(shared.params.lock().unwrap().capacity(), cap);
    let changes = ParamChanges::input();
    shared.missed.drain(|id, v, _| _ = changes.merge(id, v, 1));
    assert_eq!(values(&changes), [(10_000, 0.5)]);
}

fn edit(handler: &ComponentHandler, id: ParamID, value: ParamValue, audio: bool) {
    let _scope = audio.then(AudioScope::enter);
    // SAFETY: plain call on a live handler.
    unsafe { handler.performEdit(id, value) };
}

#[test]
fn a_later_queued_edit_beats_an_earlier_missed_one_of_the_same_id() {
    let shared = std::sync::Arc::new(Shared::new());
    let handler = ComponentHandler(shared.clone());
    let held = shared.params.lock().unwrap();
    edit(&handler, 5, 0.1, true);
    drop(held);
    edit(&handler, 5, 0.9, false);
    let changes = ParamChanges::input();
    shared.fill(&changes, Some(&mut shared.params.lock().unwrap()));
    assert_eq!(values(&changes), [(5, 0.9)]);
}

#[test]
fn a_later_missed_edit_beats_an_earlier_queued_one_of_the_same_id() {
    let shared = std::sync::Arc::new(Shared::new());
    let handler = ComponentHandler(shared.clone());
    edit(&handler, 5, 0.1, false);
    let held = shared.params.lock().unwrap();
    edit(&handler, 5, 0.9, true);
    drop(held);
    let changes = ParamChanges::input();
    shared.fill(&changes, Some(&mut shared.params.lock().unwrap()));
    assert_eq!(values(&changes), [(5, 0.9)]);
    assert!(shared.params.lock().unwrap().is_empty());
}

#[test]
fn missed_edits_are_never_crowded_out_by_a_full_params_queue() {
    let shared = std::sync::Arc::new(Shared::new());
    let handler = ComponentHandler(shared.clone());
    for id in 0..2 * MAX_PARAMS as u32 {
        edit(&handler, id, 0.5, false);
    }
    let held = shared.params.lock().unwrap();
    edit(&handler, 1000, 0.25, true);
    drop(held);
    let changes = ParamChanges::input();
    shared.fill(&changes, Some(&mut shared.params.lock().unwrap()));
    assert!(values(&changes).contains(&(1000, 0.25)));
    assert_eq!(shared.params.lock().unwrap().len(), MAX_PARAMS + 1);
}

#[test]
fn output_queues_read_back_what_the_plugin_added() {
    use vst3::Steinberg::Vst::{IParamValueQueueTrait, IParameterChangesTrait};
    let changes = crate::host::ParamChanges::output();
    let out = changes
        .as_com_ref::<vst3::Steinberg::Vst::IParameterChanges>()
        .unwrap();
    let mut index = -1;
    // SAFETY: valid out-pointers on live host objects.
    unsafe {
        let queue = ComRef::from_raw(out.addParameterData(&7, &mut index)).unwrap();
        assert_eq!((index, out.getParameterCount()), (0, 1));
        assert_eq!(queue.getPointCount(), 0);
        assert_eq!(queue.addPoint(3, 0.25, &mut index), kResultOk);
        assert_eq!(queue.addPoint(5, 0.75, &mut index), kResultOk);
        assert_eq!(out.getParameterCount(), 1);
        let back = ComRef::from_raw(out.getParameterData(0)).unwrap();
        assert_eq!((back.getParameterId(), back.getPointCount()), (7, 1));
        let (mut offset, mut value) = (-1, -1.0);
        assert_eq!(back.getPoint(0, &mut offset, &mut value), kResultOk);
        assert_eq!((offset, value), (5, 0.75));
        out.addParameterData(&7, &mut index);
        assert_eq!((index, out.getParameterCount()), (0, 1));
        for id in 100..163 {
            assert!(!out.addParameterData(&id, &mut index).is_null());
        }
        assert_eq!(out.getParameterCount(), 64);
    }
}

#[test]
fn output_ids_beyond_capacity_get_a_discarding_sink() {
    use vst3::Steinberg::Vst::{IParamValueQueueTrait, IParameterChangesTrait};
    let changes = crate::host::ParamChanges::output_with_capacity(2);
    let out = changes
        .as_com_ref::<vst3::Steinberg::Vst::IParameterChanges>()
        .unwrap();
    let mut index = -1;
    // SAFETY: valid out-pointers on live host objects.
    unsafe {
        for id in 10..13 {
            let queue = ComRef::from_raw(out.addParameterData(&id, &mut index)).unwrap();
            assert_eq!(queue.addPoint(0, 0.5, &mut index), kResultOk);
        }
        assert_eq!(out.getParameterCount(), 2);
        let mut seen = Vec::new();
        changes.for_each_value(|id, _| seen.push(id));
        assert_eq!(seen, [10, 11]);
        let sink = ComRef::from_raw(out.addParameterData(&99, &mut index)).unwrap();
        assert_eq!(sink.getPointCount(), 0);
    }
}
