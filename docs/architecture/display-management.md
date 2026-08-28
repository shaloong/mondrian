# Display Management — Architecture

Display management bridges OS monitor/surface capabilities, user policy, OCIO
display/view resolution, and the GPU Preview presentation boundary.
`DisplayOutputSnapshot` is machine-local Preview evidence; Export never
consumes it. Preview and Export share the Project/Sequence Program Output
interpretation and renderer color-boundary semantics, then diverge: Preview
adds monitor/surface adaptation, while Export consumes only the immutable
`ExportColorTarget` and delivery contract.

## Design Principles

1. **One Program Output, explicit presentation fork.** Media Metadata / User
   Override → Mondrian Color Interpretation → OCIO input/working/rendering
   transforms → Program Output. Preview then applies machine-local monitor and
   surface adaptation; Export applies its explicit delivery target. Neither
   path may reinterpret the other path's contract.

2. **Display management must be real, not plausible.** Unknown monitor / ICC /
   HDR state is never silently treated as Rec.709 / sRGB / Standard.

3. **ICC must be real or fail-closed.** If the OS ICC profile cannot be read,
   we do not fall back to Rec.709. We record an `IccProfileUnsupported` /
   `IccProfileUnmapped` status and emit a blocker.

4. **HDR/EDR must be realistic.** wgpu `SurfaceColorSpace` support does not
   equal real HDR display correctness.

5. **Multi-monitor switching must invalidate contracts.** Window move, monitor
   change, scale factor change, and surface reconfiguration all trigger a full
   contract re-resolve.

6. **Three authorities remain separate.** Sequence/Project owns Program Output;
   the machine-local policy owns monitor target and ICC calibration intent; the
   Window/platform Adapter owns actual surface and monitor capability evidence.
   No one authority may manufacture another.

## Machine-local Display Policy

`DisplayManagementPolicy` is a validated deep value Module with private fields:

- `MonitorOutputIntent`: follow Program Output, a managed display-referred
  target, or an exact engine-qualified OCIO display/view.
- `DisplayCalibrationPolicy`: disabled, OS-default ICC, or an explicit absolute
  ICC path.
- `IccRenderingIntent`: perceptual, relative colorimetric, saturation, or
  absolute colorimetric.
- `ViewerDisplayMode`: follow Program Output, SDR, PQ, or HLG.

Preview and Window both call the same `resolve_output_color_space` method. An
OCIO pair resolves only when `ColorEngine::output_display_view` maps it to one
of the six standardized monitor targets; invalid names, scene-linear/log
targets, relative ICC paths, and engine drift fail closed. Window snapshot
resolution receives the active Sequence Program Output, not the current
swapchain color space.

The policy is stored in user-level `AppUiPreferences` with a serde default for
pre-COL-009 files. `AppUiHost` installs it into `AppState` before constructing
Preview, and each production Preferences action atomically updates runtime and
disk state. It never enters `.mdp`.

## Preview Display Output Contract v2

### DisplayOutputSnapshot

The canonical machine-local snapshot consumed only by Preview presentation.
Defined in `mondrian-core::display_contract`.

```
DisplayOutputSnapshot {
    display_management_policy: DisplayManagementPolicy,
    display_id: DisplayId,             // monitor name, position, physical size
    platform: DisplayPlatform,         // Windows, macOS, Linux, Unknown
    scale_factor: ScaleFactorPpm,      // integer PPM for deterministic comparison
    surface_format: String,            // e.g. "Bgra8UnormSrgb"
    surface_color_space: String,       // e.g. "Srgb", "Bt2100Pq"
    surface_hdr_mode: String,          // "SdrOnly", "HdrPq", "HdrHlg"
    supported_surface_color_spaces: Vec<String>,
    requested_viewer_mode: String,     // user policy
    requested_output_color_space: String,
    resolved_output_color_space: String,
    ocio_display: Option<String>,
    ocio_view: Option<String>,
    monitor_profile_status: MonitorProfileStatus,
    hdr_status: HdrStatus,
    validation_status: DisplayValidationStatus,
    warnings: Vec<DisplayOutputWarning>,
    blockers: Vec<DisplayOutputBlocker>,
    refresh_reason: String,
}
```

The snapshot's `ocio_display`/`ocio_view` fields are resolved
surface-validation evidence, not the execution state stored in
`ProgramColorContext`. Production Preview first resolves the context's typed
`OutputTransformIntent` through the shared renderer Program Output boundary,
then applies this snapshot's monitor/surface contract. Export resolves the same
Program Output or an explicit `ExportColorTarget` without reading display ID,
ICC profile, desktop HDR state, swapchain format, or any other field in this
snapshot. This prevents machine-local display probing from becoming an Export
color-science authority.

### MonitorProfileStatus

Status of the OS monitor ICC profile. The fail-closed chain:

| User config | OS capability | Result |
|---|---|---|
| Calibration disabled, Program Output target | * | `NotRequested` |
| Calibration disabled, explicit managed/OCIO target | * | `ManagedColorSpace` |
| OS-default ICC | OS discovery unsupported | `IccProfileUnsupported` |
| Explicit/OS ICC | read or parse fails | `IccProfileReadError` |
| Explicit/OS ICC | device processor creation fails | `IccProfileUnmapped` + fail-closed blocker |
| Explicit/OS ICC | CPU LUT and full-fingerprint processor proof ready | `ManagedIccCalibration` |

Parsing, descriptive mapping, and device calibration are separate. A valid
generic RGB monitor profile is not implicitly Rec.709: the shared parser may
return an `Unmapped` descriptive identity while the ICC engine still builds a
device transform from the explicit presentation source. Media ingest continues
to require a mapped source identity; monitor calibration does not manufacture
one from profile names.

Mapping a profile name to sRGB/P3/Rec.709 is not monitor calibration and no
longer controls admission. The encoded source is the explicit OCIO
display/presentation boundary; any ICC payload that can produce a device
processor may become `ManagedIccCalibration`. Preview admission still rejects
an `OsIccProfile` status that merely claims `ManagedColorSpace`, so stale or
manually constructed snapshots cannot bypass processor proof.

Core now provides a renderer-neutral `DisplayCalibrationLut3d` contract for
the missing calibration processor. It parses the complete destination ICC
payload, fingerprints the payload for cache identity, samples a supported
standard encoded source into an RGBA32F 3D LUT, and exposes a CPU trilinear
reference. The default cube is 33^3; supported quality sizes are odd values
from 17 through 65. Camera-log spaces are rejected because they are acquisition
spaces, not monitor boundary encodings. The live resolver returns a serializable
snapshot plus a non-persistent CPU LUT. The snapshot carries the complete ICC
fingerprint, while the window owns the matching `Arc<DisplayCalibrationLut3d>`
and the renderer owns pipeline, GPU LUT, and per-frame output resources. Display
changes clear the GPU cache and preview resources. A mismatch at any layer fails
before command submission.

The live resolver applies the selected ICC rendering intent when constructing
the processor. A bounded eight-entry process cache keyed by source color space,
full profile fingerprint, and rendering intent avoids rebuilding the 33^3 LUT
on resize or equivalent display refresh. Preview precomputes the snapshot's
SHA-256 contract identity at the low-frequency install Seam and reuses the typed
identity per frame rather than serializing and hashing the snapshot repeatedly.

### HdrStatus

Full-chain HDR diagnosis:

| Requested | Surface | Monitor | Result |
|---|---|---|---|
| SDR | * | * | `NotRequested` |
| HDR | supports HDR | known + supports | `RequestedSupported` |
| HDR | supports HDR | known + no support | `RequestedMonitorUnsupported` |
| HDR | supports HDR | unknown | `RequestedMonitorUnknown` |
| HDR | no HDR support | * | `RequestedSurfaceUnsupported` |

Monitor evidence is explicitly three-state. `Ready` requires both physical
support and a currently HDR/EDR-capable desktop output. `Unsupported` means a
native API explicitly reports unsupported or currently disabled. `Unknown`
includes hardware-only evidence (for example DRM EDID with no compositor mode
proof). Hardware capability alone never enables HDR presentation.

### DisplayOutputBlocker

Structured failure categories:

| Blocker | Area | Action |
|---|---|---|
| `SurfaceContractMismatch` | DisplayContract | `configure_display_contract` |
| `UnsupportedDisplayColorSpace` | DisplayContract | `configure_display_contract` |
| `UnsupportedHdrSwapchainOrEdr` | DisplayContract | `configure_display_contract` |
| `MonitorIccProfileUnsupported` | MonitorProfile | `configure_monitor_icc_profile` |
| `MonitorIccProfileInvalid` | MonitorProfile | `map_icc_profile_to_ocio_display` |
| `MonitorIccProfileUnmapped` | MonitorProfile | `map_icc_profile_to_ocio_display` |
| `MonitorHdrCapabilityUnknown` | MonitorHdr | `inspect_monitor_hdr_capability` |
| `MonitorHdrCapabilityUnsupported` | MonitorHdr | `configure_display_contract` |
| `DisplayMovedContractStale` | DisplayLifecycle | `move_window_display_contract_refresh` |
| `OcioDisplayViewMissing` | OcioConfig | `prepare_ocio_gpu_resources` |
| `OcioConfigUnavailable` | OcioConfig | `prepare_ocio_gpu_resources` |

## Platform Display Probing

### PlatformDisplayProbe test contract

Defined in `mondrian-core::display_probe` and retained for deterministic core
fixtures. Production does not claim it as an OS Adapter: the Window Adapter
projects real winit/wgpu surface facts and `mondrian-platform` ICC/HDR probe
results into the live resolver.

```rust
pub trait PlatformDisplayProbe {
    fn current_display_snapshot(&self, policy, output_color_space) -> DisplayOutputSnapshot;
    fn supports_os_icc_discovery(&self) -> bool;
    fn supports_os_hdr_metadata(&self) -> bool;
}
```

### FakeDisplayProbe (for tests)

Returns pre-configured snapshots:

- `sdr_pass()` — clean SDR path, no blockers
- `icc_profile_unsupported()` — ICC requested, OS discovery unavailable
- `icc_profile_invalid(path)` — ICC found but unreadable
- `icc_profile_unmapped(path)` — ICC parsed, no OCIO match
- `hdr_monitor_unknown()` — HDR requested, monitor capability unknown
- `hdr_surface_unsupported()` — HDR requested, surface doesn't support it
- `monitor_switched(name, position)` — window moved to different monitor

## Preview GPU Output Blocker Taxonomy (Display Management)

Extended `PreviewGpuOutputBlocker` variants for display management:

| Variant | Description |
|---|---|
| `MonitorIccProfileUnsupported` | OS ICC discovery not implemented |
| `MonitorIccProfileInvalid` | ICC profile found but invalid/unreadable |
| `MonitorHdrCapabilityUnknown` | Monitor HDR capability cannot be confirmed |
| `MonitorHdrCapabilityUnsupported` | Monitor explicitly doesn't support HDR |
| `DisplayContractStale` | Contract invalid after window move/monitor change |

## Cache Invalidation

The display contract generation (deterministic hash) changes when:

- Display identity changes (name, position, physical size)
- Scale factor changes
- Surface format or color space changes
- Viewer mode changes
- OCIO display/view changes
- Monitor profile status changes
- HDR status changes

On any contract change, the preview external texture is invalidated:
- Previous texture key is unregistered
- Frame resources are cleared
- External viewer frame is cleared
- Host is marked dirty for redraw

## Health Report Integration

The display contract produces structured diagnostics for:

- **Preview GPU output blocker breakdown** — per-frame blocker counts
- **Viewer GPU output budget** — JSONL stream evaluation
- **Display contract refresh events** — resize, scale factor, window move
- **Display issue summaries** — correlation of blockers with refresh events
- **Action codes** — machine-readable follow-up actions

The Window JSONL evidence additionally records `ui_surface_carrier_active`,
carrier target rebuilds, presented external-texture batches, and separate
surface-code versus ICC/device-code batches. These are execution facts, not
inferences from the selected policy.

## Physical Viewer Display Qualification

`tests/validation/viewer-display-qualification.json` is the fail-closed COL-010
HITL profile. It requires exact Display P3, HDR-PQ, and managed-ICC scenarios on
real displays. `scripts/validation/validate-viewer-display-qualification.ps1`
consumes a product `MONDRIAN_VIEWER_GPU_OUTPUT_OUTPUT` JSONL file for each
scenario plus one operator observation. A pass requires a valid Display Output
Contract, Ready Viewer health, a registered and actually submitted external
texture, zero readback stages, the exact native surface color space, the exact
code-value transfer path, carrier allocation followed by steady reuse for
P3/HDR, ICC processor proof for the ICC scenario, and a positive visual
observation bound to the physical display identity. Missing capability or a
skipped scenario is a failure.

Surface format/color-space enumeration is not HDR compositor proof. The Window
queries the platform Advanced Color/EDR state before selecting PQ/HLG. If the
OS compositor or monitor is disabled, unsupported, or unknown, the application
retains the usable SDR sRGB UI carrier and the Viewer remains blocked by the
typed display snapshot. This prevents DXGI format enumeration from switching
an SDR desktop into a misleading PQ swapchain.

### Action Codes

| Code | Meaning |
|---|---|
| `configure_display_contract` | Reconfigure display output contract |
| `configure_monitor_icc_profile` | Configure a resolvable monitor ICC profile source |
| `map_icc_profile_to_ocio_display` | Map ICC profile to OCIO display/view |
| `enable_hdr_surface` | Enable HDR surface format and swapchain |
| `move_window_display_contract_refresh` | Refresh contract after window move |
| `inspect_monitor_hdr_capability` | Query OS for monitor HDR capability |

## Current State (Alpha)

### Supported
- Production Display Preferences UI with engine-qualified OCIO display/view,
  monitor target, HDR policy, ICC source, and ICC rendering intent
- User-level save, pre-Preview startup restore, complete snapshot diagnostics,
  and display-dependent invalidation on policy or Program Output changes
- SDR Program Output in Rec.709, sRGB, and Display P3 via OCIO, with
  Preview-only monitor/surface adaptation and independently contracted Export
  delivery
- Display contract refresh on resize, scale factor change, window move
- Surface format selection with color space capability matching
- Direct sRGB, Display P3, Rec.2100 PQ, and Rec.2100 HLG UI/Viewer native
  presentation carriers with linear-domain composition and cached resources
- Managed ICC device-code round-trip on the direct sRGB carrier
- Structured blocker taxonomy with health report integration
- Fake display probe for testable display contract logic
- Windows OS default ICC profile discovery via `mondrian-platform`
  (`EnumDisplayMonitors` + WCS default profile lookup)
- macOS active-display ICC payload discovery via CoreGraphics and real current,
  potential, and reference EDR headroom via `NSScreen`
- Linux Wayland `color-management-v1` output image descriptions, including
  direct ICC file-descriptor payloads, active transfer function, and luminance
  evidence; X11 `_ICC_PROFILE[_n]` discovery remains the X11-native path
- Linux DRM/EDID CTA-861 HDR Static Metadata fallback for physical PQ/HLG and
  luminance capability; this path deliberately leaves active compositor HDR
  state unknown
- Platform-neutral probe evidence records native backend, path or in-memory ICC
  payload, support/enabled state, transfer functions, reference white,
  luminance, bit depth, encoding, and EDR headroom
- Preview scheduling consumes the resolved Display Output Contract for
  ICC-backed monitor color-space resolution and invalidates cached preview
  frames when the contract changes
- Cache invalidation on contract change

### Not Implemented (Fail-Closed)
- **Wayland parametric-profile synthesis** — when an active Wayland output
  description provides primaries/transfer/luminance but no ICC file, Mondrian
  records the HDR evidence but does not synthesize an ICC payload. An explicit
  monitor ICC request therefore fails closed instead of inventing calibration.
- **Linux compositor HDR proof without color-management-v1** — DRM/EDID proves
  physical capability only. Compositors without the protocol remain
  `MonitorHdrCapabilityUnknown` until the active output encoding can be proven.
- **Unsupported ICC device classes/transforms** — monitor RGB profiles that the
  calibration processor supports can produce a device LUT without descriptive
  color-space name mapping. Unsupported device classes or transform structures
  still emit `IccProfileUnmapped`; they never fall back to a named gamut.

### Explicitly Unsupported
- `DataTexture` / `NonColorData` in display colorspace selection
- Parallel color management engine outside OCIO
- Silent Rec.709 fallback for unknown display states
- Non-default monitor/HDR/ICC policy through the bounded CPU Viewer fallback:
  the current CPU raster uses the fixed sRGB UI atlas and still fails closed
  with `cpu_viewer_display_policy_carrier`; qualified wide-gamut/HDR/ICC
  presentation is the GPU external-texture path.
