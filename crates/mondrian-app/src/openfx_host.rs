//! Supervised Float32 OpenFX filter execution for one selected native binary.

mod curves;
mod description;
#[cfg(test)]
use description::{describe_in_worker, DescribeRequest, DescribeResponse};
pub use description::{
    describe_openfx_filter, describe_openfx_filter_with_checkpoint, describe_openfx_generator,
    run_openfx_describe_worker,
};
pub(crate) use description::{
    observe_openfx_filter_with_checkpoint, observe_openfx_with_checkpoint, OpenFxDescribeNotices,
};
mod resident;
pub use resident::{
    run_openfx_resident_worker, OpenFxFilterSession, OpenFxRenderObservation,
    OPENFX_RESIDENT_WORKER_ARGUMENT,
};
#[cfg(any(test, feature = "validation"))]
pub use resident::{
    OpenFxChildRenderDiagnostic, OpenFxChildRenderPhase, OpenFxNativeRenderPhase,
    OpenFxRenderDiagnostic, OpenFxRenderPhase,
};

use mondrian_effects::EffectProjectGeometry;
use std::collections::BTreeMap;
use std::ffi::{c_char, c_int, c_void, CString};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
#[cfg(test)]
use std::thread;
use std::time::{Duration, Instant};

use memmap2::MmapOptions;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::openfx_adapter::{
    resolve_openfx_binary, sha256_file, sha256_file_with_checkpoint, OpenFxBinaryInspection,
    OpenFxDiscoveryError,
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum OpenFxParameterQueryMode {
    /// The supplied value is constant over the parameter time domain.
    Constant {},
    /// Only the requested render frame has been evaluated; no curve is supplied.
    CurrentFrame {},
    /// Frozen canonical author property on the SDK's frame time grid.
    Curve {
        /// Complete author curve, including sparse compound channels.
        property: Box<mondrian_core::AnimatedProperty>,
        /// Exact sequence frames per author second.
        time_scale: mondrian_core::TimeScale,
    },
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
            query_mode: OpenFxParameterQueryMode::Constant {},
        }
    }

    /// Bind one evaluated frame without claiming values at other times.
    pub fn current_frame(value: OpenFxParameterValue) -> Self {
        Self {
            value,
            query_mode: OpenFxParameterQueryMode::CurrentFrame {},
        }
    }

    /// Bind a complete curve. The worker derives the current value from Core;
    /// the supplied value declares the SDK type and is not curve authority.
    pub fn curve(
        value: OpenFxParameterValue,
        property: mondrian_core::AnimatedProperty,
        time_scale: mondrian_core::TimeScale,
    ) -> Self {
        Self {
            value,
            query_mode: OpenFxParameterQueryMode::Curve {
                property: Box::new(property),
                time_scale,
            },
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
    /// An unwrapped scalar angle; native values are always degrees.
    Angle,
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

/// Context retained by the immutable selection and both supervised protocols.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OpenFxContext {
    Filter,
    Generator,
}
impl OpenFxContext {
    const fn native(self) -> c_int {
        match self {
            Self::Filter => 0,
            Self::Generator => 1,
        }
    }
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

/// Canonical complete image value owned by Effects, including association.
pub use mondrian_effects::EffectFloatFrame;

/// Exact sequence facts lowered to the OpenFX double-valued frame contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenFxRenderTiming {
    /// Frame position supplied to the OFX render action.
    pub frame: f64,
    /// Sequence frames per second.
    pub frame_rate: f64,
    /// First frame available from this filter's source Clip.
    pub first_frame: f64,
    /// Last frame available from this filter's source Clip.
    pub last_frame: f64,
}

/// Native host failure that leaves the caller's input frame unchanged.
#[derive(Debug, thiserror::Error)]
pub enum OpenFxHostError {
    /// A valid instance awaits authored repair; its worker is not a fault.
    #[error("OpenFX effect is blocked by a persistent user error")]
    UserBlocked(Box<mondrian_effects::EffectProcessorFeedback>),
    /// A parent-owned output allocation was refused before any native action.
    /// The same live instance can be retained only by the Effects resource bank.
    #[error(transparent)]
    HostResourceRefused(mondrian_effects::EffectProcessorHostResourceRefusal),
    /// The complete Float image cannot satisfy its declared representation.
    #[error(transparent)]
    Frame(#[from] mondrian_effects::EffectFloatFrameError),
    /// The actual image cannot enter this host's unit render-scale contract.
    #[error("OpenFX requires unit image render scale, got {sampling:?}")]
    UnsupportedImageSampling {
        sampling: mondrian_core::ImageSampling,
    },
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

#[repr(C)]
struct NativeParameter {
    name: *const c_char,
    query_mode: c_int,
    kind: c_int,
    value: [f64; 4],
    string_value: *const c_char,
    string_length: usize,
    query: Option<curves::NativeQuery>,
    query_context: *const c_void,
    query_revision: u64,
}

#[repr(C)]
struct NativeFrameInfo {
    source_alpha: c_int,
    width: c_int,
    height: c_int,
    project_width: u32,
    project_height: u32,
    project_pixel_aspect_ratio: f64,
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

/// Controlled owned CPU-suite storage, without loading a native plugin.
pub(crate) fn native_thread_resident_bytes() -> Result<usize, OpenFxHostError> {
    // SAFETY: this linked host getter computes owned storage only. It does not
    // load a plugin, create a thread or retain pointers from the application.
    let bytes = unsafe { mondrian_openfx_thread_resident_bytes() };
    if bytes == 0 {
        return Err(unavailable("native callback stack cannot be configured"));
    }
    Ok(bytes)
}

/// Known fixed auxiliary report storage, without creating/loading a plugin.
pub(crate) fn native_auxiliary_resident_bytes() -> Result<usize, OpenFxHostError> {
    // SAFETY: the linked getter reads only the host's static sizing policy.
    let native = unsafe { mondrian_openfx_aux_resident_bytes() };
    native
        .checked_add(std::mem::size_of::<NativeRenderReport>())
        .and_then(|value| {
            value.checked_add(
                mondrian_effects::EffectProcessorFeedbackSnapshot::maximum_retained_bytes() * 3,
            )
        })
        // One bounded serialization/parse packet per parent and child. The
        // existing transport permits 2 MiB; count its known controlled buffers.
        .and_then(|value| value.checked_add(MAX_WORKER_REQUEST_BYTES * 4))
        .and_then(|value| value.checked_add(resident::pump::storage_bytes()?))
        .and_then(|value| value.checked_add(resident::child_diagnostic::storage_bytes()?))
        .ok_or_else(|| unavailable("OpenFX auxiliary storage charge overflows"))
}

const NATIVE_NOTICE_SLOTS: usize = 32;
const NATIVE_NOTICE_TEXT_BYTES: usize = 4096;
const NATIVE_NOTICE_ID_BYTES: usize = 256;

#[repr(C)]
#[derive(Clone, Copy)]
struct NativeNotice {
    kind: c_int,
    text_length: usize,
    id_length: usize,
    text: [c_char; NATIVE_NOTICE_TEXT_BYTES + 1],
    id: [c_char; NATIVE_NOTICE_ID_BYTES + 1],
}
// These callbacks lend only bounded POD. No borrowed scheduler closure or
// SDK/session handle runs on the control thread or a plugin callback thread.
#[repr(C)]
struct NativeInvocationControl {
    context: *const c_void,
    abort: Option<unsafe extern "C" fn(*const c_void) -> c_int>,
    progress: Option<unsafe extern "C" fn(*const c_void, *const NativeLiveProgress) -> c_int>,
    gui_open: c_int,
    interactive_render: c_int,
    question_work: c_int,
    question: Option<unsafe extern "C" fn(*const c_void, *const NativeLiveQuestion) -> c_int>,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct NativeLiveQuestion {
    owner: u64,
    invocation: u64,
    text: NativeNotice,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct NativeLiveProgress {
    owner: u64,
    invocation: u64,
    generation: u64,
    active: c_int,
    fraction: f64,
    label: NativeNotice,
}
impl NativeLiveProgress {
    fn decode(&self) -> Result<mondrian_effects::EffectProcessorProgress, c_int> {
        if !matches!(self.active, 0 | 1) || self.owner == 0 || self.invocation == 0 {
            return Err(6);
        }
        let label = self.label.decode()?.ok_or(6)?;
        mondrian_effects::EffectProcessorProgress::new(
            self.generation,
            self.active == 1,
            self.fraction,
            label,
        )
        .map_err(|_| 6)
    }
}

#[repr(C)]
struct NativeFeedback {
    owner: u64,
    invocation: u64,
    persistent_revision: u64,
    persistent: NativeNotice,
    progress_generation: u64,
    progress_active: c_int,
    progress_fraction: f64,
    progress: NativeNotice,
    notice_count: usize,
    notices: [NativeNotice; NATIVE_NOTICE_SLOTS],
}
#[repr(C)]
struct NativeRenderReport {
    completion: c_int,
    output_alpha: c_int,
    feedback: NativeFeedback,
}

#[derive(Debug, PartialEq, Eq)]
enum NativeRenderError {
    Rejected(c_int),
    UserBlocked(Box<mondrian_effects::EffectProcessorFeedbackSnapshot>),
}

impl NativeNotice {
    fn decode(&self) -> Result<Option<mondrian_effects::EffectProcessorNotice>, c_int> {
        use mondrian_effects::EffectProcessorNoticeKind as Kind;
        let kind = match self.kind {
            0 if self.text_length == 0 && self.id_length == 0 => return Ok(None),
            1 => Kind::Information,
            2 => Kind::Warning,
            3 => Kind::Error,
            4 => Kind::Log,
            5 => Kind::Fatal,
            _ => return Err(6),
        };
        let text = decode_native_notice_text(&self.text, self.text_length)?;
        let id = decode_native_notice_text(&self.id, self.id_length)?;
        mondrian_effects::EffectProcessorNotice::new(kind, text, id)
            .map(Some)
            .map_err(|_| 6)
    }
}

fn decode_native_notice_text<const N: usize>(
    data: &[c_char; N],
    length: usize,
) -> Result<String, c_int> {
    if length >= N || data[length] != 0 {
        return Err(6);
    }
    let bytes = data[..length].iter().map(|value| *value as u8).collect::<Vec<_>>();
    String::from_utf8(bytes).map_err(|_| 6)
}

impl NativeFeedback {
    fn decode(&self) -> Result<mondrian_effects::EffectProcessorFeedbackSnapshot, c_int> {
        if self.notice_count > NATIVE_NOTICE_SLOTS || !matches!(self.progress_active, 0 | 1) {
            return Err(6);
        }
        let progress = if self.progress_generation == 0 {
            if self.progress_active != 0
                || self.progress_fraction != 0.0
                || self.progress.decode()?.is_some()
            {
                return Err(6);
            }
            None
        } else {
            let label = self.progress.decode()?.ok_or(6)?;
            Some(
                mondrian_effects::EffectProcessorProgress::new(
                    self.progress_generation,
                    self.progress_active == 1,
                    self.progress_fraction,
                    label,
                )
                .map_err(|_| 6)?,
            )
        };
        let notices = self.notices[..self.notice_count]
            .iter()
            .map(|notice| notice.decode()?.ok_or(6))
            .collect::<Result<Vec<_>, c_int>>()?;
        mondrian_effects::EffectProcessorFeedbackSnapshot::new(
            self.owner,
            self.invocation,
            self.persistent_revision,
            self.persistent.decode()?,
            progress,
            notices,
        )
        .map_err(|_| 6)
    }
}

unsafe extern "C" {
    fn mondrian_openfx_aux_resident_bytes() -> usize;
    fn mondrian_openfx_thread_resident_bytes() -> usize;
    fn mondrian_openfx_describe(
        binary: *const c_char,
        bundle: *const c_char,
        identifier: *const c_char,
        selected_context: c_int,
        label: *mut c_char,
        label_capacity: usize,
        label_length: *mut usize,
        callback: unsafe extern "C" fn(*mut c_void, *const NativeParameterInfo) -> c_int,
        context: *mut c_void,
        notices: *mut description::NativeDescribeNotices,
    ) -> c_int;
    fn mondrian_openfx_session_create(
        binary: *const c_char,
        bundle: *const c_char,
        identifier: *const c_char,
        selected_context: c_int,
        image_namespace: *const u8,
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
        report: *mut NativeRenderReport,
        control: *const NativeInvocationControl,
        diagnostic: *mut resident::child_diagnostic::NativeRenderDiagnostic,
    ) -> c_int;
    fn mondrian_openfx_session_close_gui(
        session: *mut c_void,
        feedback: *mut NativeFeedback,
        invoked: *mut c_int,
    ) -> c_int;
    fn mondrian_openfx_session_destroy(session: *mut c_void);

}

// A native handle stays in the isolated child and is neither Send nor Sync.
// Its owner destroys the instance before unloading its selected plugin binary.
struct NativeSession {
    handle: std::ptr::NonNull<c_void>,
    report: Box<NativeRenderReport>,
    feedback: Option<mondrian_effects::EffectProcessorFeedbackSnapshot>,
}

impl NativeSession {
    #[cfg(test)]
    fn create(
        owner: mondrian_core::EffectProcessorOwnerId,
        binary: &CString,
        bundle: &CString,
        identifier: &CString,
        input: &[u8],
        info: &NativeFrameInfo,
        parameters: &[NativeParameter],
    ) -> Result<Self, c_int> {
        Self::create_in_context(
            owner,
            OpenFxContext::Filter,
            binary,
            bundle,
            identifier,
            input,
            info,
            parameters,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn create_in_context(
        owner: mondrian_core::EffectProcessorOwnerId,
        context: OpenFxContext,
        binary: &CString,
        bundle: &CString,
        identifier: &CString,
        input: &[u8],
        info: &NativeFrameInfo,
        parameters: &[NativeParameter],
    ) -> Result<Self, c_int> {
        if owner.0.is_nil() || (context == OpenFxContext::Generator && !input.is_empty()) {
            return Err(2);
        }
        // SAFETY: every field is an integer, float or fixed integer array;
        // all-zero has a valid representation. Allocate once, off the hot frame
        // path, without constructing a 150 KiB temporary on the Rust stack.
        let report = unsafe { Box::<NativeRenderReport>::new_zeroed().assume_init() };
        let mut pointer = std::ptr::null_mut();
        // SAFETY: C strings, frame bytes and parameter names stay live throughout
        // this call. The native host copies frame data and parameter values.
        let status = unsafe {
            mondrian_openfx_session_create(
                binary.as_ptr(),
                bundle.as_ptr(),
                identifier.as_ptr(),
                context.native(),
                owner.0.as_bytes().as_ptr(),
                if context == OpenFxContext::Generator {
                    std::ptr::null()
                } else {
                    input.as_ptr().cast()
                },
                if context == OpenFxContext::Filter {
                    input.len()
                } else {
                    (info.width as usize).saturating_mul(info.height as usize).saturating_mul(16)
                },
                info,
                parameters.as_ptr(),
                parameters.len() as c_int,
                &mut pointer,
            )
        };
        if status != 0 {
            return Err(status);
        }
        std::ptr::NonNull::new(pointer)
            .map(|handle| Self { handle, report, feedback: None })
            .ok_or(6)
    }

    #[cfg(test)]
    fn render(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        info: &NativeFrameInfo,
        parameters: &[NativeParameter],
    ) -> Result<mondrian_core::ColorFrameAlpha, NativeRenderError> {
        self.render_controlled(input, output, info, parameters, None)
    }

    #[cfg(test)]
    fn render_controlled(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        info: &NativeFrameInfo,
        parameters: &[NativeParameter],
        control: Option<&NativeInvocationControl>,
    ) -> Result<mondrian_core::ColorFrameAlpha, NativeRenderError> {
        self.render_with_native_diagnostic(input, output, info, parameters, control, None)
    }

    fn render_with_native_diagnostic(
        &mut self,
        input: &[u8],
        output: &mut [u8],
        info: &NativeFrameInfo,
        parameters: &[NativeParameter],
        control: Option<&NativeInvocationControl>,
        diagnostic: Option<&mut resident::child_diagnostic::NativeRenderDiagnostic>,
    ) -> Result<mondrian_core::ColorFrameAlpha, NativeRenderError> {
        self.feedback = None;
        if !input.is_empty() && output.len() != input.len() {
            return Err(NativeRenderError::Rejected(2));
        }
        // SAFETY: The exclusive live handle serializes native actions. Disjoint
        // input/output slices and output metadata remain live for this call.
        // Native validation checks exact extents before reading or writing.
        let status = unsafe {
            mondrian_openfx_session_render(
                self.handle.as_ptr(),
                if input.is_empty() {
                    std::ptr::null()
                } else {
                    input.as_ptr().cast()
                },
                output.as_mut_ptr().cast(),
                output.len(),
                info,
                parameters.as_ptr(),
                parameters.len() as c_int,
                self.report.as_mut(),
                control.map_or(std::ptr::null(), std::ptr::from_ref),
                diagnostic.map_or(std::ptr::null_mut(), std::ptr::from_mut),
            )
        };
        if status != 0 {
            return Err(NativeRenderError::Rejected(status));
        }
        let feedback = self.report.feedback.decode().map_err(NativeRenderError::Rejected)?;
        match self.report.completion {
            2 if feedback.is_blocked() => {
                return Err(NativeRenderError::UserBlocked(Box::new(feedback)))
            }
            1 if !feedback.is_blocked() => {}
            _ => return Err(NativeRenderError::Rejected(6)),
        }
        self.feedback = Some(feedback);
        match self.report.output_alpha {
            0 => Ok(mondrian_core::ColorFrameAlpha::StraightFloat),
            1 => Ok(mondrian_core::ColorFrameAlpha::PremultipliedFloat),
            2 => Ok(mondrian_core::ColorFrameAlpha::Opaque),
            _ => Err(NativeRenderError::Rejected(6)),
        }
    }

    fn close_gui(
        &mut self,
    ) -> Result<Option<mondrian_effects::EffectProcessorFeedbackSnapshot>, c_int> {
        let mut invoked = 0;
        // SAFETY: the exclusive owner and its already allocated report stay
        // live for this action. The SDK writes a complete bounded POD only.
        let status = unsafe {
            mondrian_openfx_session_close_gui(
                self.handle.as_ptr(),
                &raw mut self.report.feedback,
                &raw mut invoked,
            )
        };
        if status != 0 {
            return Err(status);
        }
        match invoked {
            0 => Ok(None),
            1 => self.report.feedback.decode().map(Some),
            _ => Err(6),
        }
    }

    fn take_feedback(&mut self) -> Option<mondrian_effects::EffectProcessorFeedbackSnapshot> {
        self.feedback.take()
    }
}

impl Drop for NativeSession {
    fn drop(&mut self) {
        // SAFETY: This handle was created once, has no aliases and is retired once.
        unsafe {
            mondrian_openfx_session_destroy(self.handle.as_ptr());
        }
    }
}

/// Execute one installed OFX filter in a supervised child. The caller must
/// supply the working color domain and exact alpha interpretation; this
/// adapter does not perform implicit color or alpha conversion.
pub fn render_openfx_filter_frame(
    helper_executable: &Path,
    inspection: &OpenFxBinaryInspection,
    plugin_identifier: &str,
    timing: &OpenFxRenderTiming,
    parameters: &BTreeMap<String, OpenFxParameterBinding>,
    frame: &EffectFloatFrame,
    project_geometry: EffectProjectGeometry,
) -> Result<EffectFloatFrame, OpenFxHostError> {
    let mut checkpoint = || Ok(());
    validate_render_request(timing, parameters, frame, &mut checkpoint)?;
    let mut session = OpenFxFilterSession::prepare(
        helper_executable,
        inspection,
        plugin_identifier,
        frame.width(),
        frame.height(),
        frame.format(),
        project_geometry,
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
    checkpoint: &mut impl FnMut() -> Result<(), OpenFxHostError>,
) -> Result<R, OpenFxHostError> {
    use crate::openfx_adapter::child_supervision::{wait_for_child, ChildWaitError};
    checkpoint()?;
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
    checkpoint()?;
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
    let status = wait_for_child(&mut child, deadline, checkpoint).map_err(|error| match error {
        ChildWaitError::Checkpoint(error) => error,
        ChildWaitError::Deadline => OpenFxHostError::DeadlineExceeded,
        ChildWaitError::Observation(error) => {
            unavailable(format!("OpenFX worker observation failed: {error}"))
        }
        ChildWaitError::Settlement(error) => {
            tracing::error!(pid = child.id(), %error, "OpenFX description child settlement failed");
            unavailable(format!("OpenFX worker settlement failed: {error}"))
        }
    })?;
    if !status.success() {
        return Err(OpenFxHostError::WorkerFailed(status.to_string()));
    }
    checkpoint()?;
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
            2 if raw.kind == 1 => OpenFxDoubleType::Angle,
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
    frame: &EffectFloatFrame,
    checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
) -> Result<(), OpenFxHostError> {
    checkpoint()?;
    validate_frame(frame.width(), frame.height(), frame.pixels().len())?;
    validate_timing(timing)?;
    validate_native_sampling(frame.width(), frame.sampling())?;
    validate_parameters(parameters)?;
    frame.view().validate_controlled(&mut || checkpoint())
}

// The native ABI advertises unit render scale. PAR is the original sample
// shape; it must never absorb a Reduced or nonuniform raster density.
fn validate_native_sampling(
    width: u32,
    sampling: mondrian_core::ImageSampling,
) -> Result<(), OpenFxHostError> {
    if !sampling.is_unit_scale() {
        return Err(OpenFxHostError::UnsupportedImageSampling { sampling });
    }
    if !(f64::from(width) * sampling.sample_aspect_ratio().to_f64()).is_finite() {
        return Err(invalid(
            "OpenFX canonical frame extent is not representable",
        ));
    }
    Ok(())
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
        || timing.frame_rate <= 0.0
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
    checkpoint: &mut impl FnMut() -> Result<(), OpenFxHostError>,
) -> Result<PathBuf, OpenFxHostError> {
    if !inspection.plugins.iter().any(|plugin| plugin.identifier == plugin_identifier) {
        return Err(invalid("selected plugin is absent from the pinned binary"));
    }
    let bundle = bundle_for_binary(&inspection.binary_path)?;
    if resolve_openfx_binary(&bundle)? != inspection.binary_path
        || sha256_file_with_checkpoint(&inspection.binary_path, checkpoint)?
            != inspection.binary_sha256
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
                    || (parameter.double_type == Some(OpenFxDoubleType::Angle)
                        && !matches!(value, OpenFxParameterValue::Double(_)))
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
/// Shared bounded child qualification for native adapter tests.
pub(crate) mod shared_frame_tests {
    use super::*;

    #[test]
    fn actual_sampling_requires_unit_density_and_admits_maximum_typed_par() {
        let format = mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightFloat,
        );
        let timing = OpenFxRenderTiming {
            frame: 0.0,
            frame_rate: 24.0,
            first_frame: 0.0,
            last_frame: 1.0,
        };
        let par = mondrian_core::SampleAspectRatio::new(u32::MAX, 1).unwrap();
        for scale in [
            [mondrian_core::Rational::new(1, 2); 2],
            [
                mondrian_core::Rational::new(1, 1),
                mondrian_core::Rational::new(2, 3),
            ],
        ] {
            let sampling = mondrian_core::ImageSampling::new(par, scale).unwrap();
            let frame = EffectFloatFrame::from_pixels(
                mondrian_effects::EffectFrameExtent::new(3, 2),
                format,
                sampling,
                vec![[0.0; 4]; 6],
            )
            .unwrap();
            assert!(matches!(
                validate_render_request(&timing, &BTreeMap::new(), &frame, &mut || Ok(())),
                Err(OpenFxHostError::UnsupportedImageSampling { sampling: actual }) if actual == sampling
            ));
        }
        let frame = EffectFloatFrame::from_pixels(
            mondrian_effects::EffectFrameExtent::new(3, 2),
            format,
            mondrian_core::ImageSampling::new(par, [mondrian_core::Rational::new(1, 1); 2])
                .unwrap(),
            vec![[0.0; 4]; 6],
        )
        .unwrap();
        validate_render_request(&timing, &BTreeMap::new(), &frame, &mut || Ok(()))
            .expect("maximum typed SAR is valid; raw f64 ABI overflow remains separately rejected");
    }

    #[test]
    /// Refuse invalid native geometry before module loading or host ownership.
    pub(crate) fn native_canonical_geometry_overflow_refuses_before_binary_loading() {
        let binary = CString::new("missing-geometry-fixture.ofx").expect("binary");
        let bundle = CString::new("missing-geometry-fixture.ofx.bundle").expect("bundle");
        let identifier = CString::new("test.geometry").expect("identifier");
        let input = [[0.0_f32; 4]; 6];
        // SAFETY: This aligned array is borrowed as its exact byte extent only
        // during the synchronous native validation call; no binary is available.
        let bytes = unsafe {
            std::slice::from_raw_parts(input.as_ptr().cast(), std::mem::size_of_val(&input))
        };
        let info = NativeFrameInfo {
            source_alpha: 0,
            width: 3,
            height: 2,
            frame: 0.0,
            frame_rate: 24.0,
            first_frame: 0.0,
            last_frame: 1.0,
            pixel_aspect_ratio: f64::MAX,

            project_width: 3,
            project_height: 2,
            project_pixel_aspect_ratio: 1.0,
        };
        assert_eq!(
            NativeSession::create(
                mondrian_core::EffectProcessorOwnerId::new(),
                &binary,
                &bundle,
                &identifier,
                bytes,
                &info,
                &[]
            )
            .err(),
            Some(2)
        );
    }

    #[test]
    fn native_invalid_project_geometry_refuses_before_binary_loading() {
        let binary = CString::new("must-not-load-project-geometry.ofx").unwrap();
        let bundle = CString::new("must-not-load-project-geometry.ofx.bundle").unwrap();
        let identifier = CString::new("test.geometry").unwrap();
        let input = [[0.0_f32; 4]; 15];
        // SAFETY: the complete aligned array is borrowed only for this call.
        let bytes = unsafe {
            std::slice::from_raw_parts(input.as_ptr().cast(), std::mem::size_of_val(&input))
        };
        for (project_width, project_height, project_par) in [
            (0, 7, 2.0),
            (10, 0, 2.0),
            (10, 7, 0.0),
            (10, 7, -1.0),
            (10, 7, f64::NAN),
            (10, 7, f64::INFINITY),
        ] {
            let info = NativeFrameInfo {
                source_alpha: 0,
                width: 3,
                height: 5,
                project_width,
                project_height,
                project_pixel_aspect_ratio: project_par,
                frame: 0.0,
                frame_rate: 24.0,
                first_frame: 0.0,
                last_frame: 100.0,
                pixel_aspect_ratio: 1.0,
            };
            assert_eq!(
                NativeSession::create(
                    mondrian_core::EffectProcessorOwnerId::new(),
                    &binary,
                    &bundle,
                    &identifier,
                    bytes,
                    &info,
                    &[],
                )
                .err(),
                Some(2)
            );
        }
    }

    #[test]
    #[ignore = "owned SessionProbe project_geometry in the existing bounded supervised qualification child"]
    fn native_project_geometry_rebinding_refuses_before_actions_and_preserves_original_owner() {
        const TEST: &str = "openfx_host::shared_frame_tests::native_project_geometry_rebinding_refuses_before_actions_and_preserves_original_owner";
        if !qualification_child(
            TEST,
            &[("MONDRIAN_OPENFX_SESSION_PROBE_MODE", "project_geometry")],
        ) {
            return;
        }
        let path = resolve_openfx_binary(&PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY").expect("owned geometry probe"),
        ))
        .unwrap();
        let bundle =
            path.ancestors().find(|p| p.extension().is_some_and(|e| e == "bundle")).unwrap();
        let binary = native_string(&path).unwrap();
        let bundle = native_string(bundle).unwrap();
        let identifier = CString::new("uk.co.thefoundry.BasicGainPlugin").unwrap();
        let name = CString::new("scale").unwrap();
        let parameters = [NativeParameter {
            name: name.as_ptr(),
            query_mode: 0,
            kind: 1,
            value: [0.5, 0.0, 0.0, 0.0],
            string_value: std::ptr::null(),
            string_length: 0,
            query: None,
            query_context: std::ptr::null(),
            query_revision: 0,
        }];
        let input = [[-0.25_f32, 0.5, 2.0, 0.25]; 15];
        // SAFETY: the aligned complete input stays alive throughout these synchronous calls.
        let bytes = unsafe {
            std::slice::from_raw_parts(input.as_ptr().cast(), std::mem::size_of_val(&input))
        };
        let mut info = NativeFrameInfo {
            source_alpha: 0,
            width: 3,
            height: 5,
            project_width: 10,
            project_height: 7,
            project_pixel_aspect_ratio: 2.0,
            frame: 0.0,
            frame_rate: 24.0,
            first_frame: 0.0,
            last_frame: 100.0,
            pixel_aspect_ratio: 1.0,
        };
        let mut session = NativeSession::create(
            mondrian_core::EffectProcessorOwnerId::new(),
            &binary,
            &bundle,
            &identifier,
            bytes,
            &info,
            &parameters,
        )
        .unwrap();
        let mut output = vec![0xa5; bytes.len()];
        assert_eq!(
            session.render(bytes, &mut output, &info, &parameters),
            Ok(mondrian_core::ColorFrameAlpha::StraightFloat)
        );
        let first = session.take_feedback().unwrap();
        for (width, height, par) in [(12, 7, 2.0), (10, 9, 2.0), (10, 7, 1.0), (0, 7, 2.0)] {
            info.project_width = width;
            info.project_height = height;
            info.project_pixel_aspect_ratio = par;
            output.fill(0xa5);
            assert_eq!(
                session.render(bytes, &mut output, &info, &parameters),
                Err(NativeRenderError::Rejected(2))
            );
            assert!(output.iter().all(|byte| *byte == 0xa5));
            assert!(session.take_feedback().is_none());
        }
        info.project_width = 10;
        info.project_height = 7;
        info.project_pixel_aspect_ratio = 2.0;
        info.frame = 6.0;
        info.pixel_aspect_ratio = 1.2; // Legal image change, with unchanged project binding.
        assert_eq!(
            session.render(bytes, &mut output, &info, &parameters),
            Ok(mondrian_core::ColorFrameAlpha::StraightFloat)
        );
        let recovered = session.take_feedback().unwrap();
        assert_eq!(recovered.native_owner(), first.native_owner());
        assert_eq!(
            recovered.invocation(),
            first.invocation() + 1,
            "refused geometry ran no native invocation"
        );
        let literal = [[0.1875_f32, 0.359375, 1.075, 0.125]; 15];
        let expected: Vec<u8> = literal
            .iter()
            .flat_map(|pixel| pixel.iter().flat_map(|v| v.to_ne_bytes()))
            .collect();
        assert_eq!(output, expected);
    }

    #[test]
    fn scalar_angle_description_preserves_raw_units_and_rejects_compound_or_forged_semantics() {
        let mut raw = NativeParameterInfo {
            name: c"angle".as_ptr(),
            name_length: 5,
            label: c"Angle".as_ptr(),
            label_length: 5,
            hint: c"Unwrapped degrees".as_ptr(),
            hint_length: 17,
            kind: 1,
            preferred_interpolation: 1,
            double_type: 2,
            default_value: [350.0; 4],
            minimum: [-1440.0; 4],
            maximum: [1440.0; 4],
            display_minimum: [-720.0; 4],
            display_maximum: [720.0; 4],
            can_animate: 1,
            secret: 0,
            enabled: 1,
            choices: core::ptr::null(),
            choice_count: 0,
            string_default: core::ptr::null(),
            string_default_length: 0,
            string_mode: 0,
            string_file_path_exists: -1,
        };
        // SAFETY: all text points to the exact declared static byte extents;
        // this scalar has no native choices or string-value storage.
        let decode = |raw: &NativeParameterInfo| unsafe { decode_native_parameter(raw) };
        let parameter = decode(&raw).expect("native scalar Angle code2");
        assert_eq!(parameter.double_type, Some(OpenFxDoubleType::Angle));
        assert_eq!(parameter.default_value, OpenFxParameterValue::Double(350.0));
        assert_eq!(
            parameter.numeric_channels[0],
            OpenFxNumericChannel {
                minimum: -1440.0,
                maximum: 1440.0,
                display_minimum: -720.0,
                display_maximum: 720.0,
            }
        );
        let description = OpenFxFilterDescription {
            identifier: "test.angle".into(),
            label: "Angle".into(),
            parameters: vec![parameter],
        };
        validate_description(&description, "test.angle").unwrap();
        for (semantic, code, tag) in [
            (OpenFxDoubleType::Plain, 0, "plain"),
            (OpenFxDoubleType::Scale, 1, "scale"),
            (OpenFxDoubleType::Angle, 2, "angle"),
        ] {
            raw.double_type = code;
            assert_eq!(decode(&raw).unwrap().double_type, Some(semantic));
            assert_eq!(
                serde_json::to_value(semantic).unwrap(),
                serde_json::json!(tag)
            );
        }
        raw.double_type = 2;
        for native_kind in [2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12] {
            raw.kind = native_kind;
            assert!(
                decode(&raw).is_err(),
                "Angle is scalar Double only, kind={native_kind}"
            );
        }
        raw.kind = 1;
        for code in [-1, 3, i32::MAX] {
            raw.double_type = code;
            assert!(decode(&raw).is_err());
        }
        for tag in ["time", "absolute_time", "x", "y", "xy", "unknown"] {
            assert!(serde_json::from_value::<OpenFxDoubleType>(serde_json::json!(tag)).is_err());
        }
        for value in [
            OpenFxParameterValue::Double2([350.0, 370.0]),
            OpenFxParameterValue::Double3([350.0, 360.0, 370.0]),
        ] {
            let mut forged = description.clone();
            let arity = value.numeric_layout().unwrap().0;
            forged.parameters[0].default_value = value;
            forged.parameters[0].numeric_channels =
                vec![description.parameters[0].numeric_channels[0].clone(); arity];
            let roundtrip: OpenFxFilterDescription =
                serde_json::from_slice(&serde_json::to_vec(&forged).unwrap()).unwrap();
            assert!(
                validate_description(&roundtrip, "test.angle").is_err(),
                "JSON cannot bypass scalar-only admission"
            );
        }
        for value in [-1080.0, 370.0, 1080.0] {
            let mut unwrapped = description.clone();
            unwrapped.parameters[0].default_value = OpenFxParameterValue::Double(value);
            validate_description(&unwrapped, "test.angle").unwrap();
            assert_eq!(
                unwrapped.parameters[0].default_value,
                OpenFxParameterValue::Double(value)
            );
        }
    }

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
    /// Run one named native qualification in a bounded child process.
    pub(crate) fn qualification_child(test: &str, environment: &[(&str, &str)]) -> bool {
        const CHILD: &str = "MONDRIAN_OPENFX_QUALIFICATION_CHILD";
        if std::env::var(CHILD).ok().as_deref() == Some(test) {
            // A test may supervise several fixture modes. Each child executes
            // only its selected mode, retaining process-local native state.
            return environment
                .iter()
                .all(|(key, value)| std::env::var(key).ok().as_deref() == Some(*value));
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
            schema_version: 3,
            context: OpenFxContext::Filter,
            binary_path: path.clone(),
            bundle_path: bundle.to_path_buf(),
            binary_sha256: sha256_file(&path).expect("fixture identity"),
            plugin_identifier: "uk.co.thefoundry.BasicGainPlugin".into(),
        })
        .expect("actual SDK compound description");
        let DescribeResponse::Success { description, .. } = described else {
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
            source_alpha: 0,
            width: 1,
            height: 1,
            frame: 0.0,
            frame_rate: 24.0,
            first_frame: 0.0,
            last_frame: 100.0,
            pixel_aspect_ratio: 1.0,

            project_width: 1,
            project_height: 1,
            project_pixel_aspect_ratio: 1.0,
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
                    query: None,
                    query_context: std::ptr::null(),
                    query_revision: 0,
                    query_mode: i32::from(mode == "compound_frame_sample"),
                    name: name.as_ptr(),
                    kind: native_kind,
                    value: native_value,
                    string_value: std::ptr::null(),
                    string_length: 0,
                }
            })
            .collect::<Vec<_>>();
        let mut session = NativeSession::create(
            mondrian_core::EffectProcessorOwnerId::new(),
            &binary,
            &bundle,
            &identifier,
            &input,
            &info,
            &parameters,
        )
        .expect("complete native tuple instance");
        let mut output = vec![0xa5; input.len()];
        assert_eq!(
            session.render(&input, &mut output, &info, &parameters),
            Ok(mondrian_core::ColorFrameAlpha::StraightFloat)
        );
        assert_eq!(output, input);
        let mut invalid = parameters;
        invalid[0].value[0] = 0.5;
        invalid[5].value[1] = 5.0;
        let mut unchanged = vec![0xa5; input.len()];
        assert!(session.render(&input, &mut unchanged, &info, &invalid).is_err());
        assert_eq!(unchanged, vec![0xa5; input.len()]);
        let mut recovered = vec![0xa5; input.len()];
        invalid[0].value[0] = 0.12500000000000003;
        invalid[5].value[1] = 0.0;
        assert_eq!(
            session.render(&input, &mut recovered, &info, &invalid),
            Ok(mondrian_core::ColorFrameAlpha::StraightFloat)
        );
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
                schema_version: 3,
                context: OpenFxContext::Filter,
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
                source_alpha: 0,
                width: 1,
                height: 1,
                frame: 0.0,
                frame_rate: 24.0,
                first_frame: 0.0,
                last_frame: 100.0,
                pixel_aspect_ratio: 1.0,

                project_width: 1,
                project_height: 1,
                project_pixel_aspect_ratio: 1.0,
            };
            let input =
                [0.25_f32; 4].iter().flat_map(|value| value.to_ne_bytes()).collect::<Vec<_>>();
            let parameters = |gain| {
                [NativeParameter {
                    query: None,
                    query_context: std::ptr::null(),
                    query_revision: 0,
                    query_mode: 0,
                    name: name.as_ptr(),
                    kind: 1,
                    value: [gain, 0.0, 0.0, 0.0],
                    string_value: std::ptr::null(),
                    string_length: 0,
                }]
            };
            let created = NativeSession::create(
                mondrian_core::EffectProcessorOwnerId::new(),
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
                    Err(NativeRenderError::Rejected(5))
                );
                assert_eq!(input, source_before);
                assert_eq!(
                    session.render(&input, &mut output, &info, &parameters(1.5)),
                    Err(NativeRenderError::Rejected(2))
                );
                return;
            }

            assert_eq!(
                session.render(&input, &mut output, &info, &parameters(1.5)),
                Ok(mondrian_core::ColorFrameAlpha::StraightFloat)
            );
            assert_eq!(
                output,
                [0.375_f32; 4].iter().flat_map(|value| value.to_ne_bytes()).collect::<Vec<_>>()
            );
            output.fill(0xa5);
            let status = session.render(&input, &mut output, &info, &parameters(2.0));
            if matches!(mode.as_str(), "unfetched" | "become_required") {
                assert_eq!(
                    status,
                    Err(NativeRenderError::Rejected(if mode == "unfetched" {
                        5
                    } else {
                        7
                    }))
                );
                assert_eq!(output, vec![0xa5; input.len()]);
                assert_eq!(
                    session.render(&input, &mut output, &info, &parameters(1.0)),
                    Err(NativeRenderError::Rejected(2))
                );
            } else {
                assert_eq!(status, Ok(mondrian_core::ColorFrameAlpha::StraightFloat));
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
            source_alpha: 0,
            width: 2,
            height: 3,
            frame: 1.0,
            frame_rate: 24.0,
            first_frame: 0.0,
            last_frame: 100.0,
            pixel_aspect_ratio: 1.0,

            project_width: 2,
            project_height: 3,
            project_pixel_aspect_ratio: 1.0,
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
                query: None,
                query_context: std::ptr::null(),
                query_revision: 0,
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
                mondrian_core::EffectProcessorOwnerId::new(),
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
                    Ok(mondrian_core::ColorFrameAlpha::StraightFloat)
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
            assert_eq!(
                session.render(&changed_input, &mut output, &info, &[]),
                Ok(mondrian_core::ColorFrameAlpha::StraightFloat)
            );
            assert_eq!(output, changed_input);
            info.width = 3;
            let mut output = vec![0xa5; input.len()];
            assert!(session.render(&input, &mut output, &info, &parameters(1.0)).is_err());
            assert_eq!(output, vec![0xa5; input.len()]);
            info.width = 2;
            assert!(
                session.render(&input, &mut output, &info, &parameters(1.0)).is_err(),
                "failed instance is retired"
            );
        }
        // Nonfinite input cannot produce or retain a usable native instance.
        input[..4].copy_from_slice(&f32::NAN.to_ne_bytes());
        assert!(NativeSession::create(
            mondrian_core::EffectProcessorOwnerId::new(),
            &binary,
            &bundle,
            &identifier,
            &input,
            &info,
            &parameters(1.0)
        )
        .is_err());
    }
    #[test]
    #[ignore = "requires current SessionProbe identity_output_clip and a new empty action directory"]
    fn native_identity_output_clip_refuses_without_writing_staging() {
        const TEST: &str = "openfx_host::shared_frame_tests::native_identity_output_clip_refuses_without_writing_staging";
        qualify_native_noncurrent_identity(TEST, "identity_output_clip");
    }

    #[test]
    #[ignore = "requires current SessionProbe identity_other_time and a new empty action directory"]
    fn native_identity_other_time_refuses_without_writing_staging() {
        const TEST: &str = "openfx_host::shared_frame_tests::native_identity_other_time_refuses_without_writing_staging";
        qualify_native_noncurrent_identity(TEST, "identity_other_time");
    }

    fn qualify_native_noncurrent_identity(test: &str, mode: &str) {
        use std::io::Read;
        if !qualification_child(test, &[("MONDRIAN_OPENFX_SESSION_PROBE_MODE", mode)]) {
            return;
        }
        let directory = PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_DESCRIBE_ACTION_RECEIPT_DIRECTORY")
                .expect("independent empty action-receipt directory"),
        );
        assert!(directory.is_absolute() && directory.is_dir());
        assert!(std::fs::read_dir(&directory).expect("action directory").next().is_none());
        let path = resolve_openfx_binary(&PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY")
                .expect("current owned fixture"),
        ))
        .expect("fixture binary");
        let binary = native_string(&path).expect("native binary");
        let bundle = native_string(&bundle_for_binary(&path).expect("native bundle"))
            .expect("native bundle string");
        let identifier = CString::new("uk.co.thefoundry.BasicGainPlugin").expect("identifier");
        let scale = CString::new("scale").expect("original Basic parameter");
        let parameters = [NativeParameter {
            name: scale.as_ptr(),
            kind: 1,
            value: [1.0, 0.0, 0.0, 0.0],
            query: None,
            query_context: std::ptr::null(),
            query_revision: 0,
            query_mode: 0,
            string_value: std::ptr::null(),
            string_length: 0,
        }];
        let info = NativeFrameInfo {
            source_alpha: 0,
            width: 3,
            height: 2,
            frame: 0.0,
            frame_rate: 24.0,
            first_frame: 0.0,
            last_frame: 2.0,
            pixel_aspect_ratio: 2.0,

            project_width: 3,
            project_height: 2,
            project_pixel_aspect_ratio: 2.0,
        };
        // Aligned, disjoint Float32 storage; signed/HDR RGB and varied straight
        // alpha make both an accidental copy and an Output clear observable.
        let source = [
            [-0.5_f32, 2.0, 0.125, 0.5],
            [3.0, -2.0, 0.25, 0.25],
            [0.125, 8.0, -4.0, 0.0],
            [1.5, -0.25, 4.0, 1.0],
            [-8.0, 0.5, 2.0, 0.75],
            [2.0, -4.0, 0.0625, 0.125],
        ];
        let original_source = bytemuck::cast_slice::<_, u8>(&source).to_vec();
        let mut output = [[f32::from_bits(0xa5a5_a5a5); 4]; 6];
        let sentinel = bytemuck::cast_slice::<_, u8>(&output).to_vec();
        let mut session = NativeSession::create(
            mondrian_core::EffectProcessorOwnerId::new(),
            &binary,
            &bundle,
            &identifier,
            bytemuck::cast_slice(&source),
            &info,
            &parameters,
        )
        .expect("real current-source-compatible native session");
        let result = session.render(
            bytemuck::cast_slice(&source),
            bytemuck::cast_slice_mut(&mut output),
            &info,
            &parameters,
        );
        let completion = session.report.completion;
        let output_alpha = session.report.output_alpha;
        let feedback = session.take_feedback();
        // Always retire the actual native session before any target assertion.
        // This raw CABI test has no Product worker or shutdown-timing claim.
        drop(session);

        assert!(
            matches!(result, Err(NativeRenderError::Rejected(7))),
            "{mode}: unsupported current-raster identity must return exact CABI 7: {result:?}"
        );
        assert_eq!(
            completion, 0,
            "rejected identity keeps Render completion UNSET"
        );
        assert_eq!(
            output_alpha, -1,
            "rejected identity has no output-alpha publication"
        );
        assert!(
            feedback.is_none(),
            "rejected identity has no successful frame feedback"
        );
        assert_eq!(bytemuck::cast_slice::<_, u8>(&output), sentinel);
        assert_eq!(bytemuck::cast_slice::<_, u8>(&source), original_source);

        let entries = std::fs::read_dir(&directory)
            .expect("actual native action directory")
            .take(2)
            .collect::<Result<Vec<_>, _>>()
            .expect("bounded directory entries");
        assert_eq!(
            entries.len(),
            1,
            "only the actual qualification process emits actions"
        );
        let expected_path = directory.join(format!("actions-{}.txt", std::process::id()));
        assert_eq!(entries[0].path(), expected_path);
        let mut bytes = Vec::new();
        std::fs::File::open(expected_path)
            .expect("actual native action receipt")
            .take(4097)
            .read_to_end(&mut bytes)
            .expect("bounded complete action receipt");
        assert!(!bytes.is_empty() && bytes.len() <= 4096 && bytes.is_ascii());
        let text = std::str::from_utf8(&bytes).expect("fixed ASCII action rows");
        assert!(text.ends_with('\n'));
        let rows = text.lines().collect::<Vec<_>>();
        assert!(rows.len() <= 32);
        let mut selected = Vec::new();
        let mut queries = 0;
        let mut renders = 0;
        for (index, row) in rows.into_iter().enumerate() {
            let fields = row.split_whitespace().collect::<Vec<_>>();
            assert_eq!(fields.len(), 9);
            assert_eq!(
                fields[0].parse::<usize>().expect("action ordinal"),
                index + 1
            );
            assert_eq!(&fields[1..3], ["D=0", "I=0"]);
            assert_eq!(fields[8], "complete=1");
            assert_ne!(fields[3], "overflow");
            queries +=
                usize::from(fields[3] == "OfxImageEffectActionIsIdentity" && fields[4] == "enter");
            renders +=
                usize::from(fields[3] == "OfxImageEffectActionRender" && fields[4] == "enter");
            if fields[3] == "OfxImageEffectActionIsIdentity"
                || fields[3] == "identity-current"
                || fields[3] == mode
            {
                selected.push(fields[3..].join(" "));
            }
        }
        assert_eq!(
            (queries, renders),
            (1, 0),
            "real query without any Render fallback"
        );
        assert_eq!(
            selected,
            [
                "OfxImageEffectActionIsIdentity enter attempted=0 accepted=0 status=0 complete=1"
                    .to_owned(),
                "identity-current contract attempted=1 accepted=1 status=0 complete=1".to_owned(),
                format!("{mode} contract attempted=1 accepted=1 status=0 complete=1"),
                "OfxImageEffectActionIsIdentity return attempted=0 accepted=0 status=0 complete=1"
                    .to_owned(),
            ],
            "the original Basic OK is followed only by the actual outArgs selection change"
        );
    }

    #[test]
    #[ignore = "set MONDRIAN_OPENFX_SESSION_PROBE_BINARY to the owned output_alpha wrapper"]
    fn native_session_output_alpha_reports_negotiated_clip_and_image_metadata() {
        const TEST: &str = "openfx_host::shared_frame_tests::native_session_output_alpha_reports_negotiated_clip_and_image_metadata";
        if !qualification_child(
            TEST,
            &[("MONDRIAN_OPENFX_SESSION_PROBE_MODE", "output_alpha")],
        ) {
            return;
        }
        let path = resolve_openfx_binary(&PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY")
                .expect("owned alpha probe binary"),
        ))
        .expect("probe binary");
        let bundle_path = path
            .ancestors()
            .find(|path| path.extension().is_some_and(|extension| extension == "bundle"))
            .expect("probe bundle");
        let binary = native_string(&path).expect("binary");
        let bundle = native_string(bundle_path).expect("bundle");
        let identifier = CString::new("uk.co.thefoundry.BasicGainPlugin").expect("identifier");
        let scale = CString::new("scale").expect("scale");
        let alpha = CString::new("alphaMode").expect("alpha mode");
        let original = [
            [-0.25_f32, 0.5, 2.0, 0.0],
            [2.0, -4.0, 8.0, f32::MIN_POSITIVE],
            [-0.5, 0.75, 4.0, 0.25],
            [1.0, -2.0, 0.5, 1.0],
        ];
        let encode = |pixels: &[[f32; 4]]| {
            pixels
                .iter()
                .flat_map(|pixel| pixel.iter().flat_map(|channel| channel.to_ne_bytes()))
                .collect::<Vec<_>>()
        };
        let input = encode(&original);
        let parameters = |choice: i32, gain: f64| {
            [
                NativeParameter {
                    name: scale.as_ptr(),
                    kind: 1,
                    value: [gain, 0.0, 0.0, 0.0],
                    query: None,
                    query_context: std::ptr::null(),
                    query_revision: 0,
                    query_mode: 0,
                    string_value: std::ptr::null(),
                    string_length: 0,
                },
                NativeParameter {
                    name: alpha.as_ptr(),
                    kind: 4,
                    value: [f64::from(choice), 0.0, 0.0, 0.0],
                    query: None,
                    query_context: std::ptr::null(),
                    query_revision: 0,
                    query_mode: 0,
                    string_value: std::ptr::null(),
                    string_length: 0,
                },
            ]
        };
        let mut info = NativeFrameInfo {
            source_alpha: 0,
            width: 2,
            height: 2,
            frame: 0.0,
            frame_rate: 24.0,
            first_frame: 0.0,
            last_frame: 100.0,
            pixel_aspect_ratio: 2.0,

            project_width: 2,
            project_height: 2,
            project_pixel_aspect_ratio: 2.0,
        };
        for _ in 0..2 {
            let mut session = NativeSession::create(
                mondrian_core::EffectProcessorOwnerId::new(),
                &binary,
                &bundle,
                &identifier,
                &input,
                &info,
                &parameters(0, 0.5),
            )
            .expect("create owned alpha session");
            for (frame, choice, gain) in [
                (0.0, 0, 0.5_f64),
                (0.0, 1, 0.5),
                (42.0, 2, 0.75),
                (0.0, 8, 0.5),
                (0.0, 1, 0.75),
                (0.0, 0, 0.5),
            ] {
                info.frame = frame;
                let mut output = vec![0xa5; input.len()];
                assert_eq!(session.render(&input, &mut output, &info, &parameters(choice, gain)), Ok(match choice {
                        0 => mondrian_core::ColorFrameAlpha::StraightFloat,
                        2 => mondrian_core::ColorFrameAlpha::Opaque,
                        _ => mondrian_core::ColorFrameAlpha::PremultipliedFloat,
                    }),
                    "probe independently checks Source Straight and negotiated Output clip/image metadata");
                let expected = original.map(|source| {
                    let mut pixel = source.map(|value| value * gain as f32);
                    if choice == 1 || choice == 8 {
                        pixel[0] *= pixel[3];
                        pixel[1] *= pixel[3];
                        pixel[2] *= pixel[3];
                        if choice == 8 && pixel[3] == 0.0 {
                            pixel[0] = 7.0;
                            pixel[1] = -3.0;
                            pixel[2] = 11.0;
                        }
                    } else if choice == 2 {
                        pixel[3] = 123.0;
                    }
                    pixel
                });
                assert_eq!(
                    output,
                    encode(&expected),
                    "direct native staging must retain the requested representation"
                );
            }
        }
    }

    #[test]
    #[ignore = "set MONDRIAN_OPENFX_SESSION_PROBE_BINARY to the owned output_alpha wrapper"]
    fn native_session_output_alpha_rejects_unknown_preference_before_render() {
        const TEST: &str = "openfx_host::shared_frame_tests::native_session_output_alpha_rejects_unknown_preference_before_render";
        if !qualification_child(
            TEST,
            &[("MONDRIAN_OPENFX_SESSION_PROBE_MODE", "output_alpha")],
        ) {
            return;
        }
        let path = resolve_openfx_binary(&PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY")
                .expect("owned alpha probe binary"),
        ))
        .expect("probe binary");
        let bundle_path = path
            .ancestors()
            .find(|path| path.extension().is_some_and(|extension| extension == "bundle"))
            .expect("probe bundle");
        let binary = native_string(&path).expect("binary");
        let bundle = native_string(bundle_path).expect("bundle");
        let identifier = CString::new("uk.co.thefoundry.BasicGainPlugin").expect("identifier");
        let name = CString::new("alphaMode").expect("alpha mode");
        let parameters = [NativeParameter {
            name: name.as_ptr(),
            kind: 4,
            value: [3.0, 0.0, 0.0, 0.0],
            query: None,
            query_context: std::ptr::null(),
            query_revision: 0,
            query_mode: 0,
            string_value: std::ptr::null(),
            string_length: 0,
        }];
        let info = NativeFrameInfo {
            source_alpha: 0,
            width: 1,
            height: 1,
            frame: 0.0,
            frame_rate: 24.0,
            first_frame: 0.0,
            last_frame: 100.0,
            pixel_aspect_ratio: 1.0,

            project_width: 1,
            project_height: 1,
            project_pixel_aspect_ratio: 1.0,
        };
        let input = [0.25_f32; 4]
            .iter()
            .flat_map(|channel| channel.to_ne_bytes())
            .collect::<Vec<_>>();
        let mut session = NativeSession::create(
            mondrian_core::EffectProcessorOwnerId::new(),
            &binary,
            &bundle,
            &identifier,
            &input,
            &info,
            &parameters,
        )
        .expect("preference negotiation is deferred until render");
        let mut output = vec![0xa5; input.len()];
        // The owned fixture aborts if Render is called for this invalid request.
        assert_eq!(
            session.render(&input, &mut output, &info, &parameters),
            Err(NativeRenderError::Rejected(7))
        );
        assert_eq!(
            output,
            vec![0xa5; input.len()],
            "refusal precedes any staging fetch/write"
        );
        assert_eq!(
            session.render(&input, &mut output, &info, &parameters),
            Err(NativeRenderError::Rejected(2)),
            "unsupported negotiated instance remains poisoned"
        );
    }

    #[test]
    #[ignore = "requires the owned SessionProbe image_uid native bundle"]
    fn native_actual_image_identifiers_are_stable_distinct_and_invalidate_analysis() {
        const TEST: &str = "openfx_host::shared_frame_tests::native_actual_image_identifiers_are_stable_distinct_and_invalidate_analysis";
        if !qualification_child(TEST, &[("MONDRIAN_OPENFX_SESSION_PROBE_MODE", "image_uid")]) {
            return;
        }
        native_actual_image_metadata_cases(true);
    }

    #[test]
    #[ignore = "requires the owned SessionProbe image_field native bundle"]
    fn native_actual_progressive_images_report_field_none() {
        const TEST: &str =
            "openfx_host::shared_frame_tests::native_actual_progressive_images_report_field_none";
        if !qualification_child(
            TEST,
            &[("MONDRIAN_OPENFX_SESSION_PROBE_MODE", "image_field")],
        ) {
            return;
        }
        native_actual_image_metadata_cases(false);
    }

    fn native_actual_image_metadata_cases(identifiers: bool) {
        let path = resolve_openfx_binary(&PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY")
                .expect("owned image probe binary"),
        ))
        .expect("probe binary");
        let bundle_path = path
            .ancestors()
            .find(|path| path.extension().is_some_and(|extension| extension == "bundle"))
            .expect("probe bundle");
        let binary = native_string(&path).expect("binary");
        let bundle = native_string(bundle_path).expect("bundle");
        let identifier = CString::new("uk.co.thefoundry.BasicGainPlugin").expect("identifier");
        let name = CString::new("scale").expect("scale");
        let parameters = [NativeParameter {
            name: name.as_ptr(),
            query_mode: 0,
            kind: 1,
            value: [1.0, 0.0, 0.0, 0.0],
            string_value: std::ptr::null(),
            string_length: 0,
            query: None,
            query_context: std::ptr::null(),
            query_revision: 0,
        }];
        // The fixture analysis cache deliberately stays loaded across native
        // session teardown. This independent handle invokes no host callback.
        let library = unsafe { libloading::Library::new(&path) }
            .expect("retain owned fixture analysis module");
        type Identifier = unsafe extern "C" fn(c_int, *mut c_char, usize, *mut usize) -> c_int;
        let get_identifier: libloading::Symbol<'_, Identifier> =
            unsafe { library.get(b"SessionProbeImageIdentifier\0") }
                .expect("fixture-only metadata export");
        let read_identifier = |output| {
            let mut buffer = [0_u8; 128];
            let mut length = 0;
            assert_eq!(
                unsafe {
                    get_identifier(
                        output,
                        buffer.as_mut_ptr().cast(),
                        buffer.len(),
                        &mut length,
                    )
                },
                0
            );
            assert!(length <= buffer.len());
            String::from_utf8(buffer[..length].to_vec()).expect("host image UID is ASCII")
        };
        let encode = |pixel: [f32; 4]| {
            pixel.iter().flat_map(|value| value.to_ne_bytes()).collect::<Vec<_>>()
        };
        let mut info = NativeFrameInfo {
            source_alpha: 0,
            width: 1,
            height: 1,
            frame: 0.0,
            frame_rate: 24.0,
            first_frame: 0.0,
            last_frame: 100.0,
            pixel_aspect_ratio: 2.0,

            project_width: 1,
            project_height: 1,
            project_pixel_aspect_ratio: 2.0,
        };
        let mut previous = None;
        for owner in 0..2 {
            let initial = encode([0.25, -0.5, 1.5, 0.75]);
            let mut session = NativeSession::create(
                mondrian_core::EffectProcessorOwnerId::new(),
                &binary,
                &bundle,
                &identifier,
                &initial,
                &info,
                &parameters,
            )
            .expect("create supervised actual-image session");
            for (frame, pixel) in [
                (0.0, [0.25, -0.5, 1.5, 0.75]),
                (0.0, [0.5, -0.25, 2.0, 0.75]),
                (42.0, [0.125, -1.0, 4.0, 0.75]),
                (0.0, [0.75, -0.125, 8.0, 0.75]),
            ] {
                info.frame = frame;
                let input = encode(pixel);
                let mut output = vec![0xa5; input.len()];
                session.render(&input, &mut output, &info, &parameters).expect(
                    "actual Source and Output metadata satisfy their independent fixture oracle",
                );
                assert_eq!(output, input, "analysis cache must use the changed raster, equal time, seek and new owner ({owner})");
                if identifiers {
                    let source = read_identifier(0);
                    let output = read_identifier(1);
                    assert!(!source.is_empty() && source.is_ascii());
                    assert!(!output.is_empty() && output.is_ascii());
                    assert_ne!(source, output);
                    if let Some((prior_source, prior_output)) = previous.as_ref() {
                        assert_ne!(
                            &source, prior_source,
                            "changed binding/new owner invalidates Source analysis"
                        );
                        assert_ne!(
                            &output, prior_output,
                            "Output has an independent binding identity"
                        );
                    }
                    previous = Some((source, output));
                }
            }
            drop(session);
        }
    }

    // The existing NativeSession supervision and independent pixel/rebind
    // oracle are reused. Only SessionProbe's suite-contract mode changes.
    #[test]
    fn native_curve_query_snapshot_is_safe_for_shared_callback_reads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<super::curves::CurveContext>();
    }
    #[test]
    #[ignore = "requires the owned SessionProbe mutex_contract native bundle"]
    fn native_mutex_suite_protects_foreign_threads_and_recursive_initial_count() {
        const TEST: &str = "openfx_host::shared_frame_tests::native_mutex_suite_protects_foreign_threads_and_recursive_initial_count";
        if !qualification_child(
            TEST,
            &[("MONDRIAN_OPENFX_SESSION_PROBE_MODE", "mutex_contract")],
        ) {
            return;
        }
        native_actual_image_metadata_cases(false);
    }
    #[test]
    #[ignore = "requires the owned SessionProbe mutex_negative native bundle"]
    fn native_mutex_suite_accepts_signed_initial_counts_without_fake_credits() {
        const TEST: &str = "openfx_host::shared_frame_tests::native_mutex_suite_accepts_signed_initial_counts_without_fake_credits";
        if !qualification_child(
            TEST,
            &[("MONDRIAN_OPENFX_SESSION_PROBE_MODE", "mutex_negative")],
        ) {
            return;
        }
        native_actual_image_metadata_cases(false);
    }
    #[test]
    #[ignore = "requires the owned SessionProbe mutex_lifecycle native bundle"]
    fn native_mutex_destruction_invalidates_waiters_without_dangling_state() {
        const TEST: &str = "openfx_host::shared_frame_tests::native_mutex_destruction_invalidates_waiters_without_dangling_state";
        if !qualification_child(
            TEST,
            &[("MONDRIAN_OPENFX_SESSION_PROBE_MODE", "mutex_lifecycle")],
        ) {
            return;
        }
        native_actual_image_metadata_cases(false);
    }
    #[test]
    #[ignore = "requires the owned SessionProbe thread_contract native bundle"]
    fn native_thread_suite_reports_real_spawned_callback_and_rejects_nesting() {
        const TEST: &str = "openfx_host::shared_frame_tests::native_thread_suite_reports_real_spawned_callback_and_rejects_nesting";
        if !qualification_child(
            TEST,
            &[("MONDRIAN_OPENFX_SESSION_PROBE_MODE", "thread_contract")],
        ) {
            return;
        }
        native_actual_image_metadata_cases(false);
    }
    #[test]
    #[ignore = "requires the owned SessionProbe thread_concurrent native bundle"]
    fn native_thread_suite_keeps_completion_owned_until_accepted_caller_returns() {
        const TEST: &str = "openfx_host::shared_frame_tests::native_thread_suite_keeps_completion_owned_until_accepted_caller_returns";
        if !qualification_child(
            TEST,
            &[("MONDRIAN_OPENFX_SESSION_PROBE_MODE", "thread_concurrent")],
        ) {
            return;
        }
        native_actual_image_metadata_cases(false);
    }
    #[test]
    #[ignore = "requires the owned SessionProbe mutex_capacity native bundle"]
    fn native_mutex_suite_releases_bounded_owner_slots_after_admission_refusal() {
        const TEST: &str = "openfx_host::shared_frame_tests::native_mutex_suite_releases_bounded_owner_slots_after_admission_refusal";
        if !qualification_child(
            TEST,
            &[("MONDRIAN_OPENFX_SESSION_PROBE_MODE", "mutex_capacity")],
        ) {
            return;
        }
        native_actual_image_metadata_cases(false);
    }

    #[test]
    fn callback_thread_and_mutex_storage_are_charged_before_native_loading() {
        let suite = native_thread_resident_bytes().expect("supported platform reservation");
        assert!(
            suite > 8 * 1024 * 1024,
            "stack, guard and fixed owner state are charged"
        );
        let auxiliary = native_auxiliary_resident_bytes().expect("known auxiliary policy");
        let small = OpenFxFilterSession::storage(1, 1).expect("small charge").active_peak_bytes();
        let large = OpenFxFilterSession::storage(2, 3).expect("large charge").active_peak_bytes();
        assert_eq!(
            small,
            suite
                + auxiliary
                + 3 * std::mem::size_of::<mondrian_core::EffectProcessorOwnerId>()
                + 3 * 16
        );
        assert_eq!(
            large - small,
            (6 - 1) * 3 * 16,
            "owner reservation is charged once, independent of raster extent"
        );
    }
    #[test]
    #[ignore = "requires owned SessionProbe persisted image-analysis fixture"]
    fn native_image_identifiers_invalidate_persisted_analysis_across_fresh_children() {
        const TEST: &str = "openfx_host::shared_frame_tests::native_image_identifiers_invalidate_persisted_analysis_across_fresh_children";
        const CACHE: &str = "MONDRIAN_OPENFX_IMAGE_ANALYSIS_FILE";
        const PHASE: &str = "MONDRIAN_OPENFX_IMAGE_ANALYSIS_PHASE";
        if std::env::var("MONDRIAN_OPENFX_QUALIFICATION_CHILD").ok().as_deref() != Some(TEST) {
            let directory = tempfile::tempdir().expect("owned plugin-private analysis directory");
            let cache = directory.path().join("image-analysis.bin");
            let cache = cache.to_str().expect("test fixture analysis pathname");
            for phase in ["first", "replacement", "reopen"] {
                assert!(!qualification_child(
                    TEST,
                    &[
                        ("MONDRIAN_OPENFX_SESSION_PROBE_MODE", "image_uid_persisted"),
                        (CACHE, cache),
                        (PHASE, phase)
                    ]
                ));
            }
            return;
        }
        let path = resolve_openfx_binary(&PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY").expect("owned probe binary"),
        ))
        .expect("fixture selection");
        let bundle = path
            .ancestors()
            .find(|path| path.extension().is_some_and(|extension| extension == "bundle"))
            .expect("bundle");
        let binary = native_string(&path).expect("binary");
        let bundle = native_string(bundle).expect("bundle");
        let identifier = CString::new("uk.co.thefoundry.BasicGainPlugin").expect("identifier");
        let name = CString::new("scale").expect("scale");
        let parameters = [NativeParameter {
            name: name.as_ptr(),
            query_mode: 0,
            kind: 1,
            value: [2.0, 0.0, 0.0, 0.0],
            string_value: std::ptr::null(),
            string_length: 0,
            query: None,
            query_context: std::ptr::null(),
            query_revision: 0,
        }];
        let info = NativeFrameInfo {
            source_alpha: 0,
            width: 1,
            height: 1,
            frame: 0.0,
            frame_rate: 24.0,
            first_frame: 0.0,
            last_frame: 100.0,
            pixel_aspect_ratio: 1.0,

            project_width: 1,
            project_height: 1,
            project_pixel_aspect_ratio: 1.0,
        };
        let phase = std::env::var(PHASE).expect("actual process lifecycle phase");
        let pixel = if phase == "replacement" {
            [5.0, -7.0, 11.0, 0.75]
        } else {
            [0.25, -0.5, 1.5, 0.25]
        };
        let encode =
            |pixel: [f32; 4]| pixel.into_iter().flat_map(f32::to_ne_bytes).collect::<Vec<_>>();
        let input = encode(pixel);
        let mut output = vec![0xa5; input.len()];
        let mut session = NativeSession::create(
            mondrian_core::EffectProcessorOwnerId::new(),
            &binary,
            &bundle,
            &identifier,
            &input,
            &info,
            &parameters,
        )
        .expect("fresh native owner");
        session
            .render(&input, &mut output, &info, &parameters)
            .expect("valid plugin analysis survives a fresh process");
        assert_eq!(
            output,
            encode(pixel.map(|channel| channel * 2.0)),
            "same raster/time in a fresh child must not reuse different-image analysis"
        );
    }

    #[test]
    #[ignore = "requires owned SessionProbe image_uid fixture"]
    fn native_namespace_is_parent_issued_copied_once_and_nil_rejected_before_host() {
        const TEST:&str="openfx_host::shared_frame_tests::native_namespace_is_parent_issued_copied_once_and_nil_rejected_before_host";
        if !qualification_child(TEST, &[("MONDRIAN_OPENFX_SESSION_PROBE_MODE", "image_uid")]) {
            return;
        }
        let path = resolve_openfx_binary(&PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY").expect("fixture"),
        ))
        .expect("binary");
        let bundle = path
            .ancestors()
            .find(|path| path.extension().is_some_and(|extension| extension == "bundle"))
            .expect("bundle");
        let binary = native_string(&path).expect("binary");
        let bundle = native_string(bundle).expect("bundle");
        let identifier = CString::new("uk.co.thefoundry.BasicGainPlugin").expect("identifier");
        let info = NativeFrameInfo {
            source_alpha: 0,
            width: 1,
            height: 1,
            frame: 0.0,
            frame_rate: 24.0,
            first_frame: 0.0,
            last_frame: 10.0,
            pixel_aspect_ratio: 1.0,

            project_width: 1,
            project_height: 1,
            project_pixel_aspect_ratio: 1.0,
        };
        let input = [0.25f32, -0.5, 1.5, 0.25]
            .into_iter()
            .flat_map(f32::to_ne_bytes)
            .collect::<Vec<_>>();
        assert_eq!(
            NativeSession::create(
                mondrian_core::EffectProcessorOwnerId(uuid::Uuid::nil()),
                &binary,
                &bundle,
                &identifier,
                &input,
                &info,
                &[]
            )
            .err(),
            Some(2)
        );
        let nil = [0u8; 16];
        for namespace in [std::ptr::null(), nil.as_ptr()] {
            let mut handle = std::ptr::null_mut();
            // SAFETY: all non-null ranges are complete borrowed initialization
            // facts; namespace is either explicitly null or sixteen bytes.
            let status = unsafe {
                mondrian_openfx_session_create(
                    binary.as_ptr(),
                    bundle.as_ptr(),
                    identifier.as_ptr(),
                    OpenFxContext::Filter.native(),
                    namespace,
                    input.as_ptr().cast(),
                    input.len(),
                    &info,
                    std::ptr::null(),
                    0,
                    &mut handle,
                )
            };
            assert_eq!(status, 2);
            assert!(handle.is_null(), "invalid identity acquires no SDK owner");
        }
        let owner = mondrian_core::EffectProcessorOwnerId::new();
        let mut copied_owner = owner;
        let mut session = NativeSession::create(
            copied_owner,
            &binary,
            &bundle,
            &identifier,
            &input,
            &info,
            &[],
        )
        .expect("exact parent-owned namespace");
        copied_owner = mondrian_core::EffectProcessorOwnerId::new();
        assert_ne!(copied_owner, owner);
        let mut output = vec![0; input.len()];
        session
            .render(&input, &mut output, &info, &[])
            .expect("actual native image fetch");
        let library =
            unsafe { libloading::Library::new(&path) }.expect("read owned image-oracle export");
        type Identifier = unsafe extern "C" fn(c_int, *mut c_char, usize, *mut usize) -> c_int;
        let identifier: libloading::Symbol<'_, Identifier> =
            unsafe { library.get(b"SessionProbeImageIdentifier\0") }.expect("actual UID export");
        let mut text = [0u8; 256];
        let mut length = 0;
        assert_eq!(
            unsafe { identifier(0, text.as_mut_ptr().cast(), text.len(), &mut length) },
            0
        );
        let actual = std::str::from_utf8(&text[..length]).expect("ASCII UID");
        assert!(
            actual.starts_with(&format!("mondrian-ofx:{}:", owner.0.simple())),
            "namespace is the copied parent identity: {actual}"
        );
        assert!(!actual.contains(&copied_owner.0.simple().to_string()));
    }

    // S02 controls use the existing NativeSession API and owned pixel oracle.
    #[test]
    #[ignore = "requires the owned SessionProbe aux_transient_null native bundle"]
    fn native_message_suite_accepts_null_transient_target() {
        const TEST: &str =
            "openfx_host::shared_frame_tests::native_message_suite_accepts_null_transient_target";
        if !qualification_child(
            TEST,
            &[("MONDRIAN_OPENFX_SESSION_PROBE_MODE", "aux_transient_null")],
        ) {
            return;
        }
        native_actual_image_metadata_cases(false);
    }
    #[test]
    #[ignore = "requires the owned SessionProbe aux_transient_descriptor native bundle"]
    fn native_message_suite_accepts_descriptor_transient_target() {
        const TEST: &str = "openfx_host::shared_frame_tests::native_message_suite_accepts_descriptor_transient_target";
        if !qualification_child(
            TEST,
            &[(
                "MONDRIAN_OPENFX_SESSION_PROBE_MODE",
                "aux_transient_descriptor",
            )],
        ) {
            return;
        }
        native_actual_image_metadata_cases(false);
    }
    #[test]
    #[ignore = "requires the owned SessionProbe aux_transient_instance native bundle"]
    fn native_message_suite_formats_utf8_instance_notices() {
        const TEST: &str =
            "openfx_host::shared_frame_tests::native_message_suite_formats_utf8_instance_notices";
        if !qualification_child(
            TEST,
            &[(
                "MONDRIAN_OPENFX_SESSION_PROBE_MODE",
                "aux_transient_instance",
            )],
        ) {
            return;
        }
        native_actual_image_metadata_cases(false);
    }
    #[test]
    #[ignore = "requires the owned SessionProbe aux_transient_destroy native bundle"]
    fn native_message_suite_preserves_live_target_through_sdk_base_destruction() {
        const TEST: &str = "openfx_host::shared_frame_tests::native_message_suite_preserves_live_target_through_sdk_base_destruction";
        if !qualification_child(
            TEST,
            &[(
                "MONDRIAN_OPENFX_SESSION_PROBE_MODE",
                "aux_transient_destroy",
            )],
        ) {
            return;
        }
        native_actual_image_metadata_cases(false);
    }
    #[test]
    #[ignore = "requires the owned SessionProbe aux_question native bundle"]
    fn native_message_suite_refuses_questions_in_noninteractive_worker() {
        const TEST: &str = "openfx_host::shared_frame_tests::native_message_suite_refuses_questions_in_noninteractive_worker";
        if !qualification_child(
            TEST,
            &[("MONDRIAN_OPENFX_SESSION_PROBE_MODE", "aux_question")],
        ) {
            return;
        }
        native_actual_image_metadata_cases(false);
    }
    #[test]
    #[ignore = "requires the owned SessionProbe aux_invalid_handles native bundle"]
    fn native_auxiliary_suites_reject_wrong_object_and_rubbish_handles() {
        const TEST: &str = "openfx_host::shared_frame_tests::native_auxiliary_suites_reject_wrong_object_and_rubbish_handles";
        if !qualification_child(
            TEST,
            &[("MONDRIAN_OPENFX_SESSION_PROBE_MODE", "aux_invalid_handles")],
        ) {
            return;
        }
        native_actual_image_metadata_cases(false);
    }
    #[test]
    #[ignore = "requires the owned SessionProbe aux_progress native bundle"]
    fn native_progress_suite_owns_begin_update_end_lifecycle() {
        const TEST: &str = "openfx_host::shared_frame_tests::native_progress_suite_owns_begin_update_end_lifecycle";
        if !qualification_child(
            TEST,
            &[("MONDRIAN_OPENFX_SESSION_PROBE_MODE", "aux_progress")],
        ) {
            return;
        }
        native_actual_image_metadata_cases(false);
    }
    #[test]
    #[ignore = "requires the owned SessionProbe aux_timeline native bundle"]
    fn native_timeline_suite_reads_snapshot_and_refuses_unsatisfied_navigation() {
        const TEST: &str = "openfx_host::shared_frame_tests::native_timeline_suite_reads_snapshot_and_refuses_unsatisfied_navigation";
        if !qualification_child(
            TEST,
            &[("MONDRIAN_OPENFX_SESSION_PROBE_MODE", "aux_timeline")],
        ) {
            return;
        }
        native_actual_image_metadata_cases(false);
    }
    #[test]
    #[ignore = "requires the owned SessionProbe aux_warning native bundle"]
    fn native_persistent_warning_and_information_allow_actual_output() {
        const TEST: &str = "openfx_host::shared_frame_tests::native_persistent_warning_and_information_allow_actual_output";
        if !qualification_child(
            TEST,
            &[("MONDRIAN_OPENFX_SESSION_PROBE_MODE", "aux_warning")],
        ) {
            return;
        }
        native_actual_image_metadata_cases(false);
    }
    fn qualify_native_legacy_render_priority(test: &str, selected: &str) {
        if !qualification_child(test, &[("MONDRIAN_OPENFX_SESSION_PROBE_MODE", selected)]) {
            return;
        }
        let path = resolve_openfx_binary(&PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY")
                .expect("owned priority fixture"),
        ))
        .expect("fixture module");
        let bundle_path = path
            .ancestors()
            .find(|path| path.extension().is_some_and(|extension| extension == "bundle"))
            .expect("fixture bundle");
        let binary = native_string(&path).expect("module path");
        let bundle = native_string(bundle_path).expect("bundle path");
        let identifier = CString::new("uk.co.thefoundry.BasicGainPlugin").expect("identifier");
        let scale = CString::new("scale").expect("fixture parameter");
        let mut parameters = [NativeParameter {
            name: scale.as_ptr(),
            kind: 1,
            value: [1.5, 0.0, 0.0, 0.0],
            query: None,
            query_context: std::ptr::null(),
            query_revision: 0,
            query_mode: 0,
            string_value: std::ptr::null(),
            string_length: 0,
        }];
        let pixels = [
            [-0.0_f32, -0.25, 2.0, 0.0],
            [2.0, -4.0, 8.0, f32::MIN_POSITIVE],
            [-0.5, 0.75, 4.0, 0.25],
            [1.0, -2.0, 0.5, 1.0],
        ];
        let encode = |pixels: &[[f32; 4]]| {
            pixels
                .iter()
                .flat_map(|pixel| pixel.iter().flat_map(|lane| lane.to_ne_bytes()))
                .collect::<Vec<_>>()
        };
        let input = encode(&pixels);
        let expected = |gain: f32| encode(&pixels.map(|pixel| pixel.map(|lane| lane * gain)));
        let info = NativeFrameInfo {
            source_alpha: 0,
            width: 2,
            height: 2,
            frame: 3.5,
            frame_rate: 24.0,
            first_frame: 0.0,
            last_frame: 100.0,
            pixel_aspect_ratio: 1.5,

            project_width: 2,
            project_height: 2,
            project_pixel_aspect_ratio: 1.5,
        };
        for _ in 0..2 {
            parameters[0].value[0] = 1.5;
            let mut session = NativeSession::create(
                mondrian_core::EffectProcessorOwnerId::new(),
                &binary,
                &bundle,
                &identifier,
                &input,
                &info,
                &parameters,
            )
            .expect("native owner");
            let mut output = vec![0xa5; input.len()];
            let result = session.render(&input, &mut output, &info, &parameters);
            if selected == "legacy_priority_error_only" {
                let Err(NativeRenderError::UserBlocked(first)) = result else {
                    panic!("complete frame plus Error remains recoverable: {result:?}");
                };
                assert!(first.is_blocked());
                assert_eq!(
                    first.persistent().expect("actual Error").id(),
                    "render.repair"
                );
                assert_eq!(
                    first.persistent().expect("actual Error").text(),
                    "Change scale to recover render"
                );
                assert_eq!(session.report.completion, 2);
                assert_eq!(session.report.output_alpha, -1);
                assert_eq!(
                    output,
                    expected(1.5),
                    "private staging is complete but unpublished"
                );
                assert!(
                    session.take_feedback().is_none(),
                    "no successful frame feedback"
                );
                output.fill(0xa5);
                let again = session.render(&input, &mut output, &info, &parameters);
                let Err(NativeRenderError::UserBlocked(second)) = again else {
                    panic!("unchanged author remains blocked without Render: {again:?}");
                };
                assert_eq!(second.native_owner(), first.native_owner());
                assert_eq!(second.persistent_revision(), first.persistent_revision());
                assert_eq!(
                    output,
                    vec![0xa5; input.len()],
                    "early blocking performs no Output fetch"
                );
                parameters[0].value[0] = 2.0;
                assert_eq!(
                    session.render(&input, &mut output, &info, &parameters),
                    Ok(mondrian_core::ColorFrameAlpha::StraightFloat)
                );
                assert_eq!(session.report.completion, 1);
                assert_eq!(session.report.output_alpha, 0);
                assert_eq!(output, expected(2.0));
                let recovered = session.take_feedback().expect("completed repaired frame");
                assert_eq!(recovered.native_owner(), first.native_owner());
                assert!(!recovered.is_blocked());
                assert!(recovered.persistent().is_none());
            } else {
                assert_eq!(result, Err(NativeRenderError::Rejected(5)), "{selected}");
                assert_eq!(
                    session.report.completion, 0,
                    "failure cannot publish a blocked/frame completion"
                );
                assert_eq!(
                    session.report.output_alpha, -1,
                    "failure cannot publish an output representation"
                );
                let native = session.report.feedback.decode().expect("actual conjunction report");
                assert!(
                    native.is_blocked(),
                    "actual Error remains in report despite rejected native action"
                );
                assert_eq!(
                    native.persistent().expect("actual Error").kind(),
                    mondrian_effects::EffectProcessorNoticeKind::Error
                );
                assert_eq!(
                    native.persistent().expect("actual Error").id(),
                    "render.repair"
                );
                assert_eq!(
                    native.persistent().expect("actual Error").text(),
                    "Change scale to recover render"
                );
                if selected == "legacy_priority_end_failed_error" {
                    assert_eq!(
                        output,
                        expected(1.5),
                        "End failure follows complete finite Basic output"
                    );
                } else {
                    assert_eq!(
                        output,
                        vec![0xa5; input.len()],
                        "ROI/Render failure precedes Output fetch"
                    );
                }
                assert!(
                    session.take_feedback().is_none(),
                    "rejected frame has no successful feedback"
                );
            }
            assert_eq!(input, encode(&pixels), "immutable Source bits");
        }
    }

    #[test]
    #[ignore = "requires newly built owned legacy Render priority SessionProbe"]
    fn native_render_failures_dominate_persistent_error_without_publication() {
        const TEST: &str = "openfx_host::shared_frame_tests::native_render_failures_dominate_persistent_error_without_publication";
        for selected in [
            "legacy_priority_render_fatal_error",
            "legacy_priority_roi_failed_error",
            "legacy_priority_end_failed_error",
        ] {
            qualify_native_legacy_render_priority(TEST, selected);
        }
    }

    #[test]
    #[ignore = "requires newly built owned complete-frame Error/recovery SessionProbe"]
    fn native_complete_render_error_recovers_same_owner_after_author_change() {
        const TEST: &str = "openfx_host::shared_frame_tests::native_complete_render_error_recovers_same_owner_after_author_change";
        qualify_native_legacy_render_priority(TEST, "legacy_priority_error_only");
    }

    #[test]
    #[ignore = "requires the owned SessionProbe aux_error native bundle"]
    fn native_persistent_error_blocks_render_and_recovers_same_owner_after_author_change() {
        const TEST: &str = "openfx_host::shared_frame_tests::native_persistent_error_blocks_render_and_recovers_same_owner_after_author_change";
        if !qualification_child(TEST, &[("MONDRIAN_OPENFX_SESSION_PROBE_MODE", "aux_error")]) {
            return;
        }
        let path = resolve_openfx_binary(&PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY")
                .expect("owned auxiliary suite probe binary"),
        ))
        .expect("probe binary");
        let bundle_path = path
            .ancestors()
            .find(|path| path.extension().is_some_and(|extension| extension == "bundle"))
            .expect("probe bundle");
        let binary = native_string(&path).expect("binary");
        let bundle = native_string(bundle_path).expect("bundle");
        let identifier = CString::new("uk.co.thefoundry.BasicGainPlugin").expect("identifier");
        let name = CString::new("scale").expect("scale");
        let mut parameters = [NativeParameter {
            name: name.as_ptr(),
            query_mode: 0,
            kind: 1,
            value: [1.0, 0.0, 0.0, 0.0],
            string_value: std::ptr::null(),
            string_length: 0,
            query: None,
            query_context: std::ptr::null(),
            query_revision: 0,
        }];
        let info = NativeFrameInfo {
            source_alpha: 0,
            width: 1,
            height: 1,
            frame: 0.0,
            frame_rate: 24.0,
            first_frame: 0.0,
            last_frame: 100.0,
            pixel_aspect_ratio: 2.0,

            project_width: 1,
            project_height: 1,
            project_pixel_aspect_ratio: 2.0,
        };
        let encode = |pixel: [f32; 4]| {
            pixel.iter().flat_map(|value| value.to_ne_bytes()).collect::<Vec<_>>()
        };
        let input = encode([0.25, -0.5, 1.5, 0.25]);
        let library =
            unsafe { libloading::Library::new(&path) }.expect("retain fixture counter module");
        type RenderCount = unsafe extern "C" fn() -> c_int;
        let count: libloading::Symbol<'_, RenderCount> =
            unsafe { library.get(b"SessionProbeAuxRenderCount\0") }
                .expect("fixture-only render counter");
        let mut session = NativeSession::create(
            mondrian_core::EffectProcessorOwnerId::new(),
            &binary,
            &bundle,
            &identifier,
            &input,
            &info,
            &parameters,
        )
        .expect("persistent Error leaves a valid native instance");
        let mut output = vec![0xa5; input.len()];
        let Err(NativeRenderError::UserBlocked(blocked)) =
            session.render(&input, &mut output, &info, &parameters)
        else {
            panic!("actual persistent Error is a recoverable typed disposition");
        };
        assert_eq!(blocked.invocation(), 1);
        assert_eq!(blocked.persistent_revision(), 1);
        let persistent = blocked.persistent().expect("actual persistent Error");
        assert_eq!(
            persistent.kind(),
            mondrian_effects::EffectProcessorNoticeKind::Error
        );
        assert_eq!(persistent.text(), "Change scale to recover");
        assert_eq!(persistent.id(), "user.repair");
        assert_eq!(
            unsafe { count() },
            0,
            "Render must not run while the persistent Error remains"
        );
        assert_eq!(
            output,
            vec![0xa5; input.len()],
            "no blocked frame bytes are published"
        );
        parameters[0].value[0] = 2.0;
        session
            .render(&input, &mut output, &info, &parameters)
            .expect("same instance recovers after authored InstanceChanged clears Error");
        assert_eq!(
            unsafe { count() },
            1,
            "recovery processes once without replay or owner replacement"
        );
        assert_eq!(
            output,
            encode([0.5, -1.0, 3.0, 0.5]),
            "independent official Basic analytic output"
        );
        let recovered = session.take_feedback().expect("actual same-owner clear");
        assert_eq!(recovered.native_owner(), blocked.native_owner());
        assert_eq!(recovered.invocation(), 2);
        assert!(recovered.persistent_revision() > blocked.persistent_revision());
        assert!(recovered.persistent().is_none());
    }

    #[test]
    #[ignore = "requires the owned SessionProbe aux_transient_instance bundle"]
    fn native_report_carries_exact_bounded_utf8_and_current_invocation() {
        const TEST: &str = "openfx_host::shared_frame_tests::native_report_carries_exact_bounded_utf8_and_current_invocation";
        if !qualification_child(
            TEST,
            &[(
                "MONDRIAN_OPENFX_SESSION_PROBE_MODE",
                "aux_transient_instance",
            )],
        ) {
            return;
        }
        let path = resolve_openfx_binary(&PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY").expect("owned fixture"),
        ))
        .expect("binary");
        let bundle = path
            .ancestors()
            .find(|path| path.extension().is_some_and(|extension| extension == "bundle"))
            .expect("bundle");
        let binary = native_string(&path).expect("binary");
        let bundle = native_string(bundle).expect("bundle");
        let identifier = CString::new("uk.co.thefoundry.BasicGainPlugin").expect("identifier");
        let name = CString::new("scale").expect("scale");
        let parameters = [NativeParameter {
            name: name.as_ptr(),
            query_mode: 0,
            kind: 1,
            value: [2.0, 0.0, 0.0, 0.0],
            string_value: std::ptr::null(),
            string_length: 0,
            query: None,
            query_context: std::ptr::null(),
            query_revision: 0,
        }];
        let info = NativeFrameInfo {
            source_alpha: 0,
            width: 1,
            height: 1,
            frame: 0.0,
            frame_rate: 24.0,
            first_frame: 0.0,
            last_frame: 100.0,
            pixel_aspect_ratio: 2.0,

            project_width: 1,
            project_height: 1,
            project_pixel_aspect_ratio: 2.0,
        };
        let encode =
            |pixel: [f32; 4]| pixel.into_iter().flat_map(f32::to_ne_bytes).collect::<Vec<_>>();
        let input = encode([0.25, -0.5, 1.5, 0.25]);
        let mut output = vec![0xa5; input.len()];
        let mut session = NativeSession::create(
            mondrian_core::EffectProcessorOwnerId::new(),
            &binary,
            &bundle,
            &identifier,
            &input,
            &info,
            &parameters,
        )
        .expect("actual owner");
        let mut native_owner = None;
        for invocation in 1..=2 {
            assert_eq!(
                session.render(&input, &mut output, &info, &parameters),
                Ok(mondrian_core::ColorFrameAlpha::StraightFloat)
            );
            assert_eq!(output, encode([0.5, -1.0, 3.0, 0.5]));
            let snapshot = session.take_feedback().expect("actual matching report");
            assert_eq!(snapshot.invocation(), invocation);
            assert_eq!(snapshot.notices().len(), 6);
            assert!(!snapshot.is_blocked());
            for pair in snapshot.notices().chunks_exact(2) {
                assert_eq!(pair[0].text(), "说明🙂 7 1.250");
                assert_eq!(pair[0].id(), "");
                assert_eq!(pair[1].text(), "第二条通知✓");
                assert_eq!(pair[1].id(), "");
            }
            let kinds: Vec<_> = snapshot.notices().iter().map(|notice| notice.kind()).collect();
            use mondrian_effects::EffectProcessorNoticeKind as Kind;
            assert_eq!(
                kinds,
                [
                    Kind::Information,
                    Kind::Information,
                    Kind::Warning,
                    Kind::Warning,
                    Kind::Log,
                    Kind::Log
                ]
            );
            if let Some(owner) = native_owner {
                assert_eq!(snapshot.native_owner(), owner);
            } else {
                native_owner = Some(snapshot.native_owner());
            }
            assert!(
                session.take_feedback().is_none(),
                "one report is consumed once"
            );
        }
    }
    #[test]
    #[ignore = "requires current product and owned SessionProbe cooperative_abort mode"]
    fn native_public_owner_cancellation_reaches_live_abort_before_worker_retirement() {
        const TEST: &str = "openfx_host::shared_frame_tests::native_public_owner_cancellation_reaches_live_abort_before_worker_retirement";
        const STARTED: &str = "MONDRIAN_OPENFX_COOPERATIVE_STARTED";
        const ABORTED: &str = "MONDRIAN_OPENFX_COOPERATIVE_ABORTED";
        let directory = tempfile::tempdir().expect("owned cooperative receipts");
        let started = directory.path().join("started.tsv");
        let aborted = directory.path().join("aborted.tsv");
        if !qualification_child(
            TEST,
            &[
                ("MONDRIAN_OPENFX_SESSION_PROBE_MODE", "cooperative_abort"),
                (STARTED, started.to_str().expect("UTF-8 started path")),
                (ABORTED, aborted.to_str().expect("UTF-8 aborted path")),
            ],
        ) {
            return;
        }
        let helper = PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_TEST_HELPER").expect("pinned current product"),
        );
        let probe = PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY")
                .expect("owned live-control probe"),
        );
        let started = PathBuf::from(std::env::var_os(STARTED).expect("actual receipt path"));
        let aborted = PathBuf::from(std::env::var_os(ABORTED).expect("actual receipt path"));
        let inspection = crate::openfx_adapter::inspect_openfx_binary(&helper, &probe)
            .expect("actual isolated discovery");
        let format = mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightCoverage,
        );
        let frame = mondrian_effects::EffectFloatFrame::from_pixels(
            mondrian_effects::EffectFrameExtent::new(1, 1),
            format,
            mondrian_core::ImageSampling::UNIT_SQUARE,
            vec![[0.25, -0.5, 1.5, 0.25]],
        )
        .expect("complete finite input");
        let mut session = OpenFxFilterSession::prepare(
            &helper,
            &inspection,
            "uk.co.thefoundry.BasicGainPlugin",
            1,
            1,
            format,
            mondrian_effects::EffectProjectGeometry::new(
                1,
                1,
                mondrian_core::SampleAspectRatio::SQUARE,
            )
            .expect("explicit owner project geometry"),
            &mut || Ok(()),
        )
        .expect("same supervised native owner");
        let process = session.worker_process_id();
        let owner = session.processor_owner();
        let timing = OpenFxRenderTiming {
            frame: 0.0,
            frame_rate: 24.0,
            first_frame: 0.0,
            last_frame: 100.0,
        };
        let mut canceled_after_started = false;
        let result = session.render(&timing, &BTreeMap::new(), &frame, &mut || {
            if started.is_file() {
                let receipt =
                    std::fs::read_to_string(&started).expect("actual callback started record");
                if receipt.ends_with('\n') {
                    canceled_after_started = true;
                    return Err(OpenFxHostError::Canceled);
                }
            }
            Ok(())
        });
        assert!(
            canceled_after_started,
            "actual Render reached the fixture before owner cancellation"
        );
        assert!(
            matches!(result, Err(OpenFxHostError::Canceled)),
            "exact owner cancel, no frame/native fault: {result:?}"
        );
        assert_eq!(
            frame.pixels(),
            [[0.25, -0.5, 1.5, 0.25]],
            "no canceled pixels are published"
        );
        session.shutdown().expect("same bounded retirement authority");
        let started = std::fs::read_to_string(&started).expect("actual Render observation");
        let aborted = std::fs::read_to_string(&aborted)
            .expect("cooperative native abort must be observed before supervisor retirement");
        assert_eq!(
            started, aborted,
            "same live request/image/worker observed the signal, no DSP replay"
        );
        let (pid, uid) = aborted.trim_end().split_once('\t').expect("actual PID and Source UID");
        assert_eq!(pid.parse::<u32>().expect("actual PID"), process);
        let parts: Vec<_> = uid.split(':').collect();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], "mondrian-ofx");
        assert_eq!(parts[1], owner.0.simple().to_string());
        assert_eq!(parts[3], "Source");
        assert!(
            parts[2].parse::<std::num::NonZeroU64>().is_ok(),
            "actual image generation"
        );
        assert_eq!(started.lines().count(), 1, "one actual Render, no replay");
    }
    #[test]
    #[ignore = "requires current Stage B product and owned cooperative_progress fixture"]
    fn native_live_progress_precedes_completion_on_the_public_owner_thread() {
        const TEST: &str = "openfx_host::shared_frame_tests::native_live_progress_precedes_completion_on_the_public_owner_thread";
        const STARTED: &str = "MONDRIAN_OPENFX_COOPERATIVE_STARTED";
        const RELEASE: &str = "MONDRIAN_OPENFX_COOPERATIVE_RELEASE";
        let directory = tempfile::tempdir().expect("owned progress receipts");
        let started = directory.path().join("started.tsv");
        let release = directory.path().join("release");
        if !qualification_child(
            TEST,
            &[
                ("MONDRIAN_OPENFX_SESSION_PROBE_MODE", "cooperative_progress"),
                (STARTED, started.to_str().expect("UTF-8 started path")),
                (RELEASE, release.to_str().expect("UTF-8 release path")),
            ],
        ) {
            return;
        }
        let helper = PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_TEST_HELPER").expect("pinned current product"),
        );
        let probe = PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY")
                .expect("owned live-control probe"),
        );
        let started = PathBuf::from(std::env::var_os(STARTED).expect("actual started path"));
        let release = PathBuf::from(std::env::var_os(RELEASE).expect("actual release path"));
        let inspection = crate::openfx_adapter::inspect_openfx_binary(&helper, &probe)
            .expect("real isolated discovery");
        let format = mondrian_effects::EffectValueFormat::new(
            mondrian_effects::EffectWorkingPrecision::Float32,
            mondrian_effects::EffectColorDomain::SceneLinearRgb,
            mondrian_core::ColorFrameAlpha::StraightCoverage,
        );
        let pixels = [[0.25_f32, -0.5, 1.5, 0.25]];
        let frame = mondrian_effects::EffectFloatFrame::from_pixels(
            mondrian_effects::EffectFrameExtent::new(1, 1),
            format,
            mondrian_core::ImageSampling::UNIT_SQUARE,
            pixels.to_vec(),
        )
        .expect("independent finite HDR input");
        let mut session = OpenFxFilterSession::prepare(
            &helper,
            &inspection,
            "uk.co.thefoundry.BasicGainPlugin",
            1,
            1,
            format,
            mondrian_effects::EffectProjectGeometry::new(
                1,
                1,
                mondrian_core::SampleAspectRatio::SQUARE,
            )
            .expect("explicit owner project geometry"),
            &mut || Ok(()),
        )
        .expect("same supervised owner");
        let parent_thread = std::thread::current().id();
        let owner = session.processor_owner();
        let process_id = session.worker_process_id();
        let timing = OpenFxRenderTiming {
            frame: 0.0,
            frame_rate: 24.0,
            first_frame: 0.0,
            last_frame: 100.0,
        };
        let mut saw_live = false;
        let mut last_revision = 0;
        let output = session
            .render_observed(
                &timing,
                &BTreeMap::new(),
                &frame,
                Some(Instant::now() + Duration::from_secs(5)),
                mondrian_effects::EffectProcessorGuiAdmission::CLOSED,
                &mut |observation| {
                    assert_eq!(
                        std::thread::current().id(),
                        parent_thread,
                        "borrowed owner control never runs on a foreign callback thread"
                    );
                    if let OpenFxRenderObservation::Progress(progress) = observation {
                        assert_eq!(progress.owner, owner);
                        assert_eq!(progress.invocation, 1);
                        assert_eq!(
                            progress.origin,
                            mondrian_effects::EffectProcessorExecutionOrigin::SupervisedChild {
                                process_id: std::num::NonZeroU32::new(process_id)
                                    .expect("actual PID"),
                            }
                        );
                        assert!(progress.revision > last_revision);
                        last_revision = progress.revision;
                        if progress.progress.active() && progress.progress.fraction() == 0.375 {
                            assert_eq!(progress.progress.label().text(), "Live native render");
                            assert!(
                                !release.exists(),
                                "fixture remains inside Render until this observation"
                            );
                            let receipt =
                                std::fs::read_to_string(&started).expect("actual Render receipt");
                            assert_eq!(
                                receipt.lines().count(),
                                1,
                                "one stateful action, no replay"
                            );
                            saw_live = true;
                            std::fs::write(&release, b"release this actual render")
                                .expect("independent continuation");
                        }
                    }
                    Ok(mondrian_effects::EffectProcessorQuestionState::Unavailable)
                },
            )
            .expect("one completed real output");
        assert!(
            saw_live,
            "terminal-only snapshots cannot satisfy this oracle"
        );
        assert_eq!(
            output.pixels(),
            pixels,
            "independent Basic unit-gain pixel oracle"
        );
        assert_eq!(
            output.alpha(),
            mondrian_core::ColorFrameAlpha::StraightFloat
        );
        let feedback = session.take_feedback().expect("same terminal receipt");
        assert_eq!(feedback.owner, owner);
        assert!(!feedback.snapshot.progress().expect("actual end generation").active());
        session.shutdown().expect("joined same-owner teardown");
    }
}

#[cfg(test)]
mod native_gui_qualification;
