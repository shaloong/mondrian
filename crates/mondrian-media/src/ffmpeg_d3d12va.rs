//! Shared public FFmpeg D3D12VA ABI for decoded and encoder input frames.

use std::ffi::c_void;

#[repr(C)]
pub(crate) struct AvD3D12VaSyncContext {
    pub(crate) fence: *mut c_void,
    pub(crate) event: *mut c_void,
    pub(crate) fence_value: u64,
}

#[repr(C)]
pub(crate) struct AvD3D12VaFrame {
    pub(crate) texture: *mut c_void,
    #[cfg(mondrian_ffmpeg_d3d12va_frame_v2)]
    pub(crate) subresource_index: i32,
    pub(crate) sync_ctx: AvD3D12VaSyncContext,
    #[cfg(mondrian_ffmpeg_d3d12va_frame_v2)]
    pub(crate) flags: i32,
}

// The installed hwcontext_d3d12va.h selects this layout in build.rs. The
// texture-array fields were added after FFmpeg 8.0, not with D3D12VA itself.
// Reading the newer layout on an older runtime interprets the event handle
// as an ID3D12Fence pointer, which cannot safely reach any COM operation.
#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(std::mem::size_of::<AvD3D12VaSyncContext>() == 24);
    assert!(std::mem::offset_of!(AvD3D12VaSyncContext, fence_value) == 16);
    #[cfg(not(mondrian_ffmpeg_d3d12va_frame_v2))]
    {
        assert!(std::mem::size_of::<AvD3D12VaFrame>() == 32);
        assert!(std::mem::offset_of!(AvD3D12VaFrame, sync_ctx) == 8);
    }
    #[cfg(mondrian_ffmpeg_d3d12va_frame_v2)]
    {
        assert!(std::mem::size_of::<AvD3D12VaFrame>() == 48);
        assert!(std::mem::offset_of!(AvD3D12VaFrame, sync_ctx) == 16);
        assert!(std::mem::offset_of!(AvD3D12VaFrame, flags) == 40);
    }
};
