use super::*;

#[test]
fn autosave_retention_trims_by_count() {
    let root = std::env::temp_dir().join(format!(
        "mondrian_autosave_retention_count_{}_{}",
        std::process::id(),
        unix_now_ms()
    ));
    fs::create_dir_all(&root).expect("create temp root");

    let now = unix_now_ms();
    let f1 = root.join("s1.mdp");
    let f2 = root.join("s2.mdp");
    let f3 = root.join("s3.mdp");
    fs::write(&f1, b"a").expect("write f1");
    fs::write(&f2, b"b").expect("write f2");
    fs::write(&f3, b"c").expect("write f3");

    let mut manifest = AutosaveManifest {
        project_file: root.join("project.mdp"),
        snapshots: vec![
            AutosaveSnapshotEntry {
                file: f1.clone(),
                saved_at_unix_ms: now.saturating_sub(3),
            },
            AutosaveSnapshotEntry {
                file: f2.clone(),
                saved_at_unix_ms: now.saturating_sub(2),
            },
            AutosaveSnapshotEntry {
                file: f3.clone(),
                saved_at_unix_ms: now.saturating_sub(1),
            },
        ],
        autosave_file: None,
        saved_at_unix_ms: None,
    };

    apply_autosave_retention(&mut manifest, 2, 365);
    assert_eq!(manifest.snapshots.len(), 2);
    assert!(manifest.snapshots.iter().any(|s| s.file == f3));
    assert!(manifest.snapshots.iter().any(|s| s.file == f2));
    assert!(!f1.exists(), "oldest snapshot should be removed from disk");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn autosave_retention_trims_by_age() {
    let root = std::env::temp_dir().join(format!(
        "mondrian_autosave_retention_age_{}_{}",
        std::process::id(),
        unix_now_ms()
    ));
    fs::create_dir_all(&root).expect("create temp root");

    let now = unix_now_ms();
    let recent = root.join("recent.mdp");
    let old = root.join("old.mdp");
    fs::write(&recent, b"r").expect("write recent");
    fs::write(&old, b"o").expect("write old");

    let one_day_ms = 24_u64 * 60 * 60 * 1000;
    let mut manifest = AutosaveManifest {
        project_file: root.join("project.mdp"),
        snapshots: vec![
            AutosaveSnapshotEntry {
                file: recent.clone(),
                saved_at_unix_ms: now.saturating_sub(one_day_ms / 2),
            },
            AutosaveSnapshotEntry {
                file: old.clone(),
                saved_at_unix_ms: now.saturating_sub(one_day_ms * 3),
            },
        ],
        autosave_file: None,
        saved_at_unix_ms: None,
    };

    apply_autosave_retention(&mut manifest, 10, 1);
    assert_eq!(manifest.snapshots.len(), 1);
    assert_eq!(manifest.snapshots[0].file, recent);
    assert!(
        !old.exists(),
        "expired snapshot should be removed from disk"
    );

    let _ = fs::remove_dir_all(&root);
}
