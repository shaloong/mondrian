use mondrian_platform_core::{
    EnduranceRunOwnerClosureEvidence, QualifiedRuntimeCapsuleClosureEvidence,
};

fn clean() -> QualifiedRuntimeCapsuleClosureEvidence {
    QualifiedRuntimeCapsuleClosureEvidence {
        namespace_seal_verified: true,
        children_admitted: 7,
        children_settled: 7,
        children_remaining: 0,
        children_abandoned: 0,
        child_cleanup_failures: Vec::new(),
        deadline_exceeded: false,
        capsule_removed: true,
        cleanup_error: None,
    }
}

#[test]
fn every_capsule_owner_fact_is_required_independently() {
    let base = serde_json::to_value(clean()).expect("receipt");
    for (field, bad) in [
        ("namespace_seal_verified", serde_json::json!(false)),
        ("children_settled", serde_json::json!(8)),
        ("children_remaining", serde_json::json!(1)),
        ("children_abandoned", serde_json::json!(1)),
        ("deadline_exceeded", serde_json::json!(true)),
        ("capsule_removed", serde_json::json!(false)),
        ("cleanup_error", serde_json::json!("sharing violation")),
    ] {
        let mut value = base.clone();
        value[field] = bad;
        let receipt: QualifiedRuntimeCapsuleClosureEvidence =
            serde_json::from_value(value).expect("typed dirty receipt");
        assert!(
            !receipt.all_resources_released(),
            "{field} cannot be erased by clean aggregate facts"
        );
    }
}

#[test]
fn nullable_cleanup_and_every_other_field_must_be_present() {
    let base = serde_json::to_value(clean()).expect("receipt");
    for field in base.as_object().expect("object").keys() {
        let mut value = base.clone();
        value.as_object_mut().expect("object").remove(field);
        assert!(
            serde_json::from_value::<QualifiedRuntimeCapsuleClosureEvidence>(value).is_err(),
            "missing {field}"
        );
    }
    let mut value = base;
    value["unexpected"] = serde_json::json!(true);
    assert!(serde_json::from_value::<QualifiedRuntimeCapsuleClosureEvidence>(value).is_err());
}

#[test]
fn combined_closure_rejects_duplicate_capsules_and_does_not_claim_physical_termination() {
    let closure = EnduranceRunOwnerClosureEvidence::WithFfmpeg {
        surface: Box::new(EnduranceRunOwnerClosureEvidence::not_applicable()),
        ffmpeg: clean(),
    };
    assert!(closure.all_owned_authority_released());
    assert!(!closure.qualifies_physical_native_termination());
    let nested = EnduranceRunOwnerClosureEvidence::WithFfmpeg {
        surface: Box::new(closure),
        ffmpeg: clean(),
    };
    assert!(!nested.all_owned_authority_released());
}

#[test]
fn raw_native_child_cleanup_failure_is_terminal_and_strict() {
    let mut receipt = clean();
    receipt.child_cleanup_failures.push(
        mondrian_platform_core::QualifiedRuntimeCapsuleChildCleanupEvidence {
            child_pid: 17,
            native_exit_observed: true,
            kill_error: None,
            wait_error: None,
            deadline_exceeded: true,
            stdin_error: None,
            stdout_error: Some("worker did not return by original deadline".to_owned()),
            stderr_error: Some("process worker panicked".to_owned()),
        },
    );
    assert!(!receipt.all_resources_released());
    let value = serde_json::to_value(&receipt).expect("raw typed receipt");
    let parsed: QualifiedRuntimeCapsuleClosureEvidence =
        serde_json::from_value(value.clone()).expect("lossless raw failure");
    assert_eq!(parsed, receipt);
    for field in [
        "child_pid",
        "native_exit_observed",
        "kill_error",
        "wait_error",
        "deadline_exceeded",
        "stdin_error",
        "stdout_error",
        "stderr_error",
    ] {
        let mut missing = value.clone();
        missing["child_cleanup_failures"][0]
            .as_object_mut()
            .expect("raw child object")
            .remove(field);
        assert!(
            serde_json::from_value::<QualifiedRuntimeCapsuleClosureEvidence>(missing).is_err(),
            "missing child field {field}"
        );
    }
}
