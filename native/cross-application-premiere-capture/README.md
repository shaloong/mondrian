# Premiere 24.x native capture adapter

This CEP 11 panel calls the public Premiere ExtendScript interface used by
[Adobe's PProPanel](https://github.com/Adobe-CEP/Samples/tree/master/PProPanel).
It is deliberately separate from the UXP adapter surface introduced in newer
Premiere versions. There is no QE, undocumented headless process, remote code,
automatic account login, or change to Premiere's extension security settings.

Package/sign this folder through your normal approved CEP deployment process,
then open **Window → Extensions → Mondrian Color Capture** in Premiere 24.x.
The repository does not install unsigned code or enable PlayerDebugMode.
Open the frozen `.prproj` and select a request JSON shaped as follows:

```json
{
  "schema_version": 1,
  "run_id": "same-run-as-blender-and-mondrian",
  "expected_version": "24.0.0",
  "expected_build": "58",
  "project": {"path": "native.prproj", "sha256": "<64 lowercase hex>"},
  "dependencies": [{"path": "stimulus.exr", "sha256": "<64 lowercase hex>"}],
  "output_directory": "fresh-premiere-capture",
  "cases": [{
    "case_id": "sdr-srgb-alpha-rgba8",
    "sequence_id": "<exact native sequence ID>",
    "frame_index": 17,
    "frame_duration_ticks": "10594584000",
    "width": 64,
    "height": 64,
    "working_color_space": "<exact getSettings readback>",
    "preset": {"path": "rgba8.epr", "sha256": "<64 lowercase hex>"},
    "suffix": ".png"
  }]
}
```

The sample build and sequence settings are examples; the panel compares every
value to native readback and refuses mismatches. It uses exact 254016000000
ticks/second frame coordinates, checks range readback, exports through the
frozen EPR, restores the prior in/out range, and checks that restoration. It
hashes the request, project, adapter, referenced files, presets, and native
outputs. The output directory must be fresh. Failure reports retain native
return values, settings, prior completed files and restoration failures.

A host timeout leaves the panel blocked and reports `native_host_settled=false`;
it never kills an existing user's Premiere process or reports native closure
without observing it. Final PNG/EXR/MOV decoding must independently prove exactly
one frame with the required channels, alpha, raster, tags, and pixel values.
The capture result is always `captured` or `failed`, never `qualified`.

The public 24.x settings API does not expose every color/tone-map/alpha control.
An actual per-run UI observation and attestation, plus a frozen native project
and EPR, remain required by the qualification importer. Hashing files before
and after capture is not a Windows share-mode lease; qualification acquisition
must additionally hold the approved input leases for the host operation. This
panel has been syntax-checked; its native execution still requires signed CEP
deployment and a frozen export preset. Desktop access was granted on 2026-09-06,
and a separate native UI run captured PNG frame 17 with Premiere 24.0.0.58.
That run does not attest execution of this panel. Its independently decoded
pixels preserve frame identity and Alpha but do not satisfy the requested
Rec.2020-to-sRGB transform. See the repository's cross-application capture
runbook for the actual source-format limitation and retained diagnostic evidence.
