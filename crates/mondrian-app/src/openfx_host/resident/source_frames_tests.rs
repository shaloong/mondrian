use super::*;
use mondrian_effects::{
    EffectColorDomain, EffectFrameExtent, EffectValueFormat, EffectWorkingPrecision,
};

fn signal(width: u32, height: u32, seed: f32) -> EffectFloatFrame {
    EffectFloatFrame::from_pixels(
        EffectFrameExtent::new(width, height),
        EffectValueFormat::new(
            EffectWorkingPrecision::Float32,
            EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        ),
        mondrian_core::ImageSampling::UNIT_SQUARE,
        (0..width * height)
            .map(|i| {
                let x = seed + i as f32 / 16.0;
                [x, -x * 2.0, x * 4.0, x / 2.0]
            })
            .collect(),
    )
    .unwrap()
}
fn timing() -> OpenFxRenderTiming {
    OpenFxRenderTiming {
        frame: 0.25,
        frame_rate: 24.0,
        first_frame: -10.0,
        last_frame: 10.0,
    }
}
fn input_set<'a>(past: &'a EffectFloatFrame, future: &'a EffectFloatFrame) -> [SourceFrame<'a>; 2] {
    [
        SourceFrame { time: -1.5, x: -2, y: 3, frame: past },
        SourceFrame { time: 2.25, x: 4, y: -3, frame: future },
    ]
}

#[test]
fn delivery_wire_has_bounded_descriptors_and_refuses_duplicate_or_missing_fields() {
    let d =
        serde_json::json!({"time":-1.5,"x":-2,"y":3,"width":2,"height":3,"offset":0,"bytes":96});
    let mut m = serde_json::json!({"path":"unused","bytes":96,"frames":[d.clone()]});
    assert!(serde_json::from_value::<Manifest>(m.clone()).is_ok());
    m["frames"] = serde_json::Value::Array(vec![d.clone(); LIMIT]);
    assert!(serde_json::from_value::<Manifest>(m.clone()).is_ok());
    m["frames"] = serde_json::Value::Array(vec![d; LIMIT + 1]);
    assert!(serde_json::from_value::<Manifest>(m).is_err());
    let raw = r#"{"path":"a","path":"b","bytes":16,"frames":[]}"#;
    assert!(serde_json::from_str::<Manifest>(raw).is_err());
    assert!(serde_json::from_str::<Manifest>(r#"{"path":"a","bytes":16}"#).is_err());
}

#[test]
fn source_delivery_admits_before_copy_and_releases_backing_without_submission() {
    let past = signal(2, 3, -2.0);
    let future = signal(4, 1, 1.0);
    let sources = input_set(&past, &future);
    let mut calls = Vec::new();
    let denied = Delivery::acquire(
        &sources,
        &timing(),
        past.sampling(),
        past.format(),
        &mut |n| {
            calls.push(n);
            Ok(false)
        },
        &mut || Ok(()),
    );
    assert!(matches!(
        denied,
        Err(OpenFxHostError::HostResourceRefused(
            mondrian_effects::EffectProcessorHostResourceRefusal::InputDelivery {
                required_bytes: 160
            }
        ))
    ));
    assert_eq!(calls, [160]);
    let delivery = Delivery::acquire(
        &sources,
        &timing(),
        past.sampling(),
        past.format(),
        &mut |_| Ok(true),
        &mut || Ok(()),
    )
    .unwrap()
    .unwrap();
    let path = delivery.manifest.path.clone();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[..96], bytemuck::cast_slice::<_, u8>(past.pixels()));
    assert_eq!(&bytes[96..], bytemuck::cast_slice::<_, u8>(future.pixels()));
    assert_eq!(delivery.manifest.frames[1].offset, 96);
    drop(delivery);
    assert!(!path.exists());
    let mut bad = sources;
    bad[1].time = bad[0].time;
    let mut admitted = false;
    assert!(Delivery::acquire(
        &bad,
        &timing(),
        past.sampling(),
        past.format(),
        &mut |_| {
            admitted = true;
            Ok(true)
        },
        &mut || Ok(())
    )
    .is_err());
    assert!(!admitted);
    let canceled = Delivery::acquire(
        &sources,
        &timing(),
        past.sampling(),
        past.format(),
        &mut |_| panic!("canceled before admission"),
        &mut || Err(OpenFxHostError::Canceled),
    );
    assert!(matches!(canceled, Err(OpenFxHostError::Canceled)));
}

#[test]
fn child_source_mapping_rejects_malformed_storage_pixels_and_context_before_native_load() {
    let past = signal(2, 3, -2.0);
    let future = signal(4, 1, 1.0);
    let mut delivery = Delivery::acquire(
        &input_set(&past, &future),
        &timing(),
        past.sampling(),
        past.format(),
        &mut |_| Ok(true),
        &mut || Ok(()),
    )
    .unwrap()
    .unwrap();
    let mut prepare = Prepare {
        context: OpenFxContext::Filter,
        version: VERSION,
        nonce: String::new(),
        binary_path: PathBuf::from("must-never-load.ofx"),
        bundle_path: PathBuf::new(),
        binary_sha256: String::new(),
        plugin: OpenFxPluginDescriptor {
            identifier: "not-loaded".into(),
            api_version: 1,
            version_major: 1,
            version_minor: 0,
        },
        project_geometry: mondrian_effects::EffectProjectGeometry::new(
            3,
            2,
            mondrian_core::SampleAspectRatio::SQUARE,
        )
        .unwrap(),
        width: 3,
        height: 2,
        shared_path: PathBuf::new(),
        input_alpha: mondrian_core::ColorFrameAlpha::StraightFloat,
        owner: mondrian_core::EffectProcessorOwnerId::new(),
    };
    let open = |m: &Manifest, p: &Prepare| {
        MappedSources::open(Some(m), p, &timing(), past.sampling(), &mut || Ok(()))
    };
    let mapped = open(&delivery.manifest, &prepare).unwrap();
    assert_eq!(mapped.frames.len(), 2);
    // SAFETY: the native span borrows this same still-live read-only mapping.
    let actual = unsafe { std::slice::from_raw_parts(mapped.frames[0].pixels, 6) };
    assert_eq!(actual, past.pixels());
    drop(mapped);
    for offset in [0, 95, 97, usize::MAX - 15] {
        delivery.manifest.frames[1].offset = offset;
        assert!(open(&delivery.manifest, &prepare).is_err());
    }
    delivery.manifest.frames[1].offset = 96;
    delivery.manifest.frames[1].time = -1.5;
    assert!(open(&delivery.manifest, &prepare).is_err());
    delivery.manifest.frames[1].time = 2.25;
    prepare.context = OpenFxContext::Generator;
    assert!(open(&delivery.manifest, &prepare).is_err());
    prepare.context = OpenFxContext::Filter;
    for len in [0, 159, 161] {
        delivery._file.as_file().set_len(len).unwrap();
        assert!(open(&delivery.manifest, &prepare).is_err());
    }
    delivery._file.as_file().set_len(160).unwrap();
    let mut bytes = [
        bytemuck::cast_slice(past.pixels()),
        bytemuck::cast_slice(future.pixels()),
    ]
    .concat();
    for lane in 0..4 {
        let saved = bytes[lane * 4..lane * 4 + 4].to_vec();
        bytes[lane * 4..lane * 4 + 4].copy_from_slice(&f32::NAN.to_ne_bytes());
        std::fs::write(delivery._file.path(), &bytes).unwrap();
        assert!(open(&delivery.manifest, &prepare).is_err());
        bytes[lane * 4..lane * 4 + 4].copy_from_slice(&saved);
    }
    std::fs::write(delivery._file.path(), &bytes).unwrap();
    prepare.input_alpha = mondrian_core::ColorFrameAlpha::Opaque;
    assert!(open(&delivery.manifest, &prepare).is_err());
    prepare.input_alpha = mondrian_core::ColorFrameAlpha::StraightFloat;
    assert!(open(&delivery.manifest, &prepare).is_ok());
    let stopped = MappedSources::open(
        Some(&delivery.manifest),
        &prepare,
        &timing(),
        past.sampling(),
        &mut || Err(OpenFxHostError::Canceled),
    );
    assert!(matches!(stopped, Err(OpenFxHostError::Canceled)));
}

#[test]
#[ignore = "requires a fresh normal worker and SourceDeliveryProbe; ABI calibration, not temporal compatibility"]
fn actual_resident_source_delivery_preserves_pixels_owner_refusal_and_cancellation() {
    const TEST: &str = "openfx_host::resident::source_frames::tests::actual_resident_source_delivery_preserves_pixels_owner_refusal_and_cancellation";
    if !super::super::super::shared_frame_tests::qualification_child(TEST, &[]) {
        return;
    }
    let helper = PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_TEST_HELPER").unwrap());
    let binary = PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_SOURCE_DELIVERY_BINARY").unwrap());
    let log = tempfile::NamedTempFile::new().unwrap();
    // This is an isolated single-test qualification process. Only its own
    // serially spawned workers consume this fixture-specific environment.
    unsafe {
        std::env::set_var("MONDRIAN_OPENFX_SOURCE_DELIVERY_LOG", log.path());
    }
    let inspection = crate::openfx_adapter::inspect_openfx_binary(&helper, &binary).unwrap();
    let current = signal(3, 2, 0.5);
    let future = signal(4, 1, 2.0);
    let prepare = || {
        OpenFxFilterSession::prepare(
            &helper,
            &inspection,
            "org.mondrian.fixture.source-delivery",
            3,
            2,
            current.format(),
            mondrian_effects::EffectProjectGeometry::new(
                3,
                2,
                mondrian_core::SampleAspectRatio::SQUARE,
            )
            .unwrap(),
            &mut || Ok(()),
        )
        .unwrap()
    };
    let mut session = prepare();
    let worker = session.worker_process_id();
    let mut native_owner = None;
    for pass in 0..4 {
        let past = signal(2, 3, -2.0 - pass as f32);
        let sources = input_set(&past, &future);
        let serial = session.sequence;
        let mut grants = Vec::new();
        let denied = session.render_observed_with_sources(
            &timing(),
            &BTreeMap::new(),
            &current,
            &sources,
            None,
            mondrian_effects::EffectProcessorGuiAdmission::CLOSED,
            &mut |n| {
                grants.push(n);
                Ok(false)
            },
            &mut |_| Ok(mondrian_effects::EffectProcessorQuestionState::Unavailable),
        );
        assert!(matches!(
            denied,
            Err(OpenFxHostError::HostResourceRefused(
                mondrian_effects::EffectProcessorHostResourceRefusal::InputDelivery {
                    required_bytes: 160
                }
            ))
        ));
        assert_eq!(grants, [160]);
        assert_eq!(session.sequence, serial);
        assert!(session.pending_inputs.is_empty());
        assert_eq!(session.process.state, ProcessState::Running);
        grants.clear();
        let denied_domain = session.render_observed_with_sources(
            &timing(),
            &BTreeMap::new(),
            &current,
            &sources,
            None,
            mondrian_effects::EffectProcessorGuiAdmission::CLOSED,
            &mut |n| {
                grants.push(n);
                Ok(n == 160)
            },
            &mut |_| Ok(mondrian_effects::EffectProcessorQuestionState::Unavailable),
        );
        assert!(matches!(
            denied_domain,
            Err(OpenFxHostError::PreflightBudgetRefused { required_bytes: 480, .. })
        ));
        assert_eq!(
            grants,
            [160, 480],
            "held inputs compose with the actual 320-byte output domain"
        );
        assert!(session.pending_inputs.is_empty());
        assert_eq!(session.process.state, ProcessState::Running);
        grants.clear();
        let output = session
            .render_observed_with_sources(
                &timing(),
                &BTreeMap::new(),
                &current,
                &sources,
                None,
                mondrian_effects::EffectProcessorGuiAdmission::CLOSED,
                &mut |n| {
                    grants.push(n);
                    Ok(true)
                },
                &mut |_| Ok(mondrian_effects::EffectProcessorQuestionState::Unavailable),
            )
            .unwrap();
        assert_eq!(grants, [160, 480]);
        assert!(session.pending_inputs.is_empty());
        assert_eq!(session.worker_process_id(), worker);
        let feedback = session.take_feedback().unwrap();
        let owner = feedback.snapshot.native_owner();
        assert_eq!(*native_owner.get_or_insert(owner), owner);
        for i in 0..6 {
            let a = current.pixels()[i];
            let b = past.pixels()[(i + 1) % 6];
            let c = future.pixels()[(i + 2) % 4];
            let expected =
                std::array::from_fn::<_, 4, _>(|lane| a[lane] + 2.0 * b[lane] + 3.0 * c[lane]);
            assert_eq!(
                output.pixels()[i].map(f32::to_bits),
                expected.map(f32::to_bits),
                "pass {pass}, pixel {i}"
            );
        }
        let mut duplicate = sources;
        duplicate[1].time = duplicate[0].time;
        let serial = session.sequence;
        assert!(matches!(
            session.render_observed_with_sources(
                &timing(),
                &BTreeMap::new(),
                &current,
                &duplicate,
                None,
                mondrian_effects::EffectProcessorGuiAdmission::CLOSED,
                &mut |_| panic!("bad set before allocation"),
                &mut |_| Ok(mondrian_effects::EffectProcessorQuestionState::Unavailable)
            ),
            Err(OpenFxHostError::Invalid(_))
        ));
        assert_eq!(session.sequence, serial);
    }
    session.shutdown().unwrap();
    assert_eq!(session.process.state, ProcessState::Settled);
    let actions = std::fs::read_to_string(log.path()).unwrap();
    assert_eq!(
        actions.lines().filter(|s| *s == "OfxActionCreateInstance").count(),
        1
    );
    assert_eq!(
        actions.lines().filter(|s| *s == "OfxImageEffectActionRender").count(),
        4
    );
    assert_eq!(
        actions.lines().filter(|s| *s == "OfxActionDestroyInstance").count(),
        1
    );
    std::fs::write(log.path(), []).unwrap();
    unsafe {
        std::env::set_var("MONDRIAN_OPENFX_SOURCE_DELIVERY_WAIT", "1");
    }
    let mut session = prepare();
    let past = signal(2, 3, -3.0);
    let canceled = session.render_observed_with_sources(
        &timing(),
        &BTreeMap::new(),
        &current,
        &input_set(&past, &future),
        None,
        mondrian_effects::EffectProcessorGuiAdmission::CLOSED,
        &mut |_| Ok(true),
        &mut |_| {
            if std::fs::read_to_string(log.path())
                .unwrap()
                .lines()
                .any(|s| s == "sources-held")
            {
                Err(OpenFxHostError::Canceled)
            } else {
                Ok(mondrian_effects::EffectProcessorQuestionState::Unavailable)
            }
        },
    );
    assert!(matches!(canceled, Err(OpenFxHostError::Canceled)));
    assert_eq!(session.process.state, ProcessState::Settled);
    assert!(session.pending_inputs.is_empty());
    assert_eq!(current.pixels(), signal(3, 2, 0.5).pixels());
    println!("Source delivery: 4 complete RGBA renders/same native owner; 4 pre-submit and 4 composed pre-DSP refusals; duplicate rejection; cancellation with four image handles held; child settled");
}
