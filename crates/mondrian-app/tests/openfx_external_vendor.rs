//! Test-only external CPU Float32/RGBA Filter qualification. No vendor dispatch.
//! Run only beneath the frozen-artifact Job/controller; ordinary UI/release
//! startup, GPU, HDR, state, temporal fetch and performance are outside this gate.
#![cfg(windows)]

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mondrian_app::openfx_adapter::{inspect_openfx_binary_with_checkpoint, OpenFxDiscoveryError};
use mondrian_app::openfx_effect::register_selected_openfx_filter;
use mondrian_app::openfx_host::{
    describe_openfx_filter_with_checkpoint, OpenFxDoubleType, OpenFxFilterDescription,
    OpenFxFilterSession, OpenFxHostError, OpenFxParameterBinding, OpenFxParameterValue,
    OpenFxRenderTiming,
};
use mondrian_core::automation::PropertyValue;
use mondrian_core::{
    ColorFrameAlpha, Rational, SampleAspectRatio, TimelineTime, TimelineTimeRange,
    WorkingColorSpace,
};
use mondrian_effects::{
    instantiate_effect_node, EffectColorDomain, EffectExecutionSession,
    EffectExecutionSessionConfig, EffectFloatFrame, EffectFrameContext, EffectFrameExtent,
    EffectProcessorControl, EffectProcessorObservation, EffectProcessorStopReason,
    EffectValueFormat, EffectWorkingPrecision, LutPreparationCache, PreparedEffectProgram,
};
use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject};

type LabResult<T> = Result<T, String>;
const CASE_LIMIT: u64 = 64 * 1024;
const JSON_LIMIT: u64 = 512 * 1024;
const ORACLE_SHA: &str = "428bb60fe78d32cb28d38fedfed95ab6ae576e56a553210c3af8fccf5ef4ebe1";
const INPUT_SHA: &str = "b48cb7d6b53629b16c6ca56b052708eceace734ea4df24ff09ed4d6f10a2a3af";
const POSTER_SHA: &str = "121f005a1e140e08107da4016769a9cf6cb382788d19ee6effe39fe4b913024e";
const OUTER_BUDGET: Duration = Duration::from_secs(60);

fn require(ok: bool, reason: impl Into<String>) -> LabResult<()> {
    if ok {
        Ok(())
    } else {
        Err(reason.into())
    }
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn valid_sha(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

// serde_json::Value otherwise silently replaces duplicate object keys. This
// visitor rejects them recursively before the deny_unknown_fields structs.
struct Unique(Value);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Unique;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("unique-key JSON")
            }
            fn visit_bool<E: serde::de::Error>(self, x: bool) -> Result<Unique, E> {
                Ok(Unique(x.into()))
            }
            fn visit_i64<E: serde::de::Error>(self, x: i64) -> Result<Unique, E> {
                Ok(Unique(x.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, x: u64) -> Result<Unique, E> {
                Ok(Unique(x.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, x: f64) -> Result<Unique, E> {
                serde_json::Number::from_f64(x)
                    .map(|n| Unique(n.into()))
                    .ok_or_else(|| E::custom("nonfinite JSON number"))
            }
            fn visit_str<E: serde::de::Error>(self, x: &str) -> Result<Unique, E> {
                Ok(Unique(x.into()))
            }
            fn visit_string<E: serde::de::Error>(self, x: String) -> Result<Unique, E> {
                Ok(Unique(x.into()))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_none<E: serde::de::Error>(self) -> Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Unique, A::Error> {
                let mut v = Vec::new();
                while let Some(Unique(x)) = a.next_element()? {
                    v.push(x);
                }
                Ok(Unique(Value::Array(v)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Unique, A::Error> {
                let mut v = serde_json::Map::new();
                while let Some(k) = a.next_key::<String>()? {
                    if v.contains_key(&k) {
                        return Err(serde::de::Error::custom(format!("duplicate key {k}")));
                    }
                    let Unique(x) = a.next_value()?;
                    v.insert(k, x);
                }
                Ok(Unique(Value::Object(v)))
            }
        }
        d.deserialize_any(V)
    }
}
fn strict_json(bytes: &[u8], maximum: u64) -> LabResult<Value> {
    require(
        bytes.len() as u64 <= maximum,
        "JSON exceeds admission limit",
    )?;
    serde_json::from_slice::<Unique>(bytes).map(|u| u.0).map_err(|e| e.to_string())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    path: PathBuf,
    sha256: String,
    bytes: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum Numeric {
    Integer(i32),
    Double(f64),
}
impl Numeric {
    fn native(&self) -> OpenFxParameterValue {
        match *self {
            Self::Integer(x) => OpenFxParameterValue::Integer(x),
            Self::Double(x) => OpenFxParameterValue::Double(x),
        }
    }
    fn scalar(&self) -> f64 {
        match *self {
            Self::Integer(x) => f64::from(x),
            Self::Double(x) => x,
        }
    }
    fn property(&self) -> PropertyValue {
        match *self {
            Self::Integer(x) => PropertyValue::Int(i64::from(x)),
            Self::Double(x) => PropertyValue::Double(x),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Contract {
    role: String,
    native_name: String,
    label: String,
    default: Numeric,
    minimum: f64,
    maximum: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    schema_version: u32,
    kind: String,
    round: u32,
    expected_source_identity: String,
    expected_source_version_major: u32,
    expected_source_version_minor: u32,
    helper: Binding,
    module: Binding,
    oracle: Binding,
    input: Binding,
    neutral: Binding,
    posterized: Binding,
    source_freeze: Binding,
    app_build_jsonl: Binding,
    producer_evidence: Binding,
    provenance: Vec<Binding>,
    parameters: Vec<Contract>,
    evidence_directory: PathBuf,
}
impl Case {
    fn bindings(&self) -> Vec<(&Binding, u64)> {
        let mut v = vec![
            (&self.helper, 1024 * 1024 * 1024),
            (&self.module, 32 * 1024 * 1024),
            (&self.oracle, JSON_LIMIT),
            (&self.input, 240),
            (&self.neutral, 240),
            (&self.posterized, 240),
            (&self.source_freeze, 16 * 1024 * 1024),
            (&self.app_build_jsonl, 16 * 1024 * 1024),
            (&self.producer_evidence, JSON_LIMIT),
        ];
        v.extend(self.provenance.iter().map(|b| (b, JSON_LIMIT)));
        v
    }
    fn validate(&self) -> LabResult<()> {
        require(
            self.schema_version == 1 && self.kind == "external_ofx_float_filter_case_v1",
            "case version/kind",
        )?;
        require((1..=2).contains(&self.round), "round must be1 or2")?;
        require(
            !self.expected_source_identity.is_empty() && self.expected_source_identity.len() <= 256,
            "source identity",
        )?;
        require(
            self.evidence_directory.is_absolute()
                && !self
                    .evidence_directory
                    .components()
                    .any(|x| matches!(x, std::path::Component::ParentDir)),
            "evidence directory must be absolute without traversal",
        )?;
        require(
            self.provenance.len() == 5,
            "exact five source/license/extraction provenance bindings",
        )?;
        for (b, cap) in self.bindings() {
            require(
                b.path.is_absolute() && valid_sha(&b.sha256) && b.bytes > 0 && b.bytes <= cap,
                "invalid binding",
            )?;
            require(
                !b.path.components().any(|x| matches!(x, std::path::Component::ParentDir)),
                "parent traversal",
            )?;
        }
        require(
            self.oracle.sha256 == ORACLE_SHA
                && self.input.sha256 == INPUT_SHA
                && self.neutral.sha256 == INPUT_SHA
                && self.posterized.sha256 == POSTER_SHA,
            "frozen oracle identity",
        )?;
        require(
            self.input.bytes == 240 && self.neutral.bytes == 240 && self.posterized.bytes == 240,
            "frozen raster size",
        )?;
        require(
            self.parameters.len() == 6,
            "six numeric parameter contracts required",
        )?;
        let roles = ["bits", "xor", "hold", "seed", "prob", "mix"];
        for (p, role) in self.parameters.iter().zip(roles) {
            require(
                p.role == role && !p.native_name.is_empty() && !p.label.is_empty(),
                "ordered role contract",
            )?;
            require(
                p.minimum.is_finite()
                    && p.maximum.is_finite()
                    && p.minimum <= p.maximum
                    && p.default.scalar().is_finite()
                    && p.default.scalar() >= p.minimum
                    && p.default.scalar() <= p.maximum,
                "numeric bounds/default",
            )?;
            require(
                matches!(
                    (&p.default, role),
                    (Numeric::Integer(_), "bits" | "xor" | "hold" | "seed")
                        | (Numeric::Double(_), "prob" | "mix")
                ),
                "role numeric kind",
            )?;
        }
        require(
            self.parameters
                .iter()
                .map(|p| &p.native_name)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == 6,
            "duplicate native parameter name",
        )
    }
}
fn parse_case(bytes: &[u8]) -> LabResult<Case> {
    let case: Case =
        serde_json::from_value(strict_json(bytes, CASE_LIMIT)?).map_err(|e| e.to_string())?;
    case.validate()?;
    Ok(case)
}

// Length is admitted on the same already-owned read-only handle BEFORE hashing.
// FILE_SHARE_READ refuses new writers/deleters for the duration of qualification.
struct Lease {
    file: File,
    binding: Binding,
}
impl Lease {
    fn open(binding: &Binding, maximum: u64) -> LabResult<Self> {
        require(
            binding.path.is_absolute() && valid_sha(&binding.sha256),
            "invalid lease binding",
        )?;
        // Leaf and every existing parent must not be a Windows reparse point.
        for path in binding.path.ancestors() {
            let m = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
            require(m.file_attributes() & 0x400 == 0, "reparse-point input")?;
        }
        let file = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&binding.path)
            .map_err(|e| e.to_string())?;
        let m = file.metadata().map_err(|e| e.to_string())?;
        require(
            m.is_file() && m.len() <= maximum && m.len() == binding.bytes,
            "bounded same-handle length admission",
        )?;
        let mut lease = Self { file, binding: binding.clone() };
        lease.check()?;
        Ok(lease)
    }
    fn check(&mut self) -> LabResult<()> {
        require(
            self.file.metadata().map_err(|e| e.to_string())?.len() == self.binding.bytes,
            "lease length changed",
        )?;
        self.file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let mut hash = Sha256::new();
        let mut remaining = self.binding.bytes;
        let mut buffer = [0u8; 65536];
        while remaining > 0 {
            let n =
                usize::try_from(remaining.min(buffer.len() as u64)).map_err(|e| e.to_string())?;
            self.file.read_exact(&mut buffer[..n]).map_err(|e| e.to_string())?;
            hash.update(&buffer[..n]);
            remaining -= n as u64;
        }
        require(
            format!("{:x}", hash.finalize()) == self.binding.sha256,
            "lease SHA mismatch",
        )
    }
    fn bytes(&mut self, maximum: u64) -> LabResult<Vec<u8>> {
        require(self.binding.bytes <= maximum, "read exceeds size admission")?;
        self.file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let mut bytes = vec![0; usize::try_from(self.binding.bytes).map_err(|e| e.to_string())?];
        self.file.read_exact(&mut bytes).map_err(|e| e.to_string())?;
        require(digest(&bytes) == self.binding.sha256, "read SHA mismatch")?;
        Ok(bytes)
    }
}
fn write_new(path: &Path, bytes: &[u8]) -> LabResult<Binding> {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    f.write_all(bytes).and_then(|()| f.sync_all()).map_err(|e| e.to_string())?;
    Ok(Binding {
        path: path.to_owned(),
        bytes: bytes.len() as u64,
        sha256: digest(bytes),
    })
}
fn write_json(directory: &Path, name: &str, value: &impl Serialize) -> LabResult<Binding> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    require(bytes.len() as u64 <= JSON_LIMIT, "evidence exceeds512KiB")?;
    write_new(&directory.join(name), &bytes)
}

fn parser_controls(case: &Case) -> LabResult<Vec<Value>> {
    let valid = serde_json::to_value(case).map_err(|e| e.to_string())?;
    let mut controls = Vec::new();
    let mut mutations = Vec::new();
    let mut v = valid.clone();
    v["unknown"] = json!(1);
    mutations.push(("unknown_field", v));
    let mut v = valid.clone();
    v["helper"]["unknown"] = json!(1);
    mutations.push(("unknown_nested_binding", v));
    let mut v = valid.clone();
    v["helper"]["sha256"] = json!("bad");
    mutations.push(("invalid_hash", v));
    let mut v = valid.clone();
    v["module"]["path"] = json!("relative.ofx");
    mutations.push(("relative_path", v));
    let mut v = valid.clone();
    v["input"]["bytes"] = json!(241);
    mutations.push(("raster_over_cap", v));
    let mut v = valid.clone();
    v["round"] = json!(3);
    mutations.push(("invalid_round", v));
    let mut v = valid.clone();
    v["parameters"][0]["default"] = json!({"kind":"double","value":5.0});
    mutations.push(("wrong_role_type", v));
    let mut v = valid.clone();
    v["parameters"][0]["minimum"] = json!(9.0);
    mutations.push(("out_of_bounds_default", v));
    for (name, value) in mutations {
        let bytes = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
        require(
            parse_case(&bytes).is_err(),
            format!("parser accepted negative control {name}"),
        )?;
        controls.push(json!({"control":name,"rejected":true,"nativeInvoked":false}));
    }
    for (name, bytes) in [
        (
            "duplicate_top",
            br#"{"schema_version":1,"schema_version":1}"#.as_slice(),
        ),
        ("duplicate_nested", br#"{"a":{"x":1,"x":2}}"#.as_slice()),
        ("nonfinite", br#"{"x":NaN}"#.as_slice()),
        ("nonfinite_overflow", br#"{"x":1e9999}"#.as_slice()),
    ] {
        require(
            strict_json(bytes, CASE_LIMIT).is_err(),
            format!("accepted {name}"),
        )?;
        controls.push(json!({"control":name,"rejected":true,"nativeInvoked":false}));
    }
    require(
        parse_case(&vec![b' '; CASE_LIMIT as usize + 1]).is_err(),
        "accepted oversized JSON",
    )?;
    controls.push(json!({"control":"oversized_json","rejected":true,"nativeInvoked":false}));
    Ok(controls)
}

fn value_format() -> EffectValueFormat {
    EffectValueFormat::new(
        EffectWorkingPrecision::Float32,
        EffectColorDomain::SceneLinearRgb,
        ColorFrameAlpha::StraightFloat,
    )
}
fn frame_from_bytes(bytes: &[u8]) -> LabResult<EffectFloatFrame> {
    require(bytes.len() == 240, "raster length")?;
    let pixels = bytes
        .chunks_exact(16)
        .map(|pixel| {
            let mut lanes = [0.0; 4];
            for (lane, raw) in lanes.iter_mut().zip(pixel.chunks_exact(4)) {
                *lane = f32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
            }
            lanes
        })
        .collect();
    EffectFloatFrame::from_pixels(
        EffectFrameExtent::new(5, 3),
        value_format(),
        mondrian_core::ImageSampling::UNIT_SQUARE,
        pixels,
    )
    .map_err(|e| e.to_string())
}
fn output(
    directory: &Path,
    name: &str,
    frame: &EffectFloatFrame,
    expected: &[u8],
) -> LabResult<Binding> {
    require(
        frame.width() == 5 && frame.height() == 3 && frame.format() == value_format(),
        "output extent/complete Float association",
    )?;
    let bytes: Vec<u8> = frame
        .pixels()
        .iter()
        .flatten()
        .flat_map(|x| x.to_bits().to_le_bytes())
        .collect();
    let binding = write_new(&directory.join(format!("{name}.rgba32f")), &bytes)?;
    // Preserve actual output BEFORE rejecting; compare every bit, including zero sign.
    if bytes != expected {
        let mismatches: Vec<_> = bytes
            .chunks_exact(4)
            .zip(expected.chunks_exact(4))
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(lane, (a, b))| json!({"lane":lane,"actualLE":a,"expectedLE":b}))
            .collect();
        let primary = format!("exact-bit oracle mismatch in {name}");
        if let Err(publication) =
            write_json(directory, &format!("{name}-mismatch.json"), &mismatches)
        {
            return Err(format!(
                "{primary}; mismatch publication also failed: {publication}"
            ));
        }
        return Err(primary);
    }
    Ok(binding)
}
fn values(case: &Case, bits: i32, mix: f64) -> LabResult<BTreeMap<String, OpenFxParameterBinding>> {
    let values = [
        Numeric::Integer(bits),
        Numeric::Integer(0),
        Numeric::Integer(4),
        Numeric::Integer(1),
        Numeric::Double(0.0),
        Numeric::Double(mix),
    ];
    let mut result = BTreeMap::new();
    for (p, value) in case.parameters.iter().zip(values) {
        require(
            value.scalar() >= p.minimum && value.scalar() <= p.maximum,
            "authored value outside actual contract",
        )?;
        result.insert(
            p.native_name.clone(),
            OpenFxParameterBinding::constant(value.native()),
        );
    }
    Ok(result)
}
fn check_description(
    case: &Case,
    actual: &OpenFxFilterDescription,
    identifier: &str,
) -> LabResult<()> {
    require(
        actual.identifier == identifier && actual.parameters.len() == case.parameters.len(),
        "actual Describe identity/count",
    )?;
    for p in &case.parameters {
        let matches: Vec<_> =
            actual.parameters.iter().filter(|a| a.name == p.native_name).collect();
        require(matches.len() == 1, "native name missing/ambiguous")?;
        let a = matches[0];
        require(
            a.label == p.label
                && a.default_value == p.default.native()
                && a.numeric_channels.len() == 1,
            format!("native schema/default mismatch: {}", p.native_name),
        )?;
        let n = &a.numeric_channels[0];
        require(
            n.minimum == p.minimum
                && n.maximum == p.maximum
                && n.display_minimum == p.minimum
                && n.display_maximum == p.maximum
                && a.can_animate
                && !a.secret
                && a.enabled,
            format!(
                "native numeric bounds/editability mismatch: {}",
                p.native_name
            ),
        )?;
        let expected_double_type = match p.default {
            Numeric::Integer(_) => None,
            Numeric::Double(_) => Some(OpenFxDoubleType::Plain),
        };
        require(
            a.double_type == expected_double_type
                && a.string_mode.is_none()
                && a.choices.is_empty(),
            format!(
                "native numeric interpretation/choice/string metadata mismatch: {}",
                p.native_name
            ),
        )?;
        // Hint and preferred interpolation are observed public fields, not
        // pinned SDK-derived expected values. Complete Describe equality below
        // detects their later drift; this does not claim an initial golden match.
    }
    Ok(())
}

// After an actual render response, acquire and retain a process object while
// alive. Public API supplies a PID, not Child's original kernel handle: there
// remains a narrow PID->OpenProcess acquisition race. Subsequent exit checks
// concern this retained object, not a fresh PID lookup; outer Job still required.
// Zero-time post-checks add no retirement/deadline allowance.
struct ProcessHandle(HANDLE);
impl ProcessHandle {
    fn open(pid: u32) -> LabResult<Self> {
        // SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, no mutation rights.
        // SAFETY: pid is the public resident worker PID obtained after a
        // successful render response; only query/synchronize access is requested.
        // The returned noninheritable handle is checked for null and owned here.
        // This does not eliminate the disclosed PID->OpenProcess acquisition race.
        let h = unsafe { OpenProcess(0x0010_1000, 0, pid) };
        require(
            !h.is_null(),
            format!(
                "OpenProcess({pid}) failed: {}",
                std::io::Error::last_os_error()
            ),
        )?;
        let handle = Self(h);
        // SAFETY: h is the owned nonnull process handle with SYNCHRONIZE rights;
        // no other owner closes it or transfers it while this zero-time poll runs.
        require(
            unsafe { WaitForSingleObject(h, 0) } == 258,
            "worker not alive at handle capture",
        )?;
        Ok(handle)
    }
    fn exited(&self) -> LabResult<()> {
        // SAFETY: self retains the same nonnull process object with SYNCHRONIZE
        // rights, not a fresh PID lookup. Borrowing prevents this owner closing
        // or moving it during the zero-time poll; no handle is transferred.
        require(
            unsafe { WaitForSingleObject(self.0, 0) } == 0,
            "retained native process handle not signaled",
        )
    }
    fn close(mut self) -> LabResult<()> {
        // SAFETY: this consumes the sole owned valid nonnull handle, with no
        // outstanding wait or borrower. Clear ownership only after success;
        // failure remains RED and leaves the handle for best-effort Drop retry.
        require(unsafe { CloseHandle(self.0) } != 0, "CloseHandle failed")?;
        self.0 = std::ptr::null_mut();
        Ok(())
    }
}
impl Drop for ProcessHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: a nonnull handle is still solely owned here and no wait
            // borrows it; successful explicit close cleared it. Drop is a
            // best-effort last release, not evidence of guaranteed close success.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

fn execute(case: &Case, leases: &mut [Lease], deadline: Instant) -> LabResult<()> {
    let directory = &case.evidence_directory;
    let mut checkpoint = || {
        if Instant::now() >= deadline {
            Err(OpenFxHostError::DeadlineExceeded)
        } else {
            Ok(())
        }
    };
    let mut discovery_checkpoint = || {
        if Instant::now() >= deadline {
            Err(OpenFxDiscoveryError::DeadlineExceeded)
        } else {
            Ok(())
        }
    };
    let inspection = inspect_openfx_binary_with_checkpoint(
        &case.helper.path,
        &case.module.path,
        &mut discovery_checkpoint,
    )
    .map_err(|e| e.to_string())?;
    let inspection_record = write_json(
        directory,
        "actual-inspection.json",
        &json!({
            "binary_path":inspection.binary_path,"binary_sha256":inspection.binary_sha256,"plugins":inspection.plugins
        }),
    )?;
    require(
        inspection.binary_sha256 == case.module.sha256 && inspection.plugins.len() == 1,
        "actual inspection module SHA/sole class",
    )?;
    let selected = &inspection.plugins[0];
    require(
        selected.api_version == 1 && selected.identifier == case.expected_source_identity,
        "actual identifier differs source expectation; stop dependent render",
    )?;
    require(
        selected.version_major == case.expected_source_version_major
            && selected.version_minor == case.expected_source_version_minor,
        "actual plugin version differs pinned source factory expectation",
    )?;
    let identifier = &selected.identifier; // The actual native response is authority.
    let description = describe_openfx_filter_with_checkpoint(
        &case.helper.path,
        &inspection,
        identifier,
        &mut checkpoint,
    )
    .map_err(|e| e.to_string())?;
    let description_record = write_json(directory, "actual-describe.json", &description)?;
    check_description(case, &description, identifier)?;
    write_json(
        directory,
        "native-admission.json",
        &json!({"inspection":inspection_record,
        "description":description_record,"actualSelected":selected,"scope":"CPU Float32 RGBA Filter; first actual metadata captured here"}),
    )?;
    let input = frame_from_bytes(&leases[3].bytes(240)?)?;
    let neutral = leases[4].bytes(240)?;
    let posterized = leases[5].bytes(240)?;
    let steps = [
        ("neutral_bits8_mix1", 0, 8, 1.0, neutral.as_slice()),
        ("posterized_bits4_mix1", 4, 4, 1.0, posterized.as_slice()),
        ("neutral_mix0", 0, 4, 0.0, neutral.as_slice()),
        ("posterized_repeat", 4, 4, 1.0, posterized.as_slice()),
    ];
    let mut outputs = Vec::new();
    let mut resident = OpenFxFilterSession::prepare(
        &case.helper.path,
        &inspection,
        identifier,
        5,
        3,
        value_format(),
        mondrian_effects::EffectProjectGeometry::new(
            5,
            3,
            mondrian_core::SampleAspectRatio::SQUARE,
        )
        .expect("explicit owner project geometry"),
        &mut checkpoint,
    )
    .map_err(|e| e.to_string())?;
    let pid = resident.worker_process_id();
    let mut process = None;
    for (index, (key, frame, bits, mix, expected)) in steps.iter().enumerate() {
        let timing = OpenFxRenderTiming {
            frame: f64::from(*frame),
            frame_rate: 24.0,
            first_frame: 0.0,
            last_frame: 4.0,
        };
        let parameters = values(case, *bits, *mix)?;
        write_json(
            directory,
            &format!("direct-{key}-request.json"),
            &json!({"timing":timing,"parameters":parameters,"description":description_record}),
        )?;
        let actual = resident
            .render(&timing, &parameters, &input, &mut checkpoint)
            .map_err(|e| e.to_string())?;
        require(
            resident.worker_process_id() == pid,
            "resident PID changed across frames",
        )?;
        if index == 0 {
            process = Some(ProcessHandle::open(pid)?);
            write_json(
                directory,
                "direct-live-owner.json",
                &json!({"pid":pid,"retainedProcessHandle":true,"aliveAtCapture":true,
                "capturedAfterSuccessfulRender":true,"originalChildKernelHandleExposed":false,"pidToOpenProcessAcquisitionRaceRemains":true}),
            )?;
        }
        outputs.push(output(
            directory,
            &format!("direct-{key}"),
            &actual,
            expected,
        )?);
    }
    resident
        .shutdown()
        .map_err(|e| format!("mandatory explicit shutdown failed: {e}"))?;
    let process =
        process.ok_or_else(|| "no retained process handle after actual render".to_owned())?;
    process.exited()?;
    process.close()?;
    write_json(
        directory,
        "direct-shutdown.json",
        &json!({"pid":pid,"shutdownReturnedOk":true,"retainedHandleSignaled":true}),
    )?;
    drop(resident);
    let mut dropped = OpenFxFilterSession::prepare(
        &case.helper.path,
        &inspection,
        identifier,
        5,
        3,
        value_format(),
        mondrian_effects::EffectProjectGeometry::new(
            5,
            3,
            mondrian_core::SampleAspectRatio::SQUARE,
        )
        .expect("explicit owner project geometry"),
        &mut checkpoint,
    )
    .map_err(|e| e.to_string())?;
    let drop_pid = dropped.worker_process_id();
    let actual = dropped
        .render(
            &OpenFxRenderTiming {
                frame: 4.0,
                frame_rate: 24.0,
                first_frame: 0.0,
                last_frame: 4.0,
            },
            &values(case, 4, 1.0)?,
            &input,
            &mut checkpoint,
        )
        .map_err(|e| e.to_string())?;
    let drop_handle = ProcessHandle::open(drop_pid)?;
    write_json(
        directory,
        "drop-live-owner.json",
        &json!({"pid":drop_pid,"retainedProcessHandle":true,"aliveAtCapture":true,
        "capturedAfterSuccessfulRender":true,"originalChildKernelHandleExposed":false,"pidToOpenProcessAcquisitionRaceRemains":true}),
    )?;
    outputs.push(output(directory, "drop-nontrivial", &actual, &posterized)?);
    drop(dropped);
    drop_handle.exited()?;
    drop_handle.close()?;
    write_json(
        directory,
        "direct-drop.json",
        &json!({"pid":drop_pid,"retainedHandleSignaled":true,"actualRenderBeforeDrop":true}),
    )?;

    checkpoint().map_err(|e| e.to_string())?;
    let kind = register_selected_openfx_filter(&case.helper.path, &inspection, identifier)
        .map_err(|e| e.to_string())?;
    checkpoint().map_err(|e| e.to_string())?;
    // Registration performs its own existing Describe. Compare full public
    // metadata again; capture drift and fail rather than normalize schema.
    let after = describe_openfx_filter_with_checkpoint(
        &case.helper.path,
        &inspection,
        identifier,
        &mut checkpoint,
    )
    .map_err(|e| e.to_string())?;
    write_json(directory, "actual-describe-after-registration.json", &after)?;
    require(
        description == after,
        "native public schema changed across lifecycle/registration",
    )?;
    let mut effect = instantiate_effect_node(kind).map_err(|e| e.to_string())?;
    let mut actual_ids = Vec::new();
    for p in &case.parameters {
        let found: Vec<_> = effect
            .properties
            .iter()
            .filter(|(_, property)| property.descriptor.display_name == p.label)
            .collect();
        require(
            found.len() == 1,
            "canonical property label missing/ambiguous",
        )?;
        let property = found[0].1;
        require(
            property.static_value() == &p.default.property(),
            "canonical property default differs actual Describe",
        )?;
        let id = property.descriptor.parameter_id().clone();
        actual_ids.push(id);
    }
    require(
        effect.properties.iter().count() == case.parameters.len(),
        "unexpected canonical property count",
    )?;
    write_json(
        directory,
        "canonical-parameter-map.json",
        &json!({"actualParameterIds":actual_ids,"nativeContracts":case.parameters,"description":description_record}),
    )?;
    let range = TimelineTimeRange::new(
        TimelineTime::ZERO,
        TimelineTime::new(5, 24).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let frame_context = EffectFrameContext::new(
        Rational::FPS_24,
        mondrian_effects::EffectProjectGeometry::new(5, 3, SampleAspectRatio::SQUARE)
            .expect("explicit owner project geometry"),
        range,
    )
    .map_err(|e| e.to_string())?;
    require(
        frame_context.available_frames().map_err(|e| e.to_string())? == (0, 4),
        "graph/direct time availability mismatch",
    )?;
    let defaults = EffectExecutionSessionConfig::default();
    let mut graph_session = EffectExecutionSession::new(EffectExecutionSessionConfig {
        max_cache_entries: 0,
        max_cache_bytes: 0,
        max_gpu_plan_entries: 0,
        max_gpu_plan_bytes: 0,
        max_processor_instances: 1,
        ..defaults
    });
    for (key, frame, bits, mix, expected) in &steps {
        let authored = [
            Numeric::Integer(*bits),
            Numeric::Integer(0),
            Numeric::Integer(4),
            Numeric::Integer(1),
            Numeric::Double(0.0),
            Numeric::Double(*mix),
        ];
        for (id, value) in actual_ids.iter().zip(&authored) {
            effect
                .set_static_value_by_parameter(id, value.property())
                .map_err(|e| e.to_string())?;
        }
        let program = PreparedEffectProgram::prepare_hierarchical_with_frame_context(
            &[effect.clone()],
            &[],
            &[],
            &[],
            WorkingColorSpace::LinearRec709,
            frame_context,
            &LutPreparationCache::uncached(),
        )
        .map_err(|e| e.to_string())?;
        let graph = program
            .evaluate(TimelineTime::new(i64::from(*frame), 24).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        graph_session.clear_pixel_caches();
        let mut stop =
            || (Instant::now() >= deadline).then_some(EffectProcessorStopReason::DeadlineExpired);
        let mut control = EffectProcessorControl::new(&mut stop);
        control
            .bind_observation(EffectProcessorObservation { sink: None, deadline: Some(deadline) });
        let actual = graph_session
            .apply_compiled_rgba_f32(input.view(), &graph, i64::from(*frame), &mut control)
            .map_err(|e| format!("{e:?}"))?;
        outputs.push(output(
            directory,
            &format!("graph-{key}"),
            &actual,
            expected,
        )?);
        require(
            graph_session.diagnostics().processor_instances == 1,
            "graph residency must own exactly one native processor",
        )?;
        write_json(
            directory,
            &format!("graph-{key}-author.json"),
            &json!({"frame":frame,"actualParameterIds":actual_ids,
            "values":authored,"ownerCount":graph_session.diagnostics().processor_instances,"availableFrames":[0,4]}),
        )?;
    }
    graph_session.retire_processor_resources();
    require(
        graph_session.diagnostics().processor_instances == 0,
        "graph processor bank not retired",
    )?;
    checkpoint().map_err(|e| e.to_string())?;
    write_json(
        directory,
        "slice-results.json",
        &json!({"actualInspection":inspection_record,"actualDescription":description_record,
        "outputs":outputs,"graphOwnerCountAfterRetirement":0,"graphNativePidExposed":false,
        "outerJobDescendantSettlementStillRequired":true,"nativeStateOrGuiOrPerformanceQualified":false}),
    )?;
    Ok(())
}

#[test]
#[ignore = "requires explicit external case, frozen producer/artifact leases and enclosing native Job/controller"]
fn external_cpu_float_filter_round() {
    let result = run();
    if let Err(error) = result {
        panic!("external OFX qualification failed: {error}");
    }
}
fn run() -> LabResult<()> {
    let deadline = Instant::now() + OUTER_BUDGET;
    let path = PathBuf::from(
        std::env::var_os("MONDRIAN_EXTERNAL_OPENFX_CASE")
            .ok_or_else(|| "missing external OFX case".to_owned())?,
    );
    let sha = std::env::var("MONDRIAN_EXTERNAL_OPENFX_CASE_SHA256").map_err(|e| e.to_string())?;
    require(
        path.is_absolute() && valid_sha(&sha),
        "case path/hash binding",
    )?;
    for ancestor in path.ancestors() {
        let metadata = std::fs::symlink_metadata(ancestor).map_err(|e| e.to_string())?;
        require(
            metadata.file_attributes() & 0x400 == 0,
            "reparse-point case",
        )?;
    }
    // Read bounded length before hashing even the initial untrusted case.
    let file = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&path)
        .map_err(|e| e.to_string())?;
    let m = file.metadata().map_err(|e| e.to_string())?;
    require(
        m.is_file() && m.len() > 0 && m.len() <= CASE_LIMIT,
        "case admission length",
    )?;
    let mut case_lease = Lease {
        file,
        binding: Binding { path, sha256: sha, bytes: m.len() },
    };
    case_lease.check()?;
    let case = parse_case(&case_lease.bytes(CASE_LIMIT)?)?;
    require(
        case.evidence_directory
            == case_lease
                .binding
                .path
                .parent()
                .ok_or_else(|| "case parent missing".to_owned())?
                .join("native-evidence"),
        "evidence directory must be the case's dedicated native-evidence sibling",
    )?;
    let evidence_metadata =
        std::fs::symlink_metadata(&case.evidence_directory).map_err(|e| e.to_string())?;
    require(
        evidence_metadata.is_dir() && evidence_metadata.file_attributes() & 0x400 == 0,
        "evidence directory reparse/type",
    )?;
    require(
        std::fs::read_dir(&case.evidence_directory)
            .map_err(|e| e.to_string())?
            .next()
            .is_none(),
        "evidence directory must exist and be empty",
    )?;
    let result = (|| {
        write_json(
            &case.evidence_directory,
            "parser-negative-controls.json",
            &parser_controls(&case)?,
        )?;
        let mut leases = case
            .bindings()
            .into_iter()
            .map(|(b, cap)| Lease::open(b, cap))
            .collect::<LabResult<Vec<_>>>()?;
        // Frozen oracle identity supplies exact image/step/operator authority.
        let oracle = strict_json(&leases[2].bytes(JSON_LIMIT)?, JSON_LIMIT)?;
        require(
            oracle["operator"] == "rgb_u8_top_bit_mask_normalized_endpoint_oracle_v1",
            "unknown oracle operator",
        )?;
        write_json(
            &case.evidence_directory,
            "input-admission.json",
            &json!({"case":case_lease.binding,"bindings":case.bindings().iter().map(|(b, _)| *b).collect::<Vec<_>>(),
            "firstNativeEvidenceGeneratedInThisTest":true,"producerChronologyQualificationByOuterControllerRequired":true}),
        )?;
        let native = execute(&case, &mut leases, deadline);
        // Preserve the original primary error while still checking every lease.
        let mut lease_failures = Vec::new();
        for lease in &mut leases {
            if let Err(e) = lease.check() {
                lease_failures.push(e);
            }
        }
        if let Err(e) = case_lease.check() {
            lease_failures.push(e);
        }
        let post = write_json(
            &case.evidence_directory,
            "input-post-admission.json",
            &json!({"leaseFailures":lease_failures,"sameHandlesHeld":true}),
        );
        match (native, post) {
            (Err(primary), Err(publication)) => Err(format!(
                "{primary}; post-admission publication also failed: {publication}"
            )),
            (Err(primary), _) => Err(primary),
            (Ok(()), Err(publication)) => Err(publication),
            (Ok(()), Ok(_)) => require(
                lease_failures.is_empty(),
                "input changed under held read leases",
            ),
        }
    })();
    let receipt = json!({"kind":"EXTERNAL_OFX_TEST_ROUND_ACTUAL_RESULT","round":case.round,
        "passed":result.is_ok(),"primaryFailure":result.as_ref().err(),"case":case_lease.binding,
        "scope":"Only this CPU Float endpoint/numeric author/lifetime slice; outer Job acceptance required"});
    let publication = write_json(&case.evidence_directory, "result.json", &receipt);
    match (result, publication) {
        (Err(primary), Err(write)) => Err(format!(
            "{primary}; evidence publication also failed: {write}"
        )),
        (Err(primary), _) => Err(primary),
        (Ok(()), Err(write)) => Err(format!("evidence publication failed: {write}")),
        (Ok(()), Ok(_)) => Ok(()),
    }
}
