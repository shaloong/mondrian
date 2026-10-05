//! Bounded ownership transfer of existing Viewer device caches for backend preparation.
//!
//! This worker prepares device objects only. It owns no frames, demands,
//! presentation tickets or publication authority. The same cache returns to
//! the Renderer before recording resumes; retirement consumes the worker.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};

use crate::viewer_execution::{NativeVideoInputBackendCache, NativeVideoInputBackendPreparation};
use mondrian_core::{WaveformMode, WorkingColorSpace};

use crate::{
    ColorFrameAlpha, ColorFrameDescriptor, ColorFrameDomain, ColorFrameEncoding,
    ColorFrameResidency, GpuProgramScopesRuntime, GpuProgramScopesRuntimeDiagnostics,
    RenderColorTransformGpuOptions, RenderGpuOutputBoundaryRuntime,
    RenderGpuOutputBoundaryRuntimeDiagnostics, RenderGpuOutputBoundaryRuntimeRecordError,
    RenderMonitorAdaptation, RenderOutputColorBoundary, ViewerGpuOutputPrecision,
};

/// Frozen production contract for preparing Program Output and scopes device objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewerGpuBackendPreparation {
    pub working_color_space: WorkingColorSpace,
    pub program_output_boundary: RenderOutputColorBoundary,
    pub monitor_adaptation: RenderMonitorAdaptation,
    pub output_precision: ViewerGpuOutputPrecision,
    pub signal_monitoring_active: bool,
    /// Compile only the requested scope mode; hidden scopes prepare nothing.
    pub waveform_mode: Option<WaveformMode>,
}

impl ViewerGpuBackendPreparation {
    pub(crate) fn prepare(
        &self,
        runtime: &mut RenderGpuOutputBoundaryRuntime,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<(), RenderGpuOutputBoundaryRuntimeRecordError> {
        let output_texture_format = self.output_precision.program_texture_format(
            &self.monitor_adaptation,
            self.waveform_mode.is_some(),
            self.signal_monitoring_active,
        );
        runtime.prepare_wgpu_output_boundary_gpu_frame_backend(
            &self.program_output_boundary,
            ColorFrameDescriptor {
                width: 1,
                height: 1,
                color_space: self.working_color_space.into(),
                domain: ColorFrameDomain::Working,
                encoding: ColorFrameEncoding::LinearFloat,
                residency: ColorFrameResidency::Gpu,
                alpha: ColorFrameAlpha::StraightCoverage,
            },
            output_texture_format,
            RenderColorTransformGpuOptions::default(),
            device,
            queue,
        )
    }
}

/// Preparation does not make a frame Ready or change a demand deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerGpuBackendPreparationStatus {
    Pending,
    Ready,
}

/// Failure retained against the exact requested preparation contract.
#[derive(Debug, thiserror::Error)]
pub enum ViewerGpuBackendPreparationError {
    #[error("Viewer backend preparation admission is closed")]
    AdmissionClosed,
    #[error("Viewer backend preparation worker could not start: {0}")]
    Start(#[source] std::io::Error),
    #[error("Viewer backend preparation worker lost its cache transport")]
    WorkerDisconnected,
    #[error("Viewer backend preparation violated its single-job capacity")]
    Capacity,
    #[error("Viewer backend preparation failed: {0:?}")]
    Preparation(Box<RenderGpuOutputBoundaryRuntimeRecordError>),
    #[error("Viewer backend preparation panicked: {0}")]
    Panicked(String),
    #[error("Viewer native input backend preparation failed: {0}")]
    NativeInput(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Observed, joined worker termination, independent of GPU queue completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewerBackendPreparationWorkerExit {
    NotStarted,
    Returned,
    Panicked,
}

type PreparationResult = Result<(), Arc<ViewerGpuBackendPreparationError>>;

struct PreparationJob {
    runtime: RenderGpuOutputBoundaryRuntime,
    scopes: GpuProgramScopesRuntime,
    native_cache: Option<NativeVideoInputBackendCache>,
    native_plans: Vec<NativeVideoInputBackendPreparation>,
    plan: ViewerGpuBackendPreparation,
    device: wgpu::Device,
    queue: wgpu::Queue,
    wake: Box<dyn Fn() + Send + Sync>,
}

struct PreparedCache {
    runtime: RenderGpuOutputBoundaryRuntime,
    scopes: GpuProgramScopesRuntime,
    native_cache: Option<NativeVideoInputBackendCache>,
    native_plans: Vec<NativeVideoInputBackendPreparation>,
    plan: ViewerGpuBackendPreparation,
    result: PreparationResult,
}

struct PreparationWorker {
    sender: Option<mpsc::SyncSender<PreparationJob>>,
    receiver: mpsc::Receiver<PreparedCache>,
    thread: Option<JoinHandle<()>>,
}

impl PreparationWorker {
    fn spawn() -> Result<Self, ViewerGpuBackendPreparationError> {
        Self::spawn_with_prepare(|runtime, plan, device, queue| {
            plan.prepare(runtime, device, queue)
                .map_err(|error| ViewerGpuBackendPreparationError::Preparation(Box::new(error)))
        })
    }

    fn spawn_with_prepare(
        mut prepare: impl FnMut(
                &mut RenderGpuOutputBoundaryRuntime,
                &ViewerGpuBackendPreparation,
                &wgpu::Device,
                &wgpu::Queue,
            ) -> Result<(), ViewerGpuBackendPreparationError>
            + Send
            + 'static,
    ) -> Result<Self, ViewerGpuBackendPreparationError> {
        let (sender, requests) = mpsc::sync_channel::<PreparationJob>(1);
        let (responses, receiver) = mpsc::sync_channel(1);
        // Spawn before transferring any cache, preserving it on spawn failure.
        let thread = thread::Builder::new()
            .name("mondrian-viewer-backend".to_owned())
            .spawn(move || {
                while let Ok(mut job) = requests.recv() {
                    let started = std::time::Instant::now();
                    let result = catch_unwind(AssertUnwindSafe(|| {
                        prepare(&mut job.runtime, &job.plan, &job.device, &job.queue)?;
                        if let Some(mode) = job.plan.waveform_mode {
                            job.scopes.prepare_backend(&job.device, mode);
                        }
                        if let Some(cache) = &mut job.native_cache {
                            cache.prepare(&job.native_plans, &job.device, &job.queue).map_err(
                                |error| {
                                    ViewerGpuBackendPreparationError::NativeInput(Box::new(error))
                                },
                            )?;
                        }
                        Ok(())
                    }))
                    .unwrap_or_else(|panic| {
                        let message = panic.downcast_ref::<String>().cloned().or_else(|| {
                            panic.downcast_ref::<&str>().map(|message| (*message).to_owned())
                        });
                        Err(ViewerGpuBackendPreparationError::Panicked(
                            message.unwrap_or_else(|| "non-string panic payload".to_owned()),
                        ))
                    })
                    .map_err(Arc::new);
                    tracing::debug!(
                        preparation_us = started.elapsed().as_micros(),
                        success = result.is_ok(),
                        "Viewer backend preparation completed on cache owner worker"
                    );
                    let prepared = PreparedCache {
                        runtime: job.runtime,
                        scopes: job.scopes,
                        native_cache: job.native_cache,
                        native_plans: job.native_plans,
                        plan: job.plan,
                        result,
                    };
                    if responses.try_send(prepared).is_err() {
                        // The receiver was retired incorrectly or capacity was
                        // violated. Never block a worker on response delivery.
                        break;
                    }
                    let _ = catch_unwind(AssertUnwindSafe(job.wake));
                }
            })
            .map_err(ViewerGpuBackendPreparationError::Start)?;
        Ok(Self {
            sender: Some(sender),
            receiver,
            thread: Some(thread),
        })
    }
}

/// One cache, either local or loaned to one bounded worker job.
pub(crate) struct ViewerBackendOwner {
    runtime: Option<RenderGpuOutputBoundaryRuntime>,
    snapshot: RenderGpuOutputBoundaryRuntimeDiagnostics,
    scopes: Option<GpuProgramScopesRuntime>,
    scopes_snapshot: GpuProgramScopesRuntimeDiagnostics,
    prepared: Option<(ViewerGpuBackendPreparation, PreparationResult)>,
    prepared_native: Vec<NativeVideoInputBackendPreparation>,
    returned_native_cache: Option<NativeVideoInputBackendCache>,
    worker: Option<PreparationWorker>,
    pending: bool,
    retiring: bool,
    terminal: Option<ViewerBackendPreparationWorkerExit>,
}

impl ViewerBackendOwner {
    pub(crate) fn new(runtime: RenderGpuOutputBoundaryRuntime) -> Self {
        let snapshot = runtime.diagnostics();
        Self {
            runtime: Some(runtime),
            snapshot,
            scopes: Some(GpuProgramScopesRuntime::default()),
            scopes_snapshot: GpuProgramScopesRuntimeDiagnostics::default(),
            prepared: None,
            prepared_native: Vec::new(),
            returned_native_cache: None,
            worker: None,
            pending: false,
            retiring: false,
            terminal: None,
        }
    }

    pub(crate) fn runtime(&self) -> Option<&RenderGpuOutputBoundaryRuntime> {
        self.runtime.as_ref()
    }

    pub(crate) fn runtime_mut(&mut self) -> Option<&mut RenderGpuOutputBoundaryRuntime> {
        self.runtime.as_mut()
    }

    pub(crate) fn recording_caches_mut(
        &mut self,
    ) -> Option<(
        &mut RenderGpuOutputBoundaryRuntime,
        &mut GpuProgramScopesRuntime,
    )> {
        Some((self.runtime.as_mut()?, self.scopes.as_mut()?))
    }

    pub(crate) fn scopes_diagnostics(&self) -> GpuProgramScopesRuntimeDiagnostics {
        self.scopes
            .as_ref()
            .map_or(self.scopes_snapshot, GpuProgramScopesRuntime::diagnostics)
    }

    pub(crate) fn diagnostics(&self) -> RenderGpuOutputBoundaryRuntimeDiagnostics {
        self.runtime.as_ref().map_or(self.snapshot, |runtime| runtime.diagnostics())
    }

    pub(crate) fn clear_frame_resources(&mut self) {
        if let Some(runtime) = &mut self.runtime {
            runtime.clear_frame_resources();
        }
        if let Some(scopes) = &mut self.scopes {
            scopes.clear_frame_resources();
        }
        // A loan is admitted only after clearing its frame table. Preparation
        // cannot allocate frames, so no deferred old frame can return here.
    }

    pub(crate) fn prepare_now(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        plan: ViewerGpuBackendPreparation,
    ) -> PreparationResult {
        if self.retiring {
            return Err(Arc::new(ViewerGpuBackendPreparationError::AdmissionClosed));
        }
        let runtime = self
            .runtime
            .as_mut()
            .ok_or_else(|| Arc::new(ViewerGpuBackendPreparationError::Capacity))?;
        let result = plan.prepare(runtime, device, queue).map_err(|error| {
            Arc::new(ViewerGpuBackendPreparationError::Preparation(Box::new(
                error,
            )))
        });
        if result.is_ok()
            && let Some(mode) = plan.waveform_mode
        {
            self.scopes
                .as_mut()
                .expect("local caches are paired")
                .prepare_backend(device, mode);
        }
        self.prepared_native.clear();
        self.prepared = Some((plan, result.clone()));
        result
    }

    #[cfg(test)]
    pub(crate) fn request(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        plan: ViewerGpuBackendPreparation,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<ViewerGpuBackendPreparationStatus, Arc<ViewerGpuBackendPreparationError>> {
        self.request_with_native(device, queue, plan, None, Vec::new(), wake)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn request_with_native(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        plan: ViewerGpuBackendPreparation,
        native_cache: Option<NativeVideoInputBackendCache>,
        native_plans: Vec<NativeVideoInputBackendPreparation>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<ViewerGpuBackendPreparationStatus, Arc<ViewerGpuBackendPreparationError>> {
        if native_cache.is_some() {
            assert!(self.returned_native_cache.is_none());
            self.returned_native_cache = native_cache;
        }
        if self.retiring {
            return Err(Arc::new(ViewerGpuBackendPreparationError::AdmissionClosed));
        }
        self.collect()?;
        if self.pending {
            return Ok(ViewerGpuBackendPreparationStatus::Pending);
        }
        if let Some((prepared, result)) = &self.prepared
            && prepared == &plan
            && self.prepared_native == native_plans
        {
            result.clone()?;
            return Ok(ViewerGpuBackendPreparationStatus::Ready);
        }
        if !native_plans.is_empty() && self.returned_native_cache.is_none() {
            return Err(Arc::new(ViewerGpuBackendPreparationError::Capacity));
        }
        if self.worker.is_none() {
            self.worker = Some(PreparationWorker::spawn().map_err(Arc::new)?);
        }
        let runtime = self
            .runtime
            .as_mut()
            .ok_or_else(|| Arc::new(ViewerGpuBackendPreparationError::WorkerDisconnected))?;
        runtime.clear_frame_resources();
        self.snapshot = runtime.diagnostics();
        let scopes = self.scopes.as_mut().expect("local caches are paired");
        scopes.clear_frame_resources();
        self.scopes_snapshot = scopes.diagnostics();
        let job = PreparationJob {
            runtime: self.runtime.take().expect("local cache checked"),
            scopes: self.scopes.take().expect("local caches are paired"),
            native_cache: self.returned_native_cache.take(),
            native_plans,
            plan,
            device: device.clone(),
            queue: queue.clone(),
            wake: Box::new(wake),
        };
        let sender = self
            .worker
            .as_ref()
            .and_then(|worker| worker.sender.as_ref())
            .expect("live worker sender before retirement");
        match sender.try_send(job) {
            Ok(()) => {
                self.pending = true;
                Ok(ViewerGpuBackendPreparationStatus::Pending)
            }
            Err(error) => {
                let failure = match &error {
                    mpsc::TrySendError::Full(_) => ViewerGpuBackendPreparationError::Capacity,
                    mpsc::TrySendError::Disconnected(_) => {
                        ViewerGpuBackendPreparationError::WorkerDisconnected
                    }
                };
                let job = match error {
                    mpsc::TrySendError::Full(job) | mpsc::TrySendError::Disconnected(job) => job,
                };
                let failure = Arc::new(failure);
                self.prepared = Some((job.plan, Err(Arc::clone(&failure))));
                self.runtime = Some(job.runtime);
                self.scopes = Some(job.scopes);
                self.returned_native_cache = job.native_cache;
                self.prepared_native = job.native_plans;
                Err(failure)
            }
        }
    }

    pub(crate) fn take_returned_native_cache(&mut self) -> Option<NativeVideoInputBackendCache> {
        self.returned_native_cache.take()
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.pending
    }

    pub(crate) fn is_prepared(&self, plan: &ViewerGpuBackendPreparation) -> bool {
        !self.pending
            && self.runtime.is_some()
            && self
                .prepared
                .as_ref()
                .is_some_and(|(prepared, result)| prepared == plan && result.is_ok())
    }

    pub(crate) fn collect(&mut self) -> Result<(), Arc<ViewerGpuBackendPreparationError>> {
        if !self.pending {
            return Ok(());
        }
        let worker = self.worker.as_ref().expect("pending loan owns worker");
        match worker.receiver.try_recv() {
            Ok(prepared) => {
                self.snapshot = prepared.runtime.diagnostics();
                self.runtime = Some(prepared.runtime);
                self.scopes_snapshot = prepared.scopes.diagnostics();
                self.scopes = Some(prepared.scopes);
                self.prepared_native = prepared.native_plans;
                self.returned_native_cache = prepared.native_cache;
                self.prepared = Some((prepared.plan, prepared.result));
                self.pending = false;
                Ok(())
            }
            Err(mpsc::TryRecvError::Empty) => Ok(()),
            Err(mpsc::TryRecvError::Disconnected) => Err(Arc::new(
                ViewerGpuBackendPreparationError::WorkerDisconnected,
            )),
        }
    }

    pub(crate) fn begin_retirement(&mut self) {
        self.retiring = true;
        if let Some(worker) = &mut self.worker {
            worker.sender.take();
        }
    }

    pub(crate) fn poll_retirement(&mut self) -> Option<ViewerBackendPreparationWorkerExit> {
        if let Some(terminal) = self.terminal {
            return Some(terminal);
        }
        self.begin_retirement();
        let cache_lost = self.collect().is_err();
        let terminal = match self.worker.as_mut() {
            None => ViewerBackendPreparationWorkerExit::NotStarted,
            Some(worker) => {
                let thread = worker.thread.as_ref()?;
                if !thread.is_finished() {
                    return None;
                }
                let joined = worker.thread.take().expect("finished worker").join();
                // Drain only after actual termination; a disconnected/lost
                // cache is terminal but never a healthy retirement receipt.
                let cache_lost = self.collect().is_err()
                    || cache_lost
                    || self.runtime.is_none()
                    || self.scopes.is_none();
                if joined.is_ok() && !cache_lost {
                    ViewerBackendPreparationWorkerExit::Returned
                } else {
                    ViewerBackendPreparationWorkerExit::Panicked
                }
            }
        };
        self.terminal = Some(terminal);
        Some(terminal)
    }
}

impl Drop for ViewerBackendOwner {
    fn drop(&mut self) {
        // Normal consuming retirement joins a finished worker first. A last
        // resort Drop closes admission; an already accepted job retains its
        // cache on the non-UI worker until preparation actually returns.
        self.begin_retirement();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{ColorEngine, ColorSpace};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    fn plan(space: ColorSpace) -> ViewerGpuBackendPreparation {
        ViewerGpuBackendPreparation {
            working_color_space: WorkingColorSpace::LinearRec709,
            program_output_boundary: RenderOutputColorBoundary::display(
                space,
                false,
                ColorEngine::mondrian_standard(),
            ),
            monitor_adaptation: RenderMonitorAdaptation::new(
                space,
                space,
                ColorEngine::mondrian_standard(),
            )
            .unwrap(),
            output_precision: ViewerGpuOutputPrecision::Encoded8,
            signal_monitoring_active: false,
            waveform_mode: None,
        }
    }

    fn finish(owner: &mut ViewerBackendOwner) -> ViewerBackendPreparationWorkerExit {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(exit) = owner.poll_retirement() {
                return exit;
            }
            assert!(
                Instant::now() < deadline,
                "backend worker did not consume retirement"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[tokio::test]
    #[ignore = "requires native input caches and a real GPU backend"]
    async fn backend_preparation_returns_the_original_native_input_cache_without_frames() {
        mondrian_core::ensure_mondrian_default_ocio_loaded().unwrap();
        let context = crate::GpuContext::new().await.expect("real backend adapter");
        eprintln!("adapter={:?}", context.adapter.get_info());
        let mut native =
            crate::NativeVideoImportRuntime::new(&context.adapter, &context.device, &context.queue);
        assert!(native.support().renderer_backend_ready);
        let cache = native.take_input_backend_cache().expect("original input cache");
        assert!(
            native.take_input_backend_cache().is_none(),
            "a cache cannot be loaned twice"
        );
        let before = cache.runtime.diagnostics();
        let input = ColorFrameDescriptor {
            width: 16,
            height: 16,
            color_space: ColorSpace::Rec709.into(),
            domain: ColorFrameDomain::Source,
            encoding: ColorFrameEncoding::EncodedFloat,
            residency: ColorFrameResidency::Gpu,
            alpha: ColorFrameAlpha::Opaque,
        };
        let output = ColorFrameDescriptor {
            color_space: WorkingColorSpace::LinearRec709.into(),
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            ..input
        };
        let plans = vec![NativeVideoInputBackendPreparation {
            input,
            output,
            transform: crate::RenderInputTransform::to_working_gpu(
                WorkingColorSpace::LinearRec709,
                true,
                ColorEngine::mondrian_standard(),
            ),
        }];
        let requested = plan(ColorSpace::Rec709);
        let mut owner = ViewerBackendOwner::new(RenderGpuOutputBoundaryRuntime::new().unwrap());
        assert_eq!(
            owner
                .request_with_native(
                    &context.device,
                    &context.queue,
                    requested.clone(),
                    Some(cache),
                    plans.clone(),
                    || {}
                )
                .unwrap(),
            ViewerGpuBackendPreparationStatus::Pending
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while owner
            .request_with_native(
                &context.device,
                &context.queue,
                requested.clone(),
                None,
                plans.clone(),
                || {},
            )
            .unwrap()
            != ViewerGpuBackendPreparationStatus::Ready
        {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        assert!(owner.is_prepared(&requested));
        let mut returned = owner.take_returned_native_cache().expect("same cache returned");
        assert_eq!(
            returned.runtime.diagnostics().next_frame_id,
            before.next_frame_id
        );
        assert_eq!(returned.runtime.diagnostics().frame_table_entries, 0);
        assert_eq!(
            returned.runtime.diagnostics().resource_pool.misses,
            before.resource_pool.misses
        );
        let prepared = returned.runtime.diagnostics();
        let first = returned
            .runtime
            .prepare_fused_yuv_descriptor_backend(
                &plans[0].transform,
                input,
                output,
                (returned.source.0, &returned.source.1),
                &context.device,
                &context.queue,
            )
            .unwrap();
        native.return_input_backend_cache(returned);
        let mut second = native.take_input_backend_cache().unwrap();
        let reused = second
            .runtime
            .prepare_fused_yuv_descriptor_backend(
                &plans[0].transform,
                input,
                output,
                (second.source.0, &second.source.1),
                &context.device,
                &context.queue,
            )
            .unwrap();
        assert!(
            Arc::ptr_eq(&first.pipeline, &reused.pipeline),
            "the exact device pipeline is reused after returning to its backend"
        );
        assert_eq!(
            second.runtime.diagnostics().backend_objects.misses,
            prepared.backend_objects.misses
        );
        let pq_input = ColorFrameDescriptor { color_space: ColorSpace::Rec2100Pq.into(), ..input };
        assert!(!second.runtime.fused_yuv_descriptor_backend_is_cached(
            &plans[0].transform,
            pq_input,
            output,
            second.source.0,
        ));
        second
            .runtime
            .prepare_fused_yuv_descriptor_backend(
                &plans[0].transform,
                pq_input,
                output,
                (second.source.0, &second.source.1),
                &context.device,
                &context.queue,
            )
            .unwrap();
        let before_inspection = second.runtime.diagnostics();
        // Both physical contracts remain prepared; the last worker job is not
        // a cache inventory. Inspection does not touch LRU order or frames.
        for source in [input, pq_input, input] {
            assert!(second.runtime.fused_yuv_descriptor_backend_is_cached(
                &plans[0].transform,
                source,
                output,
                second.source.0,
            ));
        }
        let invalid_output = ColorFrameDescriptor { width: output.width + 1, ..output };
        assert!(!second.runtime.fused_yuv_descriptor_backend_is_cached(
            &plans[0].transform,
            input,
            invalid_output,
            second.source.0,
        ));
        assert_eq!(second.runtime.diagnostics(), before_inspection);
        let pq_plan = NativeVideoInputBackendPreparation { input: pq_input, ..plans[0].clone() };
        native.return_input_backend_cache(second);
        assert!(native.input_backends_are_prepared(&plans));
        assert!(native.input_backends_are_prepared(&[pq_plan]));
        let mut cache = native.take_input_backend_cache().unwrap();
        assert!(
            !native.input_backends_are_prepared(&plans),
            "loaned cache is never ready"
        );
        cache.runtime = RenderGpuOutputBoundaryRuntime::new().unwrap();
        native.return_input_backend_cache(cache);
        assert!(
            !native.input_backends_are_prepared(&plans),
            "new device cache is cold"
        );
        assert_eq!(native.retained_source_count(), 0);
        assert_eq!(
            finish(&mut owner),
            ViewerBackendPreparationWorkerExit::Returned
        );
    }

    #[tokio::test]
    #[ignore = "requires native GPU pipelines for scopes preparation qualification"]
    async fn backend_preparation_returns_scope_modes_without_frame_allocation() {
        mondrian_core::ensure_mondrian_default_ocio_loaded().unwrap();
        let context = crate::GpuContext::new().await.expect("real backend adapter");
        eprintln!("adapter={:?}", context.adapter.get_info());
        let mut owner = ViewerBackendOwner::new(RenderGpuOutputBoundaryRuntime::new().unwrap());
        let before = owner.diagnostics();
        // Hidden panels do not compile scopes; switching each visible mode uses
        // the same worker/cache while buffers and frame publication stay absent.
        let mut luma_pipeline = None;
        for mode in [
            None,
            Some(WaveformMode::Luma),
            Some(WaveformMode::RgbParade),
            Some(WaveformMode::Luma),
        ] {
            let mut requested = plan(ColorSpace::Rec709);
            requested.waveform_mode = mode;
            let deadline = Instant::now() + Duration::from_secs(5);
            while owner
                .request(&context.device, &context.queue, requested.clone(), || {})
                .unwrap()
                != ViewerGpuBackendPreparationStatus::Ready
            {
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(1));
            }
            let scopes = owner.scopes_diagnostics();
            assert_eq!(scopes.pipeline_creations, u64::from(mode.is_some()));
            assert_eq!(scopes.frames_recorded, 0);
            assert_eq!(scopes.buffer_allocations, 0);
            assert_eq!(scopes.texture_allocations, 0);
            assert_eq!(owner.diagnostics().next_frame_id, before.next_frame_id);
            assert_eq!(owner.diagnostics().backend_objects.misses, 1);
            if mode == Some(WaveformMode::Luma) {
                let pipeline = owner
                    .scopes
                    .as_mut()
                    .unwrap()
                    .prepared_pipeline(&context.device, WaveformMode::Luma);
                if let Some(previous) = &luma_pipeline {
                    assert_eq!(
                        previous, &pipeline,
                        "returning to luma must reuse the actual device pipeline"
                    );
                }
                luma_pipeline = Some(pipeline);
            }
        }
        assert_eq!(
            finish(&mut owner),
            ViewerBackendPreparationWorkerExit::Returned
        );
    }

    #[tokio::test]
    #[ignore = "requires a native GPU backend for cache ownership qualification"]
    async fn backend_loan_is_single_and_superseded_contract_cannot_become_ready() {
        mondrian_core::ensure_mondrian_default_ocio_loaded().unwrap();
        let context = crate::GpuContext::new().await.expect("real backend adapter");
        eprintln!("adapter={:?}", context.adapter.get_info());
        let mut owner = ViewerBackendOwner::new(RenderGpuOutputBoundaryRuntime::new().unwrap());
        let before = owner.diagnostics();
        let (release, gate) = mpsc::channel();
        let mut first = true;
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&calls);
        owner.worker = Some(
            PreparationWorker::spawn_with_prepare(move |runtime, plan, device, queue| {
                observed.fetch_add(1, Ordering::SeqCst);
                if first {
                    first = false;
                    gate.recv_timeout(Duration::from_secs(5)).expect("release first preparation");
                }
                plan.prepare(runtime, device, queue)
                    .map_err(|error| ViewerGpuBackendPreparationError::Preparation(Box::new(error)))
            })
            .unwrap(),
        );
        assert_eq!(
            owner
                .request(
                    &context.device,
                    &context.queue,
                    plan(ColorSpace::Rec709),
                    || {}
                )
                .unwrap(),
            ViewerGpuBackendPreparationStatus::Pending
        );
        assert!(
            owner.runtime().is_none(),
            "cache has exactly one worker owner"
        );
        assert_eq!(owner.diagnostics().next_frame_id, before.next_frame_id);
        assert_eq!(
            owner
                .request(
                    &context.device,
                    &context.queue,
                    plan(ColorSpace::DisplayP3),
                    || {}
                )
                .unwrap(),
            ViewerGpuBackendPreparationStatus::Pending
        );
        assert!(!owner.is_prepared(&plan(ColorSpace::DisplayP3)));
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while owner
            .request(
                &context.device,
                &context.queue,
                plan(ColorSpace::DisplayP3),
                || {},
            )
            .unwrap()
            != ViewerGpuBackendPreparationStatus::Ready
        {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "one exact job for each contract"
        );
        let after = owner.diagnostics();
        assert_eq!(after.next_frame_id, before.next_frame_id);
        assert_eq!(after.frame_table_entries, 0);
        assert_eq!(
            after.backend_objects.entries, 2,
            "same production cache retained both prepared contracts"
        );
        assert_eq!(
            owner
                .request(
                    &context.device,
                    &context.queue,
                    plan(ColorSpace::DisplayP3),
                    || {}
                )
                .unwrap(),
            ViewerGpuBackendPreparationStatus::Ready
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            finish(&mut owner),
            ViewerBackendPreparationWorkerExit::Returned
        );
    }

    #[tokio::test]
    #[ignore = "requires a native GPU backend for cache ownership qualification"]
    async fn backend_retirement_cannot_join_or_release_a_pending_cache_early() {
        mondrian_core::ensure_mondrian_default_ocio_loaded().unwrap();
        let context = crate::GpuContext::new().await.expect("real backend adapter");
        eprintln!("adapter={:?}", context.adapter.get_info());
        let mut owner = ViewerBackendOwner::new(RenderGpuOutputBoundaryRuntime::new().unwrap());
        let (release, gate) = mpsc::channel();
        owner.worker = Some(
            PreparationWorker::spawn_with_prepare(move |runtime, plan, device, queue| {
                gate.recv_timeout(Duration::from_secs(5))
                    .expect("release retirement preparation");
                plan.prepare(runtime, device, queue)
                    .map_err(|error| ViewerGpuBackendPreparationError::Preparation(Box::new(error)))
            })
            .unwrap(),
        );
        assert_eq!(
            owner
                .request(
                    &context.device,
                    &context.queue,
                    plan(ColorSpace::Rec709),
                    || {}
                )
                .unwrap(),
            ViewerGpuBackendPreparationStatus::Pending
        );
        owner.begin_retirement();
        assert_eq!(owner.poll_retirement(), None);
        assert!(owner.runtime().is_none());
        assert!(matches!(
            &*owner
                .request(
                    &context.device,
                    &context.queue,
                    plan(ColorSpace::Rec709),
                    || {}
                )
                .unwrap_err(),
            ViewerGpuBackendPreparationError::AdmissionClosed
        ));
        release.send(()).unwrap();
        assert_eq!(
            finish(&mut owner),
            ViewerBackendPreparationWorkerExit::Returned
        );
        assert!(
            owner.runtime().is_some(),
            "cache retained through worker termination and outer queue barrier"
        );
        assert_eq!(
            owner.poll_retirement(),
            Some(ViewerBackendPreparationWorkerExit::Returned)
        );
    }

    #[tokio::test]
    #[ignore = "requires a native GPU backend for cache ownership qualification"]
    async fn failed_preparation_is_retained_and_never_reclassified_ready() {
        let context = crate::GpuContext::new().await.expect("real backend adapter");
        eprintln!("adapter={:?}", context.adapter.get_info());
        let mut owner = ViewerBackendOwner::new(RenderGpuOutputBoundaryRuntime::new().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&calls);
        owner.worker = Some(
            PreparationWorker::spawn_with_prepare(move |_, _, _, _| {
                observed.fetch_add(1, Ordering::SeqCst);
                Err(ViewerGpuBackendPreparationError::Panicked(
                    "injected preparation failure".to_owned(),
                ))
            })
            .unwrap(),
        );
        owner
            .request(
                &context.device,
                &context.queue,
                plan(ColorSpace::Rec709),
                || {},
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match owner.request(
                &context.device,
                &context.queue,
                plan(ColorSpace::Rec709),
                || {},
            ) {
                Ok(ViewerGpuBackendPreparationStatus::Pending) => {}
                Err(error) => {
                    assert!(
                        matches!(&*error, ViewerGpuBackendPreparationError::Panicked(message) if message == "injected preparation failure")
                    );
                    break;
                }
                Ok(ViewerGpuBackendPreparationStatus::Ready) => {
                    panic!("failed backend cannot authorize recording")
                }
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        assert!(owner
            .request(
                &context.device,
                &context.queue,
                plan(ColorSpace::Rec709),
                || {}
            )
            .is_err());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "no retry flood for unchanged failed contract"
        );
        assert!(
            owner.runtime().is_some(),
            "failure did not lose the sole cache"
        );
        assert_eq!(
            finish(&mut owner),
            ViewerBackendPreparationWorkerExit::Returned
        );
    }
}
