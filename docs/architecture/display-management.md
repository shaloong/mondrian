# Display Management — Architecture

Display management bridges OS monitor/surface capabilities, user policy, OCIO
display/view resolution, and the GPU preview/export output boundary. Both
preview and export consume the same **Display Output Contract** — only
scheduling differs, not color interpretation.

## Design Principles

1. **One pipeline, OCIO is the engine.** Media Metadata / User Override →
   Mondrian Color Interpretation → OCIO ColorSpace / DisplayView resolution →
   Render Graph → Preview / Export / Cache.

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

## Display Output Contract v2

### DisplayOutputSnapshot

The canonical type that both preview and export consume. Defined in
`mondrian-core::display_contract`.

```
DisplayOutputSnapshot {
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

### MonitorProfileStatus

Status of the OS monitor ICC profile. The fail-closed chain:

| User config | OS capability | Result |
|---|---|---|
| No ICC | * | `NotRequested` |
| ColorSpace(x) | * | `ManagedColorSpace` |
| IccProfile | OS discovery unsupported | `IccProfileUnsupported` |
| IccProfile | OS discovery works, parse fails | `IccProfileReadError` |
| IccProfile | OS discovery works, parse OK, no OCIO match | `IccProfileUnmapped` + fail-closed blocker |
| IccProfile | OS discovery works, parse OK, OCIO match | `ManagedColorSpace` |

### HdrStatus

Full-chain HDR diagnosis:

| Requested | Surface | Monitor | Result |
|---|---|---|---|
| SDR | * | * | `NotRequested` |
| HDR | supports HDR | known + supports | `RequestedSupported` |
| HDR | supports HDR | known + no support | `RequestedMonitorUnsupported` |
| HDR | supports HDR | unknown | `RequestedMonitorUnknown` |
| HDR | no HDR support | * | `RequestedSurfaceUnsupported` |

### DisplayOutputBlocker

Structured failure categories:

| Blocker | Area | Action |
|---|---|---|
| `SurfaceContractMismatch` | DisplayContract | `configure_display_contract` |
| `UnsupportedDisplayColorSpace` | DisplayContract | `configure_display_contract` |
| `UnsupportedHdrSwapchainOrEdr` | DisplayContract | `configure_display_contract` |
| `MonitorIccProfileUnsupported` | MonitorProfile | `implement_os_icc_profile_probe` |
| `MonitorIccProfileInvalid` | MonitorProfile | `map_icc_profile_to_ocio_display` |
| `MonitorIccProfileUnmapped` | MonitorProfile | `map_icc_profile_to_ocio_display` |
| `MonitorHdrCapabilityUnknown` | MonitorHdr | `inspect_monitor_hdr_capability` |
| `MonitorHdrCapabilityUnsupported` | MonitorHdr | `configure_display_contract` |
| `DisplayMovedContractStale` | DisplayLifecycle | `move_window_display_contract_refresh` |
| `OcioDisplayViewMissing` | OcioConfig | `prepare_ocio_gpu_resources` |
| `OcioConfigUnavailable` | OcioConfig | `prepare_ocio_gpu_resources` |

## Platform Display Probing

### PlatformDisplayProbe trait

Defined in `mondrian-core::display_probe`. Abstracts OS-level display queries:

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

### Action Codes

| Code | Meaning |
|---|---|
| `configure_display_contract` | Reconfigure display output contract |
| `implement_os_icc_profile_probe` | Implement OS ICC profile discovery |
| `map_icc_profile_to_ocio_display` | Map ICC profile to OCIO display/view |
| `enable_hdr_surface` | Enable HDR surface format and swapchain |
| `move_window_display_contract_refresh` | Refresh contract after window move |
| `inspect_monitor_hdr_capability` | Query OS for monitor HDR capability |

## Current State (Alpha)

### Supported
- SDR preview/export with managed color spaces (Rec.709, sRGB, DCI-P3 via OCIO)
- Display contract refresh on resize, scale factor change, window move
- Surface format selection with color space capability matching
- Structured blocker taxonomy with health report integration
- Fake display probe for testable display contract logic
- Cache invalidation on contract change

### Not Implemented (Fail-Closed)
- **OS ICC profile discovery** — All platforms report `IccProfileUnsupported`.
  ICC profiles must be provided explicitly via project settings.
- **OS HDR display metadata** — `display_hdr_info` from wgpu is available but
  real monitor HDR capability cannot be confirmed on most platforms.
- **OS EDR information** — Not available on any platform.
- **ICC-to-OCIO mapping** — Even if ICC bytes are provided, mapping to OCIO
  display/view pairs is not implemented.

### Explicitly Unsupported
- `DataTexture` / `NonColorData` in display colorspace selection
- Parallel color management engine outside OCIO
- Silent Rec.709 fallback for unknown display states
