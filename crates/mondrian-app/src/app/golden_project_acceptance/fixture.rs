//! Canonical corpus and generated-artifact identity for Golden execution.

use super::{load_json, GoldenProjectContract};
use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Deserialize)]
pub(super) struct CorpusManifest {
    pub(super) schema_version: u32,
    pub(super) corpus_revision: String,
    entries: Vec<CorpusEntry>,
}

#[derive(Debug, Deserialize)]
struct CorpusEntry {
    id: String,
    path: PathBuf,
    availability: String,
    generation: Option<GeneratedFixtureContract>,
    purposes: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct GeneratedFixtureContract {
    recipe_path: PathBuf,
    recipe_sha256: String,
    attestation_suffix: String,
    artifact_hash_scope: String,
}

#[derive(Debug, Deserialize)]
struct GeneratedFixtureAttestation {
    schema_version: u32,
    fixture_id: String,
    recipe: AttestedRecipe,
    artifact: AttestedArtifact,
}

#[derive(Debug, Deserialize)]
struct AttestedRecipe {
    path: PathBuf,
    sha256: String,
}

#[derive(Debug, Deserialize)]
struct AttestedArtifact {
    file_name: String,
    size_bytes: u64,
    sha256: String,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct FixtureEvidence {
    role: String,
    fixture_id: String,
    corpus_revision: String,
    pub(super) path: PathBuf,
    size_bytes: u64,
    sha256: String,
    recipe_path: PathBuf,
    recipe_sha256: String,
    attestation_path: PathBuf,
    attestation_sha256: String,
}

fn resolve_contained(root: &Path, relative: &Path) -> anyhow::Result<PathBuf> {
    ensure!(!relative.is_absolute(), "contained path must be relative");
    ensure!(
        relative
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir)),
        "contained path escapes its root: {}",
        relative.display()
    );
    Ok(root.join(relative))
}

pub(super) fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
}

pub(super) fn resolve_fixture(
    root: &Path,
    fixture_root: &Path,
    contract: &GoldenProjectContract,
    manifest: &CorpusManifest,
    role_name: &str,
) -> anyhow::Result<FixtureEvidence> {
    ensure!(
        manifest.schema_version == 2,
        "unsupported corpus manifest schema"
    );
    let role = contract
        .required_fixture_roles
        .iter()
        .find(|role| role.role == role_name)
        .with_context(|| format!("Golden fixture role is absent: {role_name}"))?;
    let fixture_id = role
        .fixture_id
        .as_deref()
        .with_context(|| format!("Golden fixture role is unresolved: {role_name}"))?;
    let fixture = manifest
        .entries
        .iter()
        .find(|entry| entry.id == fixture_id)
        .with_context(|| format!("Golden fixture id is absent from corpus: {fixture_id}"))?;
    ensure!(
        fixture.purposes.iter().any(|purpose| purpose == &role.required_purpose),
        "fixture {} does not qualify for role {} purpose {}",
        fixture.id,
        role.role,
        role.required_purpose
    );
    ensure!(
        fixture.availability == "generated",
        "Golden fixture must be generated"
    );
    let generation = fixture.generation.as_ref().context("generated fixture contract missing")?;
    ensure!(
        generation.artifact_hash_scope == "reference-run",
        "generated fixture must use reference-run artifact identity"
    );

    let artifact_path = resolve_contained(fixture_root, &fixture.path)?;
    ensure!(
        artifact_path.is_file(),
        "Golden fixture is absent: {}",
        artifact_path.display()
    );
    let artifact_path = artifact_path.canonicalize()?;
    let artifact_size = artifact_path.metadata()?.len();
    let artifact_hash = sha256_file(&artifact_path)?;

    let recipe_path = resolve_contained(root, &generation.recipe_path)?;
    ensure!(recipe_path.is_file(), "Golden fixture recipe is absent");
    ensure!(
        sha256_file(&recipe_path)? == generation.recipe_sha256,
        "Golden fixture recipe differs from the corpus manifest"
    );

    let mut attestation_path = artifact_path.as_os_str().to_os_string();
    attestation_path.push(&generation.attestation_suffix);
    let attestation_path = PathBuf::from(attestation_path);
    let attestation: GeneratedFixtureAttestation = load_json(&attestation_path)?;
    ensure!(
        attestation.schema_version == 1,
        "unsupported fixture attestation schema"
    );
    ensure!(
        attestation.fixture_id == fixture.id,
        "attestation fixture id mismatch"
    );
    ensure!(
        attestation.recipe.path == generation.recipe_path
            && attestation.recipe.sha256 == generation.recipe_sha256,
        "attestation recipe identity mismatch"
    );
    ensure!(
        attestation.artifact.file_name
            == artifact_path.file_name().and_then(|name| name.to_str()).unwrap_or_default(),
        "attestation artifact file name mismatch"
    );
    ensure!(
        attestation.artifact.size_bytes == artifact_size
            && attestation.artifact.sha256 == artifact_hash,
        "attestation artifact identity mismatch"
    );

    Ok(FixtureEvidence {
        role: role.role.clone(),
        fixture_id: fixture.id.clone(),
        corpus_revision: manifest.corpus_revision.clone(),
        path: artifact_path,
        size_bytes: artifact_size,
        sha256: artifact_hash,
        recipe_path: generation.recipe_path.clone(),
        recipe_sha256: generation.recipe_sha256.clone(),
        attestation_sha256: sha256_file(&attestation_path)?,
        attestation_path,
    })
}
