//! Installed-header D3D12VA frame ABI shared by decode and encode.

use std::ffi::c_void;

/// Borrowed native fence and completion event in an FFmpeg-owned frame.
#[repr(C)]
pub(crate) struct AvD3D12VaSyncContext {
    pub(crate) fence: *mut c_void,
    pub(crate) event: *mut c_void,
    pub(crate) fence_value: u64,
}

/// Public hardware-frame descriptor selected from the installed FFmpeg header.
#[repr(C)]
pub(crate) struct AvD3D12VaFrame {
    pub(crate) texture: *mut c_void,
    #[cfg(mondrian_ffmpeg_d3d12va_frame_v2)]
    pub(crate) subresource_index: i32,
    pub(crate) sync_ctx: AvD3D12VaSyncContext,
    #[cfg(mondrian_ffmpeg_d3d12va_frame_v2)]
    pub(crate) flags: i32,
}

// Public ABI from libavutil/hwcontext_d3d12va.h. FFmpeg added texture-array
// fields after 8.0, so build.rs derives the layout from the installed header
// rather than guessing from the libavcodec major version.
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
