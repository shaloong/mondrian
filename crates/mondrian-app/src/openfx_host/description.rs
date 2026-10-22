//! Cold metadata-request notices, separate from Render/instance feedback.

use super::*;
use mondrian_effects::{EffectProcessorNotice, EffectProcessorNoticeKind};

const DESCRIBE_SCHEMA: u32 = 3;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DescribeRequest {
    pub(super) schema_version: u32,
    pub(super) context: OpenFxContext,
    pub(super) binary_path: PathBuf,
    pub(super) bundle_path: PathBuf,
    pub(super) binary_sha256: String,
    pub(super) plugin_identifier: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DescribeNoticeReport {
    complete: bool,
    fatal: bool,
    notices: Vec<EffectProcessorNotice>,
}

impl DescribeNoticeReport {
    fn validate(&self) -> Result<(), OpenFxHostError> {
        if self.notices.len() > NATIVE_NOTICE_SLOTS
            || (!self.fatal
                && self
                    .notices
                    .iter()
                    .any(|notice| notice.kind() == EffectProcessorNoticeKind::Fatal))
        {
            return Err(invalid("invalid OpenFX metadata notice report"));
        }
        Ok(())
    }

    fn refusal(&self) -> Option<&'static str> {
        if !self.complete {
            Some("metadata notice recording is incomplete")
        } else if self.fatal {
            Some("metadata request reported a fatal notice")
        } else {
            None
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum DescribeResponse {
    Success {
        schema_version: u32,
        description: OpenFxFilterDescription,
        notices: DescribeNoticeReport,
    },
    Rejected {
        schema_version: u32,
        status: i32,
        reason: Option<String>,
        notices: DescribeNoticeReport,
    },
}

/// These facts identify the parent's verified request, never the callback's
/// emitter class or its OFX action. A binary may describe several classes.
pub(crate) struct OpenFxDescribeNotices {
    pub binary_path: PathBuf,
    pub binary_sha256: String,
    pub requested_plugin_identifier: String,
    pub complete: bool,
    pub fatal: bool,
    pub notices: Vec<EffectProcessorNotice>,
}

pub(crate) struct OpenFxDescriptionObservation {
    pub outcome: Result<OpenFxFilterDescription, OpenFxHostError>,
    pub notices: OpenFxDescribeNotices,
}

#[repr(C)]
pub(super) struct NativeDescribeNotices {
    complete: c_int,
    fatal: c_int,
    notice_count: usize,
    notices: [NativeNotice; NATIVE_NOTICE_SLOTS],
}

impl Default for NativeDescribeNotices {
    fn default() -> Self {
        Self {
            complete: 0,
            fatal: 0,
            notice_count: 0,
            notices: [NativeNotice {
                kind: 0,
                text_length: 0,
                id_length: 0,
                text: [0; NATIVE_NOTICE_TEXT_BYTES + 1],
                id: [0; NATIVE_NOTICE_ID_BYTES + 1],
            }; NATIVE_NOTICE_SLOTS],
        }
    }
}

impl NativeDescribeNotices {
    fn decode(&self) -> Result<DescribeNoticeReport, OpenFxHostError> {
        if !matches!(self.complete, 0 | 1)
            || !matches!(self.fatal, 0 | 1)
            || self.notice_count > NATIVE_NOTICE_SLOTS
        {
            return Err(invalid("malformed native metadata notice report"));
        }
        let mut notices = Vec::new();
        notices
            .try_reserve_exact(self.notice_count)
            .map_err(|_| unavailable("metadata notice allocation failed"))?;
        for raw in &self.notices[..self.notice_count] {
            notices.push(
                raw.decode()
                    .map_err(|_| invalid("malformed native metadata notice"))?
                    .ok_or_else(|| invalid("empty native metadata notice"))?,
            );
        }
        let report = DescribeNoticeReport {
            complete: self.complete == 1,
            fatal: self.fatal == 1,
            notices,
        };
        report.validate()?;
        Ok(report)
    }
}

/// Describe the admitted Filter parameter schema in a supervised native child.
pub fn describe_openfx_filter(
    helper_executable: &Path,
    inspection: &OpenFxBinaryInspection,
    plugin_identifier: &str,
) -> Result<OpenFxFilterDescription, OpenFxHostError> {
    describe_openfx_filter_with_checkpoint(
        helper_executable,
        inspection,
        plugin_identifier,
        &mut || Ok(()),
    )
}

/// Describe one Filter while observing the catalog owner, without registration.
pub fn describe_openfx_filter_with_checkpoint(
    helper_executable: &Path,
    inspection: &OpenFxBinaryInspection,
    plugin_identifier: &str,
    checkpoint: &mut impl FnMut() -> Result<(), OpenFxHostError>,
) -> Result<OpenFxFilterDescription, OpenFxHostError> {
    observe_openfx_filter_with_checkpoint(
        helper_executable,
        inspection,
        plugin_identifier,
        checkpoint,
    )?
    .outcome
}

pub(crate) fn observe_openfx_filter_with_checkpoint(
    helper_executable: &Path,
    inspection: &OpenFxBinaryInspection,
    plugin_identifier: &str,
    checkpoint: &mut impl FnMut() -> Result<(), OpenFxHostError>,
) -> Result<OpenFxDescriptionObservation, OpenFxHostError> {
    observe_openfx_with_checkpoint(
        helper_executable,
        inspection,
        plugin_identifier,
        OpenFxContext::Filter,
        checkpoint,
    )
}

/// Describe the independently selected output-only Generator context.
pub fn describe_openfx_generator(
    helper_executable: &Path,
    inspection: &OpenFxBinaryInspection,
    plugin_identifier: &str,
) -> Result<OpenFxFilterDescription, OpenFxHostError> {
    observe_openfx_with_checkpoint(
        helper_executable,
        inspection,
        plugin_identifier,
        OpenFxContext::Generator,
        &mut || Ok(()),
    )?
    .outcome
}

pub(crate) fn observe_openfx_with_checkpoint(
    helper_executable: &Path,
    inspection: &OpenFxBinaryInspection,
    plugin_identifier: &str,
    context: OpenFxContext,
    checkpoint: &mut impl FnMut() -> Result<(), OpenFxHostError>,
) -> Result<OpenFxDescriptionObservation, OpenFxHostError> {
    checkpoint()?;
    let bundle = verified_selection(inspection, plugin_identifier, checkpoint)?;
    let staging = tempfile::tempdir()
        .map_err(|error| unavailable(format!("description staging failed: {error}")))?;
    let request = DescribeRequest {
        schema_version: DESCRIBE_SCHEMA,
        context,
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
        checkpoint,
    )?;
    if sha256_file_with_checkpoint(&inspection.binary_path, checkpoint)? != inspection.binary_sha256
    {
        return Err(invalid("OpenFX binary changed during description"));
    }
    let observed = accept_response(response, inspection, plugin_identifier)?;
    checkpoint()?;
    Ok(observed)
}

fn accept_response(
    response: DescribeResponse,
    inspection: &OpenFxBinaryInspection,
    plugin_identifier: &str,
) -> Result<OpenFxDescriptionObservation, OpenFxHostError> {
    let (outcome, report) = match response {
        DescribeResponse::Success { schema_version, description, notices } => {
            if schema_version != DESCRIBE_SCHEMA {
                return Err(invalid("unsupported OpenFX Describe response schema"));
            }
            notices.validate()?;
            validate_description(&description, plugin_identifier)?;
            if notices.refusal().is_some() {
                return Err(invalid(
                    "successful metadata response contains refused notices",
                ));
            }
            (Ok(description), notices)
        }
        DescribeResponse::Rejected { schema_version, status, reason, notices } => {
            if schema_version != DESCRIBE_SCHEMA {
                return Err(invalid("unsupported OpenFX Describe response schema"));
            }
            notices.validate()?;
            if status == 0 && notices.refusal().is_none() && reason.is_none() {
                return Err(invalid("metadata refusal has no failure"));
            }
            (
                Err(OpenFxHostError::WorkerFailed(format!(
                    "native status {status}: {}",
                    reason.unwrap_or_default()
                ))),
                notices,
            )
        }
    };
    Ok(OpenFxDescriptionObservation {
        outcome,
        notices: OpenFxDescribeNotices {
            binary_path: inspection.binary_path.clone(),
            binary_sha256: inspection.binary_sha256.clone(),
            requested_plugin_identifier: plugin_identifier.to_owned(),
            complete: report.complete,
            fatal: report.fatal,
            notices: report.notices,
        },
    })
}

/// Describe one selected native Filter before graphics or media startup.
pub fn run_openfx_describe_worker() -> Result<(), OpenFxHostError> {
    let (request, response_path): (DescribeRequest, PathBuf) =
        read_worker_request(OPENFX_DESCRIBE_WORKER_ARGUMENT)?;
    let response = describe_in_worker(&request)?;
    write_worker_response(&response_path, &response)
}

pub(super) fn describe_in_worker(
    request: &DescribeRequest,
) -> Result<DescribeResponse, OpenFxHostError> {
    if request.schema_version != DESCRIBE_SCHEMA {
        return Err(invalid("unsupported OpenFX Describe request schema"));
    }
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
    let mut notices = Box::new(NativeDescribeNotices::default());
    // SAFETY: All output storage and the sink live through this synchronous,
    // supervised call; the C ABI owns no pointers after returning.
    let status = unsafe {
        mondrian_openfx_describe(
            binary.as_ptr(),
            bundle.as_ptr(),
            identifier.as_ptr(),
            request.context.native(),
            label.as_mut_ptr().cast(),
            label.len(),
            &mut label_length,
            collect_native_parameter,
            (&mut sink as *mut ParameterSink).cast(),
            &mut *notices,
        )
    };
    let notices = notices.decode()?;
    let refusal = notices.refusal().map(str::to_owned);
    if status != 0 || refusal.is_some() {
        return Ok(DescribeResponse::Rejected {
            schema_version: DESCRIBE_SCHEMA,
            status,
            reason: sink.error.or(refusal),
            notices,
        });
    }
    let description: Result<OpenFxFilterDescription, OpenFxHostError> = (|| {
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
        Ok(description)
    })();
    Ok(match description {
        Ok(description) => DescribeResponse::Success {
            schema_version: DESCRIBE_SCHEMA,
            description,
            notices,
        },
        Err(error) => DescribeResponse::Rejected {
            schema_version: DESCRIBE_SCHEMA,
            status,
            reason: Some(error.to_string()),
            notices,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openfx_adapter::OpenFxPluginDescriptor;

    fn inspection() -> OpenFxBinaryInspection {
        OpenFxBinaryInspection {
            binary_path: PathBuf::from("request-context.ofx"),
            binary_sha256: "a".repeat(64),
            plugins: vec![OpenFxPluginDescriptor {
                identifier: "test.requested.class".into(),
                api_version: 1,
                version_major: 1,
                version_minor: 0,
            }],
        }
    }

    fn report() -> DescribeNoticeReport {
        DescribeNoticeReport {
            complete: true,
            fatal: false,
            notices: vec![EffectProcessorNotice::new(
                EffectProcessorNoticeKind::Information,
                "notice".to_owned(),
                "id".to_owned(),
            )
            .unwrap()],
        }
    }

    fn description() -> OpenFxFilterDescription {
        OpenFxFilterDescription {
            identifier: "test.requested.class".into(),
            label: "Metadata".into(),
            parameters: Vec::new(),
        }
    }

    #[test]
    fn describe_schema_rejects_old_wrong_and_unknown_requests_and_responses() {
        let mut request = serde_json::json!({"schema_version":3,"context":"filter","binary_path":"none","bundle_path":"none","binary_sha256":"a","plugin_identifier":"test"});
        assert!(serde_json::from_value::<DescribeRequest>(request.clone()).is_ok());
        request.as_object_mut().unwrap().remove("schema_version");
        assert!(serde_json::from_value::<DescribeRequest>(request.clone()).is_err());
        request["schema_version"] = serde_json::json!(1);
        let wrong = serde_json::from_value::<DescribeRequest>(request.clone()).unwrap();
        assert!(
            matches!(describe_in_worker(&wrong), Err(OpenFxHostError::Invalid(reason)) if reason.contains("schema"))
        );
        request["schema_version"] = serde_json::json!(3);
        request["unexpected"] = serde_json::json!(0);
        assert!(serde_json::from_value::<DescribeRequest>(request).is_err());
        // This frozen legacy consumer shape rejects the new mandatory field.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct OldRequest {
            binary_path: PathBuf,
            bundle_path: PathBuf,
            binary_sha256: String,
            plugin_identifier: String,
        }
        let old = serde_json::json!({"binary_path":"none","bundle_path":"none","binary_sha256":"a","plugin_identifier":"test"});
        let decoded = serde_json::from_value::<OldRequest>(old.clone()).unwrap();
        assert_eq!(
            (
                decoded.binary_path,
                decoded.bundle_path,
                decoded.binary_sha256,
                decoded.plugin_identifier
            ),
            (
                PathBuf::from("none"),
                PathBuf::from("none"),
                "a".into(),
                "test".into()
            )
        );
        let mut new = old;
        new["schema_version"] = serde_json::json!(3);
        assert!(serde_json::from_value::<OldRequest>(new).is_err());
        let current = DescribeResponse::Success {
            schema_version: DESCRIBE_SCHEMA,
            description: description(),
            notices: report(),
        };
        let mut response = serde_json::to_value(current).unwrap();
        let parsed = serde_json::from_value::<DescribeResponse>(response.clone()).unwrap();
        assert!(accept_response(parsed, &inspection(), "test.requested.class").is_ok());
        response.as_object_mut().unwrap().remove("schema_version");
        assert!(serde_json::from_value::<DescribeResponse>(response.clone()).is_err());
        response["schema_version"] = serde_json::json!(1);
        let parsed = serde_json::from_value::<DescribeResponse>(response.clone()).unwrap();
        assert!(accept_response(parsed, &inspection(), "test.requested.class").is_err());
        response["schema_version"] = serde_json::json!(2);
        response["unexpected"] = serde_json::json!(0);
        assert!(serde_json::from_value::<DescribeResponse>(response).is_err());
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
        enum OldResponse {
            Success {
                description: OpenFxFilterDescription,
            },
            Rejected {
                status: i32,
                reason: Option<String>,
            },
        }
        let old = serde_json::json!({"kind":"success","description":description()});
        let OldResponse::Success { description: old_description } =
            serde_json::from_value::<OldResponse>(old.clone()).unwrap()
        else {
            panic!("old success")
        };
        assert_eq!(old_description, description());
        assert!(serde_json::from_value::<DescribeResponse>(old).is_err());
        let new = serde_json::to_value(DescribeResponse::Rejected {
            schema_version: DESCRIBE_SCHEMA,
            status: 7,
            reason: Some("native".into()),
            notices: report(),
        })
        .unwrap();
        assert!(serde_json::from_value::<OldResponse>(new).is_err());
        let OldResponse::Rejected { status, reason } = serde_json::from_value::<OldResponse>(
            serde_json::json!({"kind":"rejected","status":7,"reason":"native"}),
        )
        .unwrap() else {
            panic!("old rejection")
        };
        assert_eq!((status, reason.as_deref()), (7, Some("native")));
    }

    #[test]
    fn describe_refusal_keeps_native_status_reason_and_verified_request_context() {
        let mut notices = report();
        notices.complete = false;
        let observed = accept_response(
            DescribeResponse::Rejected {
                schema_version: DESCRIBE_SCHEMA,
                status: 8,
                reason: Some("original callback failure".into()),
                notices,
            },
            &inspection(),
            "test.requested.class",
        )
        .unwrap();
        assert!(
            matches!(observed.outcome, Err(OpenFxHostError::WorkerFailed(reason)) if reason == "native status 8: original callback failure")
        );
        assert!(!observed.notices.complete);
        assert!(!observed.notices.fatal);
        assert_eq!(observed.notices.notices, report().notices);
        assert_eq!(observed.notices.binary_path, inspection().binary_path);
        assert_eq!(observed.notices.binary_sha256, inspection().binary_sha256);
        assert_eq!(
            observed.notices.requested_plugin_identifier,
            "test.requested.class"
        );
    }

    #[test]
    fn describe_report_rejects_overbound_utf8_and_false_success_atomically() {
        let mut raw = NativeDescribeNotices::default();
        raw.complete = 1;
        raw.notice_count = 32;
        for notice in &mut raw.notices {
            notice.kind = 1;
            notice.text_length = 1;
            notice.text[0] = b'x' as c_char;
        }
        assert_eq!(raw.decode().unwrap().notices.len(), 32);
        raw.notice_count = 33;
        assert!(raw.decode().is_err());
        raw.notice_count = 32;
        raw.notices[31].text[0] = 0xff_u8 as c_char;
        assert!(raw.decode().is_err());
        for (complete, fatal) in [(false, false), (true, true)] {
            let mut notices = report();
            notices.complete = complete;
            notices.fatal = fatal;
            assert!(accept_response(
                DescribeResponse::Success {
                    schema_version: DESCRIBE_SCHEMA,
                    description: description(),
                    notices
                },
                &inspection(),
                "test.requested.class"
            )
            .is_err());
        }
        let mut value = serde_json::to_value(report()).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 3);
        value["notices"][0]["text"] = serde_json::json!("x".repeat(4097));
        assert!(serde_json::from_value::<DescribeNoticeReport>(value).is_err());
    }

    #[cfg(windows)]
    fn native_observation() -> OpenFxDescriptionObservation {
        let helper = PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_TEST_HELPER").expect("current product"),
        );
        let binary = PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY").expect("owned fixture"),
        );
        let inspection = crate::openfx_adapter::inspect_openfx_binary(&helper, &binary)
            .expect("actual binary inspection");
        let identifier = "uk.co.thefoundry.BasicGainPlugin";
        assert!(inspection.plugins.iter().any(|plugin| plugin.identifier == identifier));
        observe_openfx_filter_with_checkpoint(&helper, &inspection, identifier, &mut || Ok(()))
            .expect("trusted Describe response")
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires current Product and SessionProbe aux_transient_null or aux_transient_descriptor"]
    fn native_describe_only_returns_six_ordered_utf8_notices_without_render() {
        let mode = std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("owned mode");
        assert!(matches!(
            mode.as_str(),
            "aux_transient_null" | "aux_transient_descriptor"
        ));
        let observed = native_observation();
        assert!(observed.outcome.is_ok());
        assert!(observed.notices.complete);
        assert!(!observed.notices.fatal);
        let notices = observed.notices.notices;
        assert_eq!(notices.len(), 6);
        for (index, kind) in [
            EffectProcessorNoticeKind::Information,
            EffectProcessorNoticeKind::Warning,
            EffectProcessorNoticeKind::Log,
        ]
        .into_iter()
        .enumerate()
        {
            for offset in 0..2 {
                let notice = &notices[index * 2 + offset];
                assert_eq!(notice.kind(), kind);
                assert_eq!(
                    notice.text(),
                    if offset == 0 {
                        "说明🙂 7 1.250"
                    } else {
                        "第二条通知✓"
                    }
                );
                assert_eq!(notice.id(), "");
            }
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires current Product and rebuilt owned SessionProbe aux_describe_* mode"]
    fn native_describe_enforces_notice_completeness_and_preserves_failed_metadata_prefix() {
        let mode = std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("owned mode");
        assert!(matches!(
            mode.as_str(),
            "aux_describe_32"
                | "aux_describe_33"
                | "aux_describe_fatal"
                | "aux_describe_invalid_utf8"
                | "aux_describe_native_failure"
        ));
        let observed = native_observation();
        let notices = observed.notices;
        match mode.as_str() {
            "aux_describe_32" | "aux_describe_33" => {
                assert_eq!(notices.complete, mode == "aux_describe_32");
                assert!(!notices.fatal);
                assert_eq!(notices.notices.len(), 32);
                for (index, notice) in notices.notices.iter().enumerate() {
                    assert_eq!(notice.kind(), EffectProcessorNoticeKind::Information);
                    assert_eq!(notice.id(), "bounded");
                    assert_eq!(notice.text(), format!("notice {index}"));
                }
            }
            "aux_describe_fatal" => {
                assert!(notices.complete && notices.fatal);
                assert_eq!(notices.notices.len(), 1);
                assert_eq!(notices.notices[0].kind(), EffectProcessorNoticeKind::Fatal);
                assert_eq!(notices.notices[0].text(), "metadata fatal");
            }
            _ => {
                assert_eq!(notices.complete, mode == "aux_describe_native_failure");
                assert!(!notices.fatal);
                assert_eq!(notices.notices.len(), 1);
                assert_eq!(
                    notices.notices[0].kind(),
                    EffectProcessorNoticeKind::Warning
                );
                assert_eq!(notices.notices[0].id(), "before-refusal");
                assert_eq!(notices.notices[0].text(), "retained prefix");
            }
        }
        if mode == "aux_describe_32" {
            assert!(observed.outcome.is_ok());
        } else {
            let Err(OpenFxHostError::WorkerFailed(reason)) = observed.outcome else {
                panic!("definition must be refused")
            };
            if mode == "aux_describe_native_failure" {
                let status = reason
                    .strip_prefix("native status ")
                    .unwrap()
                    .split(':')
                    .next()
                    .unwrap()
                    .parse::<i32>()
                    .unwrap();
                assert_ne!(
                    status, 0,
                    "original native failure cannot become a successful Describe"
                );
            }
        }
    }
    #[cfg(windows)]
    #[test]
    #[ignore = "requires current Product, rebuilt SessionProbe, and a new empty action receipt directory"]
    fn native_describe_owns_one_load_and_exact_failure_retirement() {
        let mode = std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("owned mode");
        assert!(matches!(
            mode.as_str(),
            "aux_transient_null" | "aux_describe_native_failure" | "aux_describe_load_failure"
        ));
        let directory = PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_DESCRIBE_ACTION_RECEIPT_DIRECTORY")
                .expect("new action receipt directory"),
        );
        assert!(directory.is_absolute() && directory.is_dir());
        assert!(
            fs::read_dir(&directory).unwrap().next().is_none(),
            "do not combine requests or prior observations"
        );
        let observed = native_observation();
        assert!(observed.notices.complete && !observed.notices.fatal);
        if mode == "aux_transient_null" {
            assert!(observed.outcome.is_ok());
            assert_eq!(observed.notices.notices.len(), 6);
        } else {
            assert!(
                matches!(observed.outcome, Err(OpenFxHostError::WorkerFailed(reason)) if reason.starts_with("native status 7:")),
                "cleanup must preserve the native admission refusal"
            );
            if mode == "aux_describe_native_failure" {
                assert_eq!(observed.notices.notices.len(), 1);
                assert_eq!(
                    observed.notices.notices[0].kind(),
                    EffectProcessorNoticeKind::Warning
                );
                assert_eq!(observed.notices.notices[0].id(), "before-refusal");
                assert_eq!(observed.notices.notices[0].text(), "retained prefix");
            } else {
                assert!(observed.notices.notices.is_empty());
            }
        }
        // Files are emitted by the actual discovery/Describe helper processes.
        // A PID may be reused after settlement; role flags, not PID inequality,
        // distinguish the two request paths. Every physical file stays bounded.
        let mut discovery = Vec::new();
        let mut describe = Vec::new();
        let entries = fs::read_dir(&directory)
            .unwrap()
            .take(3)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!((1..=2).contains(&entries.len()));
        for entry in entries {
            let file_type = entry.file_type().unwrap();
            assert!(file_type.is_file() && !file_type.is_symlink());
            let name = entry.file_name().into_string().unwrap();
            let pid = name
                .strip_prefix("actions-")
                .and_then(|name| name.strip_suffix(".txt"))
                .expect("fixed receipt name");
            assert!(
                pid.bytes().all(|byte| byte.is_ascii_digit()) && pid.parse::<u32>().unwrap() > 0
            );
            let mut bytes = Vec::new();
            fs::File::open(entry.path())
                .unwrap()
                .take(4097)
                .read_to_end(&mut bytes)
                .unwrap();
            assert!(bytes.len() <= 4096 && bytes.is_ascii() && bytes.last() == Some(&b'\n'));
            let text = std::str::from_utf8(&bytes).unwrap();
            let lines = text.lines().collect::<Vec<_>>();
            assert!(
                lines.len() < 32,
                "overflow or a truncated prefix is not evidence"
            );
            for (index, line) in lines.into_iter().enumerate() {
                let (sequence, row) = line.split_once(' ').unwrap();
                assert_eq!(sequence.parse::<usize>().unwrap(), index + 1);
                if let Some(row) = row.strip_prefix("D=0 I=1 ") {
                    discovery.push(row.to_owned());
                } else if let Some(row) = row.strip_prefix("D=1 I=0 ") {
                    describe.push(row.to_owned());
                } else {
                    panic!("ambiguous or unexpected worker request context");
                }
            }
        }
        assert_eq!(
            discovery,
            vec!["OfxSetHost enter attempted=0 accepted=0 status=0 complete=1"]
        );
        let mut expected = vec![
            ("OfxSetHost", "enter", 0, 0, 0),
            ("OfxActionLoad", "enter", 0, 0, 0),
        ];
        if mode == "aux_describe_load_failure" {
            expected.push(("OfxActionLoad", "return", 0, 0, 1));
        } else {
            if mode == "aux_transient_null" {
                expected.push(("transient", "batch", 6, 6, 0));
            }
            expected.push(("OfxActionLoad", "return", 0, 0, 0));
            expected.push(("OfxActionDescribe", "enter", 0, 0, 0));
            expected.push((
                "OfxActionDescribe",
                "return",
                0,
                0,
                if mode == "aux_transient_null" { 0 } else { 1 },
            ));
            if mode == "aux_transient_null" {
                expected.push(("OfxImageEffectActionDescribeInContext", "enter", 0, 0, 0));
                expected.push(("OfxImageEffectActionDescribeInContext", "return", 0, 0, 0));
            }
            expected.push(("OfxActionUnload", "enter", 0, 0, 0));
            // Unknown-kind Failed proves registration remains live without
            // dereferencing native descriptor addresses or adding a notice.
            expected.push(("descriptor-base", "check", 0, 0, 1));
            if mode == "aux_transient_null" {
                expected.push(("descriptor-context", "check", 0, 0, 1));
            }
            expected.push(("OfxActionUnload", "return", 0, 0, 0));
        }
        let expected = expected.into_iter().map(|(action, point, attempted, accepted, status)| {
            format!("{action} {point} attempted={attempted} accepted={accepted} status={status} complete=1")
        }).collect::<Vec<_>>();
        assert_eq!(describe, expected, "exact lifecycle: no extra Load, missing/double Unload, Render, or context after refusal");
    }
}
