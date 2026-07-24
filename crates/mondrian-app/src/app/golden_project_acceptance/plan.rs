//! Compiled coverage ledger for the complete M1 Golden Project contract.
//!
//! A slice may prove only its declared obligations. This Module is the single
//! place that determines whether the available slices could cover the complete
//! contract; actual execution evidence and consecutive-run acceptance remain
//! separate and cannot be inferred from this plan.

use super::GoldenProjectContract;
use serde::Serialize;
use std::collections::BTreeSet;

const GOLDEN_ACCEPTANCE_PLAN_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum GoldenAcceptancePlanStatus {
    Complete,
    Blocked,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(super) struct GoldenObligationSet {
    pub fixture_roles: BTreeSet<String>,
    pub operations: BTreeSet<String>,
    pub content: BTreeSet<String>,
    pub exports: BTreeSet<String>,
}

impl GoldenObligationSet {
    fn missing_from(&self, planned: &Self) -> Self {
        Self {
            fixture_roles: self.fixture_roles.difference(&planned.fixture_roles).cloned().collect(),
            operations: self.operations.difference(&planned.operations).cloned().collect(),
            content: self.content.difference(&planned.content).cloned().collect(),
            exports: self.exports.difference(&planned.exports).cloned().collect(),
        }
    }

    fn is_empty(&self) -> bool {
        self.fixture_roles.is_empty()
            && self.operations.is_empty()
            && self.content.is_empty()
            && self.exports.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct GoldenSlicePlan {
    pub id: String,
    pub obligations: GoldenObligationSet,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct GoldenAcceptancePlan {
    pub schema_version: u32,
    pub contract_id: String,
    pub required_consecutive_passes: u32,
    pub status: GoldenAcceptancePlanStatus,
    /// A compiled plan is not execution evidence, even when structurally complete.
    pub complete_golden_project: bool,
    pub required: GoldenObligationSet,
    pub planned: GoldenObligationSet,
    pub missing: GoldenObligationSet,
    pub unassigned_required_fixture_roles: BTreeSet<String>,
    pub slices: Vec<GoldenSlicePlan>,
}

impl GoldenAcceptancePlan {
    /// Compile all declared Golden obligations into one deterministic ledger.
    pub(super) fn compile(contract: &GoldenProjectContract) -> Self {
        let required = GoldenObligationSet {
            fixture_roles: contract
                .required_fixture_roles
                .iter()
                .filter(|role| role.required)
                .map(|role| role.role.clone())
                .collect(),
            operations: contract.required_operations.iter().cloned().collect(),
            content: contract.required_content.iter().cloned().collect(),
            exports: contract.exports.iter().map(|export| export.id.clone()).collect(),
        };
        let slices = contract
            .execution_slices
            .iter()
            .map(|slice| GoldenSlicePlan {
                id: slice.id.clone(),
                obligations: GoldenObligationSet {
                    fixture_roles: slice.required_fixture_roles.iter().cloned().collect(),
                    operations: slice.required_operations.iter().cloned().collect(),
                    content: slice.required_content.iter().cloned().collect(),
                    exports: slice.required_exports.iter().cloned().collect(),
                },
            })
            .collect::<Vec<_>>();
        let mut planned = GoldenObligationSet::default();
        for slice in &slices {
            planned.fixture_roles.extend(slice.obligations.fixture_roles.iter().cloned());
            planned.operations.extend(slice.obligations.operations.iter().cloned());
            planned.content.extend(slice.obligations.content.iter().cloned());
            planned.exports.extend(slice.obligations.exports.iter().cloned());
        }
        let missing = required.missing_from(&planned);
        let unassigned_required_fixture_roles = contract
            .required_fixture_roles
            .iter()
            .filter(|role| role.required && role.fixture_id.is_none())
            .map(|role| role.role.clone())
            .collect::<BTreeSet<_>>();
        let status = if missing.is_empty() && unassigned_required_fixture_roles.is_empty() {
            GoldenAcceptancePlanStatus::Complete
        } else {
            GoldenAcceptancePlanStatus::Blocked
        };

        Self {
            schema_version: GOLDEN_ACCEPTANCE_PLAN_SCHEMA_VERSION,
            contract_id: contract.id.clone(),
            required_consecutive_passes: contract.acceptance.consecutive_passes,
            status,
            complete_golden_project: false,
            required,
            planned,
            missing,
            unassigned_required_fixture_roles,
            slices,
        }
    }
}
