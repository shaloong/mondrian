//! Native GPU wake hints; exact wgpu fences retain completion authority.

/// One bounded notification attached to the next Metal queue submission.
///
/// Queue staging is not exclusive to a caller. Another submit can consume the
/// signal early, so this object must never authorize publication or release.
/// Retain the originating progress permit in `capture` until Metal releases its
/// notification block. Dropping the registration revokes any staged signal and
/// signals this private event to retire the block even after device loss.
#[must_use = "retain the wake registration through the originating progress command"]
pub struct GpuCompletionWake {
    #[cfg(target_os = "macos")]
    queue: wgpu::Queue,
    #[cfg(target_os = "macos")]
    event: objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_metal::MTLSharedEvent>>,
}

/// Platform listener owned by one existing device progress generation.
pub struct GpuCompletionWakeSource {
    #[cfg(target_os = "macos")]
    listener: objc2::rc::Retained<objc2_metal::MTLSharedEventListener>,
}

impl Default for GpuCompletionWakeSource {
    fn default() -> Self {
        Self {
            #[cfg(target_os = "macos")]
            listener: objc2_metal::MTLSharedEventListener::new(),
        }
    }
}

impl GpuCompletionWakeSource {
    /// Stage a non-authoritative wake, preserving the existing command stream.
    ///
    /// `capture` must contain only bounded, non-panicking lifetime tokens. It
    /// must not retain pixel resources or a semantic completion callback.
    pub fn stage(
        &self,
        queue: &wgpu::Queue,
        progress_thread: std::thread::Thread,
        capture: impl Send + Sync + 'static,
    ) -> Option<GpuCompletionWake> {
        #[cfg(target_os = "macos")]
        {
            use objc2_metal::{MTLCommandQueue, MTLDevice, MTLSharedEvent};

            // SAFETY: only inspect this queue's live Metal backend. Neither
            // its command buffers nor its handles are ended or destroyed.
            let native = unsafe { queue.as_hal::<wgpu::hal::api::Metal>() }?;
            let event = native.as_raw().device().newSharedEvent()?;
            let listener = self.listener.clone();
            let captured_listener = listener.clone();
            let block = block2::RcBlock::new(
                move |_: std::ptr::NonNull<objc2::runtime::ProtocolObject<dyn MTLSharedEvent>>,
                      _: u64| {
                    let _ = (&capture, &captured_listener);
                    progress_thread.unpark();
                },
            );
            // SAFETY: Metal copies the valid block. Its capture owns the
            // original bounded permit tokens until the copied block drops.
            unsafe {
                event.notifyListener_atValue_block(&listener, 1, block2::RcBlock::as_ptr(&block));
            }
            native.add_signal_event(event.clone(), 1);
            Some(GpuCompletionWake { queue: queue.clone(), event })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (queue, progress_thread, capture);
            None
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for GpuCompletionWake {
    fn drop(&mut self) {
        use objc2_metal::MTLSharedEvent;

        // SAFETY: inspect only the same retained queue. Removing a staged
        // hint and signaling our private event cannot complete a wgpu fence.
        if let Some(native) = unsafe { self.queue.as_hal::<wgpu::hal::api::Metal>() } {
            native.remove_signal_event(&self.event);
        }
        self.event.setSignaledValue(1);
    }
}
