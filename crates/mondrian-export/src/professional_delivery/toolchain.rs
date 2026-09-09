use super::PackageAssetId;
use crate::preset::ProfessionalDeliveryProfile;
use mondrian_core::ExecutionCancellationToken;
use mondrian_media::{
    ApprovedBmxCommand, ApprovedBmxTool, BmxRuntimeHandle, SupervisedProcessCleanupReceipt,
    SupervisedProcessPolicy, SupervisedStreamCapture,
};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const MAX_VERSION_BYTES: usize = 64 * 1024;

/// Qualified external standards tool used behind the professional-delivery Adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProfessionalDeliveryTool {
    /// BBC BMX raw essence wrapper.
    BmxRaw2Bmx,
    /// BBC BMX independent MXF reader.
    BmxMxf2Raw,
    /// CineCert AS-DCP track wrapper.
    AsdcpWrap,
    /// CineCert AS-DCP independent reader.
    AsdcpInfo,
    /// DCP-o-matic/libdcp complete-package verifier.
    DcpOMaticVerifier,
    /// Java runtime used to host the qualified Photon validator.
    PhotonJava,
    /// Netflix Photon runtime library set.
    PhotonValidator,
}

impl ProfessionalDeliveryTool {
    fn executable_name(self) -> &'static str {
        match self {
            Self::BmxRaw2Bmx => "raw2bmx",
            Self::BmxMxf2Raw => "mxf2raw",
            Self::AsdcpWrap => "asdcp-wrap",
            Self::AsdcpInfo => "asdcp-info",
            Self::DcpOMaticVerifier => "dcpomatic2_verify_cli",
            Self::PhotonJava => "java",
            Self::PhotonValidator => "photon",
        }
    }

    fn version_arg(self) -> &'static str {
        match self {
            Self::BmxRaw2Bmx | Self::BmxMxf2Raw => "-v",
            Self::AsdcpWrap | Self::AsdcpInfo => "-h",
            Self::DcpOMaticVerifier => "-V",
            Self::PhotonJava => "-version",
            Self::PhotonValidator => "",
        }
    }
}

/// Bounded executable identity retained with validation evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfessionalDeliveryToolIdentity {
    /// Tool family.
    pub tool: ProfessionalDeliveryTool,
    /// Exact executable or validator-library path selected by the Adapter.
    pub path: PathBuf,
    /// Bounded first-line version/help identity.
    pub version: String,
    /// Actual native/pipe closure produced by the bounded version probe.
    pub native_cleanup: SupervisedProcessCleanupReceipt,
    /// SHA-256 of bounded stdout followed by stderr, before textual normalization.
    pub version_output_sha256: String,
}

/// Concrete BMX/CineCert tool paths used by execution and reimport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfessionalDeliveryToolchain {
    raw2bmx: PathBuf,
    mxf2raw: PathBuf,
    asdcp_wrap: PathBuf,
    asdcp_info: PathBuf,
    dcpomatic_verifier: PathBuf,
    photon_java: PathBuf,
    photon_lib: PathBuf,
    bmx: Option<BmxRuntimeHandle>,
}

impl ProfessionalDeliveryToolchain {
    /// Resolve exact approved BMX owners for AS-11; ordinary development retains discovery.
    pub fn discover_with_bmx(
        profile: ProfessionalDeliveryProfile,
        bmx: Option<BmxRuntimeHandle>,
    ) -> Result<Self, ProfessionalDeliveryToolchainError> {
        let Some(bmx) = bmx else {
            return Self::discover(profile);
        };
        if profile != ProfessionalDeliveryProfile::As11X9NabaHd720p5994 {
            return Err(ProfessionalDeliveryToolchainError::Qualification {
                tool: ProfessionalDeliveryTool::BmxRaw2Bmx,
                detail: "approved BMX campaign owner supports the admitted AS-11 profile only"
                    .to_owned(),
            });
        }
        let mut toolchain = Self::from_paths(
            bmx.path(ApprovedBmxTool::Raw2Bmx).to_path_buf(),
            bmx.path(ApprovedBmxTool::Mxf2Raw).to_path_buf(),
            PathBuf::new(),
            PathBuf::new(),
        );
        toolchain.bmx = Some(bmx);
        Ok(toolchain)
    }

    fn bmx_command(&self, tool: ApprovedBmxTool) -> ApprovedBmxCommand {
        if let Some(owner) = &self.bmx {
            owner.command(tool)
        } else {
            ApprovedBmxCommand::unapproved(match tool {
                ApprovedBmxTool::Raw2Bmx => &self.raw2bmx,
                ApprovedBmxTool::Mxf2Raw => &self.mxf2raw,
            })
        }
    }
    /// Construct an explicit toolchain, primarily for packaged-runtime admission and tests.
    pub fn from_paths(
        raw2bmx: PathBuf,
        mxf2raw: PathBuf,
        asdcp_wrap: PathBuf,
        asdcp_info: PathBuf,
    ) -> Self {
        Self {
            raw2bmx,
            mxf2raw,
            asdcp_wrap,
            asdcp_info,
            dcpomatic_verifier: PathBuf::new(),
            photon_java: PathBuf::new(),
            photon_lib: PathBuf::new(),
            bmx: None,
        }
    }

    /// Construct an explicit IMF-capable toolchain for qualification tests.
    pub fn from_paths_with_photon(
        raw2bmx: PathBuf,
        mxf2raw: PathBuf,
        photon_java: PathBuf,
        photon_lib: PathBuf,
    ) -> Self {
        Self {
            raw2bmx,
            mxf2raw,
            asdcp_wrap: PathBuf::new(),
            asdcp_info: PathBuf::new(),
            dcpomatic_verifier: PathBuf::new(),
            photon_java,
            photon_lib,
            bmx: None,
        }
    }

    /// Construct an explicit DCP-capable toolchain for qualification tests.
    pub fn from_paths_with_dcp(
        asdcp_wrap: PathBuf,
        asdcp_info: PathBuf,
        dcpomatic_verifier: PathBuf,
    ) -> Self {
        Self {
            raw2bmx: PathBuf::new(),
            mxf2raw: PathBuf::new(),
            asdcp_wrap,
            asdcp_info,
            dcpomatic_verifier,
            photon_java: PathBuf::new(),
            photon_lib: PathBuf::new(),
            bmx: None,
        }
    }

    /// Resolve tools beside the application first, then from PATH for development.
    pub fn discover(
        profile: ProfessionalDeliveryProfile,
    ) -> Result<Self, ProfessionalDeliveryToolchainError> {
        let unavailable = PathBuf::new();
        match profile {
            ProfessionalDeliveryProfile::ImfAppProResRdd45_1080p25 => {
                let (photon_java, photon_lib) = resolve_photon()?;
                Ok(Self {
                    raw2bmx: resolve_tool(ProfessionalDeliveryTool::BmxRaw2Bmx)?,
                    mxf2raw: resolve_tool(ProfessionalDeliveryTool::BmxMxf2Raw)?,
                    asdcp_wrap: unavailable.clone(),
                    asdcp_info: unavailable,
                    dcpomatic_verifier: PathBuf::new(),
                    photon_java,
                    photon_lib,
                    bmx: None,
                })
            }
            ProfessionalDeliveryProfile::As11X9NabaHd720p5994 => Ok(Self {
                raw2bmx: resolve_tool(ProfessionalDeliveryTool::BmxRaw2Bmx)?,
                mxf2raw: resolve_tool(ProfessionalDeliveryTool::BmxMxf2Raw)?,
                asdcp_wrap: unavailable.clone(),
                asdcp_info: unavailable.clone(),
                dcpomatic_verifier: unavailable.clone(),
                photon_java: unavailable.clone(),
                photon_lib: unavailable,
                bmx: None,
            }),
            ProfessionalDeliveryProfile::SmpteDcp2kFlat24 => Ok(Self {
                raw2bmx: unavailable.clone(),
                mxf2raw: unavailable.clone(),
                asdcp_wrap: resolve_tool(ProfessionalDeliveryTool::AsdcpWrap)?,
                asdcp_info: resolve_tool(ProfessionalDeliveryTool::AsdcpInfo)?,
                dcpomatic_verifier: resolve_tool(ProfessionalDeliveryTool::DcpOMaticVerifier)?,
                photon_java: unavailable.clone(),
                photon_lib: unavailable,
                bmx: None,
            }),
        }
    }

    /// Exact executable path, or Photon library directory, for one tool.
    pub fn path(&self, tool: ProfessionalDeliveryTool) -> &Path {
        match tool {
            ProfessionalDeliveryTool::BmxRaw2Bmx => &self.raw2bmx,
            ProfessionalDeliveryTool::BmxMxf2Raw => &self.mxf2raw,
            ProfessionalDeliveryTool::AsdcpWrap => &self.asdcp_wrap,
            ProfessionalDeliveryTool::AsdcpInfo => &self.asdcp_info,
            ProfessionalDeliveryTool::DcpOMaticVerifier => &self.dcpomatic_verifier,
            ProfessionalDeliveryTool::PhotonJava => &self.photon_java,
            ProfessionalDeliveryTool::PhotonValidator => &self.photon_lib,
        }
    }

    /// Build the qualified BMX command for one IMF RDD 45 picture Track File.
    pub fn imf_picture_command(
        &self,
        elementary_prores: &Path,
        output_pattern: &Path,
        title: &str,
    ) -> ApprovedBmxCommand {
        let mut command = self.bmx_command(ApprovedBmxTool::Raw2Bmx);
        command
            .arg("-t")
            .arg("imf")
            .arg("-o")
            .arg(output_pattern)
            .arg("--clip")
            .arg(title)
            .arg("-f")
            .arg("25")
            .arg("-a")
            .arg("16:9")
            .arg("-c")
            .arg("10")
            .arg("--frame-layout")
            .arg("fullframe")
            .arg("--transfer-ch")
            .arg("bt709")
            .arg("--coding-eq")
            .arg("bt709")
            .arg("--color-prim")
            .arg("bt709")
            .arg("--color-siting")
            .arg("cositing")
            .arg("--black-level")
            .arg("64")
            .arg("--white-level")
            .arg("940")
            .arg("--color-range")
            .arg("897")
            .arg("--rdd36_422_hq")
            .arg(elementary_prores);
        command
    }

    /// Build the qualified BMX command for one IMF stereo PCM24 Track File.
    pub fn imf_audio_command(
        &self,
        wave: &Path,
        mca_labels: &Path,
        output_pattern: &Path,
    ) -> ApprovedBmxCommand {
        let mut command = self.bmx_command(ApprovedBmxTool::Raw2Bmx);
        command
            .arg("-t")
            .arg("imf")
            .arg("-o")
            .arg(output_pattern)
            .arg("--track-map")
            .arg("singlemca")
            .arg("--track-mca-labels")
            .arg("as11")
            .arg(mca_labels)
            .arg("--wave")
            .arg(wave);
        command
    }

    /// Build the qualified BMX command for one AMWA AS-11 X9 OP1a file.
    pub fn as11_x9_command(
        &self,
        elementary_avc: &Path,
        wave: &Path,
        mca_labels: &Path,
        output: &Path,
        title: &str,
    ) -> ApprovedBmxCommand {
        let mut command = self.bmx_command(ApprovedBmxTool::Raw2Bmx);
        command
            .arg("-t")
            .arg("as11op1a")
            .arg("-o")
            .arg(output)
            .arg("--spec-id")
            .arg("as11-x9")
            .arg("-f")
            .arg("5994")
            .arg("--head-fill")
            .arg("4000000")
            .arg("--part")
            .arg("35964")
            .arg("--clip")
            .arg(title)
            .arg("--audio-layout")
            .arg("as11-mode-0")
            .arg("--track-map")
            .arg("singlemca")
            .arg("--track-mca-labels")
            .arg("as11")
            .arg(mca_labels)
            .arg("--frame-layout")
            .arg("fullframe")
            .arg("--transfer-ch")
            .arg("bt709")
            .arg("--coding-eq")
            .arg("bt709")
            .arg("--color-prim")
            .arg("bt709")
            .arg("--avc_high_422")
            .arg(elementary_avc)
            .arg("--wave")
            .arg(wave);
        command
    }

    /// Attach standard variable-sized ST436 KLV frames to the AS-11 command.
    pub fn attach_as11_ancillary(command: &mut ApprovedBmxCommand, ancillary_klv: &Path) {
        command.arg("--klv").arg("s").arg("--anc").arg(ancillary_klv);
    }

    /// Build the qualified CineCert command for one SMPTE DCP picture Track File.
    pub fn dcp_picture_command(
        &self,
        j2c_directory: &Path,
        output: &Path,
        asset_id: PackageAssetId,
        duration: u64,
    ) -> Command {
        let mut command = Command::new(&self.asdcp_wrap);
        command
            .arg("-L")
            .arg("-E")
            .arg("-M")
            .arg("-a")
            .arg(asset_id.uuid().to_string())
            .arg("-p")
            .arg("24")
            .arg("-d")
            .arg(duration.to_string())
            .arg(j2c_directory)
            .arg(output);
        command
    }

    /// Build the qualified CineCert command for one SMPTE DCP stereo PCM24 Track File.
    pub fn dcp_audio_command(
        &self,
        wave: &Path,
        output: &Path,
        asset_id: PackageAssetId,
        duration: u64,
        language: &str,
    ) -> Command {
        let mut command = Command::new(&self.asdcp_wrap);
        command
            .arg("-L")
            .arg("-E")
            .arg("-M")
            .arg("-a")
            .arg(asset_id.uuid().to_string())
            .arg("-p")
            .arg("24")
            .arg("-d")
            .arg(duration.to_string())
            .arg("-m")
            .arg("L,R")
            .arg("-g")
            .arg(language)
            .arg(wave)
            .arg(output);
        command
    }

    /// Build the independent BMX structural reimport command.
    pub fn bmx_reimport_command(&self, input: &Path, extract_as11: bool) -> ApprovedBmxCommand {
        let mut command = self.bmx_command(ApprovedBmxTool::Mxf2Raw);
        command.arg("--check-end").arg("--check-complete").arg("--read-ess").arg("-i");
        if extract_as11 {
            command.arg("--as11").arg("--mca-detail");
        }
        command.arg(input);
        command
    }

    /// Build the independent CineCert AS-DCP reimport command.
    pub fn asdcp_reimport_command(&self, input: &Path) -> Command {
        let mut command = Command::new(&self.asdcp_info);
        command.arg(input);
        command
    }

    /// Build an independent complete-package DCP validation command.
    pub fn dcp_package_validation_command(&self, root: &Path) -> Command {
        let mut command = Command::new(&self.dcpomatic_verifier);
        command.arg("--quiet").arg(root);
        command
    }

    /// Build a Photon command that extracts and validates one MXF Essence Descriptor.
    pub fn photon_track_descriptor_command(&self, input: &Path, working: &Path) -> Command {
        let mut command = Command::new(&self.photon_java);
        command
            .arg("-cp")
            .arg(photon_classpath(&self.photon_lib))
            .arg("com.netflix.imflibrary.app.IMFTrackFileReader")
            .arg(input)
            .arg(working);
        command
    }

    /// Build a Photon command that reimports and validates a complete IMP.
    pub fn photon_imp_validation_command(&self, root: &Path) -> Command {
        let mut command = Command::new(&self.photon_java);
        command
            .arg("-cp")
            .arg(photon_classpath(&self.photon_lib))
            .arg("com.netflix.imflibrary.app.IMPAnalyzer")
            .arg(root);
        command
    }

    /// Probe every tool required by a profile and retain bounded identities.
    pub fn qualify_for(
        &self,
        profile: ProfessionalDeliveryProfile,
    ) -> Result<Vec<ProfessionalDeliveryToolIdentity>, ProfessionalDeliveryToolchainError> {
        self.qualify_for_until(
            profile,
            Instant::now() + Duration::from_secs(30),
            &ExecutionCancellationToken::new(),
        )
    }

    /// Probe all required executables under one original admission deadline.
    pub fn qualify_for_until(
        &self,
        profile: ProfessionalDeliveryProfile,
        deadline: Instant,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<Vec<ProfessionalDeliveryToolIdentity>, ProfessionalDeliveryToolchainError> {
        let tools: &[ProfessionalDeliveryTool] = match profile {
            ProfessionalDeliveryProfile::ImfAppProResRdd45_1080p25 => &[
                ProfessionalDeliveryTool::BmxRaw2Bmx,
                ProfessionalDeliveryTool::BmxMxf2Raw,
                ProfessionalDeliveryTool::PhotonJava,
                ProfessionalDeliveryTool::PhotonValidator,
            ],
            ProfessionalDeliveryProfile::As11X9NabaHd720p5994 => &[
                ProfessionalDeliveryTool::BmxRaw2Bmx,
                ProfessionalDeliveryTool::BmxMxf2Raw,
            ],
            ProfessionalDeliveryProfile::SmpteDcp2kFlat24 => &[
                ProfessionalDeliveryTool::AsdcpWrap,
                ProfessionalDeliveryTool::AsdcpInfo,
                ProfessionalDeliveryTool::DcpOMaticVerifier,
            ],
        };
        tools
            .iter()
            .map(|tool| match tool {
                ProfessionalDeliveryTool::PhotonValidator => probe_photon_identity(
                    &self.photon_java,
                    &self.photon_lib,
                    deadline,
                    cancellation,
                ),
                ProfessionalDeliveryTool::BmxRaw2Bmx | ProfessionalDeliveryTool::BmxMxf2Raw => {
                    let role = if *tool == ProfessionalDeliveryTool::BmxRaw2Bmx {
                        ApprovedBmxTool::Raw2Bmx
                    } else {
                        ApprovedBmxTool::Mxf2Raw
                    };
                    let mut command = self.bmx_command(role);
                    command.arg(tool.version_arg());
                    let output = capture_identity(&mut command, *tool, deadline, cancellation)?;
                    identity_from_output(*tool, self.path(*tool), output)
                }
                _ => probe_identity(*tool, self.path(*tool), deadline, cancellation),
            })
            .collect()
    }
}

/// Tool discovery or qualification failure.
#[derive(Debug, thiserror::Error)]
pub enum ProfessionalDeliveryToolchainError {
    /// A completed native version probe failed qualification; output and cleanup remain intact.
    #[error("professional delivery tool {tool:?} version rejected: {detail}")]
    VersionRejected {
        /// Exact tool role.
        tool: ProfessionalDeliveryTool,
        /// Failed identity predicate.
        detail: String,
        /// Original bounded native output, status, and consuming closure facts.
        output: Box<mondrian_media::SupervisedProcessOutput>,
    },
    /// Bounded native supervision failed; typed original cleanup remains retained.
    #[error("professional delivery tool {tool:?} native supervision failed: {source}")]
    Native {
        /// Exact tool role.
        tool: ProfessionalDeliveryTool,
        /// Original bounded child/pipe failure.
        #[source]
        source: Box<mondrian_media::SupervisedProcessError>,
    },
    /// Required executable was not found.
    #[error("required professional delivery tool {tool:?} was not found beside the application or on PATH")]
    Missing {
        /// Missing tool.
        tool: ProfessionalDeliveryTool,
    },
    /// Tool could not be executed.
    #[error("professional delivery tool {tool:?} failed at {path}: {source}")]
    Spawn {
        /// Affected tool.
        tool: ProfessionalDeliveryTool,
        /// Executable path.
        path: PathBuf,
        /// Underlying process error.
        #[source]
        source: std::io::Error,
    },
    /// Tool exited unsuccessfully or produced unbounded identity output.
    #[error("professional delivery tool {tool:?} qualification failed: {detail}")]
    Qualification {
        /// Affected tool.
        tool: ProfessionalDeliveryTool,
        /// Specific failure.
        detail: String,
    },
}

fn resolve_tool(
    tool: ProfessionalDeliveryTool,
) -> Result<PathBuf, ProfessionalDeliveryToolchainError> {
    let name = executable_filename(tool.executable_name());
    if let Ok(current) = std::env::current_exe()
        && let Some(parent) = current.parent()
    {
        let adjacent = parent.join(&name);
        if is_direct_file(&adjacent) {
            return Ok(adjacent);
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path) {
            let candidate = directory.join(&name);
            if is_direct_file(&candidate) {
                return Ok(candidate);
            }
        }
    }
    Err(ProfessionalDeliveryToolchainError::Missing { tool })
}

fn resolve_photon() -> Result<(PathBuf, PathBuf), ProfessionalDeliveryToolchainError> {
    if let (Some(java), Some(libs)) = (
        std::env::var_os("MONDRIAN_PHOTON_JAVA"),
        std::env::var_os("MONDRIAN_PHOTON_LIB"),
    ) {
        let java = PathBuf::from(java);
        let libs = PathBuf::from(libs);
        if is_direct_file(&java) && photon_library_set_exists(&libs) {
            return Ok((java, libs));
        }
    }
    if let Ok(current) = std::env::current_exe()
        && let Some(parent) = current.parent()
    {
        let root = parent.join("professional-delivery").join("photon");
        let java = root.join("bin").join(executable_filename("java"));
        let libs = root.join("lib");
        if is_direct_file(&java) && photon_library_set_exists(&libs) {
            return Ok((java, libs));
        }
    }
    Err(ProfessionalDeliveryToolchainError::Missing {
        tool: ProfessionalDeliveryTool::PhotonValidator,
    })
}

fn executable_filename(stem: &str) -> String {
    format!("{stem}{}", std::env::consts::EXE_SUFFIX)
}

fn is_direct_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|metadata| !metadata.file_type().is_symlink() && metadata.is_file())
}

fn photon_library_set_exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|metadata| !metadata.file_type().is_symlink() && metadata.is_dir())
        && std::fs::read_dir(path).is_ok_and(|entries| {
            entries.filter_map(Result::ok).any(|entry| {
                entry.path().extension().is_some_and(|extension| extension == "jar")
                    && is_direct_file(&entry.path())
            })
        })
}

fn photon_classpath(path: &Path) -> PathBuf {
    path.join("*")
}

fn probe_photon_identity(
    java: &Path,
    libraries: &Path,
    deadline: Instant,
    cancellation: &ExecutionCancellationToken,
) -> Result<ProfessionalDeliveryToolIdentity, ProfessionalDeliveryToolchainError> {
    if !is_direct_file(java) || !photon_library_set_exists(libraries) {
        return Err(ProfessionalDeliveryToolchainError::Missing {
            tool: ProfessionalDeliveryTool::PhotonValidator,
        });
    }
    let mut command = Command::new(java);
    command
        .arg("-cp")
        .arg(photon_classpath(libraries))
        .arg("com.netflix.imflibrary.app.IMPAnalyzer")
        .stdin(Stdio::null());
    let output = capture_identity(
        &mut command,
        ProfessionalDeliveryTool::PhotonValidator,
        deadline,
        cancellation,
    )?;
    let mut bytes = output.stdout;
    bytes.extend_from_slice(&output.stderr);
    if bytes.is_empty() || bytes.len() > MAX_VERSION_BYTES {
        return Err(ProfessionalDeliveryToolchainError::Qualification {
            tool: ProfessionalDeliveryTool::PhotonValidator,
            detail: "Photon probe output is empty or exceeds 64 KiB".to_owned(),
        });
    }
    let text = String::from_utf8_lossy(&bytes);
    if !text.contains("IMPAnalyzer") && !text.contains("Usage") {
        return Err(ProfessionalDeliveryToolchainError::Qualification {
            tool: ProfessionalDeliveryTool::PhotonValidator,
            detail: "Photon IMPAnalyzer class could not be qualified".to_owned(),
        });
    }
    Ok(ProfessionalDeliveryToolIdentity {
        tool: ProfessionalDeliveryTool::PhotonValidator,
        path: libraries.to_path_buf(),
        version: text
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("Photon IMPAnalyzer")
            .trim()
            .to_owned(),
        native_cleanup: output.cleanup,
        version_output_sha256: format!("{:x}", Sha256::digest(&bytes)),
    })
}

fn probe_identity(
    tool: ProfessionalDeliveryTool,
    path: &Path,
    deadline: Instant,
    cancellation: &ExecutionCancellationToken,
) -> Result<ProfessionalDeliveryToolIdentity, ProfessionalDeliveryToolchainError> {
    if !is_direct_file(path) {
        return Err(ProfessionalDeliveryToolchainError::Missing { tool });
    }
    let mut command = Command::new(path);
    command.arg(tool.version_arg());
    let output = capture_identity(&mut command, tool, deadline, cancellation)?;
    identity_from_output(tool, path, output)
}

fn capture_identity(
    command: &mut impl mondrian_media::SupervisedCommand,
    tool: ProfessionalDeliveryTool,
    deadline: Instant,
    cancellation: &ExecutionCancellationToken,
) -> Result<mondrian_media::SupervisedProcessOutput, ProfessionalDeliveryToolchainError> {
    mondrian_media::run_supervised_command(
        command,
        None,
        SupervisedProcessPolicy {
            stdout: SupervisedStreamCapture::Head {
                limit_bytes: MAX_VERSION_BYTES,
                reject_excess: true,
            },
            stderr: SupervisedStreamCapture::Head {
                limit_bytes: MAX_VERSION_BYTES,
                reject_excess: true,
            },
            deadline: Some(deadline),
            ..Default::default()
        },
        cancellation,
    )
    .map_err(|source| ProfessionalDeliveryToolchainError::Native { tool, source: Box::new(source) })
}

fn identity_from_output(
    tool: ProfessionalDeliveryTool,
    path: &Path,
    output: mondrian_media::SupervisedProcessOutput,
) -> Result<ProfessionalDeliveryToolIdentity, ProfessionalDeliveryToolchainError> {
    if matches!(
        tool,
        ProfessionalDeliveryTool::BmxRaw2Bmx | ProfessionalDeliveryTool::BmxMxf2Raw
    ) && !output.status.success()
    {
        return Err(ProfessionalDeliveryToolchainError::VersionRejected {
            tool,
            detail: "BMX version probe exited unsuccessfully".to_owned(),
            output: Box::new(output),
        });
    }
    let mut bytes = output.stdout.clone();
    bytes.extend_from_slice(&output.stderr);
    if bytes.is_empty() || bytes.len() > MAX_VERSION_BYTES {
        return Err(ProfessionalDeliveryToolchainError::VersionRejected {
            tool,
            detail: "version output is empty or exceeds 64 KiB".to_owned(),
            output: Box::new(output),
        });
    }
    // CineCert help returns a non-zero status on some builds; bounded output
    // containing the executable identity is the qualification fact here.
    let version = String::from_utf8_lossy(&bytes)
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .trim()
        .to_owned();
    if version.is_empty() {
        return Err(ProfessionalDeliveryToolchainError::VersionRejected {
            tool,
            detail: "version identity has no non-empty line".to_owned(),
            output: Box::new(output),
        });
    }
    Ok(ProfessionalDeliveryToolIdentity {
        tool,
        path: path.to_path_buf(),
        version,
        native_cleanup: output.cleanup,
        version_output_sha256: format!("{:x}", Sha256::digest(&bytes)),
    })
}
