//! Platform-neutral closure evidence; no process or filesystem authority.

/// Consuming terminal evidence for the process-wide exact CLI capsule.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualifiedRuntimeCapsuleClosureEvidence {
    /// Protected namespace ACL was verified before consuming cleanup.
    pub namespace_seal_verified: bool,
    /// Number of native child admissions, including spawn failures.
    pub children_admitted: u64,
    /// Number of admissions returned after native exit or failed spawn.
    pub children_settled: u64,
    /// Child owners for which native termination was not observed.
    pub children_remaining: u64,
    /// Owners dropped without observed native termination.
    pub children_abandoned: u64,
    /// Raw failures from consuming native child/pipe owners, in observation order.
    pub child_cleanup_failures: Vec<QualifiedRuntimeCapsuleChildCleanupEvidence>,
    /// The original closure deadline elapsed before all child leases returned.
    pub deadline_exceeded: bool,
    /// All retained file handles were released and the exact directory removed.
    pub capsule_removed: bool,
    /// Fallible exact-directory cleanup failure, retained independently.
    #[serde(deserialize_with = "present_optional_string")]
    pub cleanup_error: Option<String>,
}

impl QualifiedRuntimeCapsuleClosureEvidence {
    /// Whether every native lease returned and consuming directory removal succeeded.
    pub fn all_resources_released(&self) -> bool {
        self.namespace_seal_verified
            && self.children_admitted == self.children_settled
            && self.children_remaining == 0
            && self.children_abandoned == 0
            && self.child_cleanup_failures.is_empty()
            && !self.deadline_exceeded
            && self.capsule_removed
            && self.cleanup_error.is_none()
    }
}

fn present_optional_string<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    <Option<String> as serde::Deserialize>::deserialize(deserializer)
}

/// Raw terminal facts from one native child and its pipe workers.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualifiedRuntimeCapsuleChildCleanupEvidence {
    /// Native process whose consuming cleanup failed.
    pub child_pid: u32,
    /// Native terminal status was actually observed.
    pub native_exit_observed: bool,
    /// Original native kill failure.
    #[serde(deserialize_with = "present_optional_string")]
    pub kill_error: Option<String>,
    /// Original native wait/exit-query failure.
    #[serde(deserialize_with = "present_optional_string")]
    pub wait_error: Option<String>,
    /// Original child closure deadline expired.
    pub deadline_exceeded: bool,
    /// Original stdin worker closure failure.
    #[serde(deserialize_with = "present_optional_string")]
    pub stdin_error: Option<String>,
    /// Original stdout worker closure failure.
    #[serde(deserialize_with = "present_optional_string")]
    pub stdout_error: Option<String>,
    /// Original stderr worker closure failure.
    #[serde(deserialize_with = "present_optional_string")]
    pub stderr_error: Option<String>,
}
