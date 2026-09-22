use anyhow::{bail, ensure, Context};
use mondrian_core::{
    build_mondrian_standard_sdr_interchange_package, MONDRIAN_STANDARD_SDR_INTERCHANGE_CONFIG_FILE,
    MONDRIAN_STANDARD_SDR_INTERCHANGE_TRANSFORM_FILE,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

const MANIFEST_FILE: &str = "manifest.json";

#[derive(Serialize)]
struct FileIdentity<'a> {
    path: &'a str,
    bytes: usize,
    sha256: String,
}

#[derive(Serialize)]
struct Manifest<'a> {
    schema_version: u8,
    kind: &'static str,
    package: &'static str,
    config: FileIdentity<'a>,
    transform: FileIdentity<'a>,
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn write_new(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    file.write_all(bytes).with_context(|| format!("write {}", path.display()))?;
    file.sync_all().with_context(|| format!("flush {}", path.display()))
}

fn fresh_staging_directory(parent: &Path, output_name: &str) -> anyhow::Result<PathBuf> {
    for attempt in 0..128_u8 {
        let candidate = parent.join(format!(
            ".{output_name}.partial-{}-{attempt}",
            std::process::id()
        ));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("create staging directory {}", candidate.display()));
            }
        }
    }
    bail!("could not allocate a fresh staging directory")
}

fn main() -> anyhow::Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let requested = PathBuf::from(arguments.next().context("expected a fresh output directory")?);
    ensure!(arguments.next().is_none(), "unexpected argument");
    ensure!(
        !requested.exists(),
        "output already exists: {}",
        requested.display()
    );

    let output_name = requested
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .context("output directory must have a Unicode final component")?;
    let parent = requested
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()
        .with_context(|| format!("resolve output parent for {}", requested.display()))?;
    let output = parent.join(output_name);
    ensure!(
        !output.exists(),
        "output already exists: {}",
        output.display()
    );

    let package = build_mondrian_standard_sdr_interchange_package().map_err(anyhow::Error::msg)?;
    let config_bytes = package.config.as_bytes();
    let transform_bytes = package.transform.as_bytes();
    let manifest = Manifest {
        schema_version: 1,
        kind: "mondrian-ocio-interchange-package",
        package: "mondrian-standard-sdr-v2",
        config: FileIdentity {
            path: MONDRIAN_STANDARD_SDR_INTERCHANGE_CONFIG_FILE,
            bytes: config_bytes.len(),
            sha256: sha256(config_bytes),
        },
        transform: FileIdentity {
            path: MONDRIAN_STANDARD_SDR_INTERCHANGE_TRANSFORM_FILE,
            bytes: transform_bytes.len(),
            sha256: sha256(transform_bytes),
        },
    };
    let mut manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    manifest_bytes.push(b'\n');

    let staging = fresh_staging_directory(&parent, output_name)?;
    let result = (|| -> anyhow::Result<()> {
        write_new(
            &staging.join(MONDRIAN_STANDARD_SDR_INTERCHANGE_CONFIG_FILE),
            config_bytes,
        )?;
        write_new(
            &staging.join(MONDRIAN_STANDARD_SDR_INTERCHANGE_TRANSFORM_FILE),
            transform_bytes,
        )?;
        write_new(&staging.join(MANIFEST_FILE), &manifest_bytes)?;
        fs::rename(&staging, &output).with_context(|| {
            format!(
                "publish OCIO package {} as {}",
                staging.display(),
                output.display()
            )
        })
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result?;

    println!(
        "{}",
        serde_json::json!({
            "schema_version": 1,
            "kind": "mondrian-ocio-interchange-package",
            "path": output,
            "manifest": manifest,
        })
    );
    Ok(())
}
