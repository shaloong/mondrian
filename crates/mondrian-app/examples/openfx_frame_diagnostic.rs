//! The original whole-public OpenFX gate without integration companion bins.
//!
//! This target is a diagnostic carrier, not an alternative timing boundary.
//! The gate body is copied byte-for-byte from the current integration test.
//! Its independent source/artifact receipt and original budget remain mandatory.

#[cfg(not(test))]
fn main() {
    eprintln!("Run cargo test --release -p mondrian-app --all-features --example openfx_frame_diagnostic -- --ignored --nocapture --test-threads=1");
    std::process::exit(2);
}

#[cfg(test)]
mod qualification {
    use mondrian_app::openfx_adapter::inspect_openfx_binary;
    use mondrian_app::openfx_host::{
        OpenFxFilterSession, OpenFxParameterBinding, OpenFxParameterValue, OpenFxRenderTiming,
    };
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    fn product_executable() -> &'static Path {
        static WORKER: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        WORKER
            .get_or_init(|| {
                PathBuf::from(
                    std::env::var_os("MONDRIAN_OPENFX_DIAGNOSTIC_HELPER")
                        .expect("explicit pinned production helper"),
                )
            })
            .as_path()
    }

    fn constant_parameters<const N: usize>(
        values: [(String, OpenFxParameterValue); N],
    ) -> BTreeMap<String, OpenFxParameterBinding> {
        values
            .into_iter()
            .map(|(name, value)| (name, OpenFxParameterBinding::constant(value)))
            .collect()
    }

    #[test]
    fn materializer_checkpoint_attribution_preserves_original_stop_frontiers() {
        use mondrian_app::openfx_host::{
            diagnose_openfx_output_materialization, OpenFxHostError, OpenFxRenderDiagnostic,
            OpenFxRenderPhase,
        };
        use mondrian_core::{ColorFrameAlpha, ImageSampling};
        use mondrian_effects::{
            EffectColorDomain, EffectFrameExtent, EffectValueFormat, EffectWorkingPrecision,
        };

        let extent = EffectFrameExtent::new(256, 257); // A complete block and a partial tail.
        let pixels = vec![[f32::MAX, -2.0, f32::from_bits(1), -0.5]; 256 * 257];
        for alpha in [ColorFrameAlpha::StraightFloat, ColorFrameAlpha::Opaque] {
            let format = EffectValueFormat::new(
                EffectWorkingPrecision::Float32,
                EffectColorDomain::SceneLinearRgb,
                alpha,
            );
            // Two original allocation checks, four copy checks, optional two
            // opaque sweeps, then two admission blocks plus the final check.
            let total = if alpha == ColorFrameAlpha::Opaque {
                11
            } else {
                9
            };
            for stop in 1..=total + 1 {
                let mut calls = 0;
                let mut report = OpenFxRenderDiagnostic::default();
                let result = diagnose_openfx_output_materialization(
                    extent,
                    format,
                    ImageSampling::UNIT_SQUARE,
                    bytemuck::cast_slice(&pixels),
                    &mut || {
                        calls += 1;
                        if calls == stop {
                            Err(OpenFxHostError::Canceled)
                        } else {
                            Ok(())
                        }
                    },
                    &mut report,
                );
                assert_eq!(calls, stop.min(total), "no added or removed checks");
                assert!(!report.counter_overflowed && !report.accepted_output);
                assert!(report.owner.is_none() && report.child.is_none());
                if stop <= total {
                    assert!(matches!(result, Err(OpenFxHostError::Canceled)));
                } else {
                    let frame = result.expect("complete original materialization");
                    assert_eq!(report.output_admission_checkpoint_calls, 3);
                    assert!(
                        report.output_admission_checkpoint_elapsed
                            <= report.elapsed(OpenFxRenderPhase::OutputAdmission).unwrap()
                    );
                    assert!(frame.pixels().iter().zip(&pixels).all(|(actual, source)| {
                        actual[..3]
                            .iter()
                            .zip(&source[..3])
                            .all(|(a, b)| a.to_bits() == b.to_bits())
                            && actual[3]
                                == if alpha == ColorFrameAlpha::Opaque {
                                    1.0
                                } else {
                                    source[3]
                                }
                    }));
                }
            }
        }
    }

    #[test]
    #[ignore = "requires release product, official Basic bundle and explicit OpenFX perf receipt/budget"]
    fn official_basic_production_resident_steady_frame_performance_gate() {
        use sha2::Digest;
        use std::time::Instant;

        if cfg!(debug_assertions) {
            panic!("frame-budget qualification requires --release");
        }
        assert!(
            std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_MODE").is_none(),
            "use untouched official Basic, with no SessionProbe behavior"
        );
        let integer = |name: &str| -> u32 { std::env::var(name).expect(name).parse().expect(name) };
        let width = integer("MONDRIAN_OPENFX_PERF_WIDTH");
        let height = integer("MONDRIAN_OPENFX_PERF_HEIGHT");
        OpenFxFilterSession::storage(width, height)
            .expect("dimensions must be admitted before source allocation")
            .active_peak_bytes();
        let rate_num = integer("MONDRIAN_OPENFX_PERF_FPS_NUM");
        let rate_den = integer("MONDRIAN_OPENFX_PERF_FPS_DEN");
        let stage_percent = integer("MONDRIAN_OPENFX_PERF_STAGE_BUDGET_PERCENT");
        let count = integer("MONDRIAN_OPENFX_PERF_FRAMES");
        let warmup = integer("MONDRIAN_OPENFX_PERF_WARMUP");
        assert!(rate_num > 0 && rate_den > 0 && (1..=100).contains(&stage_percent));
        assert!((200..=10_000).contains(&count) && (16..=1000).contains(&warmup));
        let frame_budget_ns = 1_000_000_000_u64 * u64::from(rate_den) / u64::from(rate_num);
        let stage_budget_ns =
            u64::try_from(u128::from(frame_budget_ns) * u128::from(stage_percent) / 100)
                .expect("stage budget must be representable");
        assert!(stage_budget_ns > 0);
        let machine_path = PathBuf::from(
            std::env::var_os("MONDRIAN_NATIVE_PERF_CONTEXT").expect("machine/build receipt path"),
        );
        let context: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&machine_path).expect("read machine/build receipt"),
        )
        .expect("machine/build receipt JSON");
        assert_eq!(context["schema_version"], 1);
        assert_eq!(context["machine"]["cpu_identity_and_ram_available"], true);
        assert_eq!(context["build"]["profile"], "release");
        assert!(context["build"]["commands"]
            .as_array()
            .is_some_and(|commands| !commands.is_empty()));
        assert!(context["build"]["rustc_verbose"]
            .as_str()
            .is_some_and(|value| !value.is_empty()));
        let hash = |path: &Path| -> String {
            format!(
                "{:x}",
                sha2::Sha256::digest(std::fs::read(path).expect("receipt identity"))
            )
        };
        let worktree =
            PathBuf::from(std::env::var_os("MONDRIAN_PERF_WORKTREE").expect("receipt worktree"));
        assert!(worktree.is_absolute() && worktree.is_dir());
        assert_eq!(
            hash(&worktree.join("Cargo.lock")),
            context["build"]["cargo_lock_sha256"]
        );
        let transcript = Path::new(
            context["build"]["transcript"]["path"].as_str().expect("build transcript path"),
        );
        assert_eq!(hash(transcript), context["build"]["transcript"]["sha256"]);
        let sources = context["source_receipts"].as_array().expect("source receipts");
        assert!(!sources.is_empty());
        for source in sources {
            let path = Path::new(source["path"].as_str().expect("relative source path"));
            assert!(
                path.components().all(|part| matches!(part, std::path::Component::Normal(_))),
                "source receipt must stay inside the worktree"
            );
            assert_eq!(
                hash(&worktree.join(path)),
                source["sha256"],
                "source changed after the build receipt"
            );
        }
        for required in [
            "crates/mondrian-app/src/openfx_host.rs",
            "crates/mondrian-app/src/openfx_host/resident.rs",
            "crates/mondrian-app/native/openfx/host/render.cpp",
            "crates/mondrian-app/tests/openfx_host.rs",
        ] {
            assert!(
                sources.iter().any(|source| source["path"] == required),
                "source receipt missing {required}"
            );
        }
        if let Some(external) = context["external_source_receipts"].as_array() {
            for source in external {
                let path = Path::new(source["path"].as_str().expect("external source path"));
                assert!(path.is_absolute(), "external receipt path must be explicit");
                assert_eq!(
                    hash(path),
                    source["sha256"],
                    "reference source changed after the build receipt"
                );
            }
        }
        let binary = PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_REFERENCE_BINARY").expect("official Basic binary"),
        );
        let test_executable = std::env::current_exe().expect("current test executable");
        let artifacts = context["artifacts"].as_array().expect("artifact receipts");
        for (role, actual) in [
            ("openfx_worker", product_executable()),
            ("openfx_reference", binary.as_path()),
            ("openfx_test_executable", test_executable.as_path()),
        ] {
            let entries =
                artifacts.iter().filter(|entry| entry["role"] == role).collect::<Vec<_>>();
            assert_eq!(
                entries.len(),
                1,
                "artifact identity missing or ambiguous: {role}"
            );
            let recorded = Path::new(entries[0]["path"].as_str().expect("artifact path"));
            assert_eq!(
                std::fs::canonicalize(recorded).expect("recorded artifact"),
                std::fs::canonicalize(actual).expect("actual artifact"),
                "wrong artifact selected: {role}"
            );
            assert_eq!(
                hash(actual),
                entries[0]["sha256"],
                "artifact changed: {role}"
            );
        }
        let setup = Instant::now();
        let inspection =
            inspect_openfx_binary(product_executable(), &binary).expect("inspect Basic");
        let gain = constant_parameters([("scale".to_owned(), OpenFxParameterValue::Double(0.5))]);
        let pixels = usize::try_from(u64::from(width) * u64::from(height)).expect("pixel count");
        let mut input = mondrian_effects::EffectFloatFrame::from_pixels(
            mondrian_effects::EffectFrameExtent::new(width, height),
            mondrian_effects::EffectValueFormat::new(
                mondrian_effects::EffectWorkingPrecision::Float32,
                mondrian_effects::EffectColorDomain::SceneLinearRgb,
                mondrian_core::ColorFrameAlpha::StraightFloat,
            ),
            mondrian_core::ImageSampling::UNIT_SQUARE,
            (0..pixels)
                .map(|index| {
                    let x = (index % width as usize) as f32 / width as f32;
                    let y = (index / width as usize) as f32 / height as f32;
                    [-0.25 + x, 0.5 + y, 2.0 - x, 0.25 + y / 4.0]
                })
                .collect(),
        )
        .expect("owned Float fixture");
        let mut session = OpenFxFilterSession::prepare(
            product_executable(),
            &inspection,
            "uk.co.thefoundry.BasicGainPlugin",
            width,
            height,
            mondrian_effects::EffectValueFormat::new(
                mondrian_effects::EffectWorkingPrecision::Float32,
                mondrian_effects::EffectColorDomain::SceneLinearRgb,
                mondrian_core::ColorFrameAlpha::StraightFloat,
            ),
            mondrian_effects::EffectProjectGeometry::new(
                width,
                height,
                mondrian_core::SampleAspectRatio::SQUARE,
            )
            .expect("explicit owner project geometry"),
            &mut || Ok(()),
        )
        .expect("prepare real production resident worker");
        let pid = session.worker_process_id();
        let setup_ns = u64::try_from(setup.elapsed().as_nanos()).expect("setup duration");
        let mut timing = OpenFxRenderTiming {
            frame: 0.0,
            frame_rate: f64::from(rate_num) / f64::from(rate_den),
            first_frame: 0.0,
            last_frame: f64::from(count + warmup),
        };
        let mut durations = Vec::with_capacity(count as usize);
        let mut errors = Vec::new();
        for index in 0..(warmup + count) {
            timing.frame = f64::from(index);
            // Vary a deterministic source pixel so retained output cannot masquerade
            // as completed native work. Correctness is checked outside the timer.
            input.pixels_mut()[0][0] = -0.25 + index as f32 / 65536.0;
            let begin = Instant::now();
            let result = session.render(&timing, &gain, &input, &mut || Ok(()));
            let elapsed = u64::try_from(begin.elapsed().as_nanos()).expect("render duration");
            if index >= warmup {
                durations.push(elapsed);
            }
            match result {
                Ok(output) => {
                    if session.worker_process_id() != pid {
                        errors.push("steady execution respawned a worker".into());
                        break;
                    }
                    let correct = output.pixels().len() == input.pixels().len()
                        && output.pixels().iter().zip(input.pixels()).all(|(actual, source)| {
                            (0..4).all(|channel| {
                                actual[channel].is_finite()
                                    && (actual[channel] - source[channel] * 0.5).abs() < 1e-6
                            })
                        });
                    if !correct {
                        errors.push(format!("independent gain oracle failed at frame {index}"));
                        break;
                    }
                }
                Err(error) => {
                    errors.push(error.to_string());
                    break;
                }
            }
        }
        let shutdown = Instant::now();
        let shutdown_result = session.shutdown();
        let shutdown_ns = u64::try_from(shutdown.elapsed().as_nanos()).expect("shutdown duration");
        if let Err(error) = shutdown_result {
            errors.push(error.to_string());
        }
        let mut sorted = durations.clone();
        sorted.sort_unstable();
        let percentile = |percent: usize| -> Option<u64> {
            sorted.get((sorted.len() * percent).div_ceil(100).saturating_sub(1)).copied()
        };
        let missed_frames = durations.iter().filter(|&&nanos| nanos > frame_budget_ns).count();
        let missed_stage_budget =
            durations.iter().filter(|&&nanos| nanos > stage_budget_ns).count();
        let worker_hash = format!(
            "{:x}",
            sha2::Sha256::digest(
                std::fs::read(product_executable()).expect("actual product worker identity")
            )
        );
        let receipt = serde_json::json!({ "gate": "openfx_production_resident_cpu_frame_v1",
        "context": context, "context_sha256": hash(&machine_path),
        "fixture_sha256": inspection.binary_sha256,
        "worker_sha256": worker_hash, "width": width, "height": height,
        "fps_num": rate_num, "fps_den": rate_den, "stage_budget_percent": stage_percent,
        "frame_budget_ns": frame_budget_ns, "stage_budget_ns": stage_budget_ns,
        "warmup_excluded": warmup, "samples_requested": count, "samples": durations.len(),
        "setup_ns": setup_ns, "shutdown_ns": shutdown_ns, "p50_ns": percentile(50),
        "p95_ns": percentile(95), "p99_ns": percentile(99), "max_ns": sorted.last(),
        "frame_budget_misses": missed_frames, "stage_budget_misses": missed_stage_budget,
        "render_ns": durations, "errors": errors,
        "oracle": "independent asymmetric HDR/negative/alpha gain on every output; timed render excludes oracle",
        "timed_boundary": "public OpenFxFilterSession::render including validation, hashes, copies, IPC, native work and output allocation" });
        let output_path = PathBuf::from(
            std::env::var_os("MONDRIAN_PERF_OUTPUT").expect("qualification JSON output path"),
        );
        std::fs::write(
            output_path,
            serde_json::to_vec_pretty(&receipt).expect("serialize receipt"),
        )
        .expect("write qualification receipt before budget verdict");
        assert!(errors.is_empty(), "native work failed: {errors:?}");
        assert_eq!(sorted.len(), count as usize);
        assert_eq!(
            missed_stage_budget, 0,
            "real resident frame exceeded declared NLE stage budget: {receipt}"
        );
    }

    #[test]
    #[ignore = "requires release product, official Basic bundle and explicit OpenFX perf receipt/budget"]
    fn official_basic_production_resident_phase_attribution() {
        use sha2::Digest;
        use std::time::Instant;

        if cfg!(debug_assertions) {
            panic!("frame-budget qualification requires --release");
        }
        assert!(
            std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_MODE").is_none(),
            "use untouched official Basic, with no SessionProbe behavior"
        );
        let integer = |name: &str| -> u32 { std::env::var(name).expect(name).parse().expect(name) };
        let width = integer("MONDRIAN_OPENFX_PERF_WIDTH");
        let height = integer("MONDRIAN_OPENFX_PERF_HEIGHT");
        OpenFxFilterSession::storage(width, height)
            .expect("dimensions must be admitted before source allocation")
            .active_peak_bytes();
        let rate_num = integer("MONDRIAN_OPENFX_PERF_FPS_NUM");
        let rate_den = integer("MONDRIAN_OPENFX_PERF_FPS_DEN");
        let stage_percent = integer("MONDRIAN_OPENFX_PERF_STAGE_BUDGET_PERCENT");
        let count = integer("MONDRIAN_OPENFX_PERF_FRAMES");
        let warmup = integer("MONDRIAN_OPENFX_PERF_WARMUP");
        assert!(rate_num > 0 && rate_den > 0 && (1..=100).contains(&stage_percent));
        assert!((200..=10_000).contains(&count) && (16..=1000).contains(&warmup));
        let frame_budget_ns = 1_000_000_000_u64 * u64::from(rate_den) / u64::from(rate_num);
        let stage_budget_ns =
            u64::try_from(u128::from(frame_budget_ns) * u128::from(stage_percent) / 100)
                .expect("stage budget must be representable");
        assert!(stage_budget_ns > 0);
        let machine_path = PathBuf::from(
            std::env::var_os("MONDRIAN_NATIVE_PERF_CONTEXT").expect("machine/build receipt path"),
        );
        let context: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&machine_path).expect("read machine/build receipt"),
        )
        .expect("machine/build receipt JSON");
        assert_eq!(context["schema_version"], 1);
        assert_eq!(context["machine"]["cpu_identity_and_ram_available"], true);
        assert_eq!(context["build"]["profile"], "release");
        assert!(context["build"]["commands"]
            .as_array()
            .is_some_and(|commands| !commands.is_empty()));
        assert!(context["build"]["rustc_verbose"]
            .as_str()
            .is_some_and(|value| !value.is_empty()));
        let hash = |path: &Path| -> String {
            format!(
                "{:x}",
                sha2::Sha256::digest(std::fs::read(path).expect("receipt identity"))
            )
        };
        let worktree =
            PathBuf::from(std::env::var_os("MONDRIAN_PERF_WORKTREE").expect("receipt worktree"));
        assert!(worktree.is_absolute() && worktree.is_dir());
        assert_eq!(
            hash(&worktree.join("Cargo.lock")),
            context["build"]["cargo_lock_sha256"]
        );
        let transcript = Path::new(
            context["build"]["transcript"]["path"].as_str().expect("build transcript path"),
        );
        assert_eq!(hash(transcript), context["build"]["transcript"]["sha256"]);
        let sources = context["source_receipts"].as_array().expect("source receipts");
        assert!(!sources.is_empty());
        for source in sources {
            let path = Path::new(source["path"].as_str().expect("relative source path"));
            assert!(
                path.components().all(|part| matches!(part, std::path::Component::Normal(_))),
                "source receipt must stay inside the worktree"
            );
            assert_eq!(
                hash(&worktree.join(path)),
                source["sha256"],
                "source changed after the build receipt"
            );
        }
        for required in [
            "crates/mondrian-app/src/openfx_host.rs",
            "crates/mondrian-app/src/openfx_host/resident.rs",
            "crates/mondrian-app/src/openfx_host/resident/diagnostic.rs",
            "crates/mondrian-app/src/openfx_host/resident/child_diagnostic.rs",
            "crates/mondrian-app/native/openfx/host/render_diagnostic.h",
            "crates/mondrian-app/native/openfx/host/image_clip.cpp",
            "crates/mondrian-app/examples/openfx_frame_diagnostic.rs",
            "crates/mondrian-app/native/openfx/host/render.cpp",
            "crates/mondrian-app/tests/openfx_host.rs",
        ] {
            assert!(
                sources.iter().any(|source| source["path"] == required),
                "source receipt missing {required}"
            );
        }
        if let Some(external) = context["external_source_receipts"].as_array() {
            for source in external {
                let path = Path::new(source["path"].as_str().expect("external source path"));
                assert!(path.is_absolute(), "external receipt path must be explicit");
                assert_eq!(
                    hash(path),
                    source["sha256"],
                    "reference source changed after the build receipt"
                );
            }
        }
        let binary = PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_REFERENCE_BINARY").expect("official Basic binary"),
        );
        let test_executable = std::env::current_exe().expect("current test executable");
        let artifacts = context["artifacts"].as_array().expect("artifact receipts");
        for (role, actual) in [
            ("openfx_worker", product_executable()),
            ("openfx_reference", binary.as_path()),
            ("openfx_test_executable", test_executable.as_path()),
        ] {
            let entries =
                artifacts.iter().filter(|entry| entry["role"] == role).collect::<Vec<_>>();
            assert_eq!(
                entries.len(),
                1,
                "artifact identity missing or ambiguous: {role}"
            );
            let recorded = Path::new(entries[0]["path"].as_str().expect("artifact path"));
            assert_eq!(
                std::fs::canonicalize(recorded).expect("recorded artifact"),
                std::fs::canonicalize(actual).expect("actual artifact"),
                "wrong artifact selected: {role}"
            );
            assert_eq!(
                hash(actual),
                entries[0]["sha256"],
                "artifact changed: {role}"
            );
        }
        let setup = Instant::now();
        let inspection =
            inspect_openfx_binary(product_executable(), &binary).expect("inspect Basic");
        let gain = constant_parameters([("scale".to_owned(), OpenFxParameterValue::Double(0.5))]);
        let pixels = usize::try_from(u64::from(width) * u64::from(height)).expect("pixel count");
        let mut input = mondrian_effects::EffectFloatFrame::from_pixels(
            mondrian_effects::EffectFrameExtent::new(width, height),
            mondrian_effects::EffectValueFormat::new(
                mondrian_effects::EffectWorkingPrecision::Float32,
                mondrian_effects::EffectColorDomain::SceneLinearRgb,
                mondrian_core::ColorFrameAlpha::StraightFloat,
            ),
            mondrian_core::ImageSampling::UNIT_SQUARE,
            (0..pixels)
                .map(|index| {
                    let x = (index % width as usize) as f32 / width as f32;
                    let y = (index / width as usize) as f32 / height as f32;
                    [-0.25 + x, 0.5 + y, 2.0 - x, 0.25 + y / 4.0]
                })
                .collect(),
        )
        .expect("owned Float fixture");
        let mut session = OpenFxFilterSession::prepare(
            product_executable(),
            &inspection,
            "uk.co.thefoundry.BasicGainPlugin",
            width,
            height,
            mondrian_effects::EffectValueFormat::new(
                mondrian_effects::EffectWorkingPrecision::Float32,
                mondrian_effects::EffectColorDomain::SceneLinearRgb,
                mondrian_core::ColorFrameAlpha::StraightFloat,
            ),
            mondrian_effects::EffectProjectGeometry::new(
                width,
                height,
                mondrian_core::SampleAspectRatio::SQUARE,
            )
            .expect("explicit owner project geometry"),
            &mut || Ok(()),
        )
        .expect("prepare real production resident worker");
        let pid = session.worker_process_id();
        let setup_ns = u64::try_from(setup.elapsed().as_nanos()).expect("setup duration");
        let mut timing = OpenFxRenderTiming {
            frame: 0.0,
            frame_rate: f64::from(rate_num) / f64::from(rate_den),
            first_frame: 0.0,
            last_frame: f64::from(count + warmup),
        };
        let instrumented = match std::env::var("MONDRIAN_OPENFX_DIAGNOSTIC_PHASES").as_deref() {
            Ok("0") => false,
            Ok("1") => true,
            _ => panic!("explicit diagnostic mode must be 0 (ordinary) or 1 (phases)"),
        };
        let mut phases = Vec::with_capacity(if instrumented { count as usize } else { 0 });
        let mut report = mondrian_app::openfx_host::OpenFxRenderDiagnostic::default();
        let processor_owner = session.processor_owner();
        let mut durations = Vec::with_capacity(count as usize);
        let mut errors = Vec::new();
        for index in 0..(warmup + count) {
            timing.frame = f64::from(index);
            // Vary a deterministic source pixel so retained output cannot masquerade
            // as completed native work. Correctness is checked outside the timer.
            input.pixels_mut()[0][0] = -0.25 + index as f32 / 65536.0;
            let begin = Instant::now();
            let result = if instrumented {
                session.render_diagnostic(&timing, &gain, &input, &mut || Ok(()), &mut report)
            } else {
                session.render(&timing, &gain, &input, &mut || Ok(()))
            };
            let elapsed = u64::try_from(begin.elapsed().as_nanos()).expect("render duration");
            if index >= warmup {
                durations.push(elapsed);
                if instrumented {
                    phases.push(report);
                }
            }
            if instrumented {
                assert_eq!(report.owner, Some(processor_owner), "actual parent owner");
                assert_eq!(
                    report.worker_process_id.map(std::num::NonZeroU32::get),
                    Some(pid)
                );
                assert_eq!(
                    report.sequence,
                    Some(u64::from(index) + 1),
                    "original work correlation"
                );
                assert!(
                    !report.counter_overflowed,
                    "diagnostic counters were representable"
                );
                if result.is_ok() {
                    assert_eq!(
                        report.output_admission_checkpoint_calls,
                        (width as usize * height as usize).div_ceil(65_536) + 1,
                        "same original bounded admission checkpoints"
                    );
                    assert!(
                        report.output_admission_checkpoint_elapsed
                            <= report
                                .elapsed(
                                    mondrian_app::openfx_host::OpenFxRenderPhase::OutputAdmission
                                )
                                .expect("completed output admission")
                    );
                    let child = report.child.expect("actual correlated child probe was requested");
                    assert_eq!(child.owner(), processor_owner);
                    assert_eq!(
                        child.sequence(),
                        u64::from(index) + 1,
                        "actual same wire Render correlation"
                    );
                    assert_eq!(
                        child.invocation(),
                        u64::from(index) + 1,
                        "actual native Render count on this no-idle-action carrier"
                    );
                    assert!(
                        !child.counter_overflowed(),
                        "fixed child counters were representable"
                    );
                    assert_eq!(
                        child.output_clear_bytes(),
                        Some((pixels * 16) as u64),
                        "actual complete Output initialization"
                    );
                    assert_eq!(child.elapsed_ns(mondrian_app::openfx_host::OpenFxChildRenderPhase::NativeCreate).is_some(),index==0,
                    "cold creation is observed only on actual first invocation; warmup excludes it from retained samples");
                    for phase in mondrian_app::openfx_host::OpenFxChildRenderPhase::ALL {
                        if phase != mondrian_app::openfx_host::OpenFxChildRenderPhase::NativeCreate
                        {
                            assert!(
                                child.elapsed_ns(phase).is_some(),
                                "completed child span {phase:?}"
                            );
                        }
                    }
                    for phase in mondrian_app::openfx_host::OpenFxNativeRenderPhase::ALL {
                        assert!(
                            child.native_elapsed_ns(phase).is_some(),
                            "completed native span {phase:?}"
                        );
                    }
                }
            }
            match result {
                Ok(output) => {
                    if session.worker_process_id() != pid {
                        errors.push("steady execution respawned a worker".into());
                        break;
                    }
                    let correct = output.pixels().len() == input.pixels().len()
                        && output.pixels().iter().zip(input.pixels()).all(|(actual, source)| {
                            (0..4).all(|channel| {
                                actual[channel].is_finite()
                                    && (actual[channel] - source[channel] * 0.5).abs() < 1e-6
                            })
                        });
                    if !correct {
                        errors.push(format!("independent gain oracle failed at frame {index}"));
                        break;
                    }
                }
                Err(error) => {
                    errors.push(error.to_string());
                    break;
                }
            }
        }
        let shutdown = Instant::now();
        let shutdown_result = session.shutdown();
        let shutdown_ns = u64::try_from(shutdown.elapsed().as_nanos()).expect("shutdown duration");
        if let Err(error) = shutdown_result {
            errors.push(error.to_string());
        }
        let mut sorted = durations.clone();
        sorted.sort_unstable();
        let percentile = |percent: usize| -> Option<u64> {
            sorted.get((sorted.len() * percent).div_ceil(100).saturating_sub(1)).copied()
        };
        let missed_frames = durations.iter().filter(|&&nanos| nanos > frame_budget_ns).count();
        let missed_stage_budget =
            durations.iter().filter(|&&nanos| nanos > stage_budget_ns).count();
        let worker_hash = format!(
            "{:x}",
            sha2::Sha256::digest(
                std::fs::read(product_executable()).expect("actual product worker identity")
            )
        );
        // All storage conversion, JSON and summaries run AFTER the timed renders.
        // Sleep durations are nested in await, not additive nonoverlapping phases.
        let phase_samples = phases
            .iter()
            .map(|report| {
                let nonoverlapping = mondrian_app::openfx_host::OpenFxRenderPhase::ALL
                    .into_iter()
                    .map(|phase| {
                        (
                            phase.name(),
                            report.elapsed(phase).map(|d| {
                                u64::try_from(d.as_nanos()).expect("bounded measured phase")
                            }),
                        )
                    })
                    .collect::<BTreeMap<_, _>>();
                let sum = nonoverlapping.values().flatten().copied().sum::<u64>();
                let whole = report
                    .owned_invocation_elapsed
                    .map(|d| u64::try_from(d.as_nanos()).expect("bounded method duration"));
                let child=report.child;
                let child_spans=child.map(|child|mondrian_app::openfx_host::OpenFxChildRenderPhase::ALL.into_iter()
                    .map(|phase|(phase.name(),child.elapsed_ns(phase))).collect::<BTreeMap<_,_>>());
                let native_spans=child.map(|child|mondrian_app::openfx_host::OpenFxNativeRenderPhase::ALL.into_iter()
                    .map(|phase|(phase.name(),child.native_elapsed_ns(phase))).collect::<BTreeMap<_,_>>());
                let child_sum=child_spans.as_ref().map(|spans|spans.values().flatten().copied().sum::<u64>());
                let native_sum=native_spans.as_ref().map(|spans|spans.values().flatten().copied().sum::<u64>());
                serde_json::json!({
                    "actual_report": report, "nonoverlapping_phase_ns": nonoverlapping,
                    "child_nonoverlapping_phase_ns":child_spans,"native_nonoverlapping_phase_ns":native_spans,
                    "child_dispatch_elapsed_ns":child.and_then(|v|v.dispatch_elapsed_ns()),
                    "child_unassigned_dispatch_ns":child.and_then(|v|v.dispatch_elapsed_ns()).zip(child_sum).map(|(v,sum)|v.checked_sub(sum).expect("disjoint child spans")),
                    "native_body_elapsed_ns":child.and_then(|v|v.native_total_ns()),
                    "native_unassigned_body_ns":child.and_then(|v|v.native_total_ns()).zip(native_sum).map(|(v,sum)|v.checked_sub(sum).expect("disjoint native spans")),
                    "output_clear_nested_ns":child.and_then(|v|v.output_clear_ns()),
                    "output_admission_checkpoint_nested_ns":u64::try_from(report.output_admission_checkpoint_elapsed.as_nanos())
                        .expect("bounded checkpoint duration"),
                    "nesting":"native body/clear are inside child NativeDispatch, which overlaps parent Await; do not add levels or subtract across process start coordinates",
                    "unmeasured_completion_frontier":"Shared completion/serde encoding/socket full commit/dispatch ack are not retrospectively measured by this packet",
                    "owned_invocation_ns": whole,
                    "unassigned_invocation_ns": whole.map(|whole| whole.checked_sub(sum)
                        .expect("parent phase spans must not overlap")),
                    "idle_sleep_requested_ns": u64::try_from(report.idle_sleep_requested.as_nanos())
                        .expect("bounded requested sleep"),
                    "idle_sleep_elapsed_ns": u64::try_from(report.idle_sleep_elapsed.as_nanos())
                        .expect("bounded observed sleep"),
                })
            })
            .collect::<Vec<_>>();
        let receipt = serde_json::json!({ "gate": "openfx_public_parent_child_attribution_v2",
        "acceptance": "diagnostic only; no performance PASS verdict",
        "instrumented": instrumented,
        "phase_samples": phase_samples,
        "fixed_caller_report_bytes": std::mem::size_of::<mondrian_app::openfx_host::OpenFxRenderDiagnostic>(),
        "attribution_limit": "await includes child dispatch, both pumps and OS scheduling; PluginRender includes SDK callbacks, legal spawned computation/join and nested Output clear, not pure DSP",
        "context": context, "context_sha256": hash(&machine_path),
        "fixture_sha256": inspection.binary_sha256,
        "worker_sha256": worker_hash, "width": width, "height": height,
        "fps_num": rate_num, "fps_den": rate_den, "stage_budget_percent": stage_percent,
        "frame_budget_ns": frame_budget_ns, "stage_budget_ns": stage_budget_ns,
        "warmup_excluded": warmup, "samples_requested": count, "samples": durations.len(),
        "setup_ns": setup_ns, "shutdown_ns": shutdown_ns, "p50_ns": percentile(50),
        "p95_ns": percentile(95), "p99_ns": percentile(99), "max_ns": sorted.last(),
        "frame_budget_misses": missed_frames, "stage_budget_misses": missed_stage_budget,
        "render_ns": durations, "errors": errors,
        "oracle": "independent asymmetric HDR/negative/alpha gain on every output; timed render excludes oracle",
        "timed_boundary": "whole public session call including validation, hashes, copies, IPC, native work, output allocation and enabled diagnostic overhead" });
        let output_path = PathBuf::from(
            std::env::var_os("MONDRIAN_PERF_OUTPUT").expect("qualification JSON output path"),
        );
        std::fs::write(
            output_path,
            serde_json::to_vec_pretty(&receipt).expect("serialize receipt"),
        )
        .expect("write qualification receipt before budget verdict");
        assert!(errors.is_empty(), "native work failed: {errors:?}");
        assert_eq!(sorted.len(), count as usize);
        // The original independent output oracle is still mandatory. Report the
        // original unchanged budget misses; attribution does not relax or pass it.
        if instrumented {
            assert_eq!(phases.len(), count as usize);
        }
    }
}
