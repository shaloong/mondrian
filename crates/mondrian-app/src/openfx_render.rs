//! Supervised Float32 OpenFX filter execution for one selected native binary.

use std::collections::BTreeMap;
use std::ffi::{c_char, c_int, CString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::openfx_adapter::{
    resolve_openfx_binary, sha256_file, OpenFxBinaryInspection, OpenFxDiscoveryError,
};

/// Hidden product executable mode for one native Float32 filter render.
pub const OPENFX_RENDER_WORKER_ARGUMENT: &str = "--internal-openfx-render-v1";
const REQUEST_ENV: &str = "MONDRIAN_INTERNAL_OPENFX_RENDER_REQUEST";
const RESPONSE_ENV: &str = "MONDRIAN_INTERNAL_OPENFX_RENDER_RESPONSE";
const MAX_FRAME_BYTES: usize = 128 * 1024 * 1024;
const MAX_PARAMETERS: usize = 4096;
const DEADLINE: Duration = Duration::from_secs(30);

/// A scalar OFX parameter value supported by the current CPU filter host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum OpenFxScalarValue {
    /// A finite OFX Double parameter value.
    Double(f64),
    /// An OFX Boolean parameter value.
    Boolean(bool),
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

/// Render failure that leaves the caller's input frame unchanged.
#[derive(Debug, thiserror::Error)]
pub enum OpenFxRenderError {
    /// The request cannot be represented by the admitted filter contract.
    #[error("invalid OpenFX render request: {0}")]
    Invalid(String),
    /// The binary identity or bundle could not be verified.
    #[error(transparent)]
    Discovery(#[from] OpenFxDiscoveryError),
    /// The isolated child or its output could not be used.
    #[error("OpenFX render unavailable: {0}")]
    Unavailable(String),
    /// The native render exceeded its supervised deadline.
    #[error("OpenFX render worker exceeded its deadline")]
    DeadlineExceeded,
    /// The native process exited without a valid result.
    #[error("OpenFX render worker failed: {0}")]
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
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum RenderResponse {
    Success,
    Rejected { status: i32 },
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

unsafe extern "C" {
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
) -> Result<OpenFxFloatFrame, OpenFxRenderError> {
    if !helper_executable.is_absolute() || !helper_executable.is_file() {
        return Err(invalid("helper must be an absolute regular file"));
    }
    let frame_bytes = validate_frame(frame.width, frame.height, frame.pixels.len())?;
    validate_timing(timing)?;
    if !inspection.plugins.iter().any(|plugin| plugin.identifier == plugin_identifier) {
        return Err(invalid("selected plugin is absent from the pinned binary"));
    }
    validate_parameters(parameters)?;
    let bundle = bundle_for_binary(&inspection.binary_path)?;
    if resolve_openfx_binary(&bundle)? != inspection.binary_path
        || sha256_file(&inspection.binary_path)? != inspection.binary_sha256
    {
        return Err(invalid("selected OpenFX binary changed since discovery"));
    }
    let staging = tempfile::tempdir()
        .map_err(|error| unavailable(format!("render staging failed: {error}")))?;
    let input_path = staging.path().join("input.rgba32f");
    let output_path = staging.path().join("output.rgba32f");
    let request_path = staging.path().join("request.json");
    let response_path = staging.path().join("response.json");
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
    let request_bytes = serde_json::to_vec(&request)
        .map_err(|error| invalid(format!("request encoding failed: {error}")))?;
    if request_bytes.len() > 256 * 1024 {
        return Err(invalid("render request exceeds its size limit"));
    }
    fs::write(&request_path, request_bytes)
        .map_err(|error| unavailable(format!("request staging failed: {error}")))?;
    let mut child = Command::new(helper_executable)
        .arg(OPENFX_RENDER_WORKER_ARGUMENT)
        .env(REQUEST_ENV, request_path)
        .env(RESPONSE_ENV, &response_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| unavailable(format!("render worker start failed: {error}")))?;
    let deadline = Instant::now() + DEADLINE;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(status)) => return Err(OpenFxRenderError::WorkerFailed(status.to_string())),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(OpenFxRenderError::DeadlineExceeded);
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(unavailable(format!(
                    "render worker observation failed: {error}"
                )));
            }
        }
    }
    let response_metadata = fs::metadata(&response_path)
        .map_err(|error| unavailable(format!("render response is unavailable: {error}")))?;
    if !response_metadata.is_file() || response_metadata.len() > 4096 {
        return Err(unavailable("render response exceeds its size limit"));
    }
    let response: RenderResponse = serde_json::from_slice(
        &fs::read(&response_path)
            .map_err(|error| unavailable(format!("render response is unavailable: {error}")))?,
    )
    .map_err(|error| unavailable(format!("render response is malformed: {error}")))?;
    if let RenderResponse::Rejected { status } = response {
        return Err(OpenFxRenderError::WorkerFailed(format!(
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

/// Run the native render entrypoint before graphics or media startup.
pub fn run_openfx_render_worker() -> Result<(), OpenFxRenderError> {
    let mut args = std::env::args_os();
    let _executable = args.next();
    if args.next().as_deref() != Some(std::ffi::OsStr::new(OPENFX_RENDER_WORKER_ARGUMENT))
        || args.next().is_some()
    {
        return Err(invalid("unexpected OpenFX render worker arguments"));
    }
    let request_path = std::env::var_os(REQUEST_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| invalid("render request path is missing"))?;
    let response_path = std::env::var_os(RESPONSE_ENV)
        .map(PathBuf::from)
        .ok_or_else(|| invalid("render response path is missing"))?;
    let request_metadata = fs::metadata(&request_path)
        .map_err(|error| unavailable(format!("render request is unavailable: {error}")))?;
    if !request_metadata.is_file() || request_metadata.len() > 256 * 1024 {
        return Err(invalid("render request exceeds its size limit"));
    }
    let request: RenderRequest = serde_json::from_slice(
        &fs::read(request_path)
            .map_err(|error| unavailable(format!("render request is unavailable: {error}")))?,
    )
    .map_err(|error| invalid(format!("render request is malformed: {error}")))?;
    let status = render_in_worker(&request)?;
    let response = if status == 0 {
        RenderResponse::Success
    } else {
        RenderResponse::Rejected { status }
    };
    fs::write(
        response_path,
        serde_json::to_vec(&response)
            .map_err(|error| invalid(format!("response encoding failed: {error}")))?,
    )
    .map_err(|error| unavailable(format!("render response write failed: {error}")))
}

fn render_in_worker(request: &RenderRequest) -> Result<i32, OpenFxRenderError> {
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
    if !request.binary_path.is_absolute()
        || !request.bundle_path.is_absolute()
        || resolve_openfx_binary(&request.bundle_path)? != request.binary_path
        || sha256_file(&request.binary_path)? != request.binary_sha256
    {
        return Err(invalid(
            "worker received an invalid or changed OpenFX binary",
        ));
    }
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

fn validate_frame(width: u32, height: u32, pixels: usize) -> Result<usize, OpenFxRenderError> {
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

fn validate_timing(timing: &OpenFxRenderTiming) -> Result<(), OpenFxRenderError> {
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
) -> Result<(), OpenFxRenderError> {
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

fn bundle_for_binary(binary: &Path) -> Result<PathBuf, OpenFxRenderError> {
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

fn native_string(path: &Path) -> Result<CString, OpenFxRenderError> {
    CString::new(
        path.to_str()
            .ok_or_else(|| invalid("OpenFX native path is not valid Unicode"))?,
    )
    .map_err(|_| invalid("OpenFX native path contains a null byte"))
}

fn invalid(reason: impl Into<String>) -> OpenFxRenderError {
    OpenFxRenderError::Invalid(reason.into())
}

fn unavailable(reason: impl Into<String>) -> OpenFxRenderError {
    OpenFxRenderError::Unavailable(reason.into())
}
