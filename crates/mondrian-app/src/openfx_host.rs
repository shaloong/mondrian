//! Supervised Float32 OpenFX filter execution for one selected native binary.

mod resident;
pub use resident::{
    run_openfx_resident_worker, OpenFxFilterSession, OPENFX_RESIDENT_WORKER_ARGUMENT,
};

use std::collections::BTreeMap;
use std::ffi::{c_char, c_int, c_void, CString};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use memmap2::MmapOptions;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::openfx_adapter::{
    resolve_openfx_binary, sha256_file, OpenFxBinaryInspection, OpenFxDiscoveryError,
};

/// Hidden product executable mode for selected filter description.
pub const OPENFX_DESCRIBE_WORKER_ARGUMENT: &str = "--internal-openfx-describe-v1";
const REQUEST_ENV: &str = "MONDRIAN_INTERNAL_OPENFX_DESCRIBE_REQUEST";
const RESPONSE_ENV: &str = "MONDRIAN_INTERNAL_OPENFX_DESCRIBE_RESPONSE";
const MAX_FRAME_BYTES: usize = 128 * 1024 * 1024;
const MAX_PARAMETERS: usize = 4096;
const DEADLINE: Duration = Duration::from_secs(30);
const MAX_WORKER_REQUEST_BYTES: usize = 2 * 1024 * 1024;
const MAX_WORKER_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;
/// Maximum UTF-8 bytes in one native String value, excluding its terminating NUL.
pub const OPENFX_MAX_STRING_BYTES: usize = 64 * 1024;
const MAX_PARAMETER_STRING_BYTES: usize = MAX_WORKER_REQUEST_BYTES / 2;

/// An OFX parameter value supported by the current CPU filter host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum OpenFxParameterValue {
    /// A finite OFX Double parameter value.
    Double(f64),
    /// Exact ordered SDK components of an OFX Double2 parameter.
    Double2([f64; 2]),
    /// Exact ordered SDK components of an OFX Double3 parameter.
    Double3([f64; 3]),
    /// Exact ordered SDK components of an OFX Rgb parameter.
    Rgb([f64; 3]),
    /// Exact ordered SDK components of an OFX Rgba parameter.
    Rgba([f64; 4]),
    /// Exact ordered SDK components of an OFX Integer2 parameter.
    Integer2([i32; 2]),
    /// Exact ordered SDK components of an OFX Integer3 parameter.
    Integer3([i32; 3]),
    /// An OFX Boolean parameter value.
    Boolean(bool),
    /// An exact signed 32-bit OFX Integer parameter value.
    Integer(i32),
    /// Original zero-based OFX Choice index, independent of UI ordering.
    Choice(i32),
    /// Stable OFX StrChoice enum string, independent of labels and UI order.
    StrChoice(String),
    /// Exact UTF-8 contents of an OFX String control, including empty values.
    String(String),
}

impl OpenFxParameterValue {
    fn native_fields(&self) -> (c_int, [f64; 4]) {
        match self {
            Self::Double(value) => (1, [*value, 0.0, 0.0, 0.0]),
            Self::Boolean(value) => (2, [f64::from(*value as u8), 0.0, 0.0, 0.0]),
            Self::Integer(value) => (3, [f64::from(*value), 0.0, 0.0, 0.0]),
            Self::Choice(value) => (4, [f64::from(*value), 0.0, 0.0, 0.0]),
            Self::StrChoice(_) => (5, [0.0; 4]),
            Self::String(_) => (6, [0.0; 4]),
            Self::Double2([x, y]) => (7, [*x, *y, 0.0, 0.0]),
            Self::Double3([x, y, z]) => (8, [*x, *y, *z, 0.0]),
            Self::Rgb([r, g, b]) => (9, [*r, *g, *b, 0.0]),
            Self::Rgba(value) => (10, *value),
            Self::Integer2([x, y]) => (11, [f64::from(*x), f64::from(*y), 0.0, 0.0]),
            Self::Integer3([x, y, z]) => (12, [f64::from(*x), f64::from(*y), f64::from(*z), 0.0]),
        }
    }

    pub(super) fn numeric_layout(&self) -> Option<(usize, bool)> {
        match self {
            Self::Double(_) => Some((1, false)),
            Self::Integer(_) => Some((1, true)),
            Self::Double2(_) => Some((2, false)),
            Self::Double3(_) | Self::Rgb(_) => Some((3, false)),
            Self::Rgba(_) => Some((4, false)),
            Self::Integer2(_) => Some((2, true)),
            Self::Integer3(_) => Some((3, true)),
            Self::Boolean(_) | Self::Choice(_) | Self::StrChoice(_) | Self::String(_) => None,
        }
    }
}

/// Temporal authority supplied with an OpenFX parameter value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenFxParameterQueryMode {
    /// The supplied value is constant over the parameter time domain.
    Constant,
    /// Only the requested render frame has been evaluated; no curve is supplied.
    CurrentFrame,
}

/// One immutable execution binding, distinct from descriptor animation capability.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenFxParameterBinding {
    /// Typed value at the requested render frame.
    pub value: OpenFxParameterValue,
    /// Evidence governing queries outside that frame.
    pub query_mode: OpenFxParameterQueryMode,
}

impl OpenFxParameterBinding {
    /// Bind a value which is known to be constant across time.
    pub fn constant(value: OpenFxParameterValue) -> Self {
        Self {
            value,
            query_mode: OpenFxParameterQueryMode::Constant,
        }
    }

    /// Bind one evaluated frame without claiming values at other times.
    pub fn current_frame(value: OpenFxParameterValue) -> Self {
        Self {
            value,
            query_mode: OpenFxParameterQueryMode::CurrentFrame,
        }
    }
}

/// Numeric bounds for one component, in the SDK's original component order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenFxNumericChannel {
    /// Lowest accepted component value.
    pub minimum: f64,
    /// Highest accepted component value.
    pub maximum: f64,
    /// Suggested slider lower bound.
    pub display_minimum: f64,
    /// Suggested slider upper bound.
    pub display_maximum: f64,
}

/// Native presentation and path intent declared by the SDK String descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", from = "OpenFxStringModeWire")]
pub enum OpenFxStringMode {
    /// An editable single-line value.
    SingleLine,
    /// An editable multiline value.
    MultiLine,
    /// A file path, which may name an existing input or a new destination.
    FilePath {
        /// Whether the SDK requires the selected file to exist.
        must_exist: bool,
    },
    /// A directory path, which may name an existing or new directory.
    DirectoryPath {
        /// Whether the SDK requires the selected directory to exist.
        must_exist: bool,
    },
    /// A label that the plugin may update but the user must not edit.
    Label,
    /// An SDK rich-text value whose markup must be preserved.
    RichTextFormat,
}

// Empty struct variants make internally tagged modes reject surplus fields;
// Serde's unit variants otherwise accept them even with deny_unknown_fields.
#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
enum OpenFxStringModeWire {
    SingleLine {},
    MultiLine {},
    FilePath { must_exist: bool },
    DirectoryPath { must_exist: bool },
    Label {},
    RichTextFormat {},
}

impl From<OpenFxStringModeWire> for OpenFxStringMode {
    fn from(mode: OpenFxStringModeWire) -> Self {
        match mode {
            OpenFxStringModeWire::SingleLine {} => Self::SingleLine,
            OpenFxStringModeWire::MultiLine {} => Self::MultiLine,
            OpenFxStringModeWire::FilePath { must_exist } => Self::FilePath { must_exist },
            OpenFxStringModeWire::DirectoryPath { must_exist } => {
                Self::DirectoryPath { must_exist }
            }
            OpenFxStringModeWire::Label {} => Self::Label,
            OpenFxStringModeWire::RichTextFormat {} => Self::RichTextFormat,
        }
    }
}

/// Admitted interpretation of an OpenFX Double control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenFxDoubleType {
    /// A dimensionless scalar.
    Plain,
    /// A multiplicative scale.
    Scale,
}

/// One declared choice key, caption and optional rank in native option order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenFxChoiceOption {
    /// Canonical index string for Choice, or plugin-declared enum for StrChoice.
    pub key: String,
    /// Plugin-provided caption, independent of the stored key.
    pub label: String,
    /// Optional signed visual rank; absent ranks preserve original index order.
    pub display_rank: Option<i32>,
}

/// A parameter admitted by the selected Float32 Filter host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenFxParameterDescription {
    /// Stable identifier supplied to the OFX parameter suite.
    pub name: String,
    /// Plugin-provided display label.
    pub label: String,
    /// Plugin-provided usage hint.
    pub hint: String,
    /// Value assigned when an effect instance is first created.
    pub default_value: OpenFxParameterValue,
    /// Choice options in original native index order; empty for other kinds.
    pub choices: Vec<OpenFxChoiceOption>,
    /// Definition-preferred interpolation for newly authored keys.
    pub preferred_interpolation: mondrian_core::automation::ParameterInterpolation,
    /// Semantic kind of a Double control.
    pub double_type: Option<OpenFxDoubleType>,
    /// Required for String parameters and absent for every other kind.
    pub string_mode: Option<OpenFxStringMode>,
    /// Complete ordered numeric component bounds; empty for discrete controls.
    pub numeric_channels: Vec<OpenFxNumericChannel>,
    /// Whether this parameter permits authored animation.
    pub can_animate: bool,
    /// Whether the plugin marks this parameter hidden.
    pub secret: bool,
    /// Whether the plugin initially enables this parameter.
    pub enabled: bool,
}

/// Descriptor of one selected installed Filter context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenFxFilterDescription {
    /// Identifier pinned by the prior binary inspection.
    pub identifier: String,
    /// Plugin-provided display name, with the identifier as fallback.
    pub label: String,
    /// Parameters in the order declared by the plugin.
    pub parameters: Vec<OpenFxParameterDescription>,
}

/// One straight-alpha, tightly packed RGBA Float32 image.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenFxFloatFrame {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Row-major pixels in the caller's declared working color domain.
    pub pixels: Vec<[f32; 4]>,
}

/// Exact sequence facts lowered to the OpenFX double-valued frame contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenFxRenderTiming {
    /// Frame position supplied to the OFX render action.
    pub frame: f64,
    /// Sequence frames per second.
    pub frame_rate: f64,
    /// First frame available from this filter's source Clip.
    pub first_frame: f64,
    /// Last frame available from this filter's source Clip.
    pub last_frame: f64,
    /// Sequence pixel width divided by pixel height.
    pub pixel_aspect_ratio: f64,
}

/// Native host failure that leaves the caller's input frame unchanged.
#[derive(Debug, thiserror::Error)]
pub enum OpenFxHostError {
    /// The request cannot be represented by the admitted filter contract.
    #[error("invalid OpenFX host request: {0}")]
    Invalid(String),
    /// The binary identity or bundle could not be verified.
    #[error(transparent)]
    Discovery(#[from] OpenFxDiscoveryError),
    /// The isolated child or its output could not be used.
    #[error("OpenFX host unavailable: {0}")]
    Unavailable(String),
    /// The native render exceeded its supervised deadline.
    #[error("OpenFX native worker exceeded its deadline")]
    DeadlineExceeded,
    /// The owning execution request was canceled before output publication.
    #[error("OpenFX native worker request was canceled")]
    Canceled,
    /// The native process exited without a valid result.
    #[error("OpenFX native worker failed: {0}")]
    WorkerFailed(String),
}

fn checked_openfx_integer(value: f64) -> Option<i32> {
    (value.is_finite()
        && value.fract() == 0.0
        && value >= f64::from(i32::MIN)
        && value <= f64::from(i32::MAX))
    .then_some(value as i32)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DescribeRequest {
    binary_path: PathBuf,
    bundle_path: PathBuf,
    binary_sha256: String,
    plugin_identifier: String,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum DescribeResponse {
    Success {
        description: OpenFxFilterDescription,
    },
    Rejected {
        status: i32,
        reason: Option<String>,
    },
}

#[repr(C)]
struct NativeParameter {
    name: *const c_char,
    query_mode: c_int,
    kind: c_int,
    value: [f64; 4],
    string_value: *const c_char,
    string_length: usize,
}

#[repr(C)]
struct NativeFrameInfo {
    width: c_int,
    height: c_int,
    frame: f64,
    frame_rate: f64,
    first_frame: f64,
    last_frame: f64,
    pixel_aspect_ratio: f64,
}

#[repr(C)]
struct NativeChoiceOption {
    label: *const c_char,
    label_length: usize,
    rank_present: c_int,
    display_rank: c_int,
    key: *const c_char,
    key_length: usize,
}

#[repr(C)]
struct NativeParameterInfo {
    name: *const c_char,
    name_length: usize,
    label: *const c_char,
    label_length: usize,
    hint: *const c_char,
    hint_length: usize,
    kind: c_int,
    preferred_interpolation: c_int,
    double_type: c_int,
    default_value: [f64; 4],
    minimum: [f64; 4],
    maximum: [f64; 4],
    display_minimum: [f64; 4],
    display_maximum: [f64; 4],
    can_animate: c_int,
    secret: c_int,
    enabled: c_int,
    choices: *const NativeChoiceOption,
    choice_count: usize,
    string_default: *const c_char,
    string_default_length: usize,
    string_mode: c_int,
    string_file_path_exists: c_int,
}

#[derive(Default)]
struct ParameterSink {
    parameters: Vec<OpenFxParameterDescription>,
    error: Option<String>,
}

unsafe extern "C" {
    fn mondrian_openfx_describe(
        binary: *const c_char,
        bundle: *const c_char,
        identifier: *const c_char,
        label: *mut c_char,
        label_capacity: usize,
        label_length: *mut usize,
        callback: unsafe extern "C" fn(*mut c_void, *const NativeParameterInfo) -> c_int,
        context: *mut c_void,
    ) -> c_int;
    fn mondrian_openfx_session_create(
        binary: *const c_char,
        bundle: *const c_char,
        identifier: *const c_char,
        input: *const c_void,
        frame_bytes: usize,
        frame_info: *const NativeFrameInfo,
        parameters: *const NativeParameter,
        parameter_count: c_int,
        session: *mut *mut c_void,
    ) -> c_int;
    fn mondrian_openfx_session_render(
        session: *mut c_void,
        input: *const c_void,
        output: *mut c_void,
        frame_bytes: usize,
        frame_info: *const NativeFrameInfo,
        parameters: *const NativeParameter,
        parameter_count: c_int,
    ) -> c_int;
    fn mondrian_openfx_session_destroy(session: *mut c_void);

}

// A native handle stays in the isolated child and is neither Send nor Sync.
// Its owner destroys the instance before unloading its selected plugin binary.
struct NativeSession(std::ptr::NonNull<c_void>);

impl NativeSession {
    fn create(
        binary: &CString,
        bundle: &CString,
        identifier: &CString,
        input: &[u8],
        info: &NativeFrameInfo,
        parameters: &[NativeParameter],
    ) -> Result<Self, c_int> {
        let mut pointer = std::ptr::null_mut();
        // SAFETY: C strings, frame bytes and parameter names stay live throughout
        // this call. The native host copies frame data and parameter values.
        let status = unsafe {
            mondrian_openfx_session_create(
                binary.as_ptr(),
                bundle.as_ptr(),
                identifier.as_ptr(),
                input.as_ptr().cast(),
                input.len(),
                info,
                parameters.as_ptr(),
                parameters.len() as c_int,
                &mut pointer,
            )
        };
        if status != 0 {
            return Err(status);
        }
        std::ptr::NonNull::new(pointer).map(Self).ok_or(6)
    }

    fn render(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        info: &NativeFrameInfo,
        parameters: &[NativeParameter],
    ) -> c_int {
        if output.len() != input.len() {
            return 2;
        }
        // SAFETY: The exclusive live handle serializes native actions. Disjoint
        // input/output slices remain live and the host validates the exact extent
        // before reading or writing. Parameter names outlive this synchronous call.
        unsafe {
            mondrian_openfx_session_render(
                self.0.as_ptr(),
                input.as_ptr().cast(),
                output.as_mut_ptr().cast(),
                input.len(),
                info,
                parameters.as_ptr(),
                parameters.len() as c_int,
            )
        }
    }
}

impl Drop for NativeSession {
    fn drop(&mut self) {
        // SAFETY: This handle was created once, has no aliases and is retired once.
        unsafe {
            mondrian_openfx_session_destroy(self.0.as_ptr());
        }
    }
}

/// Describe the admitted Filter parameter schema in a supervised native child.
pub fn describe_openfx_filter(
    helper_executable: &Path,
    inspection: &OpenFxBinaryInspection,
    plugin_identifier: &str,
) -> Result<OpenFxFilterDescription, OpenFxHostError> {
    let bundle = verified_selection(inspection, plugin_identifier)?;
    let staging = tempfile::tempdir()
        .map_err(|error| unavailable(format!("description staging failed: {error}")))?;
    let request = DescribeRequest {
        binary_path: inspection.binary_path.clone(),
        bundle_path: bundle,
        binary_sha256: inspection.binary_sha256.clone(),
        plugin_identifier: plugin_identifier.to_owned(),
    };
    let response: DescribeResponse = invoke_worker(
        helper_executable,
        OPENFX_DESCRIBE_WORKER_ARGUMENT,
        staging.path(),
        &request,
    )?;
    if sha256_file(&inspection.binary_path)? != inspection.binary_sha256 {
        return Err(invalid("OpenFX binary changed during description"));
    }
    match response {
        DescribeResponse::Success { description } => {
            validate_description(&description, plugin_identifier)?;
            Ok(description)
        }
        DescribeResponse::Rejected { status, reason } => Err(OpenFxHostError::WorkerFailed(
            format!("native status {status}: {}", reason.unwrap_or_default()),
        )),
    }
}

/// Execute one installed OFX filter in a supervised child. The caller must
/// supply the working color domain and straight-alpha interpretation; this
/// adapter does not perform implicit color or alpha conversion.
pub fn render_openfx_filter_frame(
    helper_executable: &Path,
    inspection: &OpenFxBinaryInspection,
    plugin_identifier: &str,
    timing: &OpenFxRenderTiming,
    parameters: &BTreeMap<String, OpenFxParameterBinding>,
    frame: &OpenFxFloatFrame,
) -> Result<OpenFxFloatFrame, OpenFxHostError> {
    validate_render_request(timing, parameters, frame)?;
    let mut checkpoint = || Ok(());
    let mut session = OpenFxFilterSession::prepare(
        helper_executable,
        inspection,
        plugin_identifier,
        frame.width,
        frame.height,
        &mut checkpoint,
    )?;
    let output = session.render(timing, parameters, frame, &mut checkpoint)?;
    session.shutdown()?;
    Ok(output)
}

fn invoke_worker<T: Serialize, R: DeserializeOwned>(
    helper_executable: &Path,
    argument: &str,
    staging: &Path,
    request: &T,
) -> Result<R, OpenFxHostError> {
    if !helper_executable.is_absolute() || !helper_executable.is_file() {
        return Err(invalid("helper must be an absolute regular file"));
    }
    let request_path = staging.join("request.json");
    let response_path = staging.join("response.json");
    let request_bytes = serde_json::to_vec(request)
        .map_err(|error| invalid(format!("request encoding failed: {error}")))?;
    if request_bytes.len() > MAX_WORKER_REQUEST_BYTES {
        return Err(invalid("OpenFX worker request exceeds its size limit"));
    }
    fs::write(&request_path, request_bytes)
        .map_err(|error| unavailable(format!("request staging failed: {error}")))?;
    let mut child = Command::new(helper_executable)
        .arg(argument)
        .env(REQUEST_ENV, request_path)
        .env(RESPONSE_ENV, &response_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| unavailable(format!("OpenFX worker start failed: {error}")))?;
    let deadline = Instant::now() + DEADLINE;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(status)) => return Err(OpenFxHostError::WorkerFailed(status.to_string())),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(OpenFxHostError::DeadlineExceeded);
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(unavailable(format!(
                    "OpenFX worker observation failed: {error}"
                )));
            }
        }
    }
    let mut response_bytes = Vec::new();
    fs::File::open(&response_path)
        .map_err(|error| unavailable(format!("OpenFX worker response is unavailable: {error}")))?
        .take(MAX_WORKER_RESPONSE_BYTES + 1)
        .read_to_end(&mut response_bytes)
        .map_err(|error| unavailable(format!("OpenFX worker response read failed: {error}")))?;
    if response_bytes.len() as u64 > MAX_WORKER_RESPONSE_BYTES {
        return Err(unavailable("OpenFX worker response exceeds its size limit"));
    }
    serde_json::from_slice(&response_bytes)
        .map_err(|error| unavailable(format!("OpenFX worker response is malformed: {error}")))
}

/// Describe one selected native Filter before graphics or media startup.
pub fn run_openfx_describe_worker() -> Result<(), OpenFxHostError> {
    let (request, response_path): (DescribeRequest, PathBuf) =
        read_worker_request(OPENFX_DESCRIBE_WORKER_ARGUMENT)?;
    let response = describe_in_worker(&request)?;
    write_worker_response(&response_path, &response)
}

fn describe_in_worker(request: &DescribeRequest) -> Result<DescribeResponse, OpenFxHostError> {
    verify_worker_binary(
        &request.binary_path,
        &request.bundle_path,
        &request.binary_sha256,
    )?;
    let binary = native_string(&request.binary_path)?;
    let bundle = native_string(&request.bundle_path)?;
    let identifier = CString::new(request.plugin_identifier.as_str())
        .map_err(|_| invalid("plugin identifier contains a null byte"))?;
    let mut sink = ParameterSink::default();
    let mut label = [0_u8; 256];
    let mut label_length = 0_usize;
    // SAFETY: The C strings, callback, and sink outlive this synchronous call.
    // Native code runs only in the supervised child.
    let status = unsafe {
        mondrian_openfx_describe(
            binary.as_ptr(),
            bundle.as_ptr(),
            identifier.as_ptr(),
            label.as_mut_ptr().cast(),
            label.len(),
            &mut label_length,
            collect_native_parameter,
            (&mut sink as *mut ParameterSink).cast(),
        )
    };
    if status != 0 {
        return Ok(DescribeResponse::Rejected { status, reason: sink.error });
    }
    let label = label
        .get(..label_length)
        .ok_or_else(|| invalid("OpenFX display name exceeds its bound"))?;
    let label =
        std::str::from_utf8(label).map_err(|_| invalid("OpenFX display name is not UTF-8"))?;
    let description = OpenFxFilterDescription {
        identifier: request.plugin_identifier.clone(),
        label: if label.is_empty() {
            request.plugin_identifier.clone()
        } else {
            label.to_owned()
        },
        parameters: sink.parameters,
    };
    validate_description(&description, &request.plugin_identifier)?;
    Ok(DescribeResponse::Success { description })
}

unsafe extern "C" fn collect_native_parameter(
    context: *mut c_void,
    parameter: *const NativeParameterInfo,
) -> c_int {
    if context.is_null() || parameter.is_null() {
        return 1;
    }
    // SAFETY: The C++ call owns both values until this callback returns.
    let sink = unsafe { &mut *context.cast::<ParameterSink>() };
    // SAFETY: The pointed-to descriptor and its strings remain live for this call.
    match unsafe { decode_native_parameter(&*parameter) } {
        Ok(value) if sink.parameters.len() < MAX_PARAMETERS => {
            sink.parameters.push(value);
            0
        }
        Ok(_) => {
            sink.error = Some("too many OpenFX parameters".to_owned());
            1
        }
        Err(error) => {
            sink.error = Some(error);
            1
        }
    }
}

unsafe fn decode_native_parameter(
    raw: &NativeParameterInfo,
) -> Result<OpenFxParameterDescription, String> {
    // SAFETY: The native host passes std::string storage valid for this callback.
    let name = unsafe { copy_native_text(raw.name, raw.name_length, 256)? };
    // SAFETY: The native host passes std::string storage valid for this callback.
    let label = unsafe { copy_native_text(raw.label, raw.label_length, 256)? };
    // SAFETY: The native host passes std::string storage valid for this callback.
    let hint = unsafe { copy_native_text(raw.hint, raw.hint_length, 1024)? };
    let choices = if matches!(raw.kind, 4 | 5) {
        if raw.choice_count == 0 || raw.choice_count > 4096 || raw.choices.is_null() {
            return Err("invalid OpenFX Choice option array".to_owned());
        }
        // SAFETY: The matched C++ host owns this bounded array for the callback.
        unsafe { std::slice::from_raw_parts(raw.choices, raw.choice_count) }
            .iter()
            .enumerate()
            .map(|(index, option)| {
                // SAFETY: Each caption's std::string storage outlives this callback.
                let label = unsafe { copy_native_text(option.label, option.label_length, 256)? };
                let display_rank = match option.rank_present {
                    0 => None,
                    1 => Some(option.display_rank),
                    _ => return Err("invalid OpenFX Choice rank flag".to_owned()),
                };
                let key = if raw.kind == 5 {
                    // SAFETY: The matched host owns each enum string for this callback.
                    unsafe { copy_native_text(option.key, option.key_length, 256)? }
                } else {
                    index.to_string()
                };
                Ok(OpenFxChoiceOption { key, label, display_rank })
            })
            .collect::<Result<Vec<_>, String>>()?
    } else {
        if raw.choice_count != 0 {
            return Err("non-Choice parameter has choices".to_owned());
        }
        Vec::new()
    };
    let double_type = match raw.kind {
        1 | 7 | 8 => Some(match raw.double_type {
            0 => OpenFxDoubleType::Plain,
            1 => OpenFxDoubleType::Scale,
            _ => return Err("unsupported OpenFX Double interpretation".to_owned()),
        }),
        _ if raw.double_type == -1 => None,
        _ => return Err("non-Double parameter has Double metadata".to_owned()),
    };
    let integer = |index: usize| {
        checked_openfx_integer(raw.default_value[index])
            .ok_or_else(|| "invalid OpenFX Integer default".to_owned())
    };
    let default_value = match raw.kind {
        1 => OpenFxParameterValue::Double(raw.default_value[0]),
        2 if raw.default_value[0] == 0.0 || raw.default_value[0] == 1.0 => {
            OpenFxParameterValue::Boolean(raw.default_value[0] == 1.0)
        }
        3 => OpenFxParameterValue::Integer(integer(0)?),
        4 => OpenFxParameterValue::Choice(integer(0)?),
        5 | 6 => {
            // SAFETY: Native storage remains borrowed and live throughout this callback.
            let text = unsafe {
                copy_native_text(
                    raw.string_default,
                    raw.string_default_length,
                    if raw.kind == 5 {
                        256
                    } else {
                        OPENFX_MAX_STRING_BYTES
                    },
                )?
            };
            if raw.kind == 5 {
                OpenFxParameterValue::StrChoice(text)
            } else {
                OpenFxParameterValue::String(text)
            }
        }
        7 => OpenFxParameterValue::Double2([raw.default_value[0], raw.default_value[1]]),
        8 => OpenFxParameterValue::Double3([
            raw.default_value[0],
            raw.default_value[1],
            raw.default_value[2],
        ]),
        9 => OpenFxParameterValue::Rgb([
            raw.default_value[0],
            raw.default_value[1],
            raw.default_value[2],
        ]),
        10 => OpenFxParameterValue::Rgba(raw.default_value),
        11 => OpenFxParameterValue::Integer2([integer(0)?, integer(1)?]),
        12 => OpenFxParameterValue::Integer3([integer(0)?, integer(1)?, integer(2)?]),
        _ => return Err("unsupported OpenFX parameter kind or default".to_owned()),
    };
    let numeric_channels = default_value.numeric_layout().map_or_else(Vec::new, |(arity, _)| {
        (0..arity)
            .map(|index| OpenFxNumericChannel {
                minimum: raw.minimum[index],
                maximum: raw.maximum[index],
                display_minimum: raw.display_minimum[index],
                display_maximum: raw.display_maximum[index],
            })
            .collect()
    });
    let flag = |value| match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err("invalid OpenFX parameter flag".to_owned()),
    };
    let string_mode = if raw.kind == 6 {
        Some(match raw.string_mode {
            1 if raw.string_file_path_exists == -1 => OpenFxStringMode::SingleLine,
            2 if raw.string_file_path_exists == -1 => OpenFxStringMode::MultiLine,
            3 => OpenFxStringMode::FilePath { must_exist: flag(raw.string_file_path_exists)? },
            4 => OpenFxStringMode::DirectoryPath { must_exist: flag(raw.string_file_path_exists)? },
            5 if raw.string_file_path_exists == -1 => OpenFxStringMode::Label,
            6 if raw.string_file_path_exists == -1 => OpenFxStringMode::RichTextFormat,
            _ => {
                return Err(format!(
                    "invalid OpenFX String metadata for `{name}`: mode {}, path flag {}",
                    raw.string_mode, raw.string_file_path_exists
                ))
            }
        })
    } else {
        if raw.string_mode != 0 || raw.string_file_path_exists != -1 {
            return Err("non-String parameter has String metadata".to_owned());
        }
        None
    };
    Ok(OpenFxParameterDescription {
        name: name.clone(),
        label: if label.is_empty() { name } else { label },
        hint,
        default_value,
        choices,
        preferred_interpolation: match raw.preferred_interpolation {
            0 => mondrian_core::automation::ParameterInterpolation::Hold,
            1 => mondrian_core::automation::ParameterInterpolation::Linear,
            2 => mondrian_core::automation::ParameterInterpolation::Bezier,
            _ => return Err("invalid OpenFX preferred interpolation".to_owned()),
        },
        double_type,
        string_mode,
        numeric_channels,
        can_animate: flag(raw.can_animate)?,
        secret: flag(raw.secret)?,
        enabled: flag(raw.enabled)?,
    })
}

unsafe fn copy_native_text(
    pointer: *const c_char,
    length: usize,
    limit: usize,
) -> Result<String, String> {
    if pointer.is_null() || length > limit {
        return Err("OpenFX parameter text exceeds its bound".to_owned());
    }
    // SAFETY: The caller verifies the pointer and length and the native host
    // supplies live std::string storage for this callback.
    let bytes = unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length) };
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| "OpenFX parameter text is not UTF-8".to_owned())
}

fn read_worker_request<T: DeserializeOwned>(
    expected_argument: &str,
) -> Result<(T, PathBuf), OpenFxHostError> {
    let mut args = std::env::args_os();
    let _executable = args.next();
    if args.next().as_deref() != Some(std::ffi::OsStr::new(expected_argument))
        || args.next().is_some()
    {
        return Err(invalid("unexpected OpenFX worker arguments"));
    }
    let request_path = std::env::var_os(REQUEST_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| invalid("render request path is missing"))?;
    let response_path = std::env::var_os(RESPONSE_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| invalid("render response path is missing"))?;
    let request_metadata = fs::metadata(&request_path)
        .map_err(|error| unavailable(format!("render request is unavailable: {error}")))?;
    if !request_metadata.is_file() || request_metadata.len() > MAX_WORKER_REQUEST_BYTES as u64 {
        return Err(invalid("render request exceeds its size limit"));
    }
    let request: T = serde_json::from_slice(
        &fs::read(request_path)
            .map_err(|error| unavailable(format!("render request is unavailable: {error}")))?,
    )
    .map_err(|error| invalid(format!("render request is malformed: {error}")))?;
    Ok((request, response_path))
}

fn write_worker_response<T: Serialize>(
    response_path: &Path,
    response: &T,
) -> Result<(), OpenFxHostError> {
    let response_bytes = serde_json::to_vec(response)
        .map_err(|error| invalid(format!("response encoding failed: {error}")))?;
    if response_bytes.len() as u64 > MAX_WORKER_RESPONSE_BYTES {
        return Err(invalid("OpenFX worker response exceeds its size limit"));
    }
    fs::write(response_path, response_bytes)
        .map_err(|error| unavailable(format!("render response write failed: {error}")))
}

fn validate_render_request(
    timing: &OpenFxRenderTiming,
    parameters: &BTreeMap<String, OpenFxParameterBinding>,
    frame: &OpenFxFloatFrame,
) -> Result<(), OpenFxHostError> {
    validate_frame(frame.width, frame.height, frame.pixels.len())?;
    if frame.pixels.iter().flatten().any(|channel| !channel.is_finite()) {
        return Err(invalid("OpenFX input contains a nonfinite channel"));
    }
    validate_timing(timing)?;
    validate_parameters(parameters)
}

fn validate_frame(width: u32, height: u32, pixels: usize) -> Result<usize, OpenFxHostError> {
    let count = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| invalid("frame dimensions overflow"))?;
    let bytes = count.checked_mul(16).ok_or_else(|| invalid("frame size overflows"))?;
    if width == 0
        || height == 0
        || width > 8192
        || height > 8192
        || bytes > MAX_FRAME_BYTES
        || pixels != count
    {
        return Err(invalid(
            "frame dimensions or pixel count exceed the admitted contract",
        ));
    }
    Ok(bytes)
}

fn validate_timing(timing: &OpenFxRenderTiming) -> Result<(), OpenFxHostError> {
    if !timing.frame.is_finite()
        || !timing.frame_rate.is_finite()
        || !timing.first_frame.is_finite()
        || !timing.last_frame.is_finite()
        || !timing.pixel_aspect_ratio.is_finite()
        || timing.frame_rate <= 0.0
        || timing.pixel_aspect_ratio <= 0.0
        || timing.first_frame > timing.frame
        || timing.frame > timing.last_frame
    {
        return Err(invalid("OpenFX frame timing is invalid"));
    }
    Ok(())
}

fn validate_parameters(
    parameters: &BTreeMap<String, OpenFxParameterBinding>,
) -> Result<(), OpenFxHostError> {
    validate_parameter_values(parameters.iter().map(|(name, binding)| (name, &binding.value)))
}

#[cfg(test)]
fn validate_parameter_values_map(
    parameters: &BTreeMap<String, OpenFxParameterValue>,
) -> Result<(), OpenFxHostError> {
    validate_parameter_values(parameters.iter())
}

fn validate_parameter_values<'a>(
    parameters: impl ExactSizeIterator<Item = (&'a String, &'a OpenFxParameterValue)>,
) -> Result<(), OpenFxHostError> {
    if parameters.len() > MAX_PARAMETERS {
        return Err(invalid("too many OpenFX parameters"));
    }
    let mut string_bytes = 0;
    for (name, value) in parameters {
        if name.is_empty()
            || name.len() > 256
            || !name.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
        {
            return Err(invalid("OpenFX parameter name is invalid"));
        }
        if let Some((arity, _)) = value.numeric_layout()
            && value.native_fields().1[..arity].iter().any(|value| !value.is_finite())
        {
            return Err(invalid("OpenFX parameter value must be finite"));
        }
        if let OpenFxParameterValue::StrChoice(value) = value
            && (value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control))
        {
            return Err(invalid("OpenFX StrChoice key is invalid"));
        }
        if let OpenFxParameterValue::Choice(value) = value
            && *value < 0
        {
            return Err(invalid("OpenFX Choice index must be nonnegative"));
        }
        if let OpenFxParameterValue::String(value) = value
            && (value.len() > OPENFX_MAX_STRING_BYTES || value.contains('\0'))
        {
            return Err(invalid("OpenFX String is oversized or contains NUL"));
        }
        if let OpenFxParameterValue::String(value) | OpenFxParameterValue::StrChoice(value) = value
        {
            string_bytes += value.len();
            if string_bytes > MAX_PARAMETER_STRING_BYTES {
                return Err(invalid(
                    "OpenFX parameter strings exceed the snapshot budget",
                ));
            }
        }
    }
    Ok(())
}

fn verified_selection(
    inspection: &OpenFxBinaryInspection,
    plugin_identifier: &str,
) -> Result<PathBuf, OpenFxHostError> {
    if !inspection.plugins.iter().any(|plugin| plugin.identifier == plugin_identifier) {
        return Err(invalid("selected plugin is absent from the pinned binary"));
    }
    let bundle = bundle_for_binary(&inspection.binary_path)?;
    if resolve_openfx_binary(&bundle)? != inspection.binary_path
        || sha256_file(&inspection.binary_path)? != inspection.binary_sha256
    {
        return Err(invalid("selected OpenFX binary changed since discovery"));
    }
    Ok(bundle)
}

fn verify_worker_binary(binary: &Path, bundle: &Path, sha256: &str) -> Result<(), OpenFxHostError> {
    if !binary.is_absolute()
        || !bundle.is_absolute()
        || resolve_openfx_binary(bundle)? != binary
        || sha256_file(binary)? != sha256
    {
        return Err(invalid(
            "worker received an invalid or changed OpenFX binary",
        ));
    }
    Ok(())
}

fn validate_description(
    description: &OpenFxFilterDescription,
    identifier: &str,
) -> Result<(), OpenFxHostError> {
    if description.identifier != identifier || description.parameters.len() > MAX_PARAMETERS {
        return Err(invalid(
            "OpenFX filter description has an invalid identity or size",
        ));
    }
    if description.label.is_empty()
        || description.label.len() > 256
        || description.label.chars().any(char::is_control)
    {
        return Err(invalid("OpenFX filter display name is invalid"));
    }
    let mut names = std::collections::BTreeSet::new();
    validate_parameter_values(
        description
            .parameters
            .iter()
            .map(|parameter| (&parameter.name, &parameter.default_value)),
    )?;
    for parameter in &description.parameters {
        let name = &parameter.name;
        if name.is_empty()
            || name.len() > 256
            || !name.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
            || !names.insert(name)
            || parameter.label.is_empty()
            || parameter.label.len() > 256
            || parameter.label.chars().any(char::is_control)
            || parameter.hint.len() > 1024
            || parameter.hint.chars().any(|character| {
                character.is_control()
                    && character != '\n'
                    && character != '\r'
                    && character != '\t'
            })
        {
            return Err(invalid("OpenFX filter parameter text is invalid"));
        }
        if !matches!(
            parameter.default_value,
            OpenFxParameterValue::Choice(_) | OpenFxParameterValue::StrChoice(_)
        ) && !parameter.choices.is_empty()
        {
            return Err(invalid("non-Choice parameter has choices"));
        }
        if matches!(parameter.default_value, OpenFxParameterValue::String(_))
            != parameter.string_mode.is_some()
        {
            return Err(invalid(
                "OpenFX String metadata does not match its parameter kind",
            ));
        }
        match &parameter.default_value {
            OpenFxParameterValue::Choice(_) | OpenFxParameterValue::StrChoice(_) => {
                let default_key = match &parameter.default_value {
                    OpenFxParameterValue::Choice(index) => index.to_string(),
                    OpenFxParameterValue::StrChoice(key) => key.clone(),
                    _ => return Err(invalid("expected a choice default")),
                };
                let mut keys = std::collections::BTreeSet::new();
                let mut ranks = std::collections::BTreeSet::new();
                if parameter.choices.is_empty()
                    || parameter.choices.len() > 4096
                    || !parameter.choices.iter().any(|option| option.key == default_key)
                    || parameter.preferred_interpolation
                        != mondrian_core::automation::ParameterInterpolation::Hold
                    || parameter.double_type.is_some()
                    || !parameter.numeric_channels.is_empty()
                    || parameter.choices.iter().enumerate().any(|(index, option)| {
                        option.key.trim().is_empty()
                            || option.key.len() > 256
                            || option.key.chars().any(char::is_control)
                            || !keys.insert(&option.key)
                            || (matches!(parameter.default_value, OpenFxParameterValue::Choice(_))
                                && option.key != index.to_string())
                            || option.label.trim().is_empty()
                            || option.label.len() > 256
                            || option.label.chars().any(char::is_control)
                            || option.display_rank.is_some()
                                != parameter.choices[0].display_rank.is_some()
                            || option.display_rank.is_some_and(|rank| !ranks.insert(rank))
                    })
                {
                    return Err(invalid(
                        "OpenFX Choice default, options or visual order is invalid",
                    ));
                }
            }
            OpenFxParameterValue::Boolean(_) | OpenFxParameterValue::String(_) => {
                if parameter.preferred_interpolation
                    != mondrian_core::automation::ParameterInterpolation::Hold
                    || parameter.double_type.is_some()
                    || !parameter.numeric_channels.is_empty()
                {
                    return Err(invalid(
                        "OpenFX discrete parameter has invalid interpolation or numeric bounds",
                    ));
                }
            }
            value => {
                let (arity, integer) =
                    value.numeric_layout().ok_or_else(|| invalid("expected numeric parameter"))?;
                let needs_semantic = matches!(
                    value,
                    OpenFxParameterValue::Double(_)
                        | OpenFxParameterValue::Double2(_)
                        | OpenFxParameterValue::Double3(_)
                );
                if parameter.double_type.is_some() != needs_semantic
                    || parameter.numeric_channels.len() != arity
                {
                    return Err(invalid(
                        "OpenFX numeric metadata does not match its value shape",
                    ));
                }
                let defaults = value.native_fields().1;
                for (index, channel) in parameter.numeric_channels.iter().enumerate() {
                    let bounds = [
                        channel.minimum,
                        channel.maximum,
                        channel.display_minimum,
                        channel.display_maximum,
                    ];
                    if bounds.iter().any(|bound| {
                        !bound.is_finite() || (integer && checked_openfx_integer(*bound).is_none())
                    }) || channel.minimum > channel.maximum
                        || channel.display_minimum > channel.display_maximum
                        || channel.display_minimum.max(channel.minimum)
                            > channel.display_maximum.min(channel.maximum)
                        || defaults[index] < channel.minimum
                        || defaults[index] > channel.maximum
                    {
                        return Err(invalid(
                            "OpenFX numeric component default or range is invalid",
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

fn bundle_for_binary(binary: &Path) -> Result<PathBuf, OpenFxHostError> {
    let bundle = binary
        .ancestors()
        .find(|ancestor| {
            ancestor
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("bundle"))
        })
        .ok_or_else(|| invalid("rendering requires a selected .ofx.bundle"))?;
    if !bundle
        .file_name()
        .is_some_and(|name| name.to_string_lossy().to_ascii_lowercase().ends_with(".ofx.bundle"))
    {
        return Err(invalid("rendering requires a selected .ofx.bundle"));
    }
    Ok(bundle.to_path_buf())
}

fn native_string(path: &Path) -> Result<CString, OpenFxHostError> {
    CString::new(
        path.to_str()
            .ok_or_else(|| invalid("OpenFX native path is not valid Unicode"))?,
    )
    .map_err(|_| invalid("OpenFX native path contains a null byte"))
}

fn invalid(reason: impl Into<String>) -> OpenFxHostError {
    OpenFxHostError::Invalid(reason.into())
}

fn unavailable(reason: impl Into<String>) -> OpenFxHostError {
    OpenFxHostError::Unavailable(reason.into())
}

#[cfg(test)]
mod shared_frame_tests {
    use super::*;

    #[test]
    fn compound_descriptors_preserve_shapes_and_reject_invalid_sibling_channels() {
        let values = [
            OpenFxParameterValue::Double2([0.12500000000000003, 0.75]),
            OpenFxParameterValue::Double3([-0.5000000000000001, 0.0, 0.75]),
            OpenFxParameterValue::Rgb([0.10000000000000002, 0.2, 0.3]),
            OpenFxParameterValue::Rgba([0.12500000000000003, 0.25, 0.5, 0.75]),
            OpenFxParameterValue::Integer2([i32::MIN, i32::MAX]),
            OpenFxParameterValue::Integer3([i32::MIN, 0, i32::MAX]),
        ];
        for (index, value) in values.into_iter().enumerate() {
            let (arity, integer) = value.numeric_layout().expect("numeric shape");
            assert_eq!(value.native_fields().0, index as c_int + 7);
            assert_eq!(
                serde_json::from_str::<OpenFxParameterValue>(
                    &serde_json::to_string(&value).expect("serialize")
                )
                .expect("deserialize"),
                value
            );
            let mut description = OpenFxFilterDescription {
                identifier: "test.compound".into(),
                label: "Compound".into(),
                parameters: vec![OpenFxParameterDescription {
                    name: "compound".into(),
                    label: "Compound".into(),
                    hint: String::new(),
                    double_type: matches!(
                        value,
                        OpenFxParameterValue::Double2(_) | OpenFxParameterValue::Double3(_)
                    )
                    .then_some(OpenFxDoubleType::Plain),
                    default_value: value,
                    choices: Vec::new(),
                    preferred_interpolation:
                        mondrian_core::automation::ParameterInterpolation::Linear,
                    string_mode: None,
                    numeric_channels: (0..arity)
                        .map(|channel| OpenFxNumericChannel {
                            minimum: if integer {
                                f64::from(i32::MIN)
                            } else {
                                -(channel as f64 + 1.0)
                            },
                            maximum: if integer {
                                f64::from(i32::MAX)
                            } else {
                                channel as f64 + 1.0
                            },
                            display_minimum: if integer { f64::from(i32::MIN) } else { -1.0 },
                            display_maximum: if integer { f64::from(i32::MAX) } else { 1.0 },
                        })
                        .collect(),
                    can_animate: false,
                    secret: false,
                    enabled: true,
                }],
            };
            assert!(validate_description(&description, "test.compound").is_ok());
            let original = description.clone();
            description.parameters[0].numeric_channels.pop();
            assert!(validate_description(&description, "test.compound").is_err());
            description = original.clone();
            description.parameters[0].numeric_channels[arity - 1].maximum = 0.0;
            assert!(validate_description(&description, "test.compound").is_err());
            description = original.clone();
            description.parameters[0].numeric_channels[arity - 1].display_minimum = f64::NAN;
            assert!(validate_description(&description, "test.compound").is_err());
            description = original;
            description.parameters[0].numeric_channels[arity - 1].minimum =
                if integer { -1.5 } else { 2.0 };
            assert!(validate_description(&description, "test.compound").is_err());
        }
        let invalid = BTreeMap::from([(
            "point".into(),
            OpenFxParameterValue::Double3([0.0, f64::NAN, 0.0]),
        )]);
        assert!(validate_parameter_values_map(&invalid).is_err());
    }

    #[test]
    fn integer_descriptor_rejects_fractional_nonfinite_and_out_of_range_bounds() {
        for value in [f64::from(i32::MIN), -1.0, 0.0, f64::from(i32::MAX)] {
            assert_eq!(checked_openfx_integer(value).map(f64::from), Some(value));
        }
        for value in [
            f64::NAN,
            f64::INFINITY,
            1.5,
            f64::from(i32::MIN) - 1.0,
            f64::from(i32::MAX) + 1.0,
        ] {
            assert_eq!(checked_openfx_integer(value), None);
        }
        let mut description = OpenFxFilterDescription {
            identifier: "test.integer".into(),
            label: "Integer".into(),
            parameters: vec![OpenFxParameterDescription {
                name: "count".into(),
                label: "Count".into(),
                hint: String::new(),
                default_value: OpenFxParameterValue::Integer(2),
                choices: Vec::new(),
                preferred_interpolation: mondrian_core::automation::ParameterInterpolation::Linear,
                double_type: None,
                string_mode: None,
                numeric_channels: vec![OpenFxNumericChannel {
                    minimum: 0.0,
                    maximum: 9.0,
                    display_minimum: 0.0,
                    display_maximum: 9.0,
                }],
                can_animate: true,
                secret: false,
                enabled: true,
            }],
        };
        assert!(validate_description(&description, "test.integer").is_ok());
        for bound in [
            None,
            Some(0.5),
            Some(f64::NAN),
            Some(f64::from(i32::MIN) - 1.0),
            Some(3.0),
            Some(10.0),
        ] {
            description.parameters[0].numeric_channels = bound.map_or_else(Vec::new, |minimum| {
                vec![OpenFxNumericChannel {
                    minimum,
                    maximum: 9.0,
                    display_minimum: 0.0,
                    display_maximum: 9.0,
                }]
            });
            assert!(validate_description(&description, "test.integer").is_err());
        }
    }

    fn choice_description_fixture() -> OpenFxFilterDescription {
        OpenFxFilterDescription {
            identifier: "test.choice".into(),
            label: "Choice".into(),
            parameters: vec![OpenFxParameterDescription {
                name: "mode".into(),
                label: "Mode".into(),
                hint: String::new(),
                default_value: OpenFxParameterValue::Choice(1),
                choices: vec![
                    OpenFxChoiceOption {
                        key: "0".into(),
                        label: "同じ表示".into(),
                        display_rank: Some(i32::MAX),
                    },
                    OpenFxChoiceOption {
                        key: "1".into(),
                        label: "同じ表示".into(),
                        display_rank: Some(i32::MIN),
                    },
                ],
                preferred_interpolation: mondrian_core::automation::ParameterInterpolation::Hold,
                double_type: None,
                string_mode: None,
                numeric_channels: Vec::new(),
                can_animate: false,
                secret: false,
                enabled: true,
            }],
        }
    }

    #[test]
    fn string_descriptor_requires_exact_metadata_and_bounded_utf8_snapshots() {
        let mut valid = choice_description_fixture();
        let parameter = &mut valid.parameters[0];
        parameter.choices.clear();
        parameter.default_value = OpenFxParameterValue::String("汉字🙂\r\n\t".into());
        for mode in [
            OpenFxStringMode::SingleLine,
            OpenFxStringMode::MultiLine,
            OpenFxStringMode::FilePath { must_exist: true },
            OpenFxStringMode::FilePath { must_exist: false },
            OpenFxStringMode::DirectoryPath { must_exist: true },
            OpenFxStringMode::DirectoryPath { must_exist: false },
            OpenFxStringMode::Label,
            OpenFxStringMode::RichTextFormat,
        ] {
            valid.parameters[0].string_mode = Some(mode);
            assert!(validate_description(&valid, "test.choice").is_ok());
            let json = serde_json::to_vec(&valid).expect("serialize String");
            assert_eq!(
                serde_json::from_slice::<OpenFxFilterDescription>(&json).expect("String metadata"),
                valid
            );
        }
        for scenario in 0..6 {
            let mut invalid = valid.clone();
            let parameter = &mut invalid.parameters[0];
            match scenario {
                0 => parameter.string_mode = None,
                1 => {
                    parameter.numeric_channels = vec![OpenFxNumericChannel {
                        minimum: 0.0,
                        maximum: 1.0,
                        display_minimum: 0.0,
                        display_maximum: 1.0,
                    }]
                }
                2 => parameter.double_type = Some(OpenFxDoubleType::Plain),
                3 => parameter.default_value = OpenFxParameterValue::String("a\0b".into()),
                4 => {
                    parameter.default_value =
                        OpenFxParameterValue::String("x".repeat(OPENFX_MAX_STRING_BYTES + 1))
                }
                _ => {
                    parameter.preferred_interpolation =
                        mondrian_core::automation::ParameterInterpolation::Linear
                }
            }
            assert!(
                validate_description(&invalid, "test.choice").is_err(),
                "scenario {scenario}"
            );
        }
        for json in [
            r#"{"mode":"file_path"}"#,
            r#"{"mode":"single_line","must_exist":true}"#,
            r#"{"mode":"unknown"}"#,
            r#"{"mode":"directory_path","must_exist":1}"#,
        ] {
            assert!(serde_json::from_str::<OpenFxStringMode>(json).is_err());
        }
        let mut values = BTreeMap::new();
        for index in 0..16 {
            values.insert(
                format!("string{index}"),
                OpenFxParameterValue::String("x".repeat(OPENFX_MAX_STRING_BYTES)),
            );
        }
        assert!(validate_parameter_values_map(&values).is_ok());
        values.insert("extra".into(), OpenFxParameterValue::String("x".into()));
        assert!(validate_parameter_values_map(&values).is_err());
        valid.parameters[0].default_value = OpenFxParameterValue::Boolean(false);
        assert!(validate_description(&valid, "test.choice").is_err());
    }

    #[test]
    fn choice_descriptor_preserves_indices_and_rejects_invalid_metadata() {
        let valid = choice_description_fixture();
        assert!(validate_description(&valid, "test.choice").is_ok());
        let restored: OpenFxFilterDescription =
            serde_json::from_slice(&serde_json::to_vec(&valid).expect("serialize descriptor"))
                .expect("restore descriptor");
        assert_eq!(restored.parameters[0].choices, valid.parameters[0].choices);
        assert_eq!(
            restored.parameters[0].default_value,
            OpenFxParameterValue::Choice(1)
        );
        for index in [-1, 2, i32::MAX] {
            let mut invalid = valid.clone();
            invalid.parameters[0].default_value = OpenFxParameterValue::Choice(index);
            assert!(validate_description(&invalid, "test.choice").is_err());
        }
        for scenario in 0..8 {
            let mut invalid = valid.clone();
            let parameter = &mut invalid.parameters[0];
            match scenario {
                0 => parameter.choices.clear(),
                1 => parameter.choices[1].display_rank = Some(i32::MAX),
                2 => parameter.choices[1].display_rank = None,
                3 => parameter.choices[1].label = " \t".into(),
                4 => parameter.choices[1].label = "bad\0label".into(),
                5 => {
                    parameter.numeric_channels = vec![OpenFxNumericChannel {
                        minimum: 0.0,
                        maximum: 1.0,
                        display_minimum: 0.0,
                        display_maximum: 1.0,
                    }]
                }
                6 => {
                    parameter.preferred_interpolation =
                        mondrian_core::automation::ParameterInterpolation::Linear
                }
                _ => parameter.default_value = OpenFxParameterValue::Boolean(false),
            }
            assert!(
                validate_description(&invalid, "test.choice").is_err(),
                "scenario {scenario}"
            );
        }
        let mut natural = valid.clone();
        for option in &mut natural.parameters[0].choices {
            option.display_rank = None;
        }
        assert!(validate_description(&natural, "test.choice").is_ok());
        assert!(validate_parameter_values_map(&BTreeMap::from([(
            "mode".into(),
            OpenFxParameterValue::Choice(-1)
        )]))
        .is_err());
        assert!(validate_parameter_values_map(&BTreeMap::from([(
            "mode".into(),
            OpenFxParameterValue::Choice(0)
        )]))
        .is_ok());
    }

    #[test]
    fn string_choice_descriptor_retains_declared_keys_and_rejects_invalid_metadata() {
        let mut valid = choice_description_fixture();
        valid.parameters[0].choices[0].key = "vendor.ä".into();
        valid.parameters[0].choices[1].key = "vendor.β".into();
        valid.parameters[0].default_value = OpenFxParameterValue::StrChoice("vendor.β".into());
        assert!(validate_description(&valid, "test.choice").is_ok());
        let restored: OpenFxFilterDescription =
            serde_json::from_slice(&serde_json::to_vec(&valid).expect("serialize string choice"))
                .expect("restore string choice");
        assert_eq!(restored, valid);
        for scenario in 0..5 {
            let mut invalid = valid.clone();
            let parameter = &mut invalid.parameters[0];
            match scenario {
                0 => {
                    parameter.default_value =
                        OpenFxParameterValue::StrChoice("vendor.unknown".into())
                }
                1 => parameter.choices[0].key = "vendor.β".into(),
                2 => parameter.choices[0].key.clear(),
                3 => parameter.choices[0].key = "bad\0key".into(),
                _ => parameter.choices[0].key = "x".repeat(257),
            }
            assert!(
                validate_description(&invalid, "test.choice").is_err(),
                "scenario {scenario}"
            );
        }
        for value in ["", "bad\0key", "\n", &"x".repeat(257)] {
            assert!(validate_parameter_values_map(&BTreeMap::from([(
                "mode".into(),
                OpenFxParameterValue::StrChoice(value.into())
            )]))
            .is_err());
        }
        assert!(validate_parameter_values_map(&BTreeMap::from([(
            "mode".into(),
            OpenFxParameterValue::StrChoice("vendor.β".into())
        )]))
        .is_ok());
    }

    // Each native qualification runs in its own bounded child; test threads never
    // change process-wide environment or load a native plugin in the supervisor.
    fn qualification_child(test: &str, environment: &[(&str, &str)]) -> bool {
        const CHILD: &str = "MONDRIAN_OPENFX_QUALIFICATION_CHILD";
        if std::env::var(CHILD).ok().as_deref() == Some(test) {
            return true;
        }
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        command
            .args(["--exact", test, "--ignored", "--nocapture"])
            .env(CHILD, test)
            .stdin(Stdio::null());
        for (key, value) in environment {
            command.env(key, value);
        }
        let mut child = command.spawn().expect("isolated native qualification child");
        let deadline = Instant::now() + DEADLINE;
        loop {
            if let Some(status) = child.try_wait().expect("observe qualification child") {
                assert!(status.success(), "native qualification failed: {status}");
                return false;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("native qualification child exceeded deadline");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    #[ignore = "requires the owned SessionProbe native bundle"]
    fn native_compound_parameters_preserve_exact_defaults_and_atomic_sdk_updates() {
        const TEST: &str = "openfx_host::shared_frame_tests::native_compound_parameters_preserve_exact_defaults_and_atomic_sdk_updates";
        qualify_native_compound_parameters(TEST, "compound");
    }

    #[test]
    #[ignore = "requires the owned SessionProbe native bundle"]
    fn native_animatable_constant_parameters_support_off_time_queries() {
        const TEST: &str = "openfx_host::shared_frame_tests::native_animatable_constant_parameters_support_off_time_queries";
        qualify_native_compound_parameters(TEST, "compound_constant_animatable");
    }

    #[test]
    #[ignore = "requires the owned SessionProbe native bundle"]
    fn native_frame_sample_parameters_reject_unprovided_curve_queries() {
        const TEST: &str = "openfx_host::shared_frame_tests::native_frame_sample_parameters_reject_unprovided_curve_queries";
        qualify_native_compound_parameters(TEST, "compound_frame_sample");
    }

    fn qualify_native_compound_parameters(test: &str, mode: &str) {
        if !qualification_child(test, &[("MONDRIAN_OPENFX_SESSION_PROBE_MODE", mode)]) {
            return;
        }
        let path = PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY").expect("probe binary"),
        );
        let path = resolve_openfx_binary(&path).expect("probe binary path");
        let bundle = path
            .ancestors()
            .find(|path| path.extension().is_some_and(|extension| extension == "bundle"))
            .expect("probe bundle");
        let described = describe_in_worker(&DescribeRequest {
            binary_path: path.clone(),
            bundle_path: bundle.to_path_buf(),
            binary_sha256: sha256_file(&path).expect("fixture identity"),
            plugin_identifier: "uk.co.thefoundry.BasicGainPlugin".into(),
        })
        .expect("actual SDK compound description");
        let DescribeResponse::Success { description } = described else {
            panic!("all six SDK compound descriptions must be admitted");
        };
        let scale = description
            .parameters
            .iter()
            .find(|parameter| parameter.name == "scale")
            .expect("official gain with SDK default upper limit");
        assert_eq!(
            scale.numeric_channels[0].maximum,
            f64::MAX,
            "SDK default limits must preserve the complete Double domain"
        );
        let binary = native_string(&path).expect("native binary");
        let bundle = native_string(bundle).expect("native bundle");
        let identifier = CString::new("uk.co.thefoundry.BasicGainPlugin").expect("identifier");
        let info = NativeFrameInfo {
            width: 1,
            height: 1,
            frame: 0.0,
            frame_rate: 24.0,
            first_frame: 0.0,
            last_frame: 100.0,
            pixel_aspect_ratio: 1.0,
        };
        let input = [0.25_f32; 4].into_iter().flat_map(f32::to_ne_bytes).collect::<Vec<_>>();
        let specs = [
            (
                "point2",
                7,
                [0.12500000000000003, 10.000000000000002, 0.0, 0.0],
            ),
            (
                "point3",
                8,
                [
                    -0.5000000000000001,
                    1.0000000000000002,
                    2.0000000000000004,
                    0.0,
                ],
            ),
            (
                "rgb",
                9,
                [
                    0.10000000000000002,
                    0.20000000000000004,
                    0.30000000000000004,
                    0.0,
                ],
            ),
            (
                "rgba",
                10,
                [
                    0.12500000000000003,
                    0.25000000000000006,
                    0.5000000000000001,
                    0.7500000000000001,
                ],
            ),
            ("integer2", 11, [-123456789.0, 123456789.0, 0.0, 0.0]),
            (
                "integer3",
                12,
                [f64::from(i32::MIN), 0.0, f64::from(i32::MAX), 0.0],
            ),
        ];
        let names = specs
            .iter()
            .map(|(name, _, _)| CString::new(*name).expect("name"))
            .collect::<Vec<_>>();
        let parameters = specs
            .iter()
            .zip(&names)
            .map(|((key, kind, value), name)| {
                let parameter = description
                    .parameters
                    .iter()
                    .find(|parameter| parameter.name == *key)
                    .expect("declared tuple");
                assert_eq!(
                    parameter.can_animate,
                    matches!(
                        mode,
                        "compound_constant_animatable" | "compound_frame_sample"
                    )
                );
                let (native_kind, native_value) = parameter.default_value.native_fields();
                assert_eq!(native_kind, *kind);
                assert_eq!(
                    native_value, *value,
                    "description must preserve exact SDK defaults"
                );
                let (arity, _) = parameter.default_value.numeric_layout().expect("numeric tuple");
                assert_eq!(parameter.numeric_channels.len(), arity);
                assert_eq!(
                    parameter.preferred_interpolation,
                    mondrian_core::automation::ParameterInterpolation::Linear
                );
                assert!(
                    parameter.numeric_channels.windows(2).all(|pair| pair[0] != pair[1]),
                    "asymmetric component bounds must remain distinct"
                );
                NativeParameter {
                    query_mode: i32::from(mode == "compound_frame_sample"),
                    name: name.as_ptr(),
                    kind: native_kind,
                    value: native_value,
                    string_value: std::ptr::null(),
                    string_length: 0,
                }
            })
            .collect::<Vec<_>>();
        let mut session =
            NativeSession::create(&binary, &bundle, &identifier, &input, &info, &parameters)
                .expect("complete native tuple instance");
        let mut output = vec![0xa5; input.len()];
        assert_eq!(session.render(&input, &mut output, &info, &parameters), 0);
        assert_eq!(output, input);
        let mut invalid = parameters;
        invalid[0].value[0] = 0.5;
        invalid[5].value[1] = 5.0;
        let mut unchanged = vec![0xa5; input.len()];
        assert_ne!(session.render(&input, &mut unchanged, &info, &invalid), 0);
        assert_eq!(unchanged, vec![0xa5; input.len()]);
        let mut recovered = vec![0xa5; input.len()];
        invalid[0].value[0] = 0.12500000000000003;
        invalid[5].value[1] = 0.0;
        assert_eq!(session.render(&input, &mut recovered, &info, &invalid), 0);
        assert_eq!(recovered, input);
    }

    #[test]
    #[ignore = "set MONDRIAN_OPENFX_SESSION_PROBE_BINARY to SessionProbe.ofx.bundle"]
    fn native_session_enforces_independent_frame_and_action_contracts() {
        const TEST: &str = "openfx_host::shared_frame_tests::native_session_enforces_independent_frame_and_action_contracts";
        const MODE: &str = "MONDRIAN_OPENFX_SESSION_PROBE_MODE";
        for mode in [
            "normal",
            "optional",
            "required",
            "temporal",
            "instance_required",
            "become_required",
            "unfetched",
            "leak_source",
        ] {
            if !qualification_child(TEST, &[(MODE, mode)]) {
                continue;
            }
            let mode = std::env::var(MODE).expect("probe scenario");
            let path = PathBuf::from(
                std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY")
                    .expect("SessionProbe binary is required"),
            );
            let path = resolve_openfx_binary(&path).expect("probe binary");
            let bundle_path = path
                .ancestors()
                .find(|path| path.extension().is_some_and(|extension| extension == "bundle"))
                .expect("probe bundle");
            let identifier = "uk.co.thefoundry.BasicGainPlugin";
            let description = describe_in_worker(&DescribeRequest {
                binary_path: path.clone(),
                bundle_path: bundle_path.to_path_buf(),
                binary_sha256: sha256_file(&path).expect("probe fingerprint"),
                plugin_identifier: identifier.to_owned(),
            })
            .expect("probe description");
            let descriptor_rejected = matches!(mode.as_str(), "required" | "temporal");
            if descriptor_rejected {
                assert!(matches!(
                    description,
                    DescribeResponse::Rejected { status: 7, .. }
                ));
            } else {
                assert!(matches!(description, DescribeResponse::Success { .. }));
            }
            let binary = native_string(&path).expect("binary path");
            let bundle = native_string(bundle_path).expect("bundle path");
            let identifier = CString::new(identifier).expect("identifier");
            let name = CString::new("scale").expect("parameter");
            let info = NativeFrameInfo {
                width: 1,
                height: 1,
                frame: 0.0,
                frame_rate: 24.0,
                first_frame: 0.0,
                last_frame: 100.0,
                pixel_aspect_ratio: 1.0,
            };
            let input =
                [0.25_f32; 4].iter().flat_map(|value| value.to_ne_bytes()).collect::<Vec<_>>();
            let parameters = |gain| {
                [NativeParameter {
                    query_mode: 0,
                    name: name.as_ptr(),
                    kind: 1,
                    value: [gain, 0.0, 0.0, 0.0],
                    string_value: std::ptr::null(),
                    string_length: 0,
                }]
            };
            let created = NativeSession::create(
                &binary,
                &bundle,
                &identifier,
                &input,
                &info,
                &parameters(1.5),
            );
            if descriptor_rejected || mode == "instance_required" {
                assert_eq!(created.err(), Some(7));
                return;
            }
            let mut session = created.expect("admitted probe session");
            let mut output = vec![0xa5; input.len()];
            if mode == "leak_source" {
                let source_before = input.clone();
                assert_eq!(
                    session.render(&input, &mut output, &info, &parameters(1.5)),
                    5
                );
                assert_eq!(input, source_before);
                assert_eq!(
                    session.render(&input, &mut output, &info, &parameters(1.5)),
                    2
                );
                return;
            }

            assert_eq!(
                session.render(&input, &mut output, &info, &parameters(1.5)),
                0
            );
            assert_eq!(
                output,
                [0.375_f32; 4].iter().flat_map(|value| value.to_ne_bytes()).collect::<Vec<_>>()
            );
            output.fill(0xa5);
            let status = session.render(&input, &mut output, &info, &parameters(2.0));
            if matches!(mode.as_str(), "unfetched" | "become_required") {
                assert_eq!(status, if mode == "unfetched" { 5 } else { 7 });
                assert_eq!(output, vec![0xa5; input.len()]);
                assert_eq!(
                    session.render(&input, &mut output, &info, &parameters(1.0)),
                    2
                );
            } else {
                assert_eq!(status, 0);
                assert_eq!(
                    output,
                    [0.5_f32; 4].iter().flat_map(|value| value.to_ne_bytes()).collect::<Vec<_>>()
                );
            }
            return;
        }
    }

    #[test]
    #[ignore = "set MONDRIAN_OPENFX_REFERENCE_BINARY to the official Basic.ofx.bundle"]
    fn official_basic_native_session_reuses_authored_frames_in_supervised_child() {
        if !qualification_child(
            "openfx_host::shared_frame_tests::official_basic_native_session_reuses_authored_frames_in_supervised_child", &[],
        ) { return; }
        let path = PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_REFERENCE_BINARY")
                .expect("official Basic binary is required"),
        );
        let path = resolve_openfx_binary(&path).expect("reference binary path");
        let bundle_path = path
            .ancestors()
            .find(|path| path.extension().is_some_and(|extension| extension == "bundle"))
            .expect("reference bundle");
        let binary = native_string(&path).expect("native binary path");
        let bundle = native_string(bundle_path).expect("native bundle path");
        let identifier = CString::new("uk.co.thefoundry.BasicGainPlugin").expect("identifier");
        let name = CString::new("scale").expect("parameter name");
        let mut info = NativeFrameInfo {
            width: 2,
            height: 3,
            frame: 1.0,
            frame_rate: 24.0,
            first_frame: 0.0,
            last_frame: 100.0,
            pixel_aspect_ratio: 1.0,
        };
        let original = [[-0.25_f32, 0.5, 2.0, 0.25]; 6];
        let encode = |pixels: &[[f32; 4]]| {
            pixels
                .iter()
                .flat_map(|pixel| pixel.iter().flat_map(|value| value.to_ne_bytes()))
                .collect::<Vec<_>>()
        };
        let mut input = encode(&original);
        let parameters = |gain| {
            [NativeParameter {
                query_mode: 0,
                name: name.as_ptr(),
                kind: 1,
                value: [gain, 0.0, 0.0, 0.0],
                string_value: std::ptr::null(),
                string_length: 0,
            }]
        };
        // Repeated destruction/recreation also checks cache handler ownership.
        for _ in 0..3 {
            let mut session = NativeSession::create(
                &binary,
                &bundle,
                &identifier,
                &input,
                &info,
                &parameters(1.5),
            )
            .expect("create native session");
            for (frame, gain, aspect) in [
                (1.0, 1.5, 1.0),
                (1.0, 2.0, 1.0),
                (42.0, 0.75, 1.2),
                (0.0, 1.25, 1.0),
            ] {
                info.frame = frame;
                info.pixel_aspect_ratio = aspect;
                let mut output = vec![0xa5; input.len()];
                assert_eq!(
                    session.render(&input, &mut output, &info, &parameters(gain)),
                    0
                );
                let expected = original.map(|mut pixel| {
                    for channel in &mut pixel {
                        *channel *= gain as f32;
                    }
                    pixel
                });
                assert_eq!(output, encode(&expected));
            }
            // Omitted controls use descriptor defaults, never prior-frame values.
            let replacement = [[0.5_f32, -1.0, 3.0, 0.75]; 6];
            let changed_input = encode(&replacement);
            let mut output = vec![0xa5; changed_input.len()];
            assert_eq!(session.render(&changed_input, &mut output, &info, &[]), 0);
            assert_eq!(output, changed_input);
            info.width = 3;
            let mut output = vec![0xa5; input.len()];
            assert_ne!(
                session.render(&input, &mut output, &info, &parameters(1.0)),
                0
            );
            assert_eq!(output, vec![0xa5; input.len()]);
            info.width = 2;
            assert_ne!(
                session.render(&input, &mut output, &info, &parameters(1.0)),
                0,
                "failed instance is retired"
            );
        }
        // Nonfinite input cannot produce or retain a usable native instance.
        input[..4].copy_from_slice(&f32::NAN.to_ne_bytes());
        assert!(NativeSession::create(
            &binary,
            &bundle,
            &identifier,
            &input,
            &info,
            &parameters(1.0)
        )
        .is_err());
    }
}
