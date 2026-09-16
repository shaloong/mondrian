//! Execution policy for real-device GPU color qualification.
//!
//! Ordinary cross-platform tests may retain diagnostic value on machines with
//! no usable adapter. A sealed qualification run has different semantics: an
//! unavailable adapter or required capability is a terminal gate failure. This
//! module owns that distinction so individual tests cannot silently invent
//! their own interpretation of missing hardware.

use std::fmt::Display;
use thiserror::Error;

/// Environment binding selecting the GPU color qualification execution policy.
pub const GPU_COLOR_QUALIFICATION_POLICY_ENV: &str = "MONDRIAN_GPU_COLOR_QUALIFICATION_POLICY";

/// Exact environment value enabling fail-closed sealed qualification.
pub const SEALED_GPU_COLOR_QUALIFICATION_POLICY: &str = "sealed-required";

/// Real-device execution policy applied by a GPU color gate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GpuColorQualificationExecutionPolicy {
    /// Local and broad CI tests may report unavailable hardware diagnostically.
    #[default]
    DevelopmentOptional,
    /// A sealed runner must execute every required GPU gate successfully.
    SealedRequired,
}

impl GpuColorQualificationExecutionPolicy {
    /// Resolve the policy from the process environment.
    ///
    /// Absence selects development behavior. Any non-empty value other than the
    /// exact sealed token fails closed instead of accidentally weakening a
    /// misspelled qualification invocation.
    pub fn from_environment() -> Result<Self, GpuColorQualificationPolicyError> {
        let value = std::env::var(GPU_COLOR_QUALIFICATION_POLICY_ENV);
        match value {
            Ok(value) => Self::from_value(Some(&value)),
            Err(std::env::VarError::NotPresent) => Ok(Self::DevelopmentOptional),
            Err(std::env::VarError::NotUnicode(_)) => {
                Err(GpuColorQualificationPolicyError::NonUnicodePolicy)
            }
        }
    }

    /// Parse an explicit policy value. This is also the pure contract seam used
    /// by validation tests and command-line supervisors.
    pub fn from_value(value: Option<&str>) -> Result<Self, GpuColorQualificationPolicyError> {
        match value {
            None | Some("") | Some("development-optional") => Ok(Self::DevelopmentOptional),
            Some(SEALED_GPU_COLOR_QUALIFICATION_POLICY) => Ok(Self::SealedRequired),
            Some(value) => {
                Err(GpuColorQualificationPolicyError::InvalidPolicy { value: value.to_owned() })
            }
        }
    }

    /// Whether unavailable hardware must fail the current process.
    pub const fn is_sealed(self) -> bool {
        matches!(self, Self::SealedRequired)
    }

    /// Admit a real adapter result under this policy.
    ///
    /// Development mode returns `Ok(None)` for diagnostic absence. Sealed mode
    /// returns a structured terminal error carrying the exact gate identity.
    pub fn admit_adapter<T, E: Display>(
        self,
        gate: &'static str,
        result: Result<T, E>,
    ) -> Result<Option<T>, GpuColorQualificationError> {
        match result {
            Ok(adapter) => Ok(Some(adapter)),
            Err(source) if self.is_sealed() => {
                Err(GpuColorQualificationError::AdapterUnavailable {
                    gate,
                    reason: source.to_string(),
                })
            }
            Err(_) => Ok(None),
        }
    }

    /// Admit a required adapter capability under this policy.
    ///
    /// The returned boolean is false only for a diagnostic development skip.
    pub fn admit_capability(
        self,
        gate: &'static str,
        capability: &'static str,
        available: bool,
    ) -> Result<bool, GpuColorQualificationError> {
        if available {
            Ok(true)
        } else if self.is_sealed() {
            Err(GpuColorQualificationError::CapabilityUnavailable { gate, capability })
        } else {
            Ok(false)
        }
    }
}

/// Failure to resolve the process-wide GPU qualification policy.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GpuColorQualificationPolicyError {
    /// The environment value was not valid UTF-8.
    #[error("{GPU_COLOR_QUALIFICATION_POLICY_ENV} is not valid Unicode")]
    NonUnicodePolicy,
    /// Only the two explicit policy tokens are accepted.
    #[error("invalid {GPU_COLOR_QUALIFICATION_POLICY_ENV} value '{value}'; expected 'development-optional' or '{SEALED_GPU_COLOR_QUALIFICATION_POLICY}'")]
    InvalidPolicy {
        /// Rejected value.
        value: String,
    },
}

/// Terminal failure raised by a sealed GPU color gate.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GpuColorQualificationError {
    /// The runner could not create the real adapter/device context.
    #[error("sealed GPU color gate '{gate}' could not acquire a real adapter: {reason}")]
    AdapterUnavailable {
        /// Stable gate identity.
        gate: &'static str,
        /// Adapter/device creation failure.
        reason: String,
    },
    /// The selected adapter lacks a capability required by the gate.
    #[error("sealed GPU color gate '{gate}' requires adapter capability '{capability}'")]
    CapabilityUnavailable {
        /// Stable gate identity.
        gate: &'static str,
        /// Missing capability.
        capability: &'static str,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_parser_is_explicit_and_rejects_misspelled_sealed_values() {
        assert_eq!(
            GpuColorQualificationExecutionPolicy::from_value(None),
            Ok(GpuColorQualificationExecutionPolicy::DevelopmentOptional)
        );
        assert_eq!(
            GpuColorQualificationExecutionPolicy::from_value(Some("development-optional")),
            Ok(GpuColorQualificationExecutionPolicy::DevelopmentOptional)
        );
        assert_eq!(
            GpuColorQualificationExecutionPolicy::from_value(Some("sealed-required")),
            Ok(GpuColorQualificationExecutionPolicy::SealedRequired)
        );
        assert!(matches!(
            GpuColorQualificationExecutionPolicy::from_value(Some("sealed")),
            Err(GpuColorQualificationPolicyError::InvalidPolicy { .. })
        ));
    }

    #[test]
    fn sealed_policy_turns_missing_adapter_and_capability_into_terminal_errors() {
        let sealed = GpuColorQualificationExecutionPolicy::SealedRequired;
        assert!(matches!(
            sealed.admit_adapter::<(), _>("pq-accuracy", Err("no adapter")),
            Err(GpuColorQualificationError::AdapterUnavailable { gate: "pq-accuracy", .. })
        ));
        assert!(matches!(
            sealed.admit_capability("view-perf", "timestamp-query", false),
            Err(GpuColorQualificationError::CapabilityUnavailable {
                gate: "view-perf",
                capability: "timestamp-query"
            })
        ));

        let development = GpuColorQualificationExecutionPolicy::DevelopmentOptional;
        assert_eq!(
            development.admit_adapter::<(), _>("pq-accuracy", Err("no adapter")),
            Ok(None)
        );
        assert_eq!(
            development.admit_capability("view-perf", "timestamp-query", false),
            Ok(false)
        );
    }
}
