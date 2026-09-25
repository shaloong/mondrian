//! Supervised Float32 OpenFX filter execution for one selected native binary.

use std::collections::BTreeMap;
use std::ffi::{c_char, c_int, c_void, CString};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::openfx_adapter::{
    resolve_openfx_binary, sha256_file, OpenFxBinaryInspection, OpenFxDiscoveryError,
};

/// Hidden product executable mode for one native Float32 filter render.
pub const OPENFX_RENDER_WORKER_ARGUMENT: &str = "--internal-openfx-render-v1";
/// Hidden product executable mode for selected filter description.
pub const OPENFX_DESCRIBE_WORKER_ARGUMENT: &str = "--internal-openfx-describe-v1";
const REQUEST_ENV: &str = "MONDRIAN_INTERNAL_OPENFX_RENDER_REQUEST";
const RESPONSE_ENV: &str = "MONDRIAN_INTERNAL_OPENFX_RENDER_RESPONSE";
const MAX_FRAME_BYTES: usize = 128 * 1024 * 1024;
const MAX_PARAMETERS: usize = 4096;
const DEADLINE: Duration = Duration::from_secs(30);
const MAX_WORKER_REQUEST_BYTES: usize = 2 * 1024 * 1024;
const MAX_WORKER_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

/// A scalar OFX parameter value supported by the current CPU filter host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum OpenFxScalarValue {
    /// A finite OFX Double parameter value.
    Double(f64),
    /// An OFX Boolean parameter value.
    Boolean(bool),
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
    pub default_value: OpenFxScalarValue,
    /// Semantic kind of a Double control.
    pub double_type: Option<OpenFxDoubleType>,
    /// Lowest accepted numeric value, when this is a Double parameter.
    pub minimum: Option<f64>,
    /// Highest accepted numeric value, when this is a Double parameter.
    pub maximum: Option<f64>,
    /// Suggested slider lower bound, when this is a Double parameter.
    pub display_minimum: Option<f64>,
    /// Suggested slider upper bound, when this is a Double parameter.
    pub display_maximum: Option<f64>,
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
    /// The native process exited without a valid result.
    #[error("OpenFX native worker failed: {0}")]
    WorkerFailed(String),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RenderRequest {
    binary_path: PathBuf,
    bundle_path: PathBuf,
    binary_sha256: String,
    plugin_identifier: String,
    width: u32,
    height: u32,
    timing: OpenFxRenderTiming,
    parameters: BTreeMap<String, OpenFxScalarValue>,
    input_path: PathBuf,
    output_path: PathBuf,
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
enum RenderResponse {
    Success,
    Rejected { status: i32 },
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
    kind: c_int,
    value: f64,
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
struct NativeParameterInfo {
    name: *const c_char,
    name_length: usize,
    label: *const c_char,
    label_length: usize,
    hint: *const c_char,
    hint_length: usize,
    kind: c_int,
    double_type: c_int,
    default_value: f64,
    minimum: f64,
    maximum: f64,
    display_minimum: f64,
    display_maximum: f64,
    can_animate: c_int,
    secret: c_int,
    enabled: c_int,
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
    fn mondrian_openfx_render(
        binary: *const c_char,
        bundle: *const c_char,
        identifier: *const c_char,
        input: *const c_char,
        output: *const c_char,
        frame_info: *const NativeFrameInfo,
        parameters: *const NativeParameter,
        parameter_count: c_int,
    ) -> c_int;
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
    parameters: &BTreeMap<String, OpenFxScalarValue>,
    frame: &OpenFxFloatFrame,
) -> Result<OpenFxFloatFrame, OpenFxHostError> {
    if !helper_executable.is_absolute() || !helper_executable.is_file() {
        return Err(invalid("helper must be an absolute regular file"));
    }
    let frame_bytes = validate_frame(frame.width, frame.height, frame.pixels.len())?;
    if frame.pixels.iter().flatten().any(|channel| !channel.is_finite()) {
        return Err(invalid("OpenFX input contains a nonfinite channel"));
    }
    validate_timing(timing)?;
    validate_parameters(parameters)?;
    let bundle = verified_selection(inspection, plugin_identifier)?;
    let staging = tempfile::tempdir()
        .map_err(|error| unavailable(format!("render staging failed: {error}")))?;
    let input_path = staging.path().join("input.rgba32f");
    let output_path = staging.path().join("output.rgba32f");
    let mut input = Vec::with_capacity(frame_bytes);
    for pixel in &frame.pixels {
        for channel in pixel {
            input.extend_from_slice(&channel.to_le_bytes());
        }
    }
    fs::write(&input_path, input)
        .map_err(|error| unavailable(format!("input staging failed: {error}")))?;
    let request = RenderRequest {
        binary_path: inspection.binary_path.clone(),
        bundle_path: bundle,
        binary_sha256: inspection.binary_sha256.clone(),
        plugin_identifier: plugin_identifier.to_owned(),
        width: frame.width,
        height: frame.height,
        timing: timing.clone(),
        parameters: parameters.clone(),
        input_path,
        output_path: output_path.clone(),
    };
    let response: RenderResponse = invoke_worker(
        helper_executable,
        OPENFX_RENDER_WORKER_ARGUMENT,
        staging.path(),
        &request,
    )?;
    if let RenderResponse::Rejected { status } = response {
        return Err(OpenFxHostError::WorkerFailed(format!(
            "native status {status}"
        )));
    }
    if sha256_file(&inspection.binary_path)? != inspection.binary_sha256 {
        return Err(invalid("OpenFX binary changed during render"));
    }
    let metadata = fs::metadata(&output_path)
        .map_err(|error| unavailable(format!("render output is unavailable: {error}")))?;
    if !metadata.is_file() || metadata.len() != frame_bytes as u64 {
        return Err(unavailable("render output has an invalid size"));
    }
    let bytes = fs::read(&output_path)
        .map_err(|error| unavailable(format!("render output cannot be read: {error}")))?;
    if bytes.len() != frame_bytes {
        return Err(unavailable("render output changed size while reading"));
    }
    let pixels: Vec<[f32; 4]> = bytes
        .chunks_exact(16)
        .map(|pixel| {
            let channel = |index| {
                f32::from_le_bytes([
                    pixel[index],
                    pixel[index + 1],
                    pixel[index + 2],
                    pixel[index + 3],
                ])
            };
            [channel(0), channel(4), channel(8), channel(12)]
        })
        .collect();
    if pixels.iter().flatten().any(|channel| !channel.is_finite()) {
        return Err(unavailable("render output contains a nonfinite channel"));
    }
    Ok(OpenFxFloatFrame { width: frame.width, height: frame.height, pixels })
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

/// Run the native render entrypoint before graphics or media startup.
pub fn run_openfx_render_worker() -> Result<(), OpenFxHostError> {
    let (request, response_path): (RenderRequest, PathBuf) =
        read_worker_request(OPENFX_RENDER_WORKER_ARGUMENT)?;
    let status = render_in_worker(&request)?;
    let response = if status == 0 {
        RenderResponse::Success
    } else {
        RenderResponse::Rejected { status }
    };
    write_worker_response(&response_path, &response)
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
    let (default_value, double_type, minimum, maximum, display_minimum, display_maximum) = match raw
        .kind
    {
        1 => (
            OpenFxScalarValue::Double(raw.default_value),
            Some(match raw.double_type {
                0 => OpenFxDoubleType::Plain,
                1 => OpenFxDoubleType::Scale,
                _ => return Err("unsupported OpenFX Double interpretation".to_owned()),
            }),
            Some(raw.minimum),
            Some(raw.maximum),
            Some(raw.display_minimum),
            Some(raw.display_maximum),
        ),
        2 if raw.double_type == -1 && (raw.default_value == 0.0 || raw.default_value == 1.0) => (
            OpenFxScalarValue::Boolean(raw.default_value == 1.0),
            None,
            None,
            None,
            None,
            None,
        ),
        _ => return Err("unsupported OpenFX parameter kind or default".to_owned()),
    };
    let flag = |value| match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err("invalid OpenFX parameter flag".to_owned()),
    };
    Ok(OpenFxParameterDescription {
        name: name.clone(),
        label: if label.is_empty() { name } else { label },
        hint,
        default_value,
        double_type,
        minimum,
        maximum,
        display_minimum,
        display_maximum,
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

fn render_in_worker(request: &RenderRequest) -> Result<i32, OpenFxHostError> {
    let pixels = (request.width as usize)
        .checked_mul(request.height as usize)
        .ok_or_else(|| invalid("frame dimensions overflow"))?;
    let frame_bytes = validate_frame(request.width, request.height, pixels)?;
    let input_metadata = fs::metadata(&request.input_path)
        .map_err(|error| unavailable(format!("render input is unavailable: {error}")))?;
    if !input_metadata.is_file() || input_metadata.len() != frame_bytes as u64 {
        return Err(invalid("render input has an invalid size"));
    }
    validate_parameters(&request.parameters)?;
    validate_timing(&request.timing)?;
    verify_worker_binary(
        &request.binary_path,
        &request.bundle_path,
        &request.binary_sha256,
    )?;
    let binary = native_string(&request.binary_path)?;
    let bundle = native_string(&request.bundle_path)?;
    let identifier = CString::new(request.plugin_identifier.as_str())
        .map_err(|_| invalid("plugin identifier contains a null byte"))?;
    let input = native_string(&request.input_path)?;
    let output = native_string(&request.output_path)?;
    let names = request
        .parameters
        .keys()
        .map(|name| {
            CString::new(name.as_str()).map_err(|_| invalid("parameter name contains a null byte"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let native_parameters = request
        .parameters
        .values()
        .zip(&names)
        .map(|(value, name)| match value {
            OpenFxScalarValue::Double(value) => {
                NativeParameter { name: name.as_ptr(), kind: 1, value: *value }
            }
            OpenFxScalarValue::Boolean(value) => NativeParameter {
                name: name.as_ptr(),
                kind: 2,
                value: f64::from(*value as u8),
            },
        })
        .collect::<Vec<_>>();
    let frame_info = NativeFrameInfo {
        width: request.width as c_int,
        height: request.height as c_int,
        frame: request.timing.frame,
        frame_rate: request.timing.frame_rate,
        first_frame: request.timing.first_frame,
        last_frame: request.timing.last_frame,
        pixel_aspect_ratio: request.timing.pixel_aspect_ratio,
    };
    // SAFETY: All C strings and the parameter array outlive this synchronous
    // call. Native code runs only inside this supervised child process.
    Ok(unsafe {
        mondrian_openfx_render(
            binary.as_ptr(),
            bundle.as_ptr(),
            identifier.as_ptr(),
            input.as_ptr(),
            output.as_ptr(),
            &frame_info,
            native_parameters.as_ptr(),
            native_parameters.len() as c_int,
        )
    })
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
    parameters: &BTreeMap<String, OpenFxScalarValue>,
) -> Result<(), OpenFxHostError> {
    if parameters.len() > MAX_PARAMETERS {
        return Err(invalid("too many OpenFX parameters"));
    }
    for (name, value) in parameters {
        if name.is_empty()
            || name.len() > 256
            || !name.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
        {
            return Err(invalid("OpenFX parameter name is invalid"));
        }
        if let OpenFxScalarValue::Double(value) = value
            && !value.is_finite()
        {
            return Err(invalid("OpenFX parameter value must be finite"));
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
        match &parameter.default_value {
            OpenFxScalarValue::Boolean(_) => {
                if parameter.double_type.is_some()
                    || parameter.minimum.is_some()
                    || parameter.maximum.is_some()
                    || parameter.display_minimum.is_some()
                    || parameter.display_maximum.is_some()
                {
                    return Err(invalid("OpenFX Boolean parameter has numeric bounds"));
                }
            }
            OpenFxScalarValue::Double(value) => {
                if parameter.double_type.is_none() {
                    return Err(invalid("OpenFX Double parameter lacks a semantic type"));
                }
                let (Some(minimum), Some(maximum), Some(display_minimum), Some(display_maximum)) = (
                    parameter.minimum,
                    parameter.maximum,
                    parameter.display_minimum,
                    parameter.display_maximum,
                ) else {
                    return Err(invalid("OpenFX Double parameter is missing numeric bounds"));
                };
                if !value.is_finite()
                    || !minimum.is_finite()
                    || !maximum.is_finite()
                    || !display_minimum.is_finite()
                    || !display_maximum.is_finite()
                    || minimum > maximum
                    || display_minimum > display_maximum
                {
                    return Err(invalid(
                        "OpenFX Double parameter has invalid numeric bounds",
                    ));
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
