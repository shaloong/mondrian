//! Sequential cold metadata work, with one bounded completion slot.
//!
//! Only the process is reused. `describe_in_worker` still creates and retires
//! its entire native Host/cache/module graph for every selected class. A fault
//! loses the in-flight class, not prior unregistered results or later classes.
use super::*;

pub const OPENFX_CATALOG_DESCRIBE_WORKER_ARGUMENT: &str = "--internal-openfx-catalog-describe-v1";
const SCHEMA: u32 = 1;
const MAX_CLASSES: usize = 1024;

// Unsupported context/admission is an ordinary refusal. Other native faults,
// incomplete observation or fatal notices retire the process before any later
// class. Every outcome still retires its native Host through the original ABI.
fn retires_process(response: &DescribeResponse) -> bool {
    match response {
        DescribeResponse::Success { .. } => false,
        DescribeResponse::Rejected { status, notices, .. } => {
            *status != 7 || !notices.complete || notices.fatal
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogRequest {
    schema_version: u32,
    context: OpenFxContext,
    binary_path: PathBuf,
    bundle_path: PathBuf,
    binary_sha256: String,
    identifiers: Vec<String>,
}

impl CatalogRequest {
    fn validate(&self) -> Result<(), OpenFxHostError> {
        let mut unique = std::collections::BTreeSet::new();
        if self.schema_version != SCHEMA
            || self.identifiers.is_empty()
            || self.identifiers.len() > MAX_CLASSES
            || self.identifiers.iter().any(|id| {
                id.is_empty()
                    || id.len() >= 256
                    || !id.bytes().all(|b| (0x21..=0x7e).contains(&b))
                    || !unique.insert(id)
            })
        {
            return Err(invalid("invalid OpenFX catalog metadata request"));
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClassResponse {
    schema_version: u32,
    context: OpenFxContext,
    index: usize,
    identifier: String,
    response: DescribeResponse,
}

impl ClassResponse {
    fn accept(
        self,
        inspection: &OpenFxBinaryInspection,
        context: OpenFxContext,
        index: usize,
        identifier: &str,
    ) -> Result<OpenFxDescriptionObservation, OpenFxHostError> {
        if self.schema_version != SCHEMA
            || self.context != context
            || self.index != index
            || self.identifier != identifier
        {
            return Err(invalid(
                "OpenFX catalog completion does not match its class",
            ));
        }
        accept_response(self.response, inspection, identifier)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogComplete {
    schema_version: u32,
    context: OpenFxContext,
    completed: usize,
    retired_after_class: bool,
}

fn read_slot(path: &Path) -> Result<Option<ClassResponse>, OpenFxHostError> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(unavailable(format!("catalog slot read failed: {error}"))),
    };
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_WORKER_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| unavailable(format!("catalog slot read failed: {error}")))?;
    if bytes.len() as u64 > MAX_WORKER_RESPONSE_BYTES {
        return Err(invalid("catalog completion exceeds its size limit"));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| invalid(format!("malformed catalog completion: {error}")))
}

/// Deliver exact-class observations to the existing unregistered preparation.
/// A completion does not itself authorize registry or preference publication.
pub(crate) fn observe_openfx_catalog_with_checkpoint(
    helper: &Path,
    inspection: &OpenFxBinaryInspection,
    context: OpenFxContext,
    checkpoint: &mut impl FnMut() -> Result<(), OpenFxHostError>,
    on_class: &mut impl FnMut(
        &str,
        Result<OpenFxDescriptionObservation, OpenFxHostError>,
    ) -> Result<(), OpenFxHostError>,
) -> Result<(), OpenFxHostError> {
    checkpoint()?;
    if inspection.plugins.len() > MAX_CLASSES {
        return Err(invalid("OpenFX catalog exceeds its class limit"));
    }
    let mut next = 0;
    while next < inspection.plugins.len() {
        let start = next;
        let bundle =
            verified_selection(inspection, &inspection.plugins[next].identifier, checkpoint)?;
        let staging = tempfile::tempdir()
            .map_err(|error| unavailable(format!("catalog staging failed: {error}")))?;
        let slot = staging.path().join("class.json");
        let request = CatalogRequest {
            schema_version: SCHEMA,
            context,
            binary_path: inspection.binary_path.clone(),
            bundle_path: bundle,
            binary_sha256: inspection.binary_sha256.clone(),
            identifiers: inspection.plugins[start..].iter().map(|p| p.identifier.clone()).collect(),
        };
        request.validate()?;
        // A completed, validated class alone advances this deadline. Neither
        // partial writes nor duplicate/wrong-class packets can keep it alive.
        let mut class_deadline = Instant::now() + DEADLINE;
        let mut class_timed_out = false;
        let mut expected_retirement = false;
        let response: Result<CatalogComplete, _> = invoke_worker_with_budget(
            helper,
            OPENFX_CATALOG_DESCRIBE_WORKER_ARGUMENT,
            staging.path(),
            &request,
            DEADLINE * request.identifiers.len() as u32,
            &mut || {
                checkpoint()?;
                if Instant::now() >= class_deadline {
                    class_timed_out = true;
                    return Err(OpenFxHostError::DeadlineExceeded);
                }
                if let Some(packet) = read_slot(&slot)? {
                    if expected_retirement {
                        return Err(invalid("catalog continued after a metadata fault"));
                    }
                    let plugin = inspection
                        .plugins
                        .get(next)
                        .ok_or_else(|| invalid("unexpected trailing catalog completion"))?;
                    let retire = retires_process(&packet.response);
                    let observed =
                        packet.accept(inspection, context, next - start, &plugin.identifier)?;
                    if sha256_file_with_checkpoint(&inspection.binary_path, checkpoint)?
                        != inspection.binary_sha256
                    {
                        return Err(invalid("OpenFX binary changed during catalog description"));
                    }
                    on_class(&plugin.identifier, Ok(observed))?;
                    checkpoint()?;
                    if let Some(following) = inspection.plugins.get(next + 1) {
                        verified_selection(inspection, &following.identifier, checkpoint)?;
                    }
                    fs::remove_file(&slot).map_err(|error| {
                        unavailable(format!("catalog acknowledgement failed: {error}"))
                    })?;
                    next += 1;
                    expected_retirement = retire;
                    if next < inspection.plugins.len() && !retire {
                        class_deadline = Instant::now() + DEADLINE;
                    }
                }
                Ok(())
            },
        );
        match response {
            Ok(done)
                if done.schema_version == SCHEMA
                    && done.context == context
                    && done.completed == next - start
                    && done.retired_after_class == expected_retirement
                    && (next == inspection.plugins.len() || expected_retirement) =>
            {
                verified_selection(
                    inspection,
                    &inspection.plugins[next - 1].identifier,
                    checkpoint,
                )?;
            }
            Ok(_) => return Err(invalid("incomplete OpenFX catalog worker settlement")),
            Err(error)
                if next < inspection.plugins.len()
                    && !expected_retirement
                    && (matches!(error, OpenFxHostError::WorkerFailed(_))
                        || (class_timed_out
                            && matches!(error, OpenFxHostError::DeadlineExceeded))) =>
            {
                // The existing direct-child owner has already killed/reaped
                // this process. Do not replay completed native actions. Resume
                // later classes only, in a fresh process, exactly once.
                verified_selection(inspection, &inspection.plugins[next].identifier, checkpoint)?;
                on_class(&inspection.plugins[next].identifier, Err(error))?;
                next += 1;
            }
            Err(error) => return Err(error),
        }
    }
    checkpoint()
}

/// Native-only early entry point; each class uses the original C ABI lifetime.
pub fn run_openfx_catalog_describe_worker() -> Result<(), OpenFxHostError> {
    let (request, response_path): (CatalogRequest, PathBuf) =
        read_worker_request(OPENFX_CATALOG_DESCRIBE_WORKER_ARGUMENT)?;
    request.validate()?;
    let directory =
        response_path.parent().ok_or_else(|| invalid("catalog staging has no parent"))?;
    let slot = directory.join("class.json");
    let pending = directory.join("class.pending");
    for (index, identifier) in request.identifiers.iter().enumerate() {
        let response = describe_in_worker(&DescribeRequest {
            schema_version: DESCRIBE_SCHEMA,
            context: request.context,
            binary_path: request.binary_path.clone(),
            bundle_path: request.bundle_path.clone(),
            binary_sha256: request.binary_sha256.clone(),
            plugin_identifier: identifier.clone(),
        })?;
        let retire = retires_process(&response);
        // All native cleanup and its metadata notices precede this publication.
        // One slot is retained until the parent consumes and acknowledges it.
        write_worker_response(
            &pending,
            &ClassResponse {
                schema_version: SCHEMA,
                context: request.context,
                index,
                identifier: identifier.clone(),
                response,
            },
        )?;
        fs::rename(&pending, &slot).map_err(|error| {
            unavailable(format!("catalog completion publication failed: {error}"))
        })?;
        let deadline = Instant::now() + DEADLINE;
        while slot.try_exists().map_err(|error| unavailable(error.to_string()))? {
            if Instant::now() >= deadline {
                return Err(OpenFxHostError::DeadlineExceeded);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if retire {
            return write_worker_response(
                &response_path,
                &CatalogComplete {
                    schema_version: SCHEMA,
                    context: request.context,
                    completed: index + 1,
                    retired_after_class: true,
                },
            );
        }
    }
    write_worker_response(
        &response_path,
        &CatalogComplete {
            schema_version: SCHEMA,
            context: request.context,
            completed: request.identifiers.len(),
            retired_after_class: false,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet() -> ClassResponse {
        ClassResponse {
            schema_version: SCHEMA,
            context: OpenFxContext::Generator,
            index: 2,
            identifier: "test.generator".into(),
            response: DescribeResponse::Success {
                schema_version: DESCRIBE_SCHEMA,
                description: OpenFxFilterDescription {
                    identifier: "test.generator".into(),
                    label: "Generator".into(),
                    parameters: vec![],
                },
                notices: DescribeNoticeReport { complete: true, fatal: false, notices: vec![] },
            },
        }
    }

    fn inspection() -> OpenFxBinaryInspection {
        OpenFxBinaryInspection {
            binary_path: PathBuf::from("test.ofx"),
            binary_sha256: "a".repeat(64),
            plugins: vec![],
        }
    }

    #[test]
    fn completions_require_exact_schema_context_order_and_both_class_identities() {
        assert!(packet()
            .accept(&inspection(), OpenFxContext::Generator, 2, "test.generator")
            .is_ok());
        for field in [
            "schema_version",
            "context",
            "index",
            "identifier",
            "description",
        ] {
            let mut value = serde_json::to_value(packet()).unwrap();
            match field {
                "schema_version" => value[field] = 0.into(),
                "context" => value[field] = "filter".into(),
                "index" => value[field] = 1.into(),
                "identifier" => value[field] = "test.other".into(),
                _ => value["response"]["description"]["identifier"] = "test.other".into(),
            }
            let wrong: ClassResponse = serde_json::from_value(value).unwrap();
            assert!(
                wrong
                    .accept(&inspection(), OpenFxContext::Generator, 2, "test.generator")
                    .is_err(),
                "{field}"
            );
        }
        let mut value = serde_json::to_value(packet()).unwrap();
        value["context"] = "retimer".into();
        assert!(serde_json::from_value::<ClassResponse>(value).is_err());
    }

    #[test]
    fn manifests_refuse_empty_duplicate_unbounded_or_noncanonical_identifiers() {
        let mut request = CatalogRequest {
            schema_version: SCHEMA,
            context: OpenFxContext::Filter,
            binary_path: PathBuf::from("test.ofx"),
            bundle_path: PathBuf::from("test.ofx.bundle"),
            binary_sha256: "a".repeat(64),
            identifiers: vec!["test.filter".into()],
        };
        assert!(request.validate().is_ok());
        for ids in [
            vec![],
            vec!["same".into(), "same".into()],
            vec!["a".repeat(256)],
            vec!["bad id".into()],
            (0..=MAX_CLASSES).map(|i| format!("class.{i}")).collect(),
        ] {
            request.identifiers = ids;
            assert!(request.validate().is_err());
        }
    }

    #[test]
    fn only_clean_metadata_and_ordinary_unsupported_refusals_reuse_a_process() {
        assert!(!retires_process(&packet().response));
        for (status, complete, fatal, retire) in [
            (7, true, false, false),
            (7, true, true, true),
            (7, false, false, true),
            (6, true, false, true),
            (8, true, false, true),
            (0, true, false, true),
        ] {
            let response = DescribeResponse::Rejected {
                schema_version: DESCRIBE_SCHEMA,
                status,
                reason: Some("refused".into()),
                notices: DescribeNoticeReport { complete, fatal, notices: vec![] },
            };
            assert_eq!(retires_process(&response), retire);
        }
    }

    #[test]
    fn slot_refuses_partial_json_before_any_completion_is_delivered() {
        let directory = tempfile::tempdir().unwrap();
        let slot = directory.path().join("class.json");
        assert!(read_slot(&slot).unwrap().is_none());
        fs::write(&slot, br#"{"schema_version":1"#).unwrap();
        assert!(read_slot(&slot).is_err());
    }
}
