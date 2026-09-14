//! Decode residency and hardware-frame import diagnostics.
//!
//! Preview frame scheduling and access-mode FFmpeg session ownership live in
//! `preview.rs` plus the app preview worker. This module intentionally does not
//! expose a second preview decode pool.

use std::collections::HashMap;
use std::ffi::CString;
use std::ptr::{self, NonNull};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ffmpeg_next as ffmpeg;
pub use mondrian_core::DecodedVideoRange;

#[cfg(target_os = "windows")]
use windows::core::Interface;
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Direct3D12::ID3D12Device;

/// GPU hardware acceleration backend family.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, Default,
)]
pub enum HwAccelBackend {
    /// CPU software decode.
    #[default]
    None,
    /// NVIDIA NVDEC.
    Cuda,
    /// Windows Direct3D 12 Video Acceleration.
    D3D12VA,
    /// Windows DirectX 11 Video Acceleration.
    D3D11VA,
    /// Legacy Windows DirectX Video Acceleration 2.
    Dxva2,
    /// macOS/iOS VideoToolbox.
    VideoToolbox,
    /// Linux VA-API.
    Vaapi,
    /// Legacy Linux VDPAU.
    Vdpau,
}

/// Backend-specific device selection supplied by the renderer admission path.
///
/// The selector is an explicit cross-layer contract, not a claim that the
/// selected decoder can be imported. Renderer admission still validates the
/// resulting native frame's physical adapter identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum HwAccelDeviceSelector {
    /// DRM render-node minor number bound to the active Linux Vulkan device.
    ///
    /// The renderer verifies the node's character-device identity before admission.
    VaapiDrmRenderNode(u32),
    /// CUDA device ordinal passed only to FFmpeg's CUDA device creator.
    ///
    /// This selects decoding but does not prove Vulkan interoperability.
    CudaDeviceOrdinal(u16),
    /// DXGI adapter index passed only to FFmpeg's D3D12VA device creator.
    D3D12VaAdapterIndex(u32),
    /// DXGI adapter index passed only to FFmpeg's D3D11VA device creator.
    D3D11VaAdapterIndex(u32),
}

impl HwAccelDeviceSelector {
    fn device_name_for(self, backend: HwAccelBackend) -> Option<CString> {
        match (self, backend) {
            (Self::VaapiDrmRenderNode(minor), HwAccelBackend::Vaapi) => {
                CString::new(format!("/dev/dri/renderD{minor}")).ok()
            }
            (Self::CudaDeviceOrdinal(index), HwAccelBackend::Cuda) => {
                CString::new(index.to_string()).ok()
            }
            (Self::D3D12VaAdapterIndex(index), HwAccelBackend::D3D12VA)
            | (Self::D3D11VaAdapterIndex(index), HwAccelBackend::D3D11VA) => {
                CString::new(index.to_string()).ok()
            }
            _ => None,
        }
    }

    pub(crate) fn selects_backend(self, backend: HwAccelBackend) -> bool {
        matches!(
            (self, backend),
            (Self::VaapiDrmRenderNode(_), HwAccelBackend::Vaapi)
                | (Self::CudaDeviceOrdinal(_), HwAccelBackend::Cuda)
                | (Self::D3D12VaAdapterIndex(_), HwAccelBackend::D3D12VA)
                | (Self::D3D11VaAdapterIndex(_), HwAccelBackend::D3D11VA)
        )
    }
}

type HwAccelDeviceProbeKey = (HwAccelBackend, Option<HwAccelDeviceSelector>);
const HW_DEVICE_FAILURE_BACKOFF_BASE: Duration = Duration::from_millis(250);
const HW_DEVICE_FAILURE_BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Immutable renderer-qualified FFmpeg hardware-device root.
///
/// Platform Adapters construct this value from the exact native device owned
/// by the active renderer. Decoder worker families install it into their
/// existing device-context pool; codec Sessions receive ordinary FFmpeg
/// `AVBufferRef` leases and never acquire a renderer or OS graphics handle.
#[derive(Clone)]
pub struct RendererHwAccelDeviceContext {
    owner: Arc<SharedHwAccelDeviceContext>,
}

impl std::fmt::Debug for RendererHwAccelDeviceContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RendererHwAccelDeviceContext")
            .field("backend", &self.owner.backend)
            .finish_non_exhaustive()
    }
}

impl RendererHwAccelDeviceContext {
    /// Hardware backend represented by this exact renderer device root.
    pub fn backend(&self) -> HwAccelBackend {
        self.owner.backend
    }

    /// Retain the exact FFmpeg device root for another media-owned hardware Session.
    ///
    /// The returned reference is independently owned and must be released with
    /// `av_buffer_unref`. It remains crate-private so platform clients cannot
    /// manufacture an unqualified FFmpeg device interpretation.
    #[cfg(any(target_os = "linux", all(target_os = "windows", mondrian_ffmpeg_7_1)))]
    pub(crate) fn retain_ffmpeg_device_ref(
        &self,
    ) -> Result<NonNull<ffmpeg::ffi::AVBufferRef>, RendererHwAccelDeviceContextCreateError> {
        let retained = unsafe { ffmpeg::ffi::av_buffer_ref(self.owner.ptr.as_ptr()) };
        NonNull::new(retained).ok_or(RendererHwAccelDeviceContextCreateError::AllocationFailed)
    }

    /// Create an FFmpeg CUDA device root for a renderer-qualified device ordinal.
    ///
    /// The renderer must first match its Vulkan physical-device UUID to this
    /// CUDA ordinal. Media deliberately accepts no implicit default device, so
    /// multi-GPU systems cannot silently encode on a different adapter.
    #[cfg(target_os = "linux")]
    pub fn from_cuda_device_ordinal(
        ordinal: u16,
    ) -> Result<Self, RendererHwAccelDeviceContextCreateError> {
        ffmpeg::init().map_err(|error| {
            RendererHwAccelDeviceContextCreateError::InitializationFailed {
                backend: HwAccelBackend::Cuda,
                reason: error.to_string(),
            }
        })?;
        let device_name = CString::new(ordinal.to_string())
            .map_err(|_| RendererHwAccelDeviceContextCreateError::InvalidDeviceSelector)?;
        let mut device_context = ptr::null_mut();
        let result = unsafe {
            ffmpeg::ffi::av_hwdevice_ctx_create(
                &mut device_context,
                ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
                device_name.as_ptr(),
                ptr::null_mut(),
                0,
            )
        };
        if result < 0 {
            return Err(
                RendererHwAccelDeviceContextCreateError::InitializationFailed {
                    backend: HwAccelBackend::Cuda,
                    reason: ffmpeg::Error::from(result).to_string(),
                },
            );
        }
        let device_context = NonNull::new(device_context)
            .ok_or(RendererHwAccelDeviceContextCreateError::AllocationFailed)?;
        Ok(Self {
            owner: Arc::new(SharedHwAccelDeviceContext {
                backend: HwAccelBackend::Cuda,
                ptr: device_context,
            }),
        })
    }

    /// Create an FFmpeg D3D12VA device root over the exact renderer device.
    ///
    /// FFmpeg takes ownership of one COM reference during initialization. The
    /// returned value is therefore safe to move to decoder workers after the
    /// temporary renderer HAL borrow has ended.
    #[cfg(target_os = "windows")]
    pub fn from_d3d12_device(
        device: ID3D12Device,
    ) -> Result<Self, RendererHwAccelDeviceContextCreateError> {
        let _ = ffmpeg::init();
        let device_context = unsafe {
            ffmpeg::ffi::av_hwdevice_ctx_alloc(
                ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D12VA,
            )
        };
        let Some(device_context) = NonNull::new(device_context) else {
            return Err(RendererHwAccelDeviceContextCreateError::AllocationFailed);
        };
        let result = initialize_ffmpeg_d3d12_device_context(device_context, device);
        if let Err(error) = result {
            let mut raw = device_context.as_ptr();
            unsafe { ffmpeg::ffi::av_buffer_unref(&mut raw) };
            return Err(error);
        }
        Ok(Self {
            owner: Arc::new(SharedHwAccelDeviceContext {
                backend: HwAccelBackend::D3D12VA,
                ptr: device_context,
            }),
        })
    }
}

/// Failure to bind an FFmpeg hardware-device root to the renderer device.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum RendererHwAccelDeviceContextCreateError {
    /// FFmpeg could not allocate the requested device context.
    #[error("FFmpeg could not allocate a renderer-qualified hardware device context")]
    AllocationFailed,
    /// A renderer-selected native device identifier could not be represented.
    #[error("renderer-qualified hardware device selector is invalid")]
    InvalidDeviceSelector,
    /// FFmpeg returned an incomplete generic device-context allocation.
    #[error("FFmpeg returned an incomplete D3D12VA device-context allocation")]
    InvalidAllocation,
    /// FFmpeg rejected the supplied renderer device.
    #[error("FFmpeg could not initialize the renderer-owned {backend:?} device context: {reason}")]
    InitializationFailed {
        /// Hardware backend whose exact device could not be initialized.
        backend: HwAccelBackend,
        /// Stable FFmpeg error text.
        reason: String,
    },
}

#[cfg(target_os = "windows")]
#[repr(C)]
struct AvD3D12VaDeviceContext {
    device: *mut std::ffi::c_void,
    video_device: *mut std::ffi::c_void,
    lock: Option<unsafe extern "C" fn(*mut std::ffi::c_void)>,
    unlock: Option<unsafe extern "C" fn(*mut std::ffi::c_void)>,
    lock_context: *mut std::ffi::c_void,
}

#[cfg(target_os = "windows")]
fn initialize_ffmpeg_d3d12_device_context(
    device_context: NonNull<ffmpeg::ffi::AVBufferRef>,
    device: ID3D12Device,
) -> Result<(), RendererHwAccelDeviceContextCreateError> {
    // SAFETY: av_hwdevice_ctx_alloc returned an owned AVBufferRef whose data
    // points to a writable AVHWDeviceContext until av_hwdevice_ctx_init.
    let generic = unsafe {
        NonNull::new((*device_context.as_ptr()).data.cast::<ffmpeg::ffi::AVHWDeviceContext>())
    }
    .ok_or(RendererHwAccelDeviceContextCreateError::InvalidAllocation)?;
    // SAFETY: FFmpeg allocated the D3D12VA-specific payload for this exact
    // device type. The local layout mirrors libavutil/hwcontext_d3d12va.h.
    let native =
        unsafe { NonNull::new((*generic.as_ptr()).hwctx.cast::<AvD3D12VaDeviceContext>()) }
            .ok_or(RendererHwAccelDeviceContextCreateError::InvalidAllocation)?;

    // Transfer one owned COM reference into FFmpeg. The D3D12VA context
    // releases it unconditionally when the final AVBufferRef is destroyed.
    let raw_device = device.into_raw();
    unsafe {
        (*native.as_ptr()).device = raw_device;
        (*native.as_ptr()).video_device = std::ptr::null_mut();
        (*native.as_ptr()).lock = None;
        (*native.as_ptr()).unlock = None;
        (*native.as_ptr()).lock_context = std::ptr::null_mut();
    }
    let result = unsafe { ffmpeg::ffi::av_hwdevice_ctx_init(device_context.as_ptr()) };
    if result < 0 {
        return Err(
            RendererHwAccelDeviceContextCreateError::InitializationFailed {
                backend: HwAccelBackend::D3D12VA,
                reason: ffmpeg::Error::from(result).to_string(),
            },
        );
    }
    Ok(())
}

/// Idle-residency policy for one explicit hardware-device context pool.
///
/// Active decoder Sessions are never revoked to satisfy this policy. A zero
/// limit makes every context cold after the last Session releases it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HwDeviceContextPoolPolicy {
    /// Maximum idle device contexts retained for future decoder Sessions.
    pub max_idle_contexts: usize,
}

impl HwDeviceContextPoolPolicy {
    /// Construct one explicit idle-residency policy.
    pub const fn new(max_idle_contexts: usize) -> Self {
        Self { max_idle_contexts }
    }
}

impl Default for HwDeviceContextPoolPolicy {
    fn default() -> Self {
        Self { max_idle_contexts: 2 }
    }
}

/// Point-in-time evidence for one hardware-device context pool.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HwDeviceContextPoolDiagnostics {
    /// Effective idle-residency policy.
    pub policy: HwDeviceContextPoolPolicy,
    /// Current policy revision.
    pub policy_revision: u64,
    /// Context generations currently addressable by new Sessions.
    pub entries: usize,
    /// Entries with at least one active Session lease.
    pub active_contexts: usize,
    /// Entries retained only as idle acceleration resources.
    pub idle_contexts: usize,
    /// External device initialization still owned by an acquiring worker,
    /// including a revoked generation waiting for its provider to return.
    pub initializing_contexts: usize,
    /// Detached pool references whose release has not returned. Independent
    /// Session leases can retain retired roots beyond this pool-owned work.
    pub retiring_contexts: usize,
    /// Most recently allocated device generation.
    pub latest_generation: u64,
    /// Acquisitions that reused one current generation.
    pub hits: u64,
    /// Acquisitions that created a new generation.
    pub misses: u64,
    /// Current generations retired after setup or execution failure.
    pub retirements: u64,
    /// Idle generations released by policy or explicit pressure.
    pub evictions: u64,
    /// Driver/device creation failures.
    pub creation_failures: u64,
    /// Codec-attachment or decoder-open failures that retired a device generation.
    pub setup_failures: u64,
    /// Backend/adapter keys currently under a bounded retry delay.
    pub failure_backoffs: usize,
    /// Acquisitions deferred without driver work while a retry delay was active.
    pub backoff_rejections: u64,
}

struct HwDeviceContextPoolEntry {
    generation: u64,
    last_used: u64,
    owner: Arc<SharedHwAccelDeviceContext>,
}

struct HwDeviceContextFailureBackoff {
    probe: HwAccelDeviceContextProbe,
    consecutive_failures: u32,
    retry_after: Instant,
}

struct HwDeviceContextPoolState {
    policy: HwDeviceContextPoolPolicy,
    policy_revision: u64,
    next_generation: u64,
    recency_clock: u64,
    entries: HashMap<HwAccelDeviceProbeKey, HwDeviceContextPoolEntry>,
    failures: HashMap<HwAccelDeviceProbeKey, HwDeviceContextFailureBackoff>,
    initializing: Option<HwDeviceContextInitialization>,
    retiring_contexts: usize,
    hits: u64,
    misses: u64,
    retirements: u64,
    evictions: u64,
    creation_failures: u64,
    setup_failures: u64,
    backoff_rejections: u64,
}

impl HwDeviceContextPoolState {
    fn new(policy: HwDeviceContextPoolPolicy) -> Self {
        Self {
            policy,
            policy_revision: 1,
            next_generation: 1,
            recency_clock: 0,
            entries: HashMap::new(),
            failures: HashMap::new(),
            initializing: None,
            retiring_contexts: 0,
            hits: 0,
            misses: 0,
            retirements: 0,
            evictions: 0,
            creation_failures: 0,
            setup_failures: 0,
            backoff_rejections: 0,
        }
    }

    fn next_recency(&mut self) -> u64 {
        self.recency_clock = self.recency_clock.saturating_add(1);
        self.recency_clock
    }

    fn allocate_generation(&mut self) -> Option<u64> {
        let generation = self.next_generation;
        self.next_generation = self.next_generation.checked_add(1)?;
        Some(generation)
    }

    fn idle_count(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| Arc::strong_count(&entry.owner) == 1)
            .count()
    }

    fn trim_idle_to(&mut self, max_idle_contexts: usize) -> Vec<HwDeviceContextPoolEntry> {
        let mut retired = Vec::new();
        while self.idle_count() > max_idle_contexts {
            let Some(key) = self
                .entries
                .iter()
                .filter(|(_, entry)| Arc::strong_count(&entry.owner) == 1)
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| *key)
            else {
                break;
            };
            if let Some(entry) = self.entries.remove(&key) {
                retired.push(entry);
            }
            self.evictions = self.evictions.saturating_add(1);
        }
        retired
    }

    fn diagnostics(&self) -> HwDeviceContextPoolDiagnostics {
        let idle_contexts = self.idle_count();
        HwDeviceContextPoolDiagnostics {
            policy: self.policy,
            policy_revision: self.policy_revision,
            entries: self.entries.len(),
            active_contexts: self.entries.len().saturating_sub(idle_contexts),
            idle_contexts,
            initializing_contexts: usize::from(self.initializing.is_some()),
            retiring_contexts: self.retiring_contexts,
            latest_generation: self.next_generation.saturating_sub(1),
            hits: self.hits,
            misses: self.misses,
            retirements: self.retirements,
            evictions: self.evictions,
            creation_failures: self.creation_failures,
            setup_failures: self.setup_failures,
            failure_backoffs: self.failures.len(),
            backoff_rejections: self.backoff_rejections,
        }
    }

    fn record_failure(
        &mut self,
        key: HwAccelDeviceProbeKey,
        probe: HwAccelDeviceContextProbe,
        setup_failure: bool,
    ) -> HwAccelDeviceContextProbe {
        let consecutive_failures = self
            .failures
            .get(&key)
            .map_or(1, |failure| failure.consecutive_failures.saturating_add(1));
        let shift = consecutive_failures.saturating_sub(1).min(7);
        let multiplier = 1u32.checked_shl(shift).unwrap_or(u32::MAX);
        let delay = HW_DEVICE_FAILURE_BACKOFF_BASE
            .checked_mul(multiplier)
            .unwrap_or(HW_DEVICE_FAILURE_BACKOFF_MAX)
            .min(HW_DEVICE_FAILURE_BACKOFF_MAX);
        self.failures.insert(
            key,
            HwDeviceContextFailureBackoff {
                probe: probe.clone(),
                consecutive_failures,
                retry_after: Instant::now() + delay,
            },
        );
        if setup_failure {
            self.setup_failures = self.setup_failures.saturating_add(1);
        } else {
            self.creation_failures = self.creation_failures.saturating_add(1);
        }
        probe
    }
}

struct HwDeviceContextPoolInner {
    state: Mutex<HwDeviceContextPoolState>,
    // Serialize foreign creation without excluding state observation or
    // acquisition of an already-published root. This gate owns no pool state.
    creation: Mutex<()>,
}

struct HwDeviceContextInitialization {
    key: HwAccelDeviceProbeKey,
    generation: u64,
    publishable: bool,
}

struct HwDeviceContextInitializationGuard {
    pool: HwDeviceContextPool,
    generation: u64,
}

impl Drop for HwDeviceContextInitializationGuard {
    fn drop(&mut self) {
        let mut state = self.pool.lock_state();
        if state
            .initializing
            .as_ref()
            .is_some_and(|value| value.generation == self.generation)
        {
            state.initializing = None;
        }
    }
}

struct HwDeviceContextRetirement {
    pool: HwDeviceContextPool,
    entries: Vec<HwDeviceContextPoolEntry>,
}

impl Drop for HwDeviceContextRetirement {
    fn drop(&mut self) {
        let count = self.entries.len();
        if count == 0 {
            return;
        }
        // Keep the accounting and this pool alive until every foreign final
        // release has returned. No shared state lock spans those releases.
        drop(std::mem::take(&mut self.entries));
        self.pool.lock_state().retiring_contexts -= count;
    }
}

/// Explicit worker-family owner of shared FFmpeg hardware device contexts.
///
/// The pool shares only immutable device roots for an exact backend/adapter.
/// Codec contexts, DPB state, frame pools, and decoded surfaces remain
/// Session-owned. Retiring a generation removes it from future lookup while
/// active leases safely keep its `AVBufferRef` alive.
#[derive(Clone)]
pub struct HwDeviceContextPool {
    inner: Arc<HwDeviceContextPoolInner>,
}

impl std::fmt::Debug for HwDeviceContextPool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HwDeviceContextPool")
            .field("diagnostics", &self.diagnostics())
            .finish()
    }
}

impl HwDeviceContextPool {
    /// Create a pool with one explicit idle-residency policy.
    pub fn new(policy: HwDeviceContextPoolPolicy) -> Self {
        Self {
            inner: Arc::new(HwDeviceContextPoolInner {
                state: Mutex::new(HwDeviceContextPoolState::new(policy)),
                creation: Mutex::new(()),
            }),
        }
    }

    /// Apply an idle-residency policy online and immediately release excess idle contexts.
    pub fn reconfigure(&self, policy: HwDeviceContextPoolPolicy) {
        let mut state = self.lock_state();
        if state.policy == policy {
            return;
        }
        state.policy = policy;
        state.policy_revision = state.policy_revision.saturating_add(1);
        let entries = state.trim_idle_to(policy.max_idle_contexts);
        let retirement = self.own_retirement(&mut state, entries);
        drop(state);
        drop(retirement);
    }

    /// Release every idle context while preserving all active Session leases.
    pub fn release_idle(&self) {
        let mut state = self.lock_state();
        let entries = state.trim_idle_to(0);
        let retirement = self.own_retirement(&mut state, entries);
        drop(state);
        drop(retirement);
    }

    fn own_retirement(
        &self,
        state: &mut HwDeviceContextPoolState,
        entries: Vec<HwDeviceContextPoolEntry>,
    ) -> HwDeviceContextRetirement {
        state.retiring_contexts += entries.len();
        HwDeviceContextRetirement { pool: self.clone(), entries }
    }

    /// Clear transient setup-failure delays, for example after an explicit
    /// adapter/device-generation change notification.
    pub fn invalidate_failure_backoff(&self) {
        self.lock_state().failures.clear();
    }

    /// Install the exact renderer-qualified device root for future decoder Sessions.
    ///
    /// Replacing a generation never revokes active Sessions: their independent
    /// `Arc` leases retain the previous FFmpeg root until the last Session and
    /// native output release it. Installing the same root is idempotent.
    pub fn install_renderer_device_context(
        &self,
        selector: HwAccelDeviceSelector,
        context: RendererHwAccelDeviceContext,
    ) -> Result<bool, RendererHwAccelDeviceContextInstallError> {
        let backend = context.backend();
        if !selector.selects_backend(backend) {
            return Err(RendererHwAccelDeviceContextInstallError::SelectorMismatch {
                selector,
                backend,
            });
        }
        let key = (backend, Some(selector));
        let mut state = self.lock_state();
        if state
            .entries
            .get(&key)
            .is_some_and(|entry| Arc::ptr_eq(&entry.owner, &context.owner))
        {
            state.failures.remove(&key);
            return Ok(false);
        }
        let generation = state
            .allocate_generation()
            .ok_or(RendererHwAccelDeviceContextInstallError::GenerationExhausted)?;
        if let Some(initializing) = state.initializing.as_mut().filter(|value| value.key == key) {
            initializing.publishable = false;
        }
        let recency = state.next_recency();
        let previous = state.entries.insert(
            key,
            HwDeviceContextPoolEntry {
                generation,
                last_used: recency,
                owner: context.owner,
            },
        );
        if previous.is_some() {
            state.retirements = state.retirements.saturating_add(1);
        }
        state.failures.remove(&key);
        state.misses = state.misses.saturating_add(1);
        let retirement = self.own_retirement(&mut state, previous.into_iter().collect());
        drop(state);
        drop(retirement);
        Ok(true)
    }

    /// Retire the renderer-qualified root currently offered for one selector.
    ///
    /// Active codec Sessions and native outputs retain independent `Arc`
    /// leases; this removes only future acquisition authority. An in-progress
    /// initialization for this selector is also revoked, but remains accounted
    /// for until its provider returns and the acquiring worker consumes it.
    pub fn retire_renderer_device_context(&self, selector: HwAccelDeviceSelector) -> bool {
        let backend = match selector {
            HwAccelDeviceSelector::VaapiDrmRenderNode(_) => HwAccelBackend::Vaapi,
            HwAccelDeviceSelector::CudaDeviceOrdinal(_) => HwAccelBackend::Cuda,
            HwAccelDeviceSelector::D3D12VaAdapterIndex(_) => HwAccelBackend::D3D12VA,
            HwAccelDeviceSelector::D3D11VaAdapterIndex(_) => HwAccelBackend::D3D11VA,
        };
        let key = (backend, Some(selector));
        let mut state = self.lock_state();
        state.failures.remove(&key);
        let initializing_revoked = state.initializing.as_mut().is_some_and(|value| {
            if value.key == key && value.publishable {
                value.publishable = false;
                true
            } else {
                false
            }
        });
        let previous = state.entries.remove(&key);
        if previous.is_none() && !initializing_revoked {
            return false;
        }
        state.retirements = state.retirements.saturating_add(1);
        let retirement = self.own_retirement(&mut state, previous.into_iter().collect());
        drop(state);
        drop(retirement);
        true
    }

    /// Return current generation and residency evidence.
    pub fn diagnostics(&self) -> HwDeviceContextPoolDiagnostics {
        self.lock_state().diagnostics()
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, HwDeviceContextPoolState> {
        match self.inner.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                tracing::warn!(
                    "hardware device context pool lock was poisoned; retaining explicit state"
                );
                poisoned.into_inner()
            }
        }
    }

    #[cfg(test)]
    fn retire(&self, key: HwAccelDeviceProbeKey, generation: u64) {
        let mut state = self.lock_state();
        let current_generation = state.entries.get(&key).map(|entry| entry.generation);
        let previous = if current_generation == Some(generation) {
            state.retirements = state.retirements.saturating_add(1);
            state.entries.remove(&key)
        } else {
            None
        };
        let retirement = self.own_retirement(&mut state, previous.into_iter().collect());
        drop(state);
        drop(retirement);
    }

    fn retire_after_setup_failure(
        &self,
        key: HwAccelDeviceProbeKey,
        generation: u64,
        probe: HwAccelDeviceContextProbe,
    ) {
        let mut state = self.lock_state();
        let current_generation = state.entries.get(&key).map(|entry| entry.generation);
        if current_generation != Some(generation) {
            // The failing Session still owns its diagnostic and old root, but
            // cannot revoke or defer acquisition of a replacement generation.
            return;
        }
        state.retirements = state.retirements.saturating_add(1);
        let previous = state.entries.remove(&key);
        let _ = state.record_failure(key, probe, true);
        let retirement = self.own_retirement(&mut state, previous.into_iter().collect());
        drop(state);
        drop(retirement);
    }

    fn release(
        &self,
        key: HwAccelDeviceProbeKey,
        generation: u64,
        owner: &Arc<SharedHwAccelDeviceContext>,
    ) {
        let mut state = self.lock_state();
        let is_last_active_lease = state.entries.get(&key).is_some_and(|entry| {
            entry.generation == generation
                && Arc::ptr_eq(&entry.owner, owner)
                && Arc::strong_count(owner) == 2
        });
        if !is_last_active_lease {
            return;
        }

        let max_idle_contexts = state.policy.max_idle_contexts;
        let mut entries = Vec::new();
        if state.idle_count() >= max_idle_contexts {
            if let Some(entry) = state.entries.remove(&key) {
                entries.push(entry);
            }
            state.evictions = state.evictions.saturating_add(1);
        }
        entries.extend(state.trim_idle_to(max_idle_contexts));
        let retirement = self.own_retirement(&mut state, entries);
        drop(state);
        drop(retirement);
    }

    pub(crate) fn acquire(
        &self,
        backend: HwAccelBackend,
        selector: Option<HwAccelDeviceSelector>,
    ) -> std::result::Result<HwAccelDeviceContext, HwAccelDeviceContextProbe> {
        self.acquire_with_device_creation(backend, selector, |device_type, device_name| {
            let mut device_context = ptr::null_mut();
            // SAFETY: The out pointer is exclusive to this call; the returned
            // AVBufferRef is transferred to the pool's creation owner or
            // released on the partial-open path before publication.
            let result = unsafe {
                ffmpeg::ffi::av_hwdevice_ctx_create(
                    &mut device_context,
                    device_type,
                    device_name.map_or(ptr::null(), |name| name.as_ptr()),
                    ptr::null_mut(),
                    0,
                )
            };
            (result, device_context)
        })
    }

    fn acquire_with_device_creation(
        &self,
        backend: HwAccelBackend,
        selector: Option<HwAccelDeviceSelector>,
        create: impl FnOnce(
            ffmpeg::ffi::AVHWDeviceType,
            Option<&std::ffi::CStr>,
        ) -> (i32, *mut ffmpeg::ffi::AVBufferRef),
    ) -> std::result::Result<HwAccelDeviceContext, HwAccelDeviceContextProbe> {
        if let Some(selector) = selector.filter(|selector| !selector.selects_backend(backend)) {
            return Err(HwAccelDeviceContextProbe::unavailable(
                backend,
                format!(
                    "hardware device selector {selector:?} does not select {}",
                    backend.as_str()
                ),
            ));
        }
        let Some(device_type) = backend.to_ffmpeg_device_type() else {
            return Err(HwAccelDeviceContextProbe::unavailable(
                backend,
                format!(
                    "{} does not map to an FFmpeg hardware device",
                    backend.as_str()
                ),
            ));
        };
        let _ = ffmpeg::init();
        let ffmpeg_device_type_available = ffmpeg_hwdevice_type_available(device_type);
        if !ffmpeg_device_type_available {
            return Err(HwAccelDeviceContextProbe {
                backend,
                backend_maps_to_ffmpeg_device: true,
                ffmpeg_device_type_available,
                device_create_attempted: false,
                device_context_created: false,
                device_create_error_code: None,
                reason: format!(
                    "linked FFmpeg build does not list {} hardware device type",
                    backend.as_str()
                ),
            });
        }

        self.acquire_available_device(backend, selector, device_type, create)
    }

    fn acquire_current_device(
        &self,
        key: HwAccelDeviceProbeKey,
        state: &mut HwDeviceContextPoolState,
    ) -> std::result::Result<Option<HwAccelDeviceContext>, HwAccelDeviceContextProbe> {
        let now = Instant::now();
        if let Some(failure) = state.failures.get(&key) {
            if now < failure.retry_after {
                let retry_after = failure.retry_after.saturating_duration_since(now);
                let mut probe = failure.probe.clone();
                probe.device_create_attempted = false;
                probe.device_context_created = false;
                probe.reason = format!(
                    "{}; retry deferred for {} ms after {} consecutive failures",
                    probe.reason,
                    retry_after.as_millis(),
                    failure.consecutive_failures
                );
                state.backoff_rejections = state.backoff_rejections.saturating_add(1);
                return Err(probe);
            }
            state.failures.remove(&key);
        }
        let recency = state.next_recency();
        if let Some(entry) = state.entries.get_mut(&key) {
            entry.last_used = recency;
            let generation = entry.generation;
            let owner = Arc::clone(&entry.owner);
            state.hits = state.hits.saturating_add(1);
            state.failures.remove(&key);
            return Ok(Some(HwAccelDeviceContext {
                owner,
                pool: self.clone(),
                key,
                generation,
                newly_created: false,
            }));
        }

        Ok(None)
    }

    fn acquire_available_device(
        &self,
        backend: HwAccelBackend,
        selector: Option<HwAccelDeviceSelector>,
        device_type: ffmpeg::ffi::AVHWDeviceType,
        create: impl FnOnce(
            ffmpeg::ffi::AVHWDeviceType,
            Option<&std::ffi::CStr>,
        ) -> (i32, *mut ffmpeg::ffi::AVBufferRef),
    ) -> std::result::Result<HwAccelDeviceContext, HwAccelDeviceContextProbe> {
        let ffmpeg_device_type_available = true;
        let key = (backend, selector);
        let mut state = self.lock_state();
        if let Some(current) = self.acquire_current_device(key, &mut state)? {
            return Ok(current);
        }
        drop(state);
        let _creation = match self.inner.creation.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let mut state = self.lock_state();
        if let Some(current) = self.acquire_current_device(key, &mut state)? {
            return Ok(current);
        }
        let Some(generation) = state.allocate_generation() else {
            return Err(HwAccelDeviceContextProbe {
                backend,
                backend_maps_to_ffmpeg_device: true,
                ffmpeg_device_type_available,
                device_create_attempted: false,
                device_context_created: false,
                device_create_error_code: None,
                reason: "hardware device generation space is exhausted".to_owned(),
            });
        };
        state.initializing =
            Some(HwDeviceContextInitialization { key, generation, publishable: true });
        drop(state);
        let _initialization = HwDeviceContextInitializationGuard { pool: self.clone(), generation };
        let device_name = selector.and_then(|selector| selector.device_name_for(backend));
        let (result, mut device_context) = create(device_type, device_name.as_deref());
        let created = if result < 0 {
            if !device_context.is_null() {
                // SAFETY: FFmpeg returned this partial AVBufferRef through the
                // exclusive out pointer; no owner was published.
                unsafe {
                    ffmpeg::ffi::av_buffer_unref(&mut device_context);
                }
            }
            let probe = HwAccelDeviceContextProbe {
                backend,
                backend_maps_to_ffmpeg_device: true,
                ffmpeg_device_type_available,
                device_create_attempted: true,
                device_context_created: false,
                device_create_error_code: Some(result),
                reason: format!(
                    "FFmpeg could not create {} hardware device context: {}",
                    backend.as_str(),
                    ffmpeg::Error::from(result)
                ),
            };
            Err(probe)
        } else if let Some(device_context) = NonNull::new(device_context) {
            Ok(Arc::new(SharedHwAccelDeviceContext {
                backend,
                ptr: device_context,
            }))
        } else {
            let probe = HwAccelDeviceContextProbe {
                backend,
                backend_maps_to_ffmpeg_device: true,
                ffmpeg_device_type_available,
                device_create_attempted: true,
                device_context_created: false,
                device_create_error_code: None,
                reason: format!(
                    "FFmpeg reported success but returned no {} hardware device context",
                    backend.as_str()
                ),
            };
            Err(probe)
        };
        // A renderer generation may be installed or retired while the foreign
        // call runs. Its later state change wins; a late result cannot reopen it.
        // `created` precedes this guard so discarded foreign roots drop only
        // after the state mutex is released, including all early-return paths.
        let mut state = self.lock_state();
        if !state
            .initializing
            .as_ref()
            .is_some_and(|value| value.generation == generation && value.publishable)
        {
            if let Some(current) = self.acquire_current_device(key, &mut state)? {
                return Ok(current);
            }
            let reason = "hardware device initialization was retired before publication";
            let probe = match &created {
                Ok(_) => HwAccelDeviceContextProbe::acquired(backend, true, reason),
                Err(failure) => {
                    let mut probe = failure.clone();
                    probe.reason = format!("{reason}; {}", probe.reason);
                    probe
                }
            };
            return Err(probe);
        }
        let owner = match created {
            Ok(owner) => owner,
            Err(probe) => return Err(state.record_failure(key, probe, false)),
        };
        let recency = state.next_recency();
        state.entries.insert(
            key,
            HwDeviceContextPoolEntry {
                generation,
                last_used: recency,
                owner: Arc::clone(&owner),
            },
        );
        state.failures.remove(&key);
        state.misses = state.misses.saturating_add(1);
        let max_idle_contexts = state.policy.max_idle_contexts;
        let entries = state.trim_idle_to(max_idle_contexts);
        let retirement = self.own_retirement(&mut state, entries);
        drop(state);
        drop(retirement);
        Ok(HwAccelDeviceContext {
            owner,
            pool: self.clone(),
            key,
            generation,
            newly_created: true,
        })
    }
}

/// Failure to install a renderer-qualified root into a decoder worker family.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum RendererHwAccelDeviceContextInstallError {
    /// The selector names a different hardware backend.
    #[error("hardware selector {selector:?} does not select renderer device backend {backend:?}")]
    SelectorMismatch {
        /// Supplied decoder device selector.
        selector: HwAccelDeviceSelector,
        /// Backend owned by the renderer-qualified context.
        backend: HwAccelBackend,
    },
    /// Pool generation identity cannot advance safely.
    #[error("hardware device-context generation space is exhausted")]
    GenerationExhausted,
}

impl Default for HwDeviceContextPool {
    fn default() -> Self {
        Self::new(HwDeviceContextPoolPolicy::default())
    }
}

/// Residency of frames produced by the media decode boundary.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, Default,
)]
pub enum DecodedFrameResidency {
    /// Decoder output is CPU RGBA memory.
    #[default]
    CpuRgba,
    /// Decoder output is CPU RGBA f32 memory.
    CpuFloat,
    /// Decoder output is compact CPU YUV planes awaiting GPU materialization.
    CpuYuv,
    /// Decoder output is a GPU texture or hardware frame.
    GpuTexture,
}

/// Decoder output surface format before Mondrian's preview CPU RGBA boundary.
///
/// This is a media-layer fact. Renderer-native import formats are modeled by
/// `mondrian-renderer` and must be mapped at the app/readiness boundary.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, Default,
)]
pub enum DecodedVideoSurfaceFormat {
    /// The decoder surface format is unknown or not yet reported.
    #[default]
    Unknown,
    /// 8-bit NV12 two-plane YUV 4:2:0 surface.
    Nv12,
    /// 10-bit P010 two-plane YUV 4:2:0 surface.
    P010,
    /// 12-bit P012 two-plane YUV 4:2:0 surface.
    P012,
    /// 16-bit P016 two-plane YUV 4:2:0 surface.
    P016,
    /// 10-bit P210 two-plane YUV 4:2:2 surface.
    P210,
    /// 12-bit P212 two-plane YUV 4:2:2 surface.
    P212,
    /// 16-bit P216 two-plane YUV 4:2:2 surface.
    P216,
    /// 10-bit P410 two-plane YUV 4:4:4 surface.
    P410,
    /// 12-bit P412 two-plane YUV 4:4:4 surface.
    P412,
    /// 16-bit P416 two-plane YUV 4:4:4 surface.
    P416,
    /// Packed 10-bit Y210 YCbCr 4:2:2 surface.
    Y210,
    /// Packed 12-bit Y212-in-Y216 YCbCr 4:2:2 surface.
    Y212,
    /// Packed 10-bit XV30-in-Y410 YCbCr 4:4:4 surface.
    Xv30,
    /// Packed 12-bit XV36-in-Y416 YCbCr 4:4:4 surface.
    Xv36,
    /// Planar 8-bit YUV 4:2:0.
    Yuv420p,
    /// Planar 10-bit YUV 4:2:0.
    Yuv420p10le,
    /// Planar little-endian twelve-bit YUV 4:2:0 with right-aligned samples.
    Yuv420p12le,
    /// Planar 8-bit YUV 4:2:2.
    Yuv422p,
    /// Planar 10-bit YUV 4:2:2.
    Yuv422p10le,
    /// Planar little-endian twelve-bit YUV 4:2:2 with right-aligned samples.
    Yuv422p12le,
    /// Planar eight-bit CPU YUV 4:4:4.
    Yuv444p,
    /// Planar little-endian ten-bit CPU YUV 4:4:4 with right-aligned samples.
    Yuv444p10le,
    /// Planar little-endian twelve-bit YUV 4:4:4 with right-aligned samples.
    Yuv444p12le,
    /// Packed RGBA8.
    Rgba8,
    /// Packed BGRA8.
    Bgra8,
    /// Packed scene- or display-referred RGBA half-float.
    Rgba16Float,
    /// Packed scene- or display-referred RGBA single-precision float.
    Rgba32Float,
    /// A known but currently non-native preview surface format.
    Other,
}

/// Color-family fact carried by a decoded surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecodedVideoSurfaceColorModel {
    /// Luma plus blue- and red-difference chroma components.
    Ycbcr,
    /// Red, green, and blue components.
    Rgb,
}

/// Chroma sampling grid carried by a decoded YCbCr surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecodedVideoSurfaceChromaSubsampling {
    /// One chroma sample covers two horizontal by two vertical luma samples.
    Cs420,
    /// One chroma sample covers two horizontal by one vertical luma sample.
    Cs422,
    /// Chroma has the same sampling grid as luma.
    Cs444,
}

/// Shader-visible plane organization of a decoded surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecodedVideoSurfacePlaneLayout {
    /// Luma and an interleaved CbCr plane.
    SemiPlanar,
    /// Independent luma, Cb, and Cr planes.
    Planar,
    /// Packed YCbCr 4:2:2 words requiring an explicit GPU unpack Adapter.
    PackedYuv422,
    /// Packed YCbCr 4:4:4 words requiring an explicit GPU unpack Adapter.
    PackedYuv444,
    /// One packed RGBA plane.
    PackedRgba,
    /// One packed BGRA plane.
    PackedBgra,
}

/// Numeric representation stored by each decoded-surface component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecodedVideoSurfaceNumericEncoding {
    /// Eight-bit unsigned normalized components.
    Unorm8,
    /// High-bit unsigned normalized components stored in sixteen-bit words.
    Unorm16 {
        /// Whether the effective code bits occupy the most-significant side.
        most_significant_bits: bool,
    },
    /// Integer code components packed into layout-specific bit fields.
    PackedUnsigned,
    /// IEEE 754 binary16 components.
    Float16,
    /// IEEE 754 binary32 components.
    Float32,
}

/// Complete physical interpretation of one known decoded surface format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DecodedVideoSurfaceDescriptor {
    /// Color component family.
    pub color_model: DecodedVideoSurfaceColorModel,
    /// Chroma grid for YCbCr, absent for RGB.
    pub chroma_subsampling: Option<DecodedVideoSurfaceChromaSubsampling>,
    /// Physical plane organization.
    pub plane_layout: DecodedVideoSurfacePlaneLayout,
    /// Numeric component storage.
    pub numeric_encoding: DecodedVideoSurfaceNumericEncoding,
    /// Effective component precision in bits.
    pub component_bit_depth: u8,
    /// Whether the physical surface carries an alpha component.
    pub has_alpha: bool,
    /// Whether the media layer may retain this physical format as a native GPU payload.
    pub native_gpu_payload: bool,
}

/// Authority-aware quantization-range contract carried into frame decode.
///
/// Automatic interpretation prefers an explicit frame-level decoder fact and
/// uses the probe result only when that frame omits range metadata. A user
/// override remains authoritative even when the frame repeats an incorrect tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(tag = "authority", rename_all = "snake_case")]
pub enum DecodedVideoRangeContract {
    /// Follow frame metadata, falling back to the latest stream probe.
    Automatic {
        /// Stream-level range reported while probing the asset.
        probed_range: DecodedVideoRange,
    },
    /// Force studio/legal range.
    OverrideLimited,
    /// Force full range.
    OverrideFull,
}

impl DecodedVideoRangeContract {
    /// Build the decode contract from persistent asset interpretation and probe facts.
    pub const fn from_interpretation(
        interpretation: mondrian_core::timeline_data::MediaRangeInterpretation,
        probed_range: DecodedVideoRange,
    ) -> Self {
        use mondrian_core::timeline_data::{MediaRangeInterpretation, MediaSignalRange};

        match interpretation {
            MediaRangeInterpretation::Auto => Self::Automatic { probed_range },
            MediaRangeInterpretation::Override { range: MediaSignalRange::Limited } => {
                Self::OverrideLimited
            }
            MediaRangeInterpretation::Override { range: MediaSignalRange::Full } => {
                Self::OverrideFull
            }
        }
    }

    /// Resolve the range to apply to one decoded frame.
    pub const fn resolve_for_frame(self, frame_range: DecodedVideoRange) -> DecodedVideoRange {
        match self {
            Self::Automatic { probed_range } => match frame_range {
                DecodedVideoRange::Unknown => probed_range,
                explicit => explicit,
            },
            Self::OverrideLimited => DecodedVideoRange::Limited,
            Self::OverrideFull => DecodedVideoRange::Full,
        }
    }

    /// Return the range available before a frame has been decoded.
    pub const fn baseline(self) -> DecodedVideoRange {
        self.resolve_for_frame(DecodedVideoRange::Unknown)
    }
}

/// Resolve the decoder-facing range from persistent asset interpretation and
/// the latest probe result.
///
/// A user override is authoritative so incorrectly tagged media can be
/// corrected consistently by preview, thumbnails, proxies, and export.
pub fn resolve_decoded_video_range(
    interpretation: mondrian_core::timeline_data::MediaRangeInterpretation,
    detected: DecodedVideoRange,
) -> DecodedVideoRange {
    DecodedVideoRangeContract::from_interpretation(interpretation, detected).baseline()
}

pub(crate) fn decoded_video_range_from_ffmpeg(
    range: ffmpeg_next::util::color::Range,
) -> DecodedVideoRange {
    match range {
        ffmpeg_next::util::color::Range::MPEG => DecodedVideoRange::Limited,
        ffmpeg_next::util::color::Range::JPEG => DecodedVideoRange::Full,
        ffmpeg_next::util::color::Range::Unspecified => DecodedVideoRange::Unknown,
    }
}

/// Chroma sample location reported by the decoder for a video frame.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, Default,
)]
pub enum DecodedVideoChromaLocation {
    /// No reliable chroma-location metadata was reported.
    #[default]
    Unknown,
    /// Left chroma siting.
    Left,
    /// Center chroma siting.
    Center,
    /// Top-left chroma siting.
    TopLeft,
    /// Top chroma siting.
    Top,
    /// Bottom-left chroma siting.
    BottomLeft,
    /// Bottom chroma siting.
    Bottom,
}

/// Decoder-reported sampling facts for a decoded video frame.
///
/// These are media payload facts, not color-interpretation decisions. The app
/// combines them with the resolved source color space before asking the renderer
/// to import a native video surface.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, Default,
)]
pub struct DecodedVideoSampling {
    /// Decoder-reported YCbCr-to-RGB matrix.
    pub matrix: DecodedVideoMatrix,
    /// Encoded quantization range.
    pub range: DecodedVideoRange,
    /// Chroma sample location.
    pub chroma_location: DecodedVideoChromaLocation,
    /// Effective coded bit depth. Zero means unknown.
    pub bit_depth: u8,
}

/// YUV matrix applied while converting a decoded CPU frame to source-encoded RGB.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, Default,
)]
pub enum DecodedVideoMatrix {
    /// No reliable matrix was reported.
    #[default]
    Unknown,
    /// The decoder reported a matrix that requires a conversion not implemented
    /// by Mondrian. This is distinct from absent metadata so callers fail closed
    /// instead of substituting the project color-space matrix.
    Unsupported,
    /// BT.709 non-constant luminance coefficients.
    Bt709,
    /// BT.2020 non-constant luminance coefficients.
    Bt2020NonConstant,
    /// FCC coefficients.
    Fcc,
    /// BT.470BG / BT.601 coefficients.
    Bt470Bg,
    /// SMPTE 170M / BT.601 coefficients.
    Smpte170M,
    /// SMPTE 240M coefficients.
    Smpte240M,
    /// Source pixels were already RGB, so no YUV matrix was applied.
    Rgb,
}

/// Native hardware-frame handle family produced by a decoder.
///
/// This enum names the cross-crate contract only. It does not claim that
/// Mondrian can import the handle into the renderer; that requires a separate
/// support contract from the active Renderer Adapter/Device runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum DecodedGpuFrameHandleKind {
    /// Windows D3D12 `ID3D12Resource` hardware decode surface.
    D3D12Resource,
    /// Windows D3D11 `ID3D11Texture2D` hardware decode surface.
    D3D11Texture2D,
    /// Legacy Windows DXVA2 `IDirect3DSurface9` hardware decode surface.
    Dxva2Surface,
    /// macOS/iOS `CVPixelBuffer` backed by an IOSurface.
    CVPixelBuffer,
    /// Linux VA-API `VASurfaceID`/DMABUF-exportable surface.
    VaapiSurface,
    /// Legacy Linux VDPAU `VdpVideoSurface`.
    VdpauVideoSurface,
    /// CUDA/NVDEC device allocation.
    CudaDeviceMemory,
}

/// FFmpeg hardware pixel format reported by `avcodec_get_hw_config`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum HwAccelPixelFormat {
    /// FFmpeg D3D12 hardware surfaces (`AV_PIX_FMT_D3D12`).
    D3D12,
    /// FFmpeg D3D11 hardware surfaces (`AV_PIX_FMT_D3D11`).
    D3D11,
    /// Legacy FFmpeg D3D11VA VLD surfaces.
    D3D11VA,
    /// Legacy FFmpeg DXVA2 VLD surfaces.
    Dxva2,
    /// FFmpeg VideoToolbox hardware surfaces.
    VideoToolbox,
    /// FFmpeg VA-API hardware surfaces.
    Vaapi,
    /// FFmpeg VDPAU hardware surfaces.
    Vdpau,
    /// FFmpeg CUDA/NVDEC hardware surfaces.
    Cuda,
    /// A hardware config exists, but Mondrian does not classify this pixel format yet.
    Other(i32),
}

impl HwAccelPixelFormat {
    /// Stable hardware pixel-format name for telemetry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::D3D12 => "D3D12",
            Self::D3D11 => "D3D11",
            Self::D3D11VA => "D3D11VA",
            Self::Dxva2 => "DXVA2",
            Self::VideoToolbox => "VideoToolbox",
            Self::Vaapi => "Vaapi",
            Self::Vdpau => "VDPAU",
            Self::Cuda => "Cuda",
            Self::Other(_) => "Other",
        }
    }

    fn from_ffmpeg(format: ffmpeg::ffi::AVPixelFormat) -> Self {
        #[cfg(mondrian_ffmpeg_7_1)]
        if format == ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_D3D12 {
            return Self::D3D12;
        }
        match format {
            ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_D3D11 => Self::D3D11,
            ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_D3D11VA_VLD => Self::D3D11VA,
            ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_DXVA2_VLD => Self::Dxva2,
            ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX => Self::VideoToolbox,
            ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_VAAPI => Self::Vaapi,
            ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_VDPAU => Self::Vdpau,
            ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_CUDA => Self::Cuda,
            other => Self::Other(other as i32),
        }
    }

    pub(crate) fn to_ffmpeg(self) -> Option<ffmpeg::ffi::AVPixelFormat> {
        match self {
            #[cfg(mondrian_ffmpeg_7_1)]
            Self::D3D12 => Some(ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_D3D12),
            #[cfg(not(mondrian_ffmpeg_7_1))]
            Self::D3D12 => None,
            Self::D3D11 => Some(ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_D3D11),
            Self::D3D11VA => Some(ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_D3D11VA_VLD),
            Self::Dxva2 => Some(ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_DXVA2_VLD),
            Self::VideoToolbox => Some(ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX),
            Self::Vaapi => Some(ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_VAAPI),
            Self::Vdpau => Some(ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_VDPAU),
            Self::Cuda => Some(ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_CUDA),
            Self::Other(_) => None,
        }
    }
}

/// Setup methods advertised by one FFmpeg hardware codec config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct HwAccelCodecConfigMethods {
    /// Config can be initialized from an `AVHWDeviceContext`.
    pub hw_device_ctx: bool,
    /// Config can be initialized from an `AVHWFramesContext`.
    pub hw_frames_ctx: bool,
    /// FFmpeg can initialize this internally.
    pub internal: bool,
    /// Config requires an ad-hoc legacy setup path.
    pub ad_hoc: bool,
}

impl HwAccelCodecConfigMethods {
    fn from_bits(bits: i32) -> Self {
        Self {
            hw_device_ctx: bits & ffmpeg::ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32 != 0,
            hw_frames_ctx: bits & ffmpeg::ffi::AV_CODEC_HW_CONFIG_METHOD_HW_FRAMES_CTX as i32 != 0,
            internal: bits & ffmpeg::ffi::AV_CODEC_HW_CONFIG_METHOD_INTERNAL as i32 != 0,
            ad_hoc: bits & ffmpeg::ffi::AV_CODEC_HW_CONFIG_METHOD_AD_HOC as i32 != 0,
        }
    }
}

/// Read-only FFmpeg codec/backend hardware decode capability probe.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HwAccelCodecConfigProbe {
    /// Backend requested for this probe.
    pub backend: HwAccelBackend,
    /// Whether this backend maps to a known FFmpeg hardware device type.
    pub backend_maps_to_ffmpeg_device: bool,
    /// Whether the linked FFmpeg build lists this hardware device type.
    pub ffmpeg_device_type_available: bool,
    /// Whether FFmpeg has a decoder for the requested codec id.
    pub ffmpeg_decoder_available: bool,
    /// Whether that decoder advertises a hardware config for this backend.
    pub ffmpeg_codec_config_available: bool,
    /// Hardware pixel format advertised by FFmpeg for this config.
    pub hw_pixel_format: Option<HwAccelPixelFormat>,
    /// Setup methods advertised by FFmpeg for this config.
    pub methods: HwAccelCodecConfigMethods,
    /// Stable diagnostic reason for unavailable or partial support.
    pub reason: String,
}

impl HwAccelCodecConfigProbe {
    fn unavailable(backend: HwAccelBackend, reason: impl Into<String>) -> Self {
        Self {
            backend,
            backend_maps_to_ffmpeg_device: backend.to_ffmpeg_device_type().is_some(),
            ffmpeg_device_type_available: false,
            ffmpeg_decoder_available: false,
            ffmpeg_codec_config_available: false,
            hw_pixel_format: None,
            methods: HwAccelCodecConfigMethods::default(),
            reason: reason.into(),
        }
    }
}

/// FFmpeg hardware device context creation probe for a backend.
///
/// This is a runtime capability probe for the local machine and linked FFmpeg
/// build. It creates and immediately releases an `AVHWDeviceContext`; it does
/// not attach that context to a decoder or claim that decoded frames are
/// GPU-resident.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HwAccelDeviceContextProbe {
    /// Backend requested for this probe.
    pub backend: HwAccelBackend,
    /// Whether this backend maps to a known FFmpeg hardware device type.
    pub backend_maps_to_ffmpeg_device: bool,
    /// Whether the linked FFmpeg build lists this hardware device type.
    pub ffmpeg_device_type_available: bool,
    /// Whether Mondrian attempted `av_hwdevice_ctx_create`.
    pub device_create_attempted: bool,
    /// Whether FFmpeg created an `AVHWDeviceContext` for this backend.
    pub device_context_created: bool,
    /// Negative FFmpeg error code returned by device creation, when any.
    pub device_create_error_code: Option<i32>,
    /// Stable diagnostic reason for unavailable or partial support.
    pub reason: String,
}

impl HwAccelDeviceContextProbe {
    pub(crate) fn unavailable(backend: HwAccelBackend, reason: impl Into<String>) -> Self {
        Self {
            backend,
            backend_maps_to_ffmpeg_device: backend.to_ffmpeg_device_type().is_some(),
            ffmpeg_device_type_available: false,
            device_create_attempted: false,
            device_context_created: false,
            device_create_error_code: None,
            reason: reason.into(),
        }
    }

    pub(crate) fn deferred(
        backend: HwAccelBackend,
        ffmpeg_device_type_available: bool,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            backend,
            backend_maps_to_ffmpeg_device: backend.to_ffmpeg_device_type().is_some(),
            ffmpeg_device_type_available,
            device_create_attempted: false,
            device_context_created: false,
            device_create_error_code: None,
            reason: reason.into(),
        }
    }

    pub(crate) fn acquired(
        backend: HwAccelBackend,
        newly_created: bool,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            backend,
            backend_maps_to_ffmpeg_device: true,
            ffmpeg_device_type_available: true,
            device_create_attempted: newly_created,
            device_context_created: true,
            device_create_error_code: None,
            reason: reason.into(),
        }
    }
}

/// Session lease on one worker-family-shared FFmpeg hardware device context.
///
/// The device is shared only for the same backend and renderer-selected
/// adapter. Codec contexts, DPB state, hardware frame pools, and decoded
/// surfaces remain session-owned.
pub(crate) struct HwAccelDeviceContext {
    owner: Arc<SharedHwAccelDeviceContext>,
    pool: HwDeviceContextPool,
    key: HwAccelDeviceProbeKey,
    generation: u64,
    newly_created: bool,
}

/// Immutable owner retained by an explicit hardware-device context pool.
struct SharedHwAccelDeviceContext {
    backend: HwAccelBackend,
    ptr: NonNull<ffmpeg::ffi::AVBufferRef>,
}

impl HwAccelDeviceContext {
    /// Backend used to create this device context.
    pub(crate) fn backend(&self) -> HwAccelBackend {
        self.owner.backend
    }

    /// Whether this acquisition created the current pool generation.
    pub(crate) fn newly_created(&self) -> bool {
        self.newly_created
    }

    /// Whether this lease still names the pool generation offered to new Sessions.
    pub(crate) fn is_current_generation(&self) -> bool {
        self.pool.lock_state().entries.get(&self.key).is_some_and(|entry| {
            entry.generation == self.generation && Arc::ptr_eq(&entry.owner, &self.owner)
        })
    }

    /// Retire this exact generation from future pool acquisitions.
    ///
    /// Other active Sessions remain safe because they retain independent Arc
    /// leases; a later acquisition creates a new generation.
    #[cfg(test)]
    pub(crate) fn retire(&self) {
        self.pool.retire(self.key, self.generation);
    }

    /// Retire this generation and apply a bounded owner-local retry delay
    /// after codec attachment or decoder-open failure.
    pub(crate) fn retire_after_setup_failure(&self, reason: impl Into<String>) {
        self.pool.retire_after_setup_failure(
            self.key,
            self.generation,
            HwAccelDeviceContextProbe::acquired(self.owner.backend, self.newly_created, reason),
        );
    }

    /// Retire this generation and apply a bounded owner-local retry delay
    /// after a runtime decode failure.
    ///
    /// A driver that opens successfully but fails during decode (a "half-bad"
    /// environment common with multi-adapter or hybrid laptops) must not be
    /// recreated on every request: without backoff each failure paid the full
    /// device-creation and decoder-open cost and surfaced a fresh error.
    pub(crate) fn retire_after_runtime_failure(&self, reason: impl Into<String>) {
        self.pool.retire_after_setup_failure(
            self.key,
            self.generation,
            HwAccelDeviceContextProbe::acquired(self.owner.backend, self.newly_created, reason),
        );
    }

    /// Attach a ref-counted hardware device context reference to an unopened
    /// FFmpeg codec context.
    pub(crate) fn attach_to_codec_context(
        &self,
        context: &mut ffmpeg::codec::context::Context,
    ) -> std::result::Result<(), String> {
        let device_ref = unsafe { ffmpeg::ffi::av_buffer_ref(self.owner.ptr.as_ptr()) };
        if device_ref.is_null() {
            return Err(format!(
                "FFmpeg could not retain {} hardware device context",
                self.owner.backend.as_str()
            ));
        }
        unsafe {
            (*context.as_mut_ptr()).hw_device_ctx = device_ref;
        }
        Ok(())
    }

    #[cfg(test)]
    fn shares_device_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.owner, &other.owner)
    }

    #[cfg(test)]
    fn generation(&self) -> u64 {
        self.generation
    }
}

impl Drop for HwAccelDeviceContext {
    fn drop(&mut self) {
        self.pool.release(self.key, self.generation, &self.owner);
    }
}

// SAFETY: FFmpeg documents AVBuffer reference/unreference as thread-safe. The
// AVHWDeviceContext is immutable after initialization; Mondrian exposes no raw
// access or mutation, and each codec receives its own AVBufferRef. Backend
// synchronization remains FFmpeg's responsibility through the initialized
// device context.
unsafe impl Send for SharedHwAccelDeviceContext {}
// SAFETY: See the `Send` justification above. Shared access only creates or
// releases AVBuffer references and never mutates the initialized context.
unsafe impl Sync for SharedHwAccelDeviceContext {}

impl Drop for SharedHwAccelDeviceContext {
    fn drop(&mut self) {
        let mut ptr = self.ptr.as_ptr();
        unsafe {
            ffmpeg::ffi::av_buffer_unref(&mut ptr);
        }
    }
}

impl DecodedGpuFrameHandleKind {
    /// Stable handle-kind name for telemetry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::D3D12Resource => "D3D12Resource",
            Self::D3D11Texture2D => "D3D11Texture2D",
            Self::Dxva2Surface => "Dxva2Surface",
            Self::CVPixelBuffer => "CVPixelBuffer",
            Self::VaapiSurface => "VaapiSurface",
            Self::VdpauVideoSurface => "VdpauVideoSurface",
            Self::CudaDeviceMemory => "CudaDeviceMemory",
        }
    }
}

/// Hardware decode / zero-copy probe result for the current process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HwAccelProbe {
    /// Hardware backend families that should be tried on this platform in
    /// priority order before runtime codec/device validation.
    pub candidate_backends: Vec<HwAccelBackend>,
    /// Hardware backend family that would be preferred on this platform, if a
    /// real decoder adapter is connected.
    pub candidate_backend: Option<HwAccelBackend>,
    /// Native handle family the platform-preferred backend is expected to
    /// produce, if known.
    pub candidate_handle_kind: Option<DecodedGpuFrameHandleKind>,
    /// Native decoded surface formats the platform-preferred backend should
    /// prioritize for GPU-native playback.
    pub candidate_surface_formats: Vec<DecodedVideoSurfaceFormat>,
    /// Whether Mondrian has an implemented decoder adapter for the candidate
    /// backend in this build.
    pub decoder_adapter_available: bool,
    /// Backend that is actually active for the media decode boundary.
    pub selected_backend: HwAccelBackend,
    /// Whether the media decode boundary currently uses a hardware decoder.
    pub hardware_decode_active: bool,
    /// Whether decoded frames currently remain GPU-resident through the media boundary.
    pub zero_copy_active: bool,
    /// Residency produced by the active decode path.
    pub frame_residency: DecodedFrameResidency,
    /// Native handle family produced by the active decoder, if GPU-resident.
    pub gpu_frame_handle_kind: Option<DecodedGpuFrameHandleKind>,
    /// Stable diagnostic reason for the selected path.
    pub reason: String,
}

impl HwAccelBackend {
    /// Return the backend that is actually active for the media decode boundary.
    ///
    /// This intentionally fails closed to `None` until Mondrian has a real
    /// hardware-frame path that exports/imports decoder textures into the
    /// renderer. Platform preference alone must not be reported as active
    /// hardware decode.
    pub fn detect() -> Self {
        Self::probe().selected_backend
    }

    /// Probe the active hardware decode / zero-copy residency state.
    pub fn probe() -> HwAccelProbe {
        let candidate_backends = Self::platform_candidates();
        let candidate_backend = candidate_backends.first().copied();
        HwAccelProbe {
            candidate_backends,
            candidate_backend,
            candidate_handle_kind: candidate_backend.and_then(Self::native_handle_kind),
            candidate_surface_formats: candidate_backend
                .map(Self::preferred_surface_formats)
                .unwrap_or_default(),
            decoder_adapter_available: false,
            selected_backend: Self::None,
            hardware_decode_active: false,
            zero_copy_active: false,
            frame_residency: DecodedFrameResidency::CpuRgba,
            gpu_frame_handle_kind: None,
            reason: hardware_decode_unavailable_reason().to_owned(),
        }
    }

    /// Probe whether the linked FFmpeg decoder advertises a hardware config for
    /// this backend and codec. This is read-only; it does not create a hardware
    /// device or modify the preview decode session.
    pub fn probe_ffmpeg_codec_config(self, codec_id: ffmpeg::codec::Id) -> HwAccelCodecConfigProbe {
        let Some(device_type) = self.to_ffmpeg_device_type() else {
            return HwAccelCodecConfigProbe::unavailable(
                self,
                format!(
                    "{} does not map to an FFmpeg hardware device",
                    self.as_str()
                ),
            );
        };
        let _ = ffmpeg::init();
        let ffmpeg_device_type_available = ffmpeg_hwdevice_type_available(device_type);
        let codec = unsafe { ffmpeg::ffi::avcodec_find_decoder(codec_id.into()) };
        if codec.is_null() {
            return HwAccelCodecConfigProbe {
                backend: self,
                backend_maps_to_ffmpeg_device: true,
                ffmpeg_device_type_available,
                ffmpeg_decoder_available: false,
                ffmpeg_codec_config_available: false,
                hw_pixel_format: None,
                methods: HwAccelCodecConfigMethods::default(),
                reason: format!("FFmpeg decoder for {codec_id:?} is unavailable"),
            };
        }

        let mut index = 0;
        loop {
            let config = unsafe { ffmpeg::ffi::avcodec_get_hw_config(codec, index) };
            if config.is_null() {
                break;
            }
            let config = unsafe { &*config };
            if config.device_type == device_type {
                return HwAccelCodecConfigProbe {
                    backend: self,
                    backend_maps_to_ffmpeg_device: true,
                    ffmpeg_device_type_available,
                    ffmpeg_decoder_available: true,
                    ffmpeg_codec_config_available: true,
                    hw_pixel_format: Some(HwAccelPixelFormat::from_ffmpeg(config.pix_fmt)),
                    methods: HwAccelCodecConfigMethods::from_bits(config.methods),
                    reason: "FFmpeg decoder advertises a hardware config for this backend"
                        .to_owned(),
                };
            }
            index += 1;
        }

        HwAccelCodecConfigProbe {
            backend: self,
            backend_maps_to_ffmpeg_device: true,
            ffmpeg_device_type_available,
            ffmpeg_decoder_available: true,
            ffmpeg_codec_config_available: false,
            hw_pixel_format: None,
            methods: HwAccelCodecConfigMethods::default(),
            reason: format!(
                "FFmpeg decoder for {codec_id:?} does not advertise {} hardware config",
                self.as_str()
            ),
        }
    }

    /// Probe whether FFmpeg can create a hardware device context for this
    /// backend. This creates and immediately releases an `AVHWDeviceContext`;
    /// it does not modify decoder negotiation or allocate hardware frames.
    pub fn probe_ffmpeg_device_context(self) -> HwAccelDeviceContextProbe {
        self.probe_ffmpeg_device_context_for(None)
    }

    fn probe_ffmpeg_device_context_for(
        self,
        selector: Option<HwAccelDeviceSelector>,
    ) -> HwAccelDeviceContextProbe {
        let Some(device_type) = self.to_ffmpeg_device_type() else {
            return HwAccelDeviceContextProbe::unavailable(
                self,
                format!(
                    "{} does not map to an FFmpeg hardware device",
                    self.as_str()
                ),
            );
        };
        let _ = ffmpeg::init();
        let ffmpeg_device_type_available = ffmpeg_hwdevice_type_available(device_type);
        if !ffmpeg_device_type_available {
            return HwAccelDeviceContextProbe {
                backend: self,
                backend_maps_to_ffmpeg_device: true,
                ffmpeg_device_type_available,
                device_create_attempted: false,
                device_context_created: false,
                device_create_error_code: None,
                reason: format!(
                    "linked FFmpeg build does not list {} hardware device type",
                    self.as_str()
                ),
            };
        }

        let mut device_context: *mut ffmpeg::ffi::AVBufferRef = ptr::null_mut();
        let device_name = selector.and_then(|selector| selector.device_name_for(self));
        let result = unsafe {
            ffmpeg::ffi::av_hwdevice_ctx_create(
                &mut device_context,
                device_type,
                device_name.as_ref().map_or(ptr::null(), |name| name.as_ptr()),
                ptr::null_mut(),
                0,
            )
        };
        if result < 0 {
            return HwAccelDeviceContextProbe {
                backend: self,
                backend_maps_to_ffmpeg_device: true,
                ffmpeg_device_type_available,
                device_create_attempted: true,
                device_context_created: false,
                device_create_error_code: Some(result),
                reason: format!(
                    "FFmpeg could not create {} hardware device context: {}",
                    self.as_str(),
                    ffmpeg::Error::from(result)
                ),
            };
        }

        let device_context_created = !device_context.is_null();
        unsafe {
            ffmpeg::ffi::av_buffer_unref(&mut device_context);
        }
        HwAccelDeviceContextProbe {
            backend: self,
            backend_maps_to_ffmpeg_device: true,
            ffmpeg_device_type_available,
            device_create_attempted: true,
            device_context_created,
            device_create_error_code: None,
            reason: if device_context_created {
                format!(
                    "FFmpeg created and released {} hardware device context",
                    self.as_str()
                )
            } else {
                format!(
                    "FFmpeg reported success but returned no {} hardware device context",
                    self.as_str()
                )
            },
        }
    }

    /// Preferred hardware backend for the current platform before runtime
    /// adapter/device validation.
    pub fn platform_candidate() -> Option<Self> {
        Self::platform_candidates().first().copied()
    }

    /// Preferred hardware backends for the current platform in industrial
    /// decode admission order. Runtime codec/device probes may skip an earlier
    /// candidate and fall through to a later one.
    pub fn platform_candidates() -> Vec<Self> {
        #[cfg(target_os = "windows")]
        {
            vec![Self::D3D12VA, Self::D3D11VA, Self::Dxva2]
        }
        #[cfg(target_os = "macos")]
        {
            vec![Self::VideoToolbox]
        }
        #[cfg(target_os = "linux")]
        {
            vec![Self::Vaapi, Self::Cuda, Self::Vdpau]
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        {
            Vec::new()
        }
    }

    /// Native handle family expected from this hardware backend.
    pub fn native_handle_kind(self) -> Option<DecodedGpuFrameHandleKind> {
        match self {
            Self::None => None,
            Self::Cuda => Some(DecodedGpuFrameHandleKind::CudaDeviceMemory),
            Self::D3D12VA => Some(DecodedGpuFrameHandleKind::D3D12Resource),
            Self::D3D11VA => Some(DecodedGpuFrameHandleKind::D3D11Texture2D),
            Self::Dxva2 => Some(DecodedGpuFrameHandleKind::Dxva2Surface),
            Self::VideoToolbox => Some(DecodedGpuFrameHandleKind::CVPixelBuffer),
            Self::Vaapi => Some(DecodedGpuFrameHandleKind::VaapiSurface),
            Self::Vdpau => Some(DecodedGpuFrameHandleKind::VdpauVideoSurface),
        }
    }

    fn to_ffmpeg_device_type(self) -> Option<ffmpeg::ffi::AVHWDeviceType> {
        match self {
            Self::None => None,
            Self::Cuda => Some(ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA),
            #[cfg(mondrian_ffmpeg_7_1)]
            Self::D3D12VA => Some(ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D12VA),
            #[cfg(not(mondrian_ffmpeg_7_1))]
            Self::D3D12VA => None,
            Self::D3D11VA => Some(ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA),
            Self::Dxva2 => Some(ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_DXVA2),
            Self::VideoToolbox => Some(ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX),
            Self::Vaapi => Some(ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI),
            Self::Vdpau => Some(ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VDPAU),
        }
    }

    /// Preferred decoded surface formats for GPU-native playback.
    pub fn preferred_surface_formats(self) -> Vec<DecodedVideoSurfaceFormat> {
        match self {
            Self::None => Vec::new(),
            Self::Dxva2 | Self::Vdpau => Vec::new(),
            Self::D3D12VA => vec![
                DecodedVideoSurfaceFormat::P010,
                DecodedVideoSurfaceFormat::Nv12,
            ],
            Self::D3D11VA => vec![
                DecodedVideoSurfaceFormat::Rgba16Float,
                DecodedVideoSurfaceFormat::Bgra8,
                DecodedVideoSurfaceFormat::Xv36,
                DecodedVideoSurfaceFormat::Xv30,
                DecodedVideoSurfaceFormat::Y212,
                DecodedVideoSurfaceFormat::Y210,
                DecodedVideoSurfaceFormat::P012,
                DecodedVideoSurfaceFormat::P010,
                DecodedVideoSurfaceFormat::Nv12,
            ],
            Self::VideoToolbox => vec![
                DecodedVideoSurfaceFormat::Bgra8,
                DecodedVideoSurfaceFormat::P416,
                DecodedVideoSurfaceFormat::P410,
                DecodedVideoSurfaceFormat::P216,
                DecodedVideoSurfaceFormat::P210,
                DecodedVideoSurfaceFormat::P010,
                DecodedVideoSurfaceFormat::Nv12,
            ],
            Self::Cuda => vec![
                DecodedVideoSurfaceFormat::P016,
                DecodedVideoSurfaceFormat::P010,
                DecodedVideoSurfaceFormat::Nv12,
            ],
            Self::Vaapi => vec![
                DecodedVideoSurfaceFormat::Bgra8,
                DecodedVideoSurfaceFormat::Rgba8,
                DecodedVideoSurfaceFormat::Xv36,
                DecodedVideoSurfaceFormat::Xv30,
                DecodedVideoSurfaceFormat::Y212,
                DecodedVideoSurfaceFormat::Y210,
                DecodedVideoSurfaceFormat::P012,
                DecodedVideoSurfaceFormat::P010,
                DecodedVideoSurfaceFormat::Nv12,
            ],
        }
    }

    /// Stable backend name for telemetry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Cuda => "Cuda",
            Self::D3D12VA => "D3D12VA",
            Self::D3D11VA => "D3D11VA",
            Self::Dxva2 => "DXVA2",
            Self::VideoToolbox => "VideoToolbox",
            Self::Vaapi => "Vaapi",
            Self::Vdpau => "VDPAU",
        }
    }
}

impl DecodedFrameResidency {
    /// Stable residency name for telemetry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CpuRgba => "CpuRgba",
            Self::CpuFloat => "CpuFloat",
            Self::CpuYuv => "CpuYuv",
            Self::GpuTexture => "GpuTexture",
        }
    }
}

impl DecodedVideoSurfaceFormat {
    /// Stable surface-format name for telemetry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Nv12 => "Nv12",
            Self::P010 => "P010",
            Self::P012 => "P012",
            Self::P016 => "P016",
            Self::P210 => "P210",
            Self::P212 => "P212",
            Self::P216 => "P216",
            Self::P410 => "P410",
            Self::P412 => "P412",
            Self::P416 => "P416",
            Self::Y210 => "Y210",
            Self::Y212 => "Y212",
            Self::Xv30 => "Xv30",
            Self::Xv36 => "Xv36",
            Self::Yuv420p => "Yuv420p",
            Self::Yuv420p10le => "Yuv420p10le",
            Self::Yuv420p12le => "Yuv420p12le",
            Self::Yuv422p => "Yuv422p",
            Self::Yuv422p10le => "Yuv422p10le",
            Self::Yuv422p12le => "Yuv422p12le",
            Self::Yuv444p => "Yuv444p",
            Self::Yuv444p10le => "Yuv444p10le",
            Self::Yuv444p12le => "Yuv444p12le",
            Self::Rgba8 => "Rgba8",
            Self::Bgra8 => "Bgra8",
            Self::Rgba16Float => "Rgba16Float",
            Self::Rgba32Float => "Rgba32Float",
            Self::Other => "Other",
        }
    }

    /// Return the complete physical descriptor for a modeled surface format.
    pub const fn descriptor(self) -> Option<DecodedVideoSurfaceDescriptor> {
        use DecodedVideoSurfaceChromaSubsampling::{Cs420, Cs422, Cs444};
        use DecodedVideoSurfaceColorModel::{Rgb, Ycbcr};
        use DecodedVideoSurfaceNumericEncoding::{
            Float16, Float32, PackedUnsigned, Unorm16, Unorm8,
        };
        use DecodedVideoSurfacePlaneLayout::{
            PackedBgra, PackedRgba, PackedYuv422, PackedYuv444, Planar, SemiPlanar,
        };

        let descriptor = match self {
            Self::Unknown | Self::Other => return None,
            Self::Nv12 => DecodedVideoSurfaceDescriptor {
                color_model: Ycbcr,
                chroma_subsampling: Some(Cs420),
                plane_layout: SemiPlanar,
                numeric_encoding: Unorm8,
                component_bit_depth: 8,
                has_alpha: false,
                native_gpu_payload: true,
            },
            Self::P010
            | Self::P012
            | Self::P016
            | Self::P210
            | Self::P212
            | Self::P216
            | Self::P410
            | Self::P412
            | Self::P416 => {
                let (chroma_subsampling, component_bit_depth) = match self {
                    Self::P010 => (Cs420, 10),
                    Self::P012 => (Cs420, 12),
                    Self::P016 => (Cs420, 16),
                    Self::P210 => (Cs422, 10),
                    Self::P212 => (Cs422, 12),
                    Self::P216 => (Cs422, 16),
                    Self::P410 => (Cs444, 10),
                    Self::P412 => (Cs444, 12),
                    Self::P416 => (Cs444, 16),
                    _ => unreachable!(),
                };
                DecodedVideoSurfaceDescriptor {
                    color_model: Ycbcr,
                    chroma_subsampling: Some(chroma_subsampling),
                    plane_layout: SemiPlanar,
                    numeric_encoding: Unorm16 { most_significant_bits: component_bit_depth < 16 },
                    component_bit_depth,
                    has_alpha: false,
                    native_gpu_payload: true,
                }
            }
            Self::Y210 | Self::Y212 | Self::Xv30 | Self::Xv36 => {
                let (chroma_subsampling, plane_layout, component_bit_depth) = match self {
                    Self::Y210 => (Cs422, PackedYuv422, 10),
                    Self::Y212 => (Cs422, PackedYuv422, 12),
                    Self::Xv30 => (Cs444, PackedYuv444, 10),
                    Self::Xv36 => (Cs444, PackedYuv444, 12),
                    _ => unreachable!(),
                };
                DecodedVideoSurfaceDescriptor {
                    color_model: Ycbcr,
                    chroma_subsampling: Some(chroma_subsampling),
                    plane_layout,
                    numeric_encoding: if matches!(self, Self::Xv30) {
                        PackedUnsigned
                    } else {
                        Unorm16 { most_significant_bits: true }
                    },
                    component_bit_depth,
                    has_alpha: false,
                    native_gpu_payload: true,
                }
            }
            Self::Yuv420p
            | Self::Yuv420p10le
            | Self::Yuv420p12le
            | Self::Yuv422p
            | Self::Yuv422p10le
            | Self::Yuv422p12le
            | Self::Yuv444p
            | Self::Yuv444p10le
            | Self::Yuv444p12le => {
                let (chroma_subsampling, component_bit_depth, numeric_encoding) = match self {
                    Self::Yuv420p => (Cs420, 8, Unorm8),
                    Self::Yuv420p10le => (Cs420, 10, Unorm16 { most_significant_bits: false }),
                    Self::Yuv420p12le => (Cs420, 12, Unorm16 { most_significant_bits: false }),
                    Self::Yuv422p => (Cs422, 8, Unorm8),
                    Self::Yuv444p => (Cs444, 8, Unorm8),
                    Self::Yuv444p10le => (Cs444, 10, Unorm16 { most_significant_bits: false }),
                    Self::Yuv444p12le => (Cs444, 12, Unorm16 { most_significant_bits: false }),
                    Self::Yuv422p10le => (Cs422, 10, Unorm16 { most_significant_bits: false }),
                    Self::Yuv422p12le => (Cs422, 12, Unorm16 { most_significant_bits: false }),
                    _ => unreachable!(),
                };
                DecodedVideoSurfaceDescriptor {
                    color_model: Ycbcr,
                    chroma_subsampling: Some(chroma_subsampling),
                    plane_layout: Planar,
                    numeric_encoding,
                    component_bit_depth,
                    has_alpha: false,
                    native_gpu_payload: false,
                }
            }
            Self::Rgba8 | Self::Bgra8 => DecodedVideoSurfaceDescriptor {
                color_model: Rgb,
                chroma_subsampling: None,
                plane_layout: if matches!(self, Self::Rgba8) {
                    PackedRgba
                } else {
                    PackedBgra
                },
                numeric_encoding: Unorm8,
                component_bit_depth: 8,
                has_alpha: true,
                native_gpu_payload: true,
            },
            Self::Rgba16Float | Self::Rgba32Float => DecodedVideoSurfaceDescriptor {
                color_model: Rgb,
                chroma_subsampling: None,
                plane_layout: PackedRgba,
                numeric_encoding: if matches!(self, Self::Rgba16Float) {
                    Float16
                } else {
                    Float32
                },
                component_bit_depth: if matches!(self, Self::Rgba16Float) {
                    16
                } else {
                    32
                },
                has_alpha: true,
                native_gpu_payload: true,
            },
        };
        Some(descriptor)
    }

    /// Whether this decoded surface format can be carried as a native GPU payload.
    pub fn supports_native_gpu_payload(self) -> bool {
        self.descriptor().is_some_and(|descriptor| descriptor.native_gpu_payload)
    }

    /// Effective bit depth for formats with a fixed Mondrian contract.
    pub fn fixed_bit_depth(self) -> Option<u8> {
        self.descriptor().map(|descriptor| descriptor.component_bit_depth)
    }
}

fn hardware_decode_unavailable_reason() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "D3D12VA/D3D11VA hardware decode adapter and texture residency are not connected; using CPU RGBA decode"
    }
    #[cfg(target_os = "macos")]
    {
        "VideoToolbox hardware decode adapter and texture residency are not connected; using CPU RGBA decode"
    }
    #[cfg(target_os = "linux")]
    {
        "VA-API hardware decode adapter and texture residency are not connected; using CPU RGBA decode"
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        "hardware decode texture residency is not connected for this platform; using CPU RGBA decode"
    }
}

fn ffmpeg_hwdevice_type_available(device_type: ffmpeg::ffi::AVHWDeviceType) -> bool {
    let mut previous = ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_NONE;
    loop {
        let next = unsafe { ffmpeg::ffi::av_hwdevice_iterate_types(previous) };
        if next == ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_NONE {
            return false;
        }
        if next == device_type {
            return true;
        }
        previous = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoded_video_range_resolution_keeps_auto_and_honors_override() {
        use mondrian_core::timeline_data::{MediaRangeInterpretation, MediaSignalRange};

        let automatic = DecodedVideoRangeContract::from_interpretation(
            MediaRangeInterpretation::Auto,
            DecodedVideoRange::Limited,
        );
        assert_eq!(
            automatic.resolve_for_frame(DecodedVideoRange::Full),
            DecodedVideoRange::Full
        );
        assert_eq!(
            automatic.resolve_for_frame(DecodedVideoRange::Unknown),
            DecodedVideoRange::Limited
        );
        let override_full = DecodedVideoRangeContract::from_interpretation(
            MediaRangeInterpretation::Override { range: MediaSignalRange::Full },
            DecodedVideoRange::Limited,
        );
        assert_eq!(
            override_full.resolve_for_frame(DecodedVideoRange::Limited),
            DecodedVideoRange::Full
        );

        assert_eq!(
            resolve_decoded_video_range(MediaRangeInterpretation::Auto, DecodedVideoRange::Limited),
            DecodedVideoRange::Limited
        );
        assert_eq!(
            resolve_decoded_video_range(
                MediaRangeInterpretation::Override { range: MediaSignalRange::Full },
                DecodedVideoRange::Unknown
            ),
            DecodedVideoRange::Full
        );
    }

    #[test]
    fn hw_accel_probe_fails_closed_until_texture_residency_exists() {
        let probe = HwAccelBackend::probe();

        assert_eq!(
            probe.candidate_backends,
            HwAccelBackend::platform_candidates()
        );
        assert_eq!(
            probe.candidate_backend,
            HwAccelBackend::platform_candidate()
        );
        assert_eq!(
            probe.candidate_handle_kind,
            probe.candidate_backend.and_then(HwAccelBackend::native_handle_kind)
        );
        assert_eq!(
            probe.candidate_surface_formats,
            probe
                .candidate_backend
                .map(HwAccelBackend::preferred_surface_formats)
                .unwrap_or_default()
        );
        assert!(!probe.decoder_adapter_available);
        assert_eq!(probe.selected_backend, HwAccelBackend::None);
        assert!(!probe.hardware_decode_active);
        assert!(!probe.zero_copy_active);
        assert_eq!(probe.frame_residency, DecodedFrameResidency::CpuRgba);
        assert_eq!(probe.gpu_frame_handle_kind, None);
        assert!(probe.reason.contains("CPU RGBA decode"));
    }

    #[test]
    fn hardware_backend_candidates_map_to_native_handles_and_surface_formats() {
        assert_eq!(
            HwAccelBackend::D3D12VA.native_handle_kind(),
            Some(DecodedGpuFrameHandleKind::D3D12Resource)
        );
        assert_eq!(
            HwAccelBackend::D3D11VA.native_handle_kind(),
            Some(DecodedGpuFrameHandleKind::D3D11Texture2D)
        );
        assert_eq!(
            HwAccelBackend::Dxva2.native_handle_kind(),
            Some(DecodedGpuFrameHandleKind::Dxva2Surface)
        );
        assert_eq!(
            HwAccelBackend::VideoToolbox.native_handle_kind(),
            Some(DecodedGpuFrameHandleKind::CVPixelBuffer)
        );
        assert_eq!(
            HwAccelBackend::Vaapi.native_handle_kind(),
            Some(DecodedGpuFrameHandleKind::VaapiSurface)
        );
        assert_eq!(
            HwAccelBackend::Vdpau.native_handle_kind(),
            Some(DecodedGpuFrameHandleKind::VdpauVideoSurface)
        );
        assert_eq!(
            HwAccelBackend::Cuda.native_handle_kind(),
            Some(DecodedGpuFrameHandleKind::CudaDeviceMemory)
        );
        assert_eq!(HwAccelBackend::None.native_handle_kind(), None);
        assert_eq!(
            HwAccelBackend::D3D12VA.preferred_surface_formats(),
            vec![
                DecodedVideoSurfaceFormat::P010,
                DecodedVideoSurfaceFormat::Nv12
            ]
        );
        assert_eq!(
            HwAccelBackend::D3D11VA.preferred_surface_formats(),
            vec![
                DecodedVideoSurfaceFormat::Rgba16Float,
                DecodedVideoSurfaceFormat::Bgra8,
                DecodedVideoSurfaceFormat::Xv36,
                DecodedVideoSurfaceFormat::Xv30,
                DecodedVideoSurfaceFormat::Y212,
                DecodedVideoSurfaceFormat::Y210,
                DecodedVideoSurfaceFormat::P012,
                DecodedVideoSurfaceFormat::P010,
                DecodedVideoSurfaceFormat::Nv12
            ]
        );
        assert_eq!(
            HwAccelBackend::VideoToolbox.preferred_surface_formats(),
            vec![
                DecodedVideoSurfaceFormat::Bgra8,
                DecodedVideoSurfaceFormat::P416,
                DecodedVideoSurfaceFormat::P410,
                DecodedVideoSurfaceFormat::P216,
                DecodedVideoSurfaceFormat::P210,
                DecodedVideoSurfaceFormat::P010,
                DecodedVideoSurfaceFormat::Nv12,
            ]
        );
        assert_eq!(
            HwAccelBackend::Vaapi.preferred_surface_formats(),
            vec![
                DecodedVideoSurfaceFormat::Bgra8,
                DecodedVideoSurfaceFormat::Rgba8,
                DecodedVideoSurfaceFormat::Xv36,
                DecodedVideoSurfaceFormat::Xv30,
                DecodedVideoSurfaceFormat::Y212,
                DecodedVideoSurfaceFormat::Y210,
                DecodedVideoSurfaceFormat::P012,
                DecodedVideoSurfaceFormat::P010,
                DecodedVideoSurfaceFormat::Nv12,
            ]
        );
        assert_eq!(
            HwAccelBackend::Dxva2.preferred_surface_formats(),
            Vec::new()
        );
        assert_eq!(
            HwAccelBackend::Vdpau.preferred_surface_formats(),
            Vec::new()
        );
    }

    #[test]
    fn hardware_backends_map_to_ffmpeg_device_types() {
        assert_eq!(HwAccelBackend::None.to_ffmpeg_device_type(), None);
        #[cfg(mondrian_ffmpeg_7_1)]
        assert_eq!(
            HwAccelBackend::D3D12VA.to_ffmpeg_device_type(),
            Some(ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D12VA)
        );
        #[cfg(not(mondrian_ffmpeg_7_1))]
        assert_eq!(HwAccelBackend::D3D12VA.to_ffmpeg_device_type(), None);
        assert_eq!(
            HwAccelBackend::D3D11VA.to_ffmpeg_device_type(),
            Some(ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA)
        );
        assert_eq!(
            HwAccelBackend::Dxva2.to_ffmpeg_device_type(),
            Some(ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_DXVA2)
        );
        assert_eq!(
            HwAccelBackend::VideoToolbox.to_ffmpeg_device_type(),
            Some(ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX)
        );
        assert_eq!(
            HwAccelBackend::Vaapi.to_ffmpeg_device_type(),
            Some(ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI)
        );
        assert_eq!(
            HwAccelBackend::Vdpau.to_ffmpeg_device_type(),
            Some(ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VDPAU)
        );
        assert_eq!(
            HwAccelBackend::Cuda.to_ffmpeg_device_type(),
            Some(ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA)
        );
    }

    #[test]
    fn dxgi_device_selector_maps_to_modern_ffmpeg_windows_backends() {
        let d3d12 = HwAccelDeviceSelector::D3D12VaAdapterIndex(7);
        let d3d11 = HwAccelDeviceSelector::D3D11VaAdapterIndex(7);

        assert_eq!(
            d3d11.device_name_for(HwAccelBackend::D3D11VA).as_deref(),
            Some(c"7")
        );
        assert_eq!(
            d3d12.device_name_for(HwAccelBackend::D3D12VA).as_deref(),
            Some(c"7")
        );
        assert_eq!(d3d12.device_name_for(HwAccelBackend::D3D11VA), None);
        assert_eq!(d3d11.device_name_for(HwAccelBackend::D3D12VA), None);
        assert!(d3d12.selects_backend(HwAccelBackend::D3D12VA));
        assert!(d3d11.selects_backend(HwAccelBackend::D3D11VA));
        assert!(!d3d12.selects_backend(HwAccelBackend::D3D11VA));
        assert!(!d3d11.selects_backend(HwAccelBackend::D3D12VA));
        assert_eq!(d3d12.device_name_for(HwAccelBackend::Cuda), None);
        assert!(!d3d12.selects_backend(HwAccelBackend::Cuda));
    }

    #[test]
    fn hw_accel_pixel_format_names_are_stable() {
        assert_eq!(HwAccelPixelFormat::D3D12.as_str(), "D3D12");
        assert_eq!(HwAccelPixelFormat::D3D11.as_str(), "D3D11");
        assert_eq!(HwAccelPixelFormat::D3D11VA.as_str(), "D3D11VA");
        assert_eq!(HwAccelPixelFormat::Dxva2.as_str(), "DXVA2");
        assert_eq!(HwAccelPixelFormat::VideoToolbox.as_str(), "VideoToolbox");
        assert_eq!(HwAccelPixelFormat::Vaapi.as_str(), "Vaapi");
        assert_eq!(HwAccelPixelFormat::Vdpau.as_str(), "VDPAU");
        assert_eq!(HwAccelPixelFormat::Cuda.as_str(), "Cuda");
        assert_eq!(HwAccelPixelFormat::Other(123).as_str(), "Other");
    }

    #[test]
    fn ffmpeg_hw_codec_config_probe_reports_structured_support_for_common_codecs() {
        let backend = HwAccelBackend::platform_candidate().unwrap_or(HwAccelBackend::D3D11VA);
        let h264 = backend.probe_ffmpeg_codec_config(ffmpeg::codec::Id::H264);
        let h265 = backend.probe_ffmpeg_codec_config(ffmpeg::codec::Id::HEVC);

        assert_eq!(h264.backend, backend);
        assert!(h264.backend_maps_to_ffmpeg_device);
        assert!(h264.ffmpeg_decoder_available);
        assert!(!h264.reason.is_empty());
        assert_eq!(h265.backend, backend);
        assert!(h265.ffmpeg_decoder_available);
        assert!(!h265.reason.is_empty());
        if h264.ffmpeg_codec_config_available {
            assert!(h264.hw_pixel_format.is_some());
            assert!(
                h264.methods.hw_device_ctx
                    || h264.methods.hw_frames_ctx
                    || h264.methods.internal
                    || h264.methods.ad_hoc
            );
        }
    }

    #[test]
    fn ffmpeg_hw_device_context_probe_reports_structured_runtime_status() {
        let backend = HwAccelBackend::platform_candidate().unwrap_or(HwAccelBackend::D3D11VA);
        let probe = backend.probe_ffmpeg_device_context();

        assert_eq!(probe.backend, backend);
        assert!(probe.backend_maps_to_ffmpeg_device);
        assert!(!probe.reason.is_empty());
        if probe.ffmpeg_device_type_available {
            assert!(probe.device_create_attempted);
        } else {
            assert!(!probe.device_create_attempted);
        }
        if probe.device_context_created {
            assert_eq!(probe.device_create_error_code, None);
        } else if probe.device_create_attempted {
            assert!(probe.device_create_error_code.is_some());
        }
    }

    #[test]
    fn hardware_device_creation_does_not_hold_pool_state_lock() {
        let pool = HwDeviceContextPool::default();
        let mut invoked = false;
        let result = pool.acquire_available_device(
            HwAccelBackend::Cuda,
            None,
            ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
            |_, _| {
                invoked = true;
                assert!(
                pool.inner.state.try_lock().is_ok(),
                "foreign device initialization must not block pool diagnostics or resource policy"
            );
                assert_eq!(pool.diagnostics().initializing_contexts, 1);
                pool.reconfigure(HwDeviceContextPoolPolicy::new(0));
                (-1, ptr::null_mut())
            },
        );
        assert!(
            invoked,
            "admitted device creation must reach the injected provider"
        );
        assert!(result.is_err());
        assert_eq!(pool.diagnostics().initializing_contexts, 0);
        assert_eq!(pool.diagnostics().policy.max_idle_contexts, 0);
    }

    // These are AVBuffer ownership tests, not fake physical device admission.
    // The bytes are never attached to a codec or interpreted as AVHWDeviceContext.
    fn ownership_test_device_root() -> Arc<SharedHwAccelDeviceContext> {
        let ptr = unsafe { ffmpeg::ffi::av_buffer_alloc(1) };
        Arc::new(SharedHwAccelDeviceContext {
            backend: HwAccelBackend::Cuda,
            ptr: NonNull::new(ptr).expect("ownership fixture allocation"),
        })
    }

    fn observed_ownership_test_device_root(
        pool: &HwDeviceContextPool,
    ) -> (
        Arc<SharedHwAccelDeviceContext>,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        struct Observation {
            pool: std::sync::Weak<HwDeviceContextPoolInner>,
            result: Arc<std::sync::atomic::AtomicUsize>,
        }
        unsafe extern "C" fn release(opaque: *mut std::ffi::c_void, data: *mut u8) {
            // SAFETY: This test allocated both pointers and gives FFmpeg the
            // unique final-release callback. No panic crosses the C boundary.
            let observation = unsafe { Box::from_raw(opaque.cast::<Observation>()) };
            let (unlocked, accounted) = observation.pool.upgrade().map_or((false, false), |pool| {
                pool.state
                    .try_lock()
                    .map_or((false, false), |state| (true, state.retiring_contexts > 0))
            });
            observation.result.store(
                1 | (usize::from(unlocked) << 1) | (usize::from(accounted) << 2),
                std::sync::atomic::Ordering::SeqCst,
            );
            unsafe { ffmpeg::ffi::av_free(data.cast()) };
        }
        let result = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observation = Box::new(Observation {
            pool: Arc::downgrade(&pool.inner),
            result: Arc::clone(&result),
        });
        let data = unsafe { ffmpeg::ffi::av_malloc(1) }.cast::<u8>();
        assert!(!data.is_null(), "test payload allocation");
        let opaque = Box::into_raw(observation).cast();
        let buffer = unsafe { ffmpeg::ffi::av_buffer_create(data, 1, Some(release), opaque, 0) };
        if buffer.is_null() {
            unsafe { release(opaque, data) };
            panic!("test AVBuffer allocation");
        }
        (
            Arc::new(SharedHwAccelDeviceContext {
                backend: HwAccelBackend::Cuda,
                ptr: NonNull::new(buffer).expect("checked AVBuffer allocation"),
            }),
            result,
        )
    }

    #[test]
    fn hardware_device_retirement_does_not_hold_pool_state_lock() {
        for operation in ["release_idle", "reconfigure", "retire", "replace", "create"] {
            let pool = HwDeviceContextPool::new(HwDeviceContextPoolPolicy::new(
                if operation == "create" { 0 } else { 2 },
            ));
            let selector = HwAccelDeviceSelector::CudaDeviceOrdinal(0);
            let (root, observed) = observed_ownership_test_device_root(&pool);
            pool.install_renderer_device_context(
                selector,
                RendererHwAccelDeviceContext { owner: root },
            )
            .expect("install idle root");
            match operation {
                "release_idle" => pool.release_idle(),
                "reconfigure" => pool.reconfigure(HwDeviceContextPoolPolicy::new(0)),
                "retire" => {
                    assert!(pool.retire_renderer_device_context(selector));
                }
                "replace" => {
                    pool.install_renderer_device_context(
                        selector,
                        RendererHwAccelDeviceContext { owner: ownership_test_device_root() },
                    )
                    .expect("replace root");
                }
                "create" => {
                    let root = ownership_test_device_root();
                    let lease = pool
                        .acquire_available_device(
                            HwAccelBackend::Cuda,
                            Some(HwAccelDeviceSelector::CudaDeviceOrdinal(1)),
                            ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
                            |_, _| (0, unsafe { ffmpeg::ffi::av_buffer_ref(root.ptr.as_ptr()) }),
                        )
                        .expect("create another root and trim idle roots");
                    drop(lease);
                }
                _ => unreachable!("fixed operation table"),
            }
            assert_eq!(
                observed.load(std::sync::atomic::Ordering::SeqCst),
                7,
                "foreign root destruction during {operation} must be unlocked and still accounted"
            );
            assert_eq!(pool.diagnostics().retiring_contexts, 0);
        }
    }

    #[test]
    fn hardware_device_creation_cannot_replace_a_later_renderer_generation() {
        for creation_succeeds in [false, true] {
            let pool = HwDeviceContextPool::default();
            let selector = HwAccelDeviceSelector::CudaDeviceOrdinal(0);
            let renderer = ownership_test_device_root();
            let late = ownership_test_device_root();
            let acquired = pool
                .acquire_available_device(
                    HwAccelBackend::Cuda,
                    Some(selector),
                    ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
                    |_, _| {
                        pool.install_renderer_device_context(
                            selector,
                            RendererHwAccelDeviceContext { owner: Arc::clone(&renderer) },
                        )
                        .expect("install superseding renderer generation");
                        assert_eq!(pool.diagnostics().initializing_contexts, 1);
                        if creation_succeeds {
                            (0, unsafe { ffmpeg::ffi::av_buffer_ref(late.ptr.as_ptr()) })
                        } else {
                            (-1, ptr::null_mut())
                        }
                    },
                )
                .expect("acquire the newer installed renderer generation");
            assert!(Arc::ptr_eq(&acquired.owner, &renderer));
            assert_eq!(acquired.generation(), 2);
            assert!(!acquired.newly_created);
            assert_eq!(
                unsafe { ffmpeg::ffi::av_buffer_get_ref_count(late.ptr.as_ptr()) },
                1
            );
            let diagnostics = pool.diagnostics();
            assert_eq!(diagnostics.initializing_contexts, 0);
            assert_eq!(diagnostics.entries, 1);
            assert_eq!(diagnostics.creation_failures, 0);
            assert_eq!(diagnostics.failure_backoffs, 0);
        }
    }

    #[test]
    fn hardware_device_stale_failure_cannot_poison_renderer_replacement() {
        let pool = HwDeviceContextPool::default();
        let selector = HwAccelDeviceSelector::CudaDeviceOrdinal(0);
        pool.install_renderer_device_context(
            selector,
            RendererHwAccelDeviceContext { owner: ownership_test_device_root() },
        )
        .expect("install original root");
        let old = pool
            .acquire_available_device(
                HwAccelBackend::Cuda,
                Some(selector),
                ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
                |_, _| panic!("installed root must be reused"),
            )
            .expect("original lease");
        let replacement = ownership_test_device_root();
        pool.install_renderer_device_context(
            selector,
            RendererHwAccelDeviceContext { owner: Arc::clone(&replacement) },
        )
        .expect("install replacement root");
        old.retire_after_runtime_failure("late failure from an old Session");
        let current = pool
            .acquire_available_device(
                HwAccelBackend::Cuda,
                Some(selector),
                ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
                |_, _| panic!("replacement root must be reused"),
            )
            .expect("old failure must not reject replacement");
        assert!(Arc::ptr_eq(&current.owner, &replacement));
        assert_eq!(pool.diagnostics().failure_backoffs, 0);
    }

    #[test]
    fn hardware_device_creation_retirement_rejects_late_publication() {
        let pool = HwDeviceContextPool::default();
        let selector = HwAccelDeviceSelector::CudaDeviceOrdinal(0);
        let late = ownership_test_device_root();
        let result = pool.acquire_available_device(
            HwAccelBackend::Cuda,
            Some(selector),
            ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
            |_, _| {
                assert!(pool.retire_renderer_device_context(selector));
                assert_eq!(
                    pool.diagnostics().initializing_contexts,
                    1,
                    "revocation must not pretend the foreign owner has already returned"
                );
                (0, unsafe { ffmpeg::ffi::av_buffer_ref(late.ptr.as_ptr()) })
            },
        );
        let rejected = result.err().expect("revoked generation must fail");
        assert!(rejected.reason.contains("retired before publication"));
        assert!(rejected.device_create_attempted);
        assert!(
            rejected.device_context_created,
            "retirement must not erase the fact that the provider created a root"
        );
        assert!(rejected.ffmpeg_device_type_available);
        assert_eq!(
            unsafe { ffmpeg::ffi::av_buffer_get_ref_count(late.ptr.as_ptr()) },
            1
        );
        assert_eq!(pool.diagnostics().initializing_contexts, 0);
        assert_eq!(pool.diagnostics().entries, 0);
    }

    #[test]
    fn hardware_device_creation_panic_releases_initialization_reservation() {
        let pool = HwDeviceContextPool::default();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            pool.acquire_available_device(
                HwAccelBackend::Cuda,
                None,
                ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
                |_, _| panic!("injected provider unwind"),
            )
        }));
        assert!(panic.is_err());
        assert!(!pool.inner.state.is_poisoned());
        assert_eq!(pool.diagnostics().initializing_contexts, 0);
        let result = pool.acquire_available_device(
            HwAccelBackend::Cuda,
            None,
            ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
            |_, _| (-1, ptr::null_mut()),
        );
        assert!(result.is_err());
        assert_eq!(pool.diagnostics().latest_generation, 2);
        assert_eq!(pool.diagnostics().initializing_contexts, 0);
    }

    #[test]
    fn hardware_device_context_pool_reuses_and_safely_retires_generations() {
        let backend = HwAccelBackend::platform_candidate().unwrap_or(HwAccelBackend::D3D11VA);
        let probe = backend.probe_ffmpeg_device_context();
        if !probe.device_context_created {
            return;
        }

        let pool = HwDeviceContextPool::default();
        let first = pool.acquire(backend, None).expect("first device lease");
        let second = pool.acquire(backend, None).expect("second device lease");

        assert!(first.shares_device_with(&second));
        assert_eq!(first.generation(), second.generation());
        first.retire();

        let replacement = pool.acquire(backend, None).expect("replacement device lease");
        assert!(replacement.generation() > first.generation());
        assert!(!first.shares_device_with(&replacement));
        assert!(first.shares_device_with(&second));
        assert_eq!(pool.diagnostics().retirements, 1);

        pool.reconfigure(HwDeviceContextPoolPolicy::new(0));
        drop(first);
        drop(second);
        drop(replacement);
        let diagnostics = pool.diagnostics();
        assert_eq!(diagnostics.entries, 0);
        assert_eq!(diagnostics.idle_contexts, 0);
        assert_eq!(diagnostics.policy.max_idle_contexts, 0);
        assert_eq!(diagnostics.evictions, 1);
    }

    #[test]
    fn hardware_device_context_pool_backs_off_failed_setup_and_can_be_invalidated() {
        let backend = HwAccelBackend::platform_candidate().unwrap_or(HwAccelBackend::D3D11VA);
        let probe = backend.probe_ffmpeg_device_context();
        if !probe.device_context_created {
            return;
        }

        let pool = HwDeviceContextPool::default();
        let lease = pool.acquire(backend, None).expect("device lease");
        lease.retire_after_setup_failure("test decoder-open failure");
        drop(lease);

        let deferred = match pool.acquire(backend, None) {
            Ok(_) => panic!("retry must be delayed"),
            Err(probe) => probe,
        };
        assert!(!deferred.device_create_attempted);
        assert!(deferred.reason.contains("retry deferred"));
        let diagnostics = pool.diagnostics();
        assert_eq!(diagnostics.setup_failures, 1);
        assert_eq!(diagnostics.failure_backoffs, 1);
        assert_eq!(diagnostics.backoff_rejections, 1);

        pool.invalidate_failure_backoff();
        let replacement = pool.acquire(backend, None).expect("explicit invalidation permits retry");
        assert!(replacement.generation() > 0);
    }

    #[test]
    fn mismatched_hardware_device_selector_is_rejected_before_driver_creation() {
        let error = HwDeviceContextPool::default()
            .acquire(
                HwAccelBackend::D3D12VA,
                Some(HwAccelDeviceSelector::D3D11VaAdapterIndex(0)),
            )
            .err()
            .expect("mismatched selector must fail");

        assert!(!error.device_create_attempted);
        assert!(error.reason.contains("does not select D3D12VA"));
    }

    #[test]
    fn decoded_gpu_frame_handle_kind_has_stable_names() {
        assert_eq!(
            DecodedGpuFrameHandleKind::D3D12Resource.as_str(),
            "D3D12Resource"
        );
        assert_eq!(
            DecodedGpuFrameHandleKind::D3D11Texture2D.as_str(),
            "D3D11Texture2D"
        );
        assert_eq!(
            DecodedGpuFrameHandleKind::Dxva2Surface.as_str(),
            "Dxva2Surface"
        );
        assert_eq!(
            DecodedGpuFrameHandleKind::CVPixelBuffer.as_str(),
            "CVPixelBuffer"
        );
        assert_eq!(
            DecodedGpuFrameHandleKind::VaapiSurface.as_str(),
            "VaapiSurface"
        );
        assert_eq!(
            DecodedGpuFrameHandleKind::VdpauVideoSurface.as_str(),
            "VdpauVideoSurface"
        );
        assert_eq!(
            DecodedGpuFrameHandleKind::CudaDeviceMemory.as_str(),
            "CudaDeviceMemory"
        );
    }

    #[test]
    fn decoded_frame_residency_has_stable_names() {
        assert_eq!(DecodedFrameResidency::CpuRgba.as_str(), "CpuRgba");
        assert_eq!(DecodedFrameResidency::CpuFloat.as_str(), "CpuFloat");
        assert_eq!(DecodedFrameResidency::GpuTexture.as_str(), "GpuTexture");
    }

    #[test]
    fn decoded_video_surface_format_has_stable_names() {
        assert_eq!(DecodedVideoSurfaceFormat::Unknown.as_str(), "Unknown");
        assert_eq!(DecodedVideoSurfaceFormat::Nv12.as_str(), "Nv12");
        assert_eq!(DecodedVideoSurfaceFormat::P010.as_str(), "P010");
        assert_eq!(DecodedVideoSurfaceFormat::Yuv420p.as_str(), "Yuv420p");
        assert_eq!(
            DecodedVideoSurfaceFormat::Yuv420p10le.as_str(),
            "Yuv420p10le"
        );
        assert_eq!(DecodedVideoSurfaceFormat::Rgba8.as_str(), "Rgba8");
        assert_eq!(DecodedVideoSurfaceFormat::Bgra8.as_str(), "Bgra8");
        assert_eq!(DecodedVideoSurfaceFormat::Other.as_str(), "Other");
    }

    #[test]
    fn decoded_video_surface_format_declares_native_gpu_payload_support() {
        assert!(DecodedVideoSurfaceFormat::Nv12.supports_native_gpu_payload());
        assert!(DecodedVideoSurfaceFormat::P010.supports_native_gpu_payload());
        assert!(DecodedVideoSurfaceFormat::Rgba8.supports_native_gpu_payload());
        assert!(DecodedVideoSurfaceFormat::Bgra8.supports_native_gpu_payload());
        assert!(!DecodedVideoSurfaceFormat::Unknown.supports_native_gpu_payload());
        assert!(!DecodedVideoSurfaceFormat::Yuv420p.supports_native_gpu_payload());
        assert!(!DecodedVideoSurfaceFormat::Yuv420p10le.supports_native_gpu_payload());
        assert!(!DecodedVideoSurfaceFormat::Other.supports_native_gpu_payload());
    }

    #[test]
    fn native_high_bit_surface_matrix_has_exact_physical_descriptors() {
        use DecodedVideoSurfaceChromaSubsampling::{Cs420, Cs422, Cs444};
        use DecodedVideoSurfaceColorModel::{Rgb, Ycbcr};
        use DecodedVideoSurfacePlaneLayout::{PackedRgba, PackedYuv422, PackedYuv444, SemiPlanar};

        for (format, model, chroma, layout, bits, alpha) in [
            (
                DecodedVideoSurfaceFormat::P012,
                Ycbcr,
                Some(Cs420),
                SemiPlanar,
                12,
                false,
            ),
            (
                DecodedVideoSurfaceFormat::P016,
                Ycbcr,
                Some(Cs420),
                SemiPlanar,
                16,
                false,
            ),
            (
                DecodedVideoSurfaceFormat::P210,
                Ycbcr,
                Some(Cs422),
                SemiPlanar,
                10,
                false,
            ),
            (
                DecodedVideoSurfaceFormat::P212,
                Ycbcr,
                Some(Cs422),
                SemiPlanar,
                12,
                false,
            ),
            (
                DecodedVideoSurfaceFormat::P216,
                Ycbcr,
                Some(Cs422),
                SemiPlanar,
                16,
                false,
            ),
            (
                DecodedVideoSurfaceFormat::P410,
                Ycbcr,
                Some(Cs444),
                SemiPlanar,
                10,
                false,
            ),
            (
                DecodedVideoSurfaceFormat::P412,
                Ycbcr,
                Some(Cs444),
                SemiPlanar,
                12,
                false,
            ),
            (
                DecodedVideoSurfaceFormat::P416,
                Ycbcr,
                Some(Cs444),
                SemiPlanar,
                16,
                false,
            ),
            (
                DecodedVideoSurfaceFormat::Y210,
                Ycbcr,
                Some(Cs422),
                PackedYuv422,
                10,
                false,
            ),
            (
                DecodedVideoSurfaceFormat::Y212,
                Ycbcr,
                Some(Cs422),
                PackedYuv422,
                12,
                false,
            ),
            (
                DecodedVideoSurfaceFormat::Xv30,
                Ycbcr,
                Some(Cs444),
                PackedYuv444,
                10,
                false,
            ),
            (
                DecodedVideoSurfaceFormat::Xv36,
                Ycbcr,
                Some(Cs444),
                PackedYuv444,
                12,
                false,
            ),
            (
                DecodedVideoSurfaceFormat::Rgba16Float,
                Rgb,
                None,
                PackedRgba,
                16,
                true,
            ),
            (
                DecodedVideoSurfaceFormat::Rgba32Float,
                Rgb,
                None,
                PackedRgba,
                32,
                true,
            ),
        ] {
            let descriptor = format.descriptor().expect("modeled native format");
            assert_eq!(descriptor.color_model, model, "{format:?}");
            assert_eq!(descriptor.chroma_subsampling, chroma, "{format:?}");
            assert_eq!(descriptor.plane_layout, layout, "{format:?}");
            assert_eq!(descriptor.component_bit_depth, bits, "{format:?}");
            assert_eq!(descriptor.has_alpha, alpha, "{format:?}");
            assert!(descriptor.native_gpu_payload, "{format:?}");
        }
    }

    #[test]
    fn hw_accel_backend_has_stable_names() {
        assert_eq!(HwAccelBackend::None.as_str(), "None");
        assert_eq!(HwAccelBackend::Cuda.as_str(), "Cuda");
        assert_eq!(HwAccelBackend::D3D12VA.as_str(), "D3D12VA");
        assert_eq!(HwAccelBackend::D3D11VA.as_str(), "D3D11VA");
        assert_eq!(HwAccelBackend::Dxva2.as_str(), "DXVA2");
        assert_eq!(HwAccelBackend::VideoToolbox.as_str(), "VideoToolbox");
        assert_eq!(HwAccelBackend::Vaapi.as_str(), "Vaapi");
        assert_eq!(HwAccelBackend::Vdpau.as_str(), "VDPAU");
    }

    #[test]
    fn linux_device_selectors_never_cross_backend_families() {
        let vaapi = HwAccelDeviceSelector::VaapiDrmRenderNode(129);
        let cuda = HwAccelDeviceSelector::CudaDeviceOrdinal(1);
        assert_eq!(
            vaapi.device_name_for(HwAccelBackend::Vaapi).as_deref(),
            Some(c"/dev/dri/renderD129")
        );
        assert_eq!(
            cuda.device_name_for(HwAccelBackend::Cuda).as_deref(),
            Some(c"1")
        );
        for backend in [
            HwAccelBackend::Vaapi,
            HwAccelBackend::Cuda,
            HwAccelBackend::Vdpau,
            HwAccelBackend::D3D12VA,
        ] {
            assert_eq!(
                vaapi.selects_backend(backend),
                backend == HwAccelBackend::Vaapi
            );
            assert_eq!(
                cuda.selects_backend(backend),
                backend == HwAccelBackend::Cuda
            );
            if backend != HwAccelBackend::Vaapi {
                assert!(vaapi.device_name_for(backend).is_none());
            }
            if backend != HwAccelBackend::Cuda {
                assert!(cuda.device_name_for(backend).is_none());
            }
        }
    }

    #[test]
    fn platform_hardware_backend_candidates_are_ordered_by_expected_native_path() {
        let candidates = HwAccelBackend::platform_candidates();
        #[cfg(target_os = "windows")]
        assert_eq!(
            candidates,
            vec![
                HwAccelBackend::D3D12VA,
                HwAccelBackend::D3D11VA,
                HwAccelBackend::Dxva2
            ]
        );
        #[cfg(target_os = "macos")]
        assert_eq!(candidates, vec![HwAccelBackend::VideoToolbox]);
        #[cfg(target_os = "linux")]
        assert_eq!(
            candidates,
            vec![
                HwAccelBackend::Vaapi,
                HwAccelBackend::Cuda,
                HwAccelBackend::Vdpau
            ]
        );
        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        assert!(candidates.is_empty());
    }
}
