# Shared ANC campaign evidence

The AS-11 extension uses one approved `FrozenAncillaryProgram` file for physical
Reference Output and every repeated Export. It does not resample captions or
create a second interpretation of source time. The optional machine-plan
`ancillary_program` binding contains an absolute `path` and the SHA-256 of the
original JSON bytes. The prepared owner retains the admitted file and shares
the same immutable program with each phase.

The alternative policy is
`tests/validation/commercial-endurance-qualification-as11-5994.json`, with the
three `*-as11-5994-v1.json` workloads in `tests/validation/endurance-workloads/`.
It explicitly uses 60000/1001 for physical playback, preserving the original
60/1 policy. Its 24-hour minimum is 5,178,821 complete frames, calculated with
integer rational floor, not a rounded 59.94 or 60 fps approximation. These are
proposed policy inputs to the existing external-approval process; adding the
files does not approve or execute a physical campaign.

For a declared program, the phase owner, producer raw evidence, producer report
and manifest producer retain the same three fields:

- `ancillary_program_sha256` identifies the original approved file.
- `ancillary_export_artifacts` binds each verified event's `artifact_id` to the
  actual `verification_path` and whole-file `verification_sha256`.
- `wire_journals` binds every retained, cleanly closed physical session's actual
  journal `path` and whole-file `sha256`.

The Export sidecar remains `<artifact>.independent-verification.json`. Its
request identifies the shared program, and `ancillary_mxf_rescan` retains that
same digest plus the complete `frames_verified` count. The event's existing
`validation_report_sha256` is the independent decoder report digest, **not**
the digest of the whole sidecar. Both bindings must agree with the recorded
event and the current final MXF bytes. Cancellation and retry reuse the frozen
program; a successful retry does not overwrite an earlier attempt's evidence.

AS-11 phases additionally require `verifier_tools.bmx` in the approved machine
plan: exact `raw2bmx` and `mxf2raw` file bindings, their bounded `-v` output
digests, and an explicit `runtime_files` DLL closure. Missing prerequisites are
admitted NotRun. Each tool invocation uses the existing native process
supervisor and child ledger; a short version-probe deadline cannot shorten or
renew the phase owner's original horizon. The phase terminal's `bmx_runtime`
receipt records actual command release, namespace verification/restoration/
removal, file-lease release and deadline outcome. An AS-11 phase cannot qualify
with a missing or unsuccessful consuming receipt.

The independent PowerShell verifier checks these cross-report bindings, reads
each sidecar, hashes each final artifact and closed wire journal, checks the
bounded first wire identity against the approved phase and devices, and includes
all files in the before/after replay snapshot. The actual complete packet-word
comparison remains the native independent receiver owner's operation, preserved
in the journal; reading its identity alone is not a fresh wire qualification.
Duplicate JSON keys, including case collisions, are rejected before evidence is
interpreted. Shared-program JSON is limited to 8 MiB; sidecars to 2 MiB; the wire
identity to 65,536 characters. Whole journals remain subject to their separately
approved machine-plan byte bounds and are never loaded as one string.

Run the structural verifier regressions with a new output directory:

```powershell
pwsh -NoProfile -File scripts/validation/test-endurance-ancillary-evidence.ps1 `
  -OutputDirectory .scratch/ancillary-verifier-review-01
pwsh -NoProfile -File scripts/validation/test-phase-owner-closure.ps1
```

The separate ignored Media integration tests in `tests/approved_bmx.rs` exercise
actual official tools. Set `MONDRIAN_BMX_TOOL_DIR` to the installed binary
directory and `MONDRIAN_BMX_NATIVE_EVIDENCE_OUTPUT` to a new JSON file, then run
the freshly built `approved_bmx` test harness with `--ignored --test-threads=1`.
The tests compare original/staged version bytes, reject expired probes and
canceled execution, and inspect consuming closure. The intentional live-borrow
negative case creates no private namespace and retains only read-only fixture
leases until that test process exits.

These tests include deliberately synthetic sidecars and journals to challenge
the verifier. Their reports explicitly set `qualified: false`; they establish
structural rejection behavior only. Real SDI/Genlock, approved regulatory PSE,
HDR/P3, the physical 72-hour run and each OS cell still require their actual
hardware/provider evidence.
