//! Job-owned legacy swscale adapter with frame-derived chroma positions.

use super::ffmpeg;
use ffmpeg::software::scaling::{context::Definition, flag::Flags};
use ffmpeg::util::{chroma::Location, format::pixel::Pixel, frame::video::Video};
use std::{ffi::CStr, ptr::NonNull};

struct PreparedScaler {
    pointer: NonNull<ffmpeg::ffi::SwsContext>,
    chroma_location: Location,
}

impl Drop for PreparedScaler {
    fn drop(&mut self) {
        // SAFETY: this owner uniquely owns the allocation from sws_alloc_context.
        unsafe { ffmpeg::ffi::sws_freeContext(self.pointer.as_ptr()) };
    }
}

pub(super) struct PreviewScaler {
    input: Definition,
    output: Definition,
    flags: Flags,
    prepared: Option<PreparedScaler>,
}

impl PreviewScaler {
    pub(super) fn get(
        source_format: Pixel,
        source_width: u32,
        source_height: u32,
        destination_format: Pixel,
        target_width: u32,
        target_height: u32,
        flags: Flags,
    ) -> Result<Self, ffmpeg::Error> {
        if [source_width, source_height, target_width, target_height]
            .into_iter()
            .any(|extent| extent == 0 || extent > i32::MAX as u32)
        {
            return Err(ffmpeg::Error::InvalidData);
        }
        Ok(Self {
            input: Definition {
                format: source_format,
                width: source_width,
                height: source_height,
            },
            output: Definition {
                format: destination_format,
                width: target_width,
                height: target_height,
            },
            flags,
            prepared: None,
        })
    }

    /// Prepare once per observed chroma layout. A new context invalidates its
    /// caller's matrix/range cache; never reinitialize an allocated context.
    pub(super) fn prepare(&mut self, frame: &Video) -> Result<bool, ffmpeg::Error> {
        if frame.format() != self.input.format
            || frame.width() != self.input.width
            || frame.height() != self.input.height
        {
            return Err(ffmpeg::Error::InputChanged);
        }
        let chroma_location = super::frame_contract::cpu_chroma_location_from_frame(frame);
        if self
            .prepared
            .as_ref()
            .is_some_and(|state| state.chroma_location == chroma_location)
        {
            return Ok(false);
        }
        // SAFETY: FFmpeg returns a fresh context, retained by the RAII owner.
        let pointer = NonNull::new(unsafe { ffmpeg::ffi::sws_alloc_context() })
            .ok_or(ffmpeg::Error::InvalidData)?;
        let prepared = PreparedScaler { pointer, chroma_location };
        let set = |name: &CStr, value: i64| -> Result<(), ffmpeg::Error> {
            // SAFETY: the AVOptions target is live and exclusively owned.
            let result = unsafe {
                ffmpeg::ffi::av_opt_set_int(pointer.as_ptr().cast(), name.as_ptr(), value, 0)
            };
            if result < 0 {
                Err(ffmpeg::Error::from(result))
            } else {
                Ok(())
            }
        };
        let source_format: ffmpeg::ffi::AVPixelFormat = self.input.format.into();
        let destination_format: ffmpeg::ffi::AVPixelFormat = self.output.format.into();
        for (name, value) in [
            (c"srcw", i64::from(self.input.width)),
            (c"srch", i64::from(self.input.height)),
            (c"dstw", i64::from(self.output.width)),
            (c"dsth", i64::from(self.output.height)),
            (c"src_format", i64::from(source_format as i32)),
            (c"dst_format", i64::from(destination_format as i32)),
            (c"sws_flags", i64::from(self.flags.bits())),
        ] {
            set(name, value)?;
        }
        if chroma_location != Location::Unspecified {
            let (mut x, mut y) = (0, 0);
            // SAFETY: the output integers are valid and Location maps to a public enum.
            let result = unsafe {
                ffmpeg::ffi::av_chroma_location_enum_to_pos(&mut x, &mut y, chroma_location.into())
            };
            if result < 0 {
                return Err(ffmpeg::Error::from(result));
            }
            for (format, horizontal, vertical) in [
                (self.input.format, c"src_h_chr_pos", c"src_v_chr_pos"),
                (self.output.format, c"dst_h_chr_pos", c"dst_v_chr_pos"),
            ] {
                if let Some(descriptor) = format.descriptor() {
                    // SAFETY: FFmpeg owns this immutable pixel-format descriptor.
                    let descriptor = unsafe { &*descriptor.as_ptr() };
                    if descriptor.log2_chroma_w > 0 {
                        set(horizontal, i64::from(x))?;
                    }
                    if descriptor.log2_chroma_h > 0 {
                        set(vertical, i64::from(y))?;
                    }
                }
            }
        }
        // SAFETY: all options precede the allocation's only initialization.
        let result = unsafe {
            ffmpeg::ffi::sws_init_context(
                pointer.as_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if result < 0 {
            return Err(ffmpeg::Error::from(result));
        }
        self.prepared = Some(prepared);
        Ok(true)
    }

    pub(super) fn as_mut_ptr(&mut self) -> *mut ffmpeg::ffi::SwsContext {
        self.prepared
            .as_ref()
            .expect("frame must prepare the scaler first")
            .pointer
            .as_ptr()
    }

    pub(super) fn input(&self) -> &Definition {
        &self.input
    }

    pub(super) fn output(&self) -> &Definition {
        &self.output
    }

    pub(super) fn run(&mut self, input: &Video, output: &mut Video) -> Result<(), ffmpeg::Error> {
        self.prepare(input)?;
        // SAFETY: synchronous scaling borrows live frames and the exclusive context.
        let rows = unsafe {
            if output.is_empty() {
                output.alloc(self.output.format, self.output.width, self.output.height);
            }
            if output.format() != self.output.format
                || output.width() != self.output.width
                || output.height() != self.output.height
            {
                return Err(ffmpeg::Error::OutputChanged);
            }
            ffmpeg::ffi::sws_scale(
                self.as_mut_ptr(),
                (*input.as_ptr()).data.as_ptr().cast(),
                (*input.as_ptr()).linesize.as_ptr(),
                0,
                self.input.height as i32,
                (*output.as_mut_ptr()).data.as_ptr(),
                (*output.as_mut_ptr()).linesize.as_ptr(),
            )
        };
        if rows == self.output.height as i32 {
            Ok(())
        } else if rows < 0 {
            Err(ffmpeg::Error::from(rows))
        } else {
            Err(ffmpeg::Error::InvalidData)
        }
    }
}
