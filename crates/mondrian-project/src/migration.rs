use anyhow::Context;
use serde_json::Value;

pub(super) type JsonMigrationFn = fn(Value) -> anyhow::Result<Value>;

pub(super) struct JsonMigrationStep {
    pub from: u32,
    pub to: u32,
    pub migrate: JsonMigrationFn,
}

pub(super) struct JsonMigrationRegistry {
    domain: &'static str,
    version_field: &'static str,
    current_version: u32,
    steps: &'static [JsonMigrationStep],
}

impl JsonMigrationRegistry {
    pub const fn new(
        domain: &'static str,
        version_field: &'static str,
        current_version: u32,
        steps: &'static [JsonMigrationStep],
    ) -> Self {
        Self { domain, version_field, current_version, steps }
    }

    pub fn migrate(&self, mut value: Value) -> anyhow::Result<Value> {
        self.validate_registry()?;
        let mut version = json_version(&value, self.version_field)
            .with_context(|| format!("invalid {} version", self.domain))?;
        if version > self.current_version {
            anyhow::bail!(
                "unsupported {} version: {} (current {})",
                self.domain,
                version,
                self.current_version
            );
        }
        while version < self.current_version {
            let step = self.steps.iter().find(|step| step.from == version).with_context(|| {
                format!(
                    "missing {} migration from version {} to {}",
                    self.domain, version, self.current_version
                )
            })?;
            value = (step.migrate)(value).with_context(|| {
                format!(
                    "failed {} migration {} -> {}",
                    self.domain, step.from, step.to
                )
            })?;
            let migrated_version = json_version(&value, self.version_field)
                .with_context(|| format!("{} migration omitted version", self.domain))?;
            if migrated_version != step.to {
                anyhow::bail!(
                    "{} migration {} -> {} produced version {}",
                    self.domain,
                    step.from,
                    step.to,
                    migrated_version
                );
            }
            version = migrated_version;
        }
        Ok(value)
    }

    fn validate_registry(&self) -> anyhow::Result<()> {
        let mut expected = self.steps.first().map(|step| step.from).unwrap_or(self.current_version);
        for step in self.steps {
            if step.from != expected || step.to != step.from.saturating_add(1) {
                anyhow::bail!(
                    "{} migration registry is not contiguous at {} -> {}",
                    self.domain,
                    step.from,
                    step.to
                );
            }
            expected = step.to;
        }
        if !self.steps.is_empty() && expected != self.current_version {
            anyhow::bail!(
                "{} migration registry ends at {}, current is {}",
                self.domain,
                expected,
                self.current_version
            );
        }
        Ok(())
    }
}

fn json_version(value: &Value, field: &str) -> anyhow::Result<u32> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .and_then(|version| u32::try_from(version).ok())
        .with_context(|| format!("missing or invalid `{field}`"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn one_to_two(mut value: Value) -> anyhow::Result<Value> {
        value["trace"] = json!(["1-2"]);
        value["version"] = json!(2);
        Ok(value)
    }

    fn two_to_three(mut value: Value) -> anyhow::Result<Value> {
        value["trace"].as_array_mut().expect("trace").push(json!("2-3"));
        value["version"] = json!(3);
        Ok(value)
    }

    #[test]
    fn registry_applies_contiguous_steps_in_order_and_is_idempotent_at_current() {
        static STEPS: &[JsonMigrationStep] = &[
            JsonMigrationStep { from: 1, to: 2, migrate: one_to_two },
            JsonMigrationStep { from: 2, to: 3, migrate: two_to_three },
        ];
        let registry = JsonMigrationRegistry::new("fixture", "version", 3, STEPS);

        let migrated = registry.migrate(json!({"version": 1})).expect("migrate");
        assert_eq!(migrated, json!({"version": 3, "trace": ["1-2", "2-3"]}));
        assert_eq!(
            registry.migrate(migrated.clone()).expect("idempotent"),
            migrated
        );
    }

    #[test]
    fn registry_rejects_future_and_gapped_versions() {
        let current = JsonMigrationRegistry::new("fixture", "version", 1, &[]);
        assert!(current.migrate(json!({"version": 2})).is_err());

        static GAPPED: &[JsonMigrationStep] =
            &[JsonMigrationStep { from: 1, to: 3, migrate: one_to_two }];
        let invalid = JsonMigrationRegistry::new("fixture", "version", 3, GAPPED);
        assert!(invalid.migrate(json!({"version": 1})).is_err());
    }
}
