//! External metadata, editable author projection and default opaque state load.
//! This gate never processes audio or borrows the first PCM slice's oracle.

use super::*;
use mondrian_timeline::audio::{AudioProcessorDefinitionRef, AudioProcessorParameter};

const METADATA_CASE_ENV: &str = "MONDRIAN_EXTERNAL_CLAP_METADATA_CASE";
const METADATA_CASE_HASH_ENV: &str = "MONDRIAN_EXTERNAL_CLAP_METADATA_CASE_SHA256";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MetadataCase {
    schema: u32,
    helper: Binding,
    plugin: Binding,
    opaque_state: Binding,
    captured_list_response: Binding,
    captured_probe_response: Binding,
    expected_identity: String,
    evidence_directory: PathBuf,
}

// An independent observation DTO, not a replacement for the product codec or
// persistent state. Unknown fields are rejected before any native operation.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CapturedProbe {
    plugin_id: String,
    latency_frames: u32,
    offline: bool,
    tail: CapturedTail,
    parameters: Vec<ClapParameterDescriptor>,
    auxiliary_buses: Vec<Value>,
    captured_state: Option<Vec<u8>>,
}

#[derive(Deserialize)]
enum CapturedTail {
    None,
    Finite(u32),
    Infinite,
}

impl CapturedTail {
    fn contract(&self) -> AudioProcessorTail {
        match self {
            Self::None => AudioProcessorTail::None,
            Self::Finite(0) => AudioProcessorTail::None,
            Self::Finite(frames) => AudioProcessorTail::Finite(*frames as usize),
            Self::Infinite => AudioProcessorTail::Infinite,
        }
    }
}

fn metadata_from_probe(value: &Value) -> CapturedProbe {
    exact_keys(value, &["descriptors", "probe"]);
    assert_eq!(value["descriptors"], json!([]));
    let probe = &value["probe"];
    exact_keys(
        probe,
        &[
            "plugin_id",
            "latency_frames",
            "offline",
            "tail",
            "parameters",
            "auxiliary_buses",
            "captured_state",
        ],
    );
    for parameter in probe["parameters"].as_array().expect("captured descriptors") {
        exact_keys(
            parameter,
            &[
                "id",
                "name",
                "min_value",
                "max_value",
                "default_value",
                "current_value",
                "automatable",
                "stepped",
                "enumeration",
                "read_only",
                "hidden",
            ],
        );
    }
    let result: CapturedProbe =
        serde_json::from_value(probe.clone()).expect("complete captured probe");
    assert!(
        !result.plugin_id.is_empty()
            && result.plugin_id.len() <= 256
            && !result.plugin_id.contains('\0')
    );
    assert!(result.offline);
    // This deliberately scoped gate does not invent auxiliary bus routes.
    assert!(result.auxiliary_buses.is_empty());
    assert!(!result.parameters.is_empty() && result.parameters.len() <= 1024);
    let mut ids = BTreeSet::new();
    for parameter in &result.parameters {
        assert!(
            ids.insert(parameter.id),
            "unique unsigned native descriptor IDs"
        );
        assert!(!parameter.name.is_empty() && parameter.name.len() <= 256);
        assert!([
            parameter.min_value,
            parameter.max_value,
            parameter.default_value,
            parameter.current_value,
        ]
        .into_iter()
        .all(f64::is_finite));
        assert!(parameter.min_value <= parameter.max_value);
        assert!((parameter.min_value..=parameter.max_value).contains(&parameter.default_value));
        assert!((parameter.min_value..=parameter.max_value).contains(&parameter.current_value));
        if parameter.stepped {
            assert_eq!(parameter.min_value.fract(), 0.0);
            assert_eq!(parameter.max_value.fract(), 0.0);
            assert_eq!(parameter.default_value.fract(), 0.0);
            assert_eq!(parameter.current_value.fract(), 0.0);
        }
    }
    assert!(
        result.parameters.iter().any(|parameter| parameter.read_only),
        "explicit RO coverage"
    );
    assert!(
        result.parameters.iter().any(|parameter| !parameter.read_only),
        "explicit editable coverage"
    );
    assert!(result
        .captured_state
        .as_ref()
        .is_some_and(|state| !state.is_empty() && state.len() <= 64 * 1024));
    result
}

// Check the product's projection, never build a second author map from queries.
// A readonly current-value observation is intentionally not an author value.
fn author_projection_matches(
    metadata: &[ClapParameterDescriptor],
    author: &AudioProcessorInstance,
) -> bool {
    let expected: BTreeSet<_> = metadata
        .iter()
        .filter(|descriptor| !descriptor.read_only)
        .filter_map(|descriptor| descriptor.parameter_id().ok())
        .collect();
    let actual: BTreeSet<_> = author.parameters.keys().cloned().collect();
    if expected.len() != metadata.iter().filter(|descriptor| !descriptor.read_only).count()
        || actual != expected
    {
        return false;
    }
    metadata.iter().filter(|descriptor| !descriptor.read_only).all(|descriptor| {
        let Ok(id) = descriptor.parameter_id() else {
            return false;
        };
        let Some(parameter) = author.parameters.get(&id) else {
            return false;
        };
        let Ok(schema) = descriptor.authoring_schema() else {
            return false;
        };
        parameter.schema == schema
            && parameter.ui.display_name.as_deref() == Some(descriptor.name.as_str())
            && parameter.ui.hidden == descriptor.hidden
            && parameter.automation.as_ref().is_some_and(|curve| {
                curve.parameter_id == id
                    && curve.keyframes.is_empty()
                    && curve.default_value.to_bits() == descriptor.current_value.to_bits()
            })
    })
}

fn same_metadata_structure(
    actual: &[ClapParameterDescriptor],
    expected: &[ClapParameterDescriptor],
) -> bool {
    actual.len() == expected.len()
        && expected.iter().all(|descriptor| {
            actual
                .iter()
                .find(|candidate| candidate.id == descriptor.id)
                .is_some_and(|candidate| {
                    candidate.name == descriptor.name
                        && candidate.min_value.to_bits() == descriptor.min_value.to_bits()
                        && candidate.max_value.to_bits() == descriptor.max_value.to_bits()
                        && candidate.default_value.to_bits() == descriptor.default_value.to_bits()
                        && candidate.automatable == descriptor.automatable
                        && candidate.stepped == descriptor.stepped
                        && candidate.enumeration == descriptor.enumeration
                        && candidate.read_only == descriptor.read_only
                        && candidate.hidden == descriptor.hidden
                        && (descriptor.read_only
                            || candidate.current_value.to_bits()
                                == descriptor.current_value.to_bits())
                })
        })
}

fn render() -> AudioRenderContract {
    AudioRenderContract {
        sample_rate: 48_000,
        channel_layout: AudioChannelLayout::Stereo,
        max_block_frames: 64,
        processing_mode: AudioProcessingMode::Offline,
        ..test_render_contract()
    }
}

fn request(author: &AudioProcessorInstance) -> AudioProcessorPrepareRequest<'_> {
    AudioProcessorPrepareRequest::new(
        AudioProcessorOccurrence {
            instance_id: author.id,
            owner: AudioProcessorOccurrenceOwner::Output(ProgramOutputId::new()),
            insertion: AudioProcessorInsertionPoint::PreFader,
        },
        &author.definition,
        &author.parameters,
        author.opaque_state.as_ref().map(AsRef::as_ref),
        render(),
    )
}

fn assert_spec_projection(
    spec: &IsolatedAudioProcessorWorkerSpec,
    metadata: &[ClapParameterDescriptor],
) {
    // Independently inspect the existing concrete payload; no product codec or
    // worker is replaced, instantiated or bypassed by this Lab observation.
    let payload: Value =
        serde_json::from_slice(&spec.preparation_payload).expect("concrete CLAP payload");
    let descriptors: Vec<ClapParameterDescriptor> =
        serde_json::from_value(payload["parameters"].clone()).expect("full native descriptors");
    assert!(same_metadata_structure(&descriptors, metadata));
    assert_eq!(
        descriptors.len(),
        metadata.len(),
        "RO metadata not erased by author projection"
    );
    let authored: BTreeSet<_> = metadata
        .iter()
        .filter(|descriptor| !descriptor.read_only)
        .map(|descriptor| descriptor.parameter_id().expect("native stable ID"))
        .collect();
    assert_eq!(
        spec.parameter_ids.iter().cloned().collect::<BTreeSet<_>>(),
        authored
    );
    assert_eq!(spec.parameter_ids.len(), authored.len());
    let native_ids: Vec<u32> =
        serde_json::from_value(payload["parameter_ids"].clone()).expect("native lane IDs");
    assert_eq!(native_ids.len(), spec.parameter_ids.len());
    for (native, canonical) in native_ids.iter().zip(&spec.parameter_ids) {
        let descriptor = metadata
            .iter()
            .find(|descriptor| descriptor.id == *native)
            .expect("lane descriptor");
        assert!(!descriptor.read_only);
        assert_eq!(
            descriptor.parameter_id().expect("exact lane ID"),
            *canonical
        );
    }
}

#[test]
#[ignore = "hash-bound external CLAP metadata case, current helper and frozen outer native Job required"]
fn external_clap_readonly_author_projection_default_state_load() {
    let path = PathBuf::from(std::env::var_os(METADATA_CASE_ENV).expect("metadata case path"));
    let size = fs::metadata(&path).expect("metadata case file").len();
    let mut case_lease = Lease::open(
        &Binding {
            path,
            bytes: size,
            sha256: std::env::var(METADATA_CASE_HASH_ENV).expect("metadata case SHA256"),
        },
        MAX_JSON,
    );
    let bytes = case_lease.read();
    let case: MetadataCase =
        serde_json::from_value(strict_json(&bytes)).expect("strict metadata-only case");
    assert_eq!(case.schema, 1);
    assert!(
        !case.expected_identity.is_empty()
            && case.expected_identity.len() <= 256
            && !case.expected_identity.contains('\0')
    );
    no_reparse(&case.evidence_directory);
    assert!(fs::metadata(&case.evidence_directory).expect("new evidence directory").is_dir());
    assert_eq!(
        fs::read_dir(&case.evidence_directory)
            .expect("empty evidence directory")
            .count(),
        0
    );
    let helper = Lease::open(&case.helper, 512 * 1024 * 1024);
    let plugin = Lease::open(&case.plugin, 512 * 1024 * 1024);
    let mut list = Lease::open(&case.captured_list_response, MAX_JSON);
    let mut probe = Lease::open(&case.captured_probe_response, MAX_JSON);
    let mut opaque = Lease::open(&case.opaque_state, 64 * 1024);
    let list_value = strict_json(&list.read());
    exact_keys(&list_value, &["descriptors", "probe"]);
    assert!(list_value["probe"].is_null());
    let list_array = list_value["descriptors"].as_array().expect("captured actual List");
    assert_eq!(list_array.len(), 1);
    exact_keys(&list_array[0], &["plugin_id", "name", "vendor", "version"]);
    assert_eq!(list_array[0]["plugin_id"], case.expected_identity);
    let metadata = metadata_from_probe(&strict_json(&probe.read()));
    assert_eq!(metadata.plugin_id, case.expected_identity);
    let state = opaque.read();
    assert_eq!(metadata.captured_state.as_deref(), Some(state.as_slice()));
    write_new(&case.evidence_directory, "case.json", &bytes);
    write_new(
        &case.evidence_directory,
        "original-default-state.bin",
        &state,
    );
    let catalog = DiscoveredClapAudioProcessorSpecResolver::discover(
        helper.path.clone(),
        [plugin.path.clone()],
    )
    .expect("actual isolated native List");
    assert_eq!(catalog.descriptors().len(), 1);
    assert_eq!(json!([catalog.descriptors()[0]]), list_value["descriptors"]);
    let default = catalog
        .create_instance(&case.expected_identity, render(), None)
        .expect("actual default author capture");
    assert!(!default.bypassed);
    assert!(author_projection_matches(&metadata.parameters, &default));
    assert_eq!(
        default.opaque_state.as_ref().map(AsRef::as_ref),
        Some(state.as_slice())
    );
    let initial = serde_json::to_vec(&default).expect("canonical author serialization");
    write_new(&case.evidence_directory, "initial-author.json", &initial);
    let reopened: AudioProcessorInstance =
        serde_json::from_slice(&initial).expect("canonical author reopen");
    assert_eq!(reopened, default);
    let fresh = catalog
        .create_instance(&case.expected_identity, render(), Some(state.clone()))
        .expect("actual inactive-main PluginState.load default seed");
    assert!(author_projection_matches(&metadata.parameters, &fresh));
    assert_eq!(fresh.definition, reopened.definition);
    assert_eq!(fresh.parameters, reopened.parameters);
    assert_eq!(fresh.opaque_state, reopened.opaque_state);
    write_new(
        &case.evidence_directory,
        "restored-author.json",
        &serde_json::to_vec(&fresh).expect("restored canonical author"),
    );
    let registration = probe_clap_plugin_registration(
        &helper.path,
        &plugin.path,
        &case.expected_identity,
        render(),
        Some(&state),
    )
    .expect("fresh actual restored metadata");
    assert!(same_metadata_structure(
        &registration.parameters,
        &metadata.parameters
    ));
    assert_eq!(
        registration.binary_sha256.as_slice(),
        decode_hash(&plugin.sha256).as_slice()
    );
    assert_eq!(
        registration.execution_contract.algorithmic_latency_frames(),
        metadata.latency_frames as usize
    );
    assert_eq!(
        registration.execution_contract.tail(),
        metadata.tail.contract()
    );
    assert!(registration.auxiliary_inputs.buses.is_empty());
    write_new(
        &case.evidence_directory,
        "restored-native-parameters.json",
        &serde_json::to_vec(&registration.parameters).expect("retain actual RO readings"),
    );
    let spec = catalog
        .resolve(request(&reopened))
        .expect("canonical nine-key resolver admission");
    assert_spec_projection(&spec, &metadata.parameters);
    write_new(
        &case.evidence_directory,
        "resolved-payload.json",
        &spec.preparation_payload,
    );
    for descriptor in metadata.parameters.iter().filter(|descriptor| descriptor.read_only) {
        let before = reopened.clone();
        let mut candidate = reopened.clone();
        assert!(matches!(
            candidate.set_parameter_automation(
                ExactAutomationCurve::new(
                    descriptor.parameter_id().expect("RO ID"),
                    descriptor.current_value
                )
                .expect("finite RO test value")
            ),
            Err(mondrian_timeline::audio::AudioAuthoringError::UnknownProcessorParameter)
        ));
        assert_eq!(candidate, before);
        // Resolver refusal proves a forged schema cannot authorize a RO lane.
        let mut forged = reopened.clone();
        forged.parameters.insert(
            descriptor.parameter_id().expect("RO ID"),
            AudioProcessorParameter::from_schema(
                descriptor.authoring_schema().expect("RO numeric schema"),
            )
            .expect("forged RO test parameter"),
        );
        assert!(
            matches!(catalog.resolve(request(&forged)), Err(AudioProcessorHostError::InvalidContract(message)) if message == "authored CLAP parameter contract differs from the plugin")
        );
    }
    write_new(&case.evidence_directory, "result.json", &serde_json::to_vec_pretty(&json!({
        "schema":1,"actual_identity":case.expected_identity,"actual_descriptor":list_array[0],
        "case_sha256":case_lease.sha256,"helper_sha256":helper.sha256,"plugin_sha256":plugin.sha256,
        "original_probe_sha256":probe.sha256,"opaque_state_sha256":opaque.sha256,"opaque_state_bytes":state.len(),
        "native_descriptor_count":metadata.parameters.len(),"canonical_author_count":reopened.parameters.len(),
        "readonly_ids":metadata.parameters.iter().filter(|d| d.read_only).map(|d| d.id).collect::<Vec<_>>(),
        "canonical_author_serde_roundtrip":true,"actual_default_opaque_load_and_metadata_agreement":true,
        "full_descriptor_payload_preserved":true,"readonly_author_lane_refused":true,
        "readonly_current_values_are_observations_only":true,"processed_pcm":false,"native_bypass_tested":false,
        "nondefault_state_discrimination":false,"private_dsp_state_qualified":false,"project_archive_qualified":false,
        "author_callback_or_undo_qualified":false,"gui_qualified":false,"performance_qualified":false,
        "realtime_qualified":false,"graph_preview_export_qualified":false,"full_compatibility_qualified":false,
        "outer_job_qualified_by_test":false,
    })).expect("metadata gate result"));
    // All exact immutable inputs remain held through nested discovery children.
    drop((case_lease, helper, plugin, list, probe, opaque));
}

// Exact factual output of the accepted external two-round Probe. This is an
// ordinary test oracle only; no native identity or metadata is dispatched by name.
// Source: ring-out-probe-native-r1/round-1-clap-probe/response.json (source26/App8).
const ACTUAL_PROBE_JSON: &str = r###"{"descriptors":[],"probe":{"plugin_id":"com.duskaudio.ring-out","latency_frames":0,"offline":true,"tail":"None","parameters":[{"id":0,"name":"Sense","min_value":0.0,"max_value":1.0,"default_value":0.0,"current_value":0.0,"automatable":true,"stepped":true,"enumeration":false,"read_only":false,"hidden":false},{"id":1,"name":"Setup","min_value":0.0,"max_value":1.0,"default_value":0.0,"current_value":0.0,"automatable":true,"stepped":true,"enumeration":false,"read_only":false,"hidden":false},{"id":2,"name":"Add","min_value":0.0,"max_value":1.0,"default_value":0.0,"current_value":0.0,"automatable":true,"stepped":true,"enumeration":false,"read_only":false,"hidden":false},{"id":3,"name":"Reset","min_value":0.0,"max_value":1.0,"default_value":0.0,"current_value":0.0,"automatable":true,"stepped":true,"enumeration":false,"read_only":false,"hidden":false},{"id":4,"name":"Global Q","min_value":0.20000000298023224,"max_value":10.0,"default_value":1.0,"current_value":1.0,"automatable":true,"stepped":false,"enumeration":false,"read_only":false,"hidden":false},{"id":5,"name":"Global Amp","min_value":-24.0,"max_value":24.0,"default_value":0.0,"current_value":0.0,"automatable":true,"stepped":false,"enumeration":false,"read_only":false,"hidden":false},{"id":6,"name":"Link","min_value":0.0,"max_value":1.0,"default_value":0.0,"current_value":0.0,"automatable":true,"stepped":true,"enumeration":false,"read_only":false,"hidden":false},{"id":7,"name":"Gain Out","min_value":-24.0,"max_value":24.0,"default_value":0.0,"current_value":0.0,"automatable":true,"stepped":false,"enumeration":false,"read_only":false,"hidden":false},{"id":8,"name":"Bypass","min_value":0.0,"max_value":1.0,"default_value":0.0,"current_value":0.0,"automatable":true,"stepped":true,"enumeration":false,"read_only":false,"hidden":false},{"id":9,"name":"Input Level","min_value":-120.0,"max_value":6.0,"default_value":-120.0,"current_value":-120.0,"automatable":false,"stepped":false,"enumeration":false,"read_only":true,"hidden":false},{"id":10,"name":"Output Level","min_value":-120.0,"max_value":6.0,"default_value":-120.0,"current_value":-120.0,"automatable":false,"stepped":false,"enumeration":false,"read_only":true,"hidden":false}],"auxiliary_buses":[],"captured_state":[95,95,100,97,102,95,112,114,111,103,114,97,109,95,95,0,48,0,95,95,100,97,102,95,115,116,97,116,101,95,98,101,103,105,110,95,95,0,101,100,105,116,0,0,102,105,108,116,101,114,115,0,0,95,95,100,97,102,95,115,116,97,116,101,95,101,110,100,95,95,0,95,95,100,97,102,95,112,97,114,97,109,101,116,101,114,115,95,98,101,103,105,110,95,95,0,115,101,110,115,101,0,48,0,103,108,111,98,97,108,95,113,0,49,0,103,108,111,98,97,108,95,97,109,112,0,48,0,108,105,110,107,0,48,0,103,97,105,110,95,111,117,116,0,48,0,100,97,102,95,98,121,112,97,115,115,0,48,0,95,95,100,97,102,95,112,97,114,97,109,101,116,101,114,115,95,101,110,100,95,95,0,254,0]}}"###;
const ACTUAL_PROBE_SHA256: &str =
    "ff825debe4a0f32249c4aff68e2e3e8f20e490accc236698b14ab15f798981c9";

fn owned_metadata() -> CapturedProbe {
    assert_eq!(
        format!("{:x}", Sha256::digest(ACTUAL_PROBE_JSON.as_bytes())),
        ACTUAL_PROBE_SHA256
    );
    metadata_from_probe(&strict_json(ACTUAL_PROBE_JSON.as_bytes()))
}

// Independent expected author value used solely by refusal controls. Actual
// product construction is exercised only through public catalog in the native gate.
fn owned_author(metadata: &CapturedProbe) -> AudioProcessorInstance {
    let mut parameters = mondrian_core::AuthoringMap::new();
    for descriptor in metadata.parameters.iter().filter(|descriptor| !descriptor.read_only) {
        let id = descriptor.parameter_id().expect("owned stable ID");
        let mut parameter = AudioProcessorParameter::from_schema(
            descriptor.authoring_schema().expect("owned schema"),
        )
        .expect("owned numeric parameter");
        parameter.ui = mondrian_timeline::audio::AudioProcessorParameterUiMetadata {
            display_name: Some(descriptor.name.clone()),
            hidden: descriptor.hidden,
        };
        parameter
            .set_automation(
                ExactAutomationCurve::new(id.clone(), descriptor.current_value)
                    .expect("owned current value"),
            )
            .expect("owned value admission");
        parameters.insert(id, parameter);
    }
    AudioProcessorInstance {
        id: mondrian_core::AudioProcessorInstanceId::new(),
        definition: AudioProcessorDefinitionRef::Clap {
            plugin_id: metadata.plugin_id.clone(),
            schema_version: 1,
            binary_sha256: Some([3; 32]),
        },
        bypassed: false,
        parameters,
        opaque_state: metadata.captured_state.clone().map(Into::into),
    }
}

fn owned_resolver(metadata: &CapturedProbe) -> ClapAudioProcessorSpecResolver {
    let path = std::env::current_exe().expect("test executable absolute path");
    ClapAudioProcessorSpecResolver::new(
        path.clone(),
        [ClapPluginRegistration {
            plugin_id: metadata.plugin_id.clone(),
            library_path: path,
            binary_sha256: [3; 32],
            execution_contract: AudioProcessorExecutionContract::new(
                metadata.latency_frames as usize,
                metadata.tail.contract(),
                true,
                true,
                true,
                0,
            )
            .expect("owned explicit state-entry contract"),
            auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
            parameters: metadata.parameters.clone(),
        }],
    )
    .expect("ordinary resolver, no native module loaded")
}

#[test]
fn captured_readonly_metadata_controls_keep_author_and_observation_sets_distinct() {
    let metadata = owned_metadata();
    assert_eq!(metadata.parameters.len(), 11);
    assert_eq!(
        metadata
            .parameters
            .iter()
            .filter(|d| d.read_only)
            .map(|d| d.id)
            .collect::<Vec<_>>(),
        [9, 10]
    );
    let author = owned_author(&metadata);
    assert_eq!(author.parameters.len(), 9);
    assert_eq!(
        author.parameters.keys().cloned().collect::<BTreeSet<_>>(),
        (0..9)
            .map(|id| ParameterId::new(format!("clap.param.{id}")).expect("expected key"))
            .collect()
    );
    assert!(author_projection_matches(&metadata.parameters, &author));
    let reopened: AudioProcessorInstance =
        serde_json::from_slice(&serde_json::to_vec(&author).expect("serde author"))
            .expect("reopen author");
    assert_eq!(reopened, author);
    let state = author.opaque_state.as_ref().expect("captured default opaque bytes");
    assert_eq!(state.len(), 184);
    assert_eq!(
        format!("{:x}", Sha256::digest(state.as_ref())),
        "1cc8e19194558d2b72358549fdf9516fb41631a58b1d784eb95e70036095b7c1"
    );
    let spec = owned_resolver(&metadata)
        .resolve(request(&author))
        .expect("nine authored lanes admitted");
    assert_spec_projection(&spec, &metadata.parameters);
    let mut changed_reading = metadata.parameters.clone();
    changed_reading[9].current_value = -40.0;
    assert!(same_metadata_structure(
        &changed_reading,
        &metadata.parameters
    ));
    assert!(author_projection_matches(&changed_reading, &author));
    changed_reading[9].read_only = false;
    assert!(!same_metadata_structure(
        &changed_reading,
        &metadata.parameters
    ));
    assert!(!author_projection_matches(&changed_reading, &author));
    let mut incomplete_payload = spec.clone();
    let mut payload: Value =
        serde_json::from_slice(&spec.preparation_payload).expect("full payload");
    payload["parameters"].as_array_mut().expect("all descriptors").pop();
    incomplete_payload.preparation_payload =
        serde_json::to_vec(&payload).expect("synthetic invalid payload");
    assert!(
        std::panic::catch_unwind(|| assert_spec_projection(
            &incomplete_payload,
            &metadata.parameters
        ))
        .is_err(),
        "missing observation metadata must fail the Lab oracle"
    );
}

#[test]
fn readonly_author_and_unknown_schema_forgeries_are_refused_without_native_loading() {
    let metadata = owned_metadata();
    let author = owned_author(&metadata);
    let resolver = owned_resolver(&metadata);
    for descriptor in metadata.parameters.iter().filter(|descriptor| descriptor.read_only) {
        let id = descriptor.parameter_id().expect("RO ID");
        let mut attempted = author.clone();
        assert!(matches!(
            attempted.set_parameter_automation(
                ExactAutomationCurve::new(id.clone(), descriptor.current_value)
                    .expect("test value")
            ),
            Err(mondrian_timeline::audio::AudioAuthoringError::UnknownProcessorParameter)
        ));
        assert_eq!(attempted, author);
        attempted.parameters.insert(
            id,
            AudioProcessorParameter::from_schema(descriptor.authoring_schema().expect("RO schema"))
                .expect("forged schema"),
        );
        assert!(
            matches!(resolver.resolve(request(&attempted)),Err(AudioProcessorHostError::InvalidContract(message)) if message.contains("authored CLAP parameter contract"))
        );
        assert!(!author_projection_matches(&metadata.parameters, &attempted));
    }
    let mut unknown = author.clone();
    let unknown_id = ParameterId::new("clap.param.2920836328").expect("unsigned above i32max");
    unknown.parameters.insert(
        unknown_id.clone(),
        AudioProcessorParameter::from_schema(mondrian_core::ParameterSchema::v1(
            unknown_id,
            mondrian_core::PropertyValue::Double(0.0),
        ))
        .expect("numeric unknown schema"),
    );
    assert!(
        matches!(resolver.resolve(request(&unknown)),Err(AudioProcessorHostError::InvalidContract(message)) if message.contains("absent"))
    );
    let mut changed = author.clone();
    changed
        .parameters
        .get_mut(&metadata.parameters[0].parameter_id().expect("owned ID"))
        .expect("editable parameter")
        .schema
        .is_animatable = false;
    assert!(
        matches!(resolver.resolve(request(&changed)),Err(AudioProcessorHostError::InvalidContract(message)) if message.contains("contract"))
    );
    let mut wrong_binary = author.clone();
    if let AudioProcessorDefinitionRef::Clap { binary_sha256, .. } = &mut wrong_binary.definition {
        *binary_sha256 = Some([4; 32]);
    }
    assert!(
        matches!(resolver.resolve(request(&wrong_binary)),Err(AudioProcessorHostError::Unavailable(message)) if message.contains("binary revision"))
    );
    assert_eq!(author.parameters.len(), 9);
    assert_eq!(
        author.opaque_state.as_ref().map(AsRef::as_ref),
        metadata.captured_state.as_deref()
    );
}

#[test]
fn readonly_worker_lane_refusal_precedes_library_access() {
    let metadata = owned_metadata();
    let factory = ClapAudioProcessorWorkerFactory;
    let directory = tempfile::tempdir().expect("owned nonexistent-library directory");
    for descriptor in metadata.parameters.iter().filter(|d| d.read_only) {
        // The nonexistent module ensures a specific RO refusal cannot be
        // mistaken for a later file/library loading failure.
        let library = directory.path().join("nonexistent-owned-readonly-control.clap");
        assert!(!library.exists());
        let payload=serde_json::to_vec(&json!({
            "library_path":library,"plugin_id":metadata.plugin_id,"binary_sha256":vec![3_u8;32],
            "state":metadata.captured_state,"parameters":metadata.parameters,"parameter_ids":[descriptor.id],
        })).expect("forged native lane payload");
        let prepared = IsolatedAudioProcessorWorkerPrepareRequest {
            native_lane: factory
                .native_worker_lane_resources()
                .expect("actual adapter lane profile"),
            payload,
            plugin_execution_contract: AudioProcessorExecutionContract::new(
                0,
                AudioProcessorTail::None,
                true,
                true,
                true,
                0,
            )
            .expect("explicit state-entry contract"),
            auxiliary_inputs: AudioProcessorAuxiliaryInputContract::default(),
            parameter_ids: vec![descriptor.parameter_id().expect("RO canonical ID")],
            render_contract: render(),
        };
        assert!(
            matches!(factory.prepare(prepared),Err(AudioProcessorHostError::InvalidContract(message)) if message == "CLAP parameter lane identity is invalid")
        );
    }
}

#[test]
fn metadata_case_oracle_refuses_duplicate_unknown_and_nonfinite_input() {
    assert!(std::panic::catch_unwind(|| strict_json(br#"{"probe":null,"probe":null}"#)).is_err());
    assert!(std::panic::catch_unwind(|| strict_json(br#"{"probe":NaN}"#)).is_err());
    let source = strict_json(ACTUAL_PROBE_JSON.as_bytes());
    let mut unknown = source.clone();
    unknown["probe"]["parameters"][0]["unknown"] = json!(true);
    assert!(std::panic::catch_unwind(|| metadata_from_probe(&unknown)).is_err());
    let mut duplicate = source.clone();
    duplicate["probe"]["parameters"][1]["id"] = duplicate["probe"]["parameters"][0]["id"].clone();
    assert!(std::panic::catch_unwind(|| metadata_from_probe(&duplicate)).is_err());
    let mut unknown_root = source;
    unknown_root["unknown"] = json!(0);
    assert!(std::panic::catch_unwind(|| metadata_from_probe(&unknown_root)).is_err());
}
