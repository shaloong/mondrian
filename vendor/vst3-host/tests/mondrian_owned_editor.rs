//! Actual loaded, owned VST3 IPlugView qualification. These ignored Windows
//! gates require a matching rebuilt fixture, not a mock controller or old DLL.
#![cfg(target_os = "windows")]

use std::path::PathBuf;
use std::ptr;
use std::rc::Rc;
use vst3_host::plugin::{
    NativeEditorEvent, NativeEditorEventDrain, ParameterEdit, ParameterEditKind, Plugin,
    PluginEditorProcessingSplit,
};
use vst3_host::window::{WindowsEditorMainThread, WindowsPluginEditorParent};
use vst3_host::Vst3Host;
use winapi::shared::windef::HWND;
use winapi::um::winuser::{
    FindWindowExW, FindWindowW, IsWindow, PostMessageW, WM_KEYDOWN, WM_SETFOCUS,
};

fn isolated_native_gate(name: &str) -> bool {
    if std::env::var("MONDRIAN_OWNED_EDITOR_GATE_CHILD")
        .ok()
        .as_deref()
        == Some(name)
    {
        // A failing native teardown must never unwind through a retained view
        // and unload its DLL. Qualification owns an isolated process too.
        std::panic::set_hook(Box::new(|info| {
            eprintln!("{info}");
            std::process::exit(1);
        }));
        return true;
    }
    let output = std::process::Command::new(
        std::env::current_exe().expect("actual qualification executable"),
    )
    .args([
        name,
        "--exact",
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ])
    .env("MONDRIAN_OWNED_EDITOR_GATE_CHILD", name)
    .output()
    .expect("isolated native gate child");
    assert!(
        output.status.success(),
        "actual native child failed: {}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    false
}

struct ActualOwnedFixture {
    // Actual loaded native owners are dropped before the STA/OLE owner.
    plugin: Plugin,
    _host: Vst3Host,
    main: Rc<WindowsEditorMainThread>,
    title: String,
}

impl ActualOwnedFixture {
    fn load(case: &str) -> Self {
        let path = PathBuf::from(
            std::env::var_os("MONDRIAN_VST3_EDITOR_FIXTURE")
                .expect("set MONDRIAN_VST3_EDITOR_FIXTURE to the rebuilt owned vst3-editor DLL"),
        );
        assert!(path.is_file(), "matching rebuilt editor fixture must exist");
        let main =
            Rc::new(WindowsEditorMainThread::initialize().expect("actual Windows STA/OLE main"));
        let mut host = Vst3Host::builder()
            .sample_rate(48_000.0)
            .block_size(64)
            .with_process_isolation(false)
            .build()
            .expect("actual in-process native host");
        let plugin = host
            .load_plugin(&path)
            .expect("load owned actual IPlugView fixture");
        let title = format!(
            "Mondrian native editor qualification {case} {}",
            std::process::id()
        );
        Self {
            plugin,
            _host: host,
            main,
            title,
        }
    }

    fn native_handles(title: &str) -> (HWND, HWND) {
        let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
        let class: Vec<u16> = "MondrianOwnedVst3EditorFixture"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let parent = unsafe { FindWindowW(ptr::null(), title.as_ptr()) };
        assert!(!parent.is_null(), "actual host parent is live");
        let child = unsafe { FindWindowExW(parent, ptr::null_mut(), class.as_ptr(), ptr::null()) };
        assert!(
            !child.is_null(),
            "actual IPlugView created its real native child window"
        );
        assert_ne!(parent, child);
        (parent, child)
    }
}

fn edit(id: u32, kind: ParameterEditKind, value: Option<f64>) -> NativeEditorEvent {
    NativeEditorEvent::Parameter(ParameterEdit { id, kind, value })
}

fn expected_group() -> Vec<NativeEditorEvent> {
    vec![
        NativeEditorEvent::GroupEditStarted,
        edit(0, ParameterEditKind::BeginGesture, None),
        edit(0, ParameterEditKind::ValueChange, Some(0.75)),
        edit(0, ParameterEditKind::EndGesture, None),
        edit(1, ParameterEditKind::BeginGesture, None),
        edit(1, ParameterEditKind::ValueChange, Some(0.25)),
        edit(1, ParameterEditKind::EndGesture, None),
        NativeEditorEvent::DirtyChanged(true),
        NativeEditorEvent::GroupEditFinished,
    ]
}

#[test]
#[ignore = "actual rebuilt owned Windows IPlugView and interactive Windows session required"]
fn actual_owned_vst3_editor_main_messages_preserve_group_and_same_controller() {
    if !isolated_native_gate(
        "actual_owned_vst3_editor_main_messages_preserve_group_and_same_controller",
    ) {
        return;
    }
    let mut fixture = ActualOwnedFixture::load("group");
    let original_owner = fixture
        .plugin
        .native_editor_attachment_owner()
        .expect("real loaded instance");
    let PluginEditorProcessingSplit {
        mut editor,
        processor,
    } = fixture
        .plugin
        .split_editor_processing()
        .expect("same-instance scoped roles");
    drop(processor);
    let mut parent =
        WindowsPluginEditorParent::open(Rc::clone(&fixture.main), &mut editor, &fixture.title)
            .expect("attach actual IPlugView");
    let (hwnd, child) = ActualOwnedFixture::native_handles(&fixture.title);
    assert_eq!(
        editor.parameter(100).expect("actual attach probe"),
        1.0 / 1024.0
    );
    assert_eq!(
        editor.parameter(0).expect("same controller initial value"),
        0.5
    );
    assert_eq!(unsafe { PostMessageW(child, WM_SETFOCUS, 0, 0) }, 1);
    assert_eq!(
        unsafe { PostMessageW(child, WM_KEYDOWN, b'G' as usize, 0) },
        1
    );
    for _ in 0..8 {
        parent
            .service(&mut editor)
            .expect("service actual main message pump");
    }
    assert_eq!(
        editor
            .take_editor_events()
            .expect("complete actual native callbacks"),
        NativeEditorEventDrain::Complete {
            events: expected_group()
        }
    );
    assert_eq!(
        editor.parameter(0).expect("same controller edited gain"),
        0.75
    );
    assert_eq!(
        editor.parameter(1).expect("same controller edited balance"),
        0.25
    );
    assert!(editor.parameter(103).expect("native focus message probe") >= 1.0 / 1024.0);
    assert_eq!(
        editor.parameter(106).expect("one native group"),
        1.0 / 1024.0
    );
    assert_eq!(
        editor
            .parameter(107)
            .expect("controller callbacks confined to main"),
        0.0
    );
    assert_eq!(
        editor
            .resize_editor(10, 50)
            .expect("actual view constraints"),
        (200, 100)
    );
    assert_eq!(
        editor.parameter(104).expect("actual onSize probe"),
        1.0 / 1024.0
    );
    parent
        .close(&mut editor)
        .expect("remove actual view before freeing parent");
    assert_eq!(unsafe { IsWindow(child) }, 0);
    assert_eq!(unsafe { IsWindow(hwnd) }, 0);
    assert_eq!(
        editor.parameter(101).expect("one actual removed"),
        1.0 / 1024.0
    );
    drop(editor);
    assert_eq!(
        fixture.plugin.native_editor_attachment_owner(),
        Some(original_owner),
        "window close keeps actual native object"
    );
    assert_eq!(
        fixture
            .plugin
            .get_parameter(0)
            .expect("original controller persists"),
        0.75
    );
    let configuration = fixture
        .plugin
        .save_state()
        .expect("coherent joined configuration snapshot");
    fixture
        .plugin
        .set_parameter(0, 0.1)
        .expect("control update");
    fixture
        .plugin
        .load_state(&configuration)
        .expect("actual component/controller state roundtrip");
    assert_eq!(
        fixture
            .plugin
            .get_parameter(0)
            .expect("restored controller"),
        0.75
    );
}

#[test]
#[ignore = "actual rebuilt owned Windows IPlugView and interactive Windows session required"]
fn actual_owned_vst3_editor_remove_failure_keeps_attachment_without_repeating_removed() {
    if !isolated_native_gate(
        "actual_owned_vst3_editor_remove_failure_keeps_attachment_without_repeating_removed",
    ) {
        return;
    }
    let mut fixture = ActualOwnedFixture::load("remove-failure");
    fixture
        .plugin
        .set_parameter(2, 0.5)
        .expect("owned fixture removal failure mode");
    let PluginEditorProcessingSplit {
        mut editor,
        processor,
    } = fixture
        .plugin
        .split_editor_processing()
        .expect("same actual object");
    drop(processor);
    let mut parent =
        WindowsPluginEditorParent::open(Rc::clone(&fixture.main), &mut editor, &fixture.title)
            .expect("actual view attach");
    let (hwnd, child) = ActualOwnedFixture::native_handles(&fixture.title);
    assert!(
        parent.close(&mut editor).is_err(),
        "real removed refusal must be reported"
    );
    assert_eq!(
        unsafe { IsWindow(hwnd) },
        1,
        "native parent must outlive still-attached view"
    );
    assert_eq!(
        unsafe { IsWindow(child) },
        1,
        "real view child is retained after failed removed"
    );
    assert_eq!(
        editor.parameter(101).expect("first removal attempt"),
        1.0 / 1024.0
    );
    assert!(
        parent.close(&mut editor).is_err(),
        "unknown native attachment cannot be removed twice"
    );
    assert_eq!(
        editor
            .parameter(101)
            .expect("no repeated foreign removed callback"),
        1.0 / 1024.0
    );
    assert_eq!(unsafe { IsWindow(child) }, 1);
    assert_eq!(unsafe { IsWindow(hwnd) }, 1);
    assert_eq!(editor.parameter(107).expect("main-only lifecycle"), 0.0);
    // The view remains attached by contract. Retire this actual isolated child
    // without running Rust/native drops or unloading a live window procedure.
    std::process::exit(0);
}

#[test]
#[ignore = "actual rebuilt owned Windows IPlugView and interactive Windows session required"]
fn actual_owned_vst3_editor_frame_clear_failure_never_repeats_removed() {
    if !isolated_native_gate("actual_owned_vst3_editor_frame_clear_failure_never_repeats_removed") {
        return;
    }
    let mut fixture = ActualOwnedFixture::load("frame-clear-failure");
    fixture
        .plugin
        .set_parameter(2, 1.0)
        .expect("owned fixture frame cleanup failure mode");
    let PluginEditorProcessingSplit {
        mut editor,
        processor,
    } = fixture
        .plugin
        .split_editor_processing()
        .expect("same actual object");
    drop(processor);
    let mut parent =
        WindowsPluginEditorParent::open(Rc::clone(&fixture.main), &mut editor, &fixture.title)
            .expect("actual view attach");
    let (hwnd, child) = ActualOwnedFixture::native_handles(&fixture.title);
    assert!(
        parent.close(&mut editor).is_err(),
        "real setFrame(null) refusal must be reported"
    );
    assert_eq!(
        unsafe { IsWindow(child) },
        0,
        "actual removed already succeeded"
    );
    assert_eq!(
        unsafe { IsWindow(hwnd) },
        1,
        "parent retained until exact unfinished frame cleanup"
    );
    assert_eq!(
        editor.parameter(101).expect("one successful removed"),
        1.0 / 1024.0
    );
    parent.close(&mut editor).expect("retry frame cleanup only");
    assert_eq!(
        editor
            .parameter(101)
            .expect("no duplicated removed callback"),
        1.0 / 1024.0
    );
    assert_eq!(unsafe { IsWindow(hwnd) }, 0);
}

#[test]
#[ignore = "actual rebuilt owned Windows IPlugView and interactive Windows session required"]
fn actual_owned_vst3_editor_scoped_dsp_and_native_gui_share_one_instance() {
    if !isolated_native_gate(
        "actual_owned_vst3_editor_scoped_dsp_and_native_gui_share_one_instance",
    ) {
        return;
    }
    let mut fixture = ActualOwnedFixture::load("scoped-dsp");
    let mut buffers = fixture
        .plugin
        .create_bus_audio_buffers(64)
        .expect("actual bus-aware storage before split");
    for channel in &mut buffers.inputs[0].channels {
        channel.fill(1.0);
    }
    fixture
        .plugin
        .start_processing()
        .expect("component activation before native scope");
    let main_id = std::thread::current().id();
    let PluginEditorProcessingSplit {
        mut editor,
        mut processor,
    } = fixture
        .plugin
        .split_editor_processing()
        .expect("real same-instance borrowed roles");
    let mut parent =
        WindowsPluginEditorParent::open(Rc::clone(&fixture.main), &mut editor, &fixture.title)
            .expect("same existing controller view");
    let (_, child) = ActualOwnedFixture::native_handles(&fixture.title);
    std::thread::scope(|scope| {
        let (start, ready) = std::sync::mpsc::sync_channel(0);
        let (finished, completion) = std::sync::mpsc::sync_channel(0);
        let dsp = scope.spawn(move || {
            assert_ne!(
                std::thread::current().id(),
                main_id,
                "actual DSP is physically separate from controller main"
            );
            ready
                .recv()
                .expect("main completed actual native message work");
            processor
                .process_bus_audio(&mut buffers)
                .expect("same component consumes real controller callbacks");
            finished.send(()).expect("explicit DSP completion");
            buffers
        });
        assert_eq!(
            unsafe { PostMessageW(child, WM_KEYDOWN, b'G' as usize, 0) },
            1
        );
        for _ in 0..8 {
            parent
                .service(&mut editor)
                .expect("real main native messages while DSP role is elsewhere");
        }
        assert_eq!(
            editor.take_editor_events().expect("ordered native source"),
            NativeEditorEventDrain::Complete {
                events: expected_group()
            }
        );
        start
            .send(())
            .expect("start actual DSP after admitted GUI update");
        completion.recv().expect("actual processing completed");
        let buffers = dsp
            .join()
            .expect("real DSP joins before native lifecycle mutation");
        assert!(
            buffers.outputs[0].channels[0]
                .iter()
                .all(|sample| *sample == 1.125),
            "same processor sees edited gain and balance"
        );
        assert!(buffers.outputs[0].channels[1]
            .iter()
            .all(|sample| *sample == 0.375));
    });
    assert_eq!(
        editor
            .parameter(107)
            .expect("no UI callbacks on actual DSP"),
        0.0
    );
    parent
        .close(&mut editor)
        .expect("close only after DSP join");
    drop(editor);
    fixture
        .plugin
        .stop_processing()
        .expect("native processing stop after scoped roles end");
}
