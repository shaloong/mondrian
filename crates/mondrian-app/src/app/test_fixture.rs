//! Test-only fixture namespace allocation. Domain owners control resource lifetime.

use std::{
    io,
    path::{Path, PathBuf},
};

pub(super) fn create_root(parent: &Path, name: &str) -> io::Result<PathBuf> {
    // PID/counter labels are diagnostic, not unique across process lifetimes.
    // Tempfile atomically claims a randomized sibling instead of adopting an
    // existing directory (which may belong to an earlier process).
    let root = tempfile::Builder::new().prefix(&format!("{name}-")).tempdir_in(parent)?;
    // Preserve the existing fixture lifetime: App domain workers may still own
    // files after allocation returns. Do not drop/delete their directory here.
    Ok(root.keep())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_pid_slot_is_never_adopted_or_modified() {
        let parent = tempfile::tempdir().expect("owned test parent");
        let name = format!("mondrian-app-test-{}-1", std::process::id());
        let stale = parent.path().join(&name);
        let runtime = stale.join("runtime-roots");
        std::fs::create_dir_all(&runtime).expect("seed stale runtime parent");
        let sentinel = stale.join("prior-owner");
        std::fs::write(&sentinel, b"do not adopt or delete").expect("seed prior owner evidence");

        let fresh = create_root(parent.path(), &name).expect("new fixture");
        assert_ne!(fresh, stale, "PID reuse must not adopt a prior fixture");
        assert_eq!(fresh.parent(), Some(parent.path()));
        assert_eq!(
            std::fs::read_dir(&fresh).expect("fresh root exists").count(),
            0
        );
        assert_eq!(
            std::fs::read(&sentinel).expect("stale evidence retained"),
            b"do not adopt or delete"
        );
        assert!(!runtime.join(".mondrian-runtime-parent-v1").exists());
    }

    #[test]
    fn concurrent_same_label_claims_are_distinct_and_persist_after_return() {
        let parent = tempfile::tempdir().expect("owned test parent");
        let roots = std::thread::scope(|scope| {
            let claims: Vec<_> = (0..16)
                .map(|_| {
                    let parent = parent.path();
                    scope.spawn(move || create_root(parent, "same-label").expect("claim fixture"))
                })
                .collect();
            claims
                .into_iter()
                .map(|claim| claim.join().expect("claim worker"))
                .collect::<Vec<_>>()
        });
        assert_eq!(
            roots.iter().collect::<std::collections::HashSet<_>>().len(),
            16
        );
        for root in roots {
            assert!(
                root.is_dir(),
                "allocation must not drop a live owner's directory"
            );
            assert_eq!(root.parent(), Some(parent.path()));
        }
    }

    #[test]
    fn missing_parent_is_not_silently_created() {
        let parent = tempfile::tempdir().expect("owned test parent");
        let missing = parent.path().join("missing");
        assert!(create_root(&missing, "fixture").is_err());
        assert!(!missing.exists());
    }
}
