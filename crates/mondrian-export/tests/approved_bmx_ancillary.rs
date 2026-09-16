//! Real approved BMX ANC wrapping/reimport; explicitly not an AS-11 or hardware qualification.
#![cfg(all(windows, feature = "validation"))]

use anyhow::{ensure, Context, Result};
use mondrian_broadcast::{
    import_caption_program, verify_st436_mxf_ancillary, write_st436_klv_frame, AncillaryField,
    AncillaryFrame, AncillaryOrigin, AncillaryPacket, AncillaryPlacement, AncillarySpace,
    AncillaryValidationLevel, CaptionImportBinding, CaptionSourceFormat, FrozenAncillaryProgram,
    St291Type2Packet,
};
use mondrian_core::{ExecutionCancellationToken, Rational, TimelineTime};
use mondrian_export::{
    preset::ProfessionalDeliveryProfile, professional_delivery::ProfessionalDeliveryToolchain,
};
use mondrian_media::{
    prepare_bmx_runtime, run_supervised_command, ApprovedBmxCommand, ApprovedBmxTool,
    ApprovedProviderFile, SupervisedProcessError, SupervisedProcessPolicy, SupervisedStreamCapture,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const MAX_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024;

fn boundary(deadline: Instant, cancel: &ExecutionCancellationToken) -> io::Result<()> {
    if cancel.is_canceled() {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "ANC fixture canceled",
        ));
    }
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "original ANC fixture deadline",
        ));
    }
    Ok(())
}

struct BoundedReader<'a, R> {
    inner: R,
    deadline: Instant,
    cancel: &'a ExecutionCancellationToken,
}

impl<R: Read> Read for BoundedReader<'_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        boundary(self.deadline, self.cancel)?;
        let length = bytes.len().min(65536);
        self.inner.read(&mut bytes[..length])
    }
}

fn hash(file: &mut File, deadline: Instant, cancel: &ExecutionCancellationToken) -> Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 65536];
    loop {
        boundary(deadline, cancel)?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(format!("{:x}", digest.finalize()))
}

fn approved(
    path: PathBuf,
    deadline: Instant,
    cancel: &ExecutionCancellationToken,
) -> Result<(ApprovedProviderFile, Value)> {
    let path = path.canonicalize()?;
    let mut file = OpenOptions::new().read(true).share_mode(1).open(&path)?;
    ensure!(
        file.metadata()?.len() <= 512 * 1024 * 1024,
        "oversized BMX executable"
    );
    let digest = hash(&mut file, deadline, cancel)?;
    let mut bytes = [0; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&digest[index * 2..index * 2 + 2], 16)?;
    }
    let evidence = json!({"path":path,"sha256":digest,"bytes":file.metadata()?.len()});
    Ok((
        ApprovedProviderFile::from_retained(path, bytes, file),
        evidence,
    ))
}

fn execute(
    command: &mut ApprovedBmxCommand,
    stage: &str,
    deadline: Instant,
    cancel: &ExecutionCancellationToken,
    observations: &mut Vec<Value>,
) -> Result<Vec<u8>> {
    let arguments: Vec<_> =
        command.get_args().map(|value| value.to_string_lossy().into_owned()).collect();
    let result = run_supervised_command(
        command,
        None,
        SupervisedProcessPolicy {
            deadline: Some(deadline),
            stdout: SupervisedStreamCapture::Head { limit_bytes: 1024 * 1024, reject_excess: true },
            stderr: SupervisedStreamCapture::Head { limit_bytes: 65536, reject_excess: true },
            ..Default::default()
        },
        cancel,
    );
    match result {
        Ok(output) => {
            let clean = output.status.success()
                && output.cleanup.all_resources_released()
                && !output.stdout_truncated
                && !output.stderr_truncated;
            observations.push(json!({"stage":stage,"executable":command.get_program().to_string_lossy(),
                "argv":arguments,"status":output.status.code(),"stdout":output.stdout,"stderr":output.stderr,
                "cleanup":output.cleanup,"stdout_truncated":output.stdout_truncated,"stderr_truncated":output.stderr_truncated}));
            ensure!(
                clean,
                "{stage} native status/cleanup failed; original streams retained"
            );
            Ok(output.stdout)
        }
        Err(error) => {
            let cleanup = match &error {
                SupervisedProcessError::Cleanup { cleanup, .. } => Some(cleanup.as_ref()),
                _ => None,
            };
            observations.push(json!({"stage":stage,"argv":arguments,"failure":format!("{error:?}"),"cleanup":cleanup}));
            Err(error.into())
        }
    }
}

fn placement() -> Result<AncillaryPlacement> {
    Ok(AncillaryPlacement::new(
        AncillarySpace::Vanc,
        AncillaryField::Progressive,
        20,
        0,
    )?)
}

fn binding(frame_count: u64) -> Result<CaptionImportBinding> {
    Ok(CaptionImportBinding {
        source_start: TimelineTime::ZERO,
        output_frame_rate: Rational::new(60000, 1001),
        frame_count,
        timecode_origin: TimelineTime::ZERO,
        placement: placement()?,
    })
}

fn programs() -> Result<Vec<(&'static str, FrozenAncillaryProgram)>> {
    let scc = import_caption_program(
        b"Scenarist_SCC V1.0\n00:00:00:00\t9420 c849 942f",
        CaptionSourceFormat::ScenaristSccV1,
        binding(6)?,
    )?;
    // One standard 59.94 CDP carrying service-1 CEA-708 text 'A', with an
    // explicit 608 padding pair, nine digital slots and a modulo-256 checksum.
    let mut cdp = vec![
        0x96, 0x69, 43, 0x7f, 0x43, 0, 0, 0x72, 0xea, 0xfc, 0x80, 0x80,
    ];
    cdp.extend_from_slice(&[0xff, 2, 0x21, 0xfe, 0x41, 0]);
    for _ in 0..7 {
        cdp.extend_from_slice(&[0xfa, 0, 0]);
    }
    cdp.extend_from_slice(&[0x74, 0, 0]);
    cdp.push(0u8.wrapping_sub(cdp.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte))));
    let digital =
        import_caption_program(&cdp, CaptionSourceFormat::RawCdpSt334_2_2015, binding(1)?)?;
    ensure!(
        scc.caption_source().is_some_and(|receipt| receipt.cea608_pairs == 3),
        "608 import provenance"
    );
    ensure!(
        digital.caption_source().is_some_and(|receipt| receipt.cea708_packets == 1),
        "708 import provenance"
    );
    let frame = |index, payload: &[u8]| -> Result<AncillaryFrame> {
        Ok(AncillaryFrame::new(
            index,
            vec![AncillaryPacket {
                placement: placement()?,
                packet: St291Type2Packet::from_8bit_payload(0x61, 1, payload)?,
                origin: AncillaryOrigin::Derived,
                validation: AncillaryValidationLevel::Transport,
            }],
        )?)
    };
    let sparse = FrozenAncillaryProgram::new(
        TimelineTime::ZERO,
        Rational::new(25, 1),
        3,
        vec![frame(0, &[])?, frame(2, &[0x55; 255])?],
    )?;
    Ok(vec![
        ("scc608", scc),
        ("cdp708", digital),
        ("sparse", sparse),
    ])
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    let mut file = File::create_new(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

#[test]
#[ignore = "requires official BMX 1.6 and explicit native evidence directory; ANC-only, no physical qualification"]
fn approved_bmx_wrap_reimport_and_exact_word_rescan() -> Result<()> {
    let tools = PathBuf::from(
        std::env::var_os("MONDRIAN_BMX_TOOL_DIR")
            .context("explicit official BMX tools required")?,
    );
    let evidence_parent = PathBuf::from(
        std::env::var_os("MONDRIAN_BMX_ANC_EVIDENCE_DIR")
            .context("existing evidence parent required")?,
    )
    .canonicalize()?;
    let output = tempfile::Builder::new()
        .prefix("approved-bmx-anc-")
        .tempdir_in(evidence_parent)?
        .keep();
    eprintln!("retained approved BMX ANC evidence: {}", output.display());
    let cancel = ExecutionCancellationToken::new();
    let deadline = Instant::now() + Duration::from_secs(180);
    let (raw2bmx, raw_identity) = approved(tools.join("raw2bmx.exe"), deadline, &cancel)?;
    let (mxf2raw, read_identity) = approved(tools.join("mxf2raw.exe"), deadline, &cancel)?;
    let owner = prepare_bmx_runtime([raw2bmx, mxf2raw], Some(Vec::new()), deadline, &cancel)?
        .context("native approved BMX capsule unavailable")?;
    let handle = owner.handle();
    let mut observations = Vec::new();
    let mut artifacts = Vec::new();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        let toolchain = ProfessionalDeliveryToolchain::discover_with_bmx(ProfessionalDeliveryProfile::As11X9NabaHd720p5994, Some(handle.clone()))?;
        for tool in [ApprovedBmxTool::Raw2Bmx, ApprovedBmxTool::Mxf2Raw] {
            let mut command = handle.command(tool);
            command.arg("-v");
            let version = execute(&mut command, &format!("version:{tool:?}"), deadline, &cancel, &mut observations)?;
            // BMX emits version identity on stdout in the approved native build.
            ensure!(String::from_utf8_lossy(&version).contains("bmx v1.6.0"), "approved BMX 1.6 identity missing");
        }
        for (name, program) in programs()? {
            boundary(deadline, &cancel)?;
            let input = output.join(format!("{name}.klv"));
            let mxf = output.join(format!("{name}.mxf"));
            write_json(&output.join(format!("{name}.canonical.json")), &serde_json::to_value(&program)?)?;
            let expected = (0..program.frame_count()).map(|index| program.frame(index)).collect::<std::result::Result<Vec<_>, _>>()?;
            let mut klv = File::create_new(&input)?;
            for frame in &expected { boundary(deadline, &cancel)?; write_st436_klv_frame(&mut klv, frame)?; }
            klv.sync_all()?;
            drop(klv);
            let mut wrap = handle.command(ApprovedBmxTool::Raw2Bmx);
            wrap.args(["-t", "op1a", "-f", if name == "sparse" { "25" } else { "5994" }, "--dur"])
                .arg(program.frame_count().to_string()).arg("-o").arg(&mxf);
            ProfessionalDeliveryToolchain::attach_as11_ancillary(&mut wrap, &input);
            execute(&mut wrap, &format!("{name}:wrap"), deadline, &cancel, &mut observations)?;
            drop(wrap);
            let mut inspect = toolchain.bmx_reimport_command(&mxf, false);
            let text = execute(&mut inspect, &format!("{name}:reimport"), deadline, &cancel, &mut observations)?;
            drop(inspect);
            let text = String::from_utf8_lossy(&text);
            for token in ["ANC_Data", "ANC_10_Bit_Luma", "VANC_Progressive_Frame", "is_complete     : true", "last_frame      : true"] {
                ensure!(text.contains(token), "{name} reader evidence missing {token}");
            }
            let mut file = OpenOptions::new().read(true).share_mode(1).open(&mxf)?;
            let bytes = file.metadata()?.len();
            ensure!(bytes > 0 && bytes <= MAX_ARTIFACT_BYTES, "native MXF extent bound");
            let digest = hash(&mut file, deadline, &cancel)?;
            let frozen_count = program.verify_mxf(&mut BoundedReader { inner: &mut file, deadline, cancel: &cancel }, bytes)?;
            file.seek(SeekFrom::Start(0))?;
            let explicit_count = verify_st436_mxf_ancillary(&mut BoundedReader { inner: &mut file, deadline, cancel: &cancel }, &expected, bytes)?;
            ensure!(frozen_count == program.frame_count() && explicit_count == program.frame_count(), "actual full frame inventory mismatch");
            let mut wrong = expected.clone();
            let frame = wrong.iter_mut().find(|frame| !frame.packets().is_empty()).context("nonempty test frame")?;
            let mut packets = frame.packets().to_vec();
            packets[0].packet = St291Type2Packet::from_8bit_payload(0x61, 1, &[0x7f])?;
            *frame = AncillaryFrame::new(frame.frame_index(), packets)?;
            file.seek(SeekFrom::Start(0))?;
            let changed_word_rejected = matches!(verify_st436_mxf_ancillary(&mut BoundedReader { inner: &mut file, deadline, cancel: &cancel }, &wrong, bytes), Err(mondrian_broadcast::St436Error::CanonicalMismatch));
            file.seek(SeekFrom::Start(0))?;
            let missing_last_frame_rejected = matches!(verify_st436_mxf_ancillary(&mut BoundedReader { inner: &mut file, deadline, cancel: &cancel }, &expected[..expected.len()-1], bytes), Err(mondrian_broadcast::St436Error::CanonicalMismatch));
            boundary(deadline, &cancel)?;
            ensure!(hash(&mut file, deadline, &cancel)? == digest, "MXF changed during same-handle rescan");
            artifacts.push(json!({"name":name,"path":mxf,"bytes":bytes,"sha256":digest,
                "canonical_program":program,"frozen_program_verified_frames":frozen_count,"explicit_verified_frames":explicit_count,
                "changed_packet_word_rejected":changed_word_rejected,"missing_last_frame_rejected":missing_last_frame_rejected}));
            ensure!(changed_word_rejected && missing_last_frame_rejected, "actual MXF accepted mismatching canonical inventory");
        }
        cancel.cancel();
        let mut canceled = handle.command(ApprovedBmxTool::Mxf2Raw);
        canceled.arg("-v");
        let rejection = execute(&mut canceled, "canceled_owner_admission", deadline, &cancel, &mut observations);
        ensure!(rejection.as_ref().err().and_then(|error| error.downcast_ref::<SupervisedProcessError>()).is_some_and(SupervisedProcessError::is_canceled), "canceled owner admitted a native command or lost the cancellation cause");
        Ok(())
    })).unwrap_or_else(|_| Err(anyhow::anyhow!("native approved BMX test panicked")));
    drop(handle);
    let closure = owner.close_until(deadline);
    let clean = closure.namespace_owned && closure.all_resources_released();
    write_json(
        &output.join("report.json"),
        &json!({"schema_version":1,"qualified":false,
        "scope":"approved BMX ANC-only OP1a wrapping/reimport and complete canonical word rescan; no picture/audio/AS11/hardware claim",
        "failure":result.as_ref().err().map(|error| format!("{error:#}")),"tools":[raw_identity,read_identity],
        "native_commands":observations,"artifacts":artifacts,"runtime_closure":closure}),
    )?;
    result?;
    ensure!(
        clean,
        "approved BMX namespace closure failed; raw evidence retained"
    );
    Ok(())
}
