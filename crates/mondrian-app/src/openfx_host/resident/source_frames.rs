//! One invocation's already-resolved Source values. No decoder or time mapping
//! lives here: the graph owner supplies exact native OFX times and image values.
use super::*;
use serde::de::{SeqAccess, Visitor};

#[cfg(test)]
#[path = "source_frames_tests.rs"]
mod tests;

const LIMIT: usize = 256;

/// Borrowed input from the original graph execution, in native OFX coordinates.
#[derive(Clone, Copy)]
pub(crate) struct SourceFrame<'a> {
    pub time: f64,
    pub x: i32,
    pub y: i32,
    pub frame: &'a EffectFloatFrame,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Descriptor {
    time: f64,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    offset: usize,
    bytes: usize,
}

/// No pixel bytes or native addresses occur in control packets.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    path: PathBuf,
    bytes: usize,
    #[serde(deserialize_with = "descriptors")]
    frames: Vec<Descriptor>,
}

pub(super) fn required_optional<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Manifest>, D::Error> {
    Deserialize::deserialize(d)
}

fn descriptors<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Descriptor>, D::Error> {
    struct Bounded;
    impl<'de> Visitor<'de> for Bounded {
        type Value = Vec<Descriptor>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("at most 256 Source frame descriptors")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut frames = Vec::new();
            frames.try_reserve_exact(LIMIT).map_err(serde::de::Error::custom)?;
            if frames.capacity() > LIMIT {
                return Err(serde::de::Error::custom(
                    "Source descriptor capacity exceeds policy",
                ));
            }
            // Ignore untrusted size_hint: storage is bounded before any push.
            while let Some(frame) = seq.next_element::<Descriptor>()? {
                if frames.len() == LIMIT {
                    return Err(serde::de::Error::custom("too many Source frames"));
                }
                frames.push(frame);
            }
            Ok(frames)
        }
    }
    d.deserialize_seq(Bounded)
}

// Charged once in the existing resident owner estimate: both packet descriptor
// vectors and the child's native span, each at its explicit maximum capacity.
pub(crate) const fn storage_bytes() -> usize {
    LIMIT * (2 * std::mem::size_of::<Descriptor>() + std::mem::size_of::<NativeSourceFrame>())
}

fn refusal(bytes: usize) -> OpenFxHostError {
    OpenFxHostError::HostResourceRefused(
        mondrian_effects::EffectProcessorHostResourceRefusal::InputDelivery {
            required_bytes: bytes,
        },
    )
}

fn descriptor(
    source: SourceFrame<'_>,
    offset: usize,
    timing: &OpenFxRenderTiming,
    sampling: mondrian_core::ImageSampling,
    format: mondrian_effects::EffectValueFormat,
) -> Result<Descriptor, OpenFxHostError> {
    let frame = source.frame;
    if frame.format() != format || frame.sampling() != sampling {
        return Err(invalid(
            "Source delivery changes the negotiated image representation",
        ));
    }
    let bytes = validate_frame(frame.width(), frame.height(), frame.pixels().len())?;
    let value = Descriptor {
        time: source.time,
        x: source.x,
        y: source.y,
        width: frame.width(),
        height: frame.height(),
        offset,
        bytes,
    };
    validate_descriptor(&value, timing)?;
    Ok(value)
}

fn validate_descriptor(d: &Descriptor, timing: &OpenFxRenderTiming) -> Result<(), OpenFxHostError> {
    if !d.time.is_finite()
        || d.time == timing.frame
        || d.time < timing.first_frame
        || d.time > timing.last_frame
        || d.width == 0
        || d.height == 0
        || d.width > 8192
        || d.height > 8192
        || i64::from(d.x) + i64::from(d.width) > i64::from(i32::MAX)
        || i64::from(d.y) + i64::from(d.height) > i64::from(i32::MAX)
        || (d.width as usize)
            .checked_mul(d.height as usize)
            .and_then(|n| n.checked_mul(16))
            != Some(d.bytes)
        || d.bytes > MAX_FRAME_BYTES
        || d.offset % 16 != 0
    {
        return Err(invalid(
            "invalid Source delivery time, bounds or byte range",
        ));
    }
    Ok(())
}

/// Own the backing until the matching terminal response or child settlement.
/// The parent does not retain a writable mapping while native code can read it.
#[derive(Debug)]
pub(super) struct Delivery {
    pub manifest: Manifest,
    pub _file: tempfile::NamedTempFile,
}

impl Manifest {
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

impl Delivery {
    pub fn acquire(
        sources: &[SourceFrame<'_>],
        timing: &OpenFxRenderTiming,
        sampling: mondrian_core::ImageSampling,
        format: mondrian_effects::EffectValueFormat,
        memory: &mut dyn FnMut(usize) -> Result<bool, OpenFxHostError>,
        checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
    ) -> Result<Option<Self>, OpenFxHostError> {
        checkpoint()?;
        if sources.is_empty() {
            return Ok(None);
        }
        if sources.len() > LIMIT {
            return Err(invalid("too many Source delivery frames"));
        }
        let mut bytes = 0usize;
        // Validate the complete set before allocation/admission. No partial file
        // or native invocation can escape a bad authored descriptor.
        for (i, source) in sources.iter().enumerate() {
            checkpoint()?;
            let d = descriptor(*source, bytes, timing, sampling, format)?;
            if sources[..i].iter().any(|previous| previous.time == d.time) {
                return Err(invalid("duplicate Source delivery time"));
            }
            bytes = bytes
                .checked_add(d.bytes)
                .filter(|n| *n <= MAX_FRAME_BYTES)
                .ok_or_else(|| invalid("Source delivery exceeds the bounded payload"))?;
        }
        if !memory(bytes)? {
            checkpoint()?;
            return Err(refusal(bytes));
        }
        checkpoint()?;
        let mut frames = Vec::new();
        frames.try_reserve_exact(LIMIT).map_err(|_| refusal(bytes))?;
        if frames.capacity() > LIMIT {
            return Err(refusal(bytes));
        }
        let file = tempfile::NamedTempFile::new().map_err(|_| refusal(bytes))?;
        file.as_file().set_len(bytes as u64).map_err(|_| refusal(bytes))?;
        // SAFETY: exclusive new file; no packet or path has been published.
        let mut mapping = unsafe { memmap2::MmapOptions::new().len(bytes).map_mut(file.as_file()) }
            .map_err(|_| refusal(bytes))?;
        let mut offset = 0;
        for source in sources {
            let d = descriptor(*source, offset, timing, sampling, format)?;
            copy_native_input(
                &mut mapping[offset..offset + d.bytes],
                source.frame.pixels(),
                checkpoint,
            )?;
            offset += d.bytes;
            frames.push(d);
        }
        checkpoint()?;
        drop(mapping);
        Ok(Some(Self {
            manifest: Manifest { path: file.path().to_owned(), bytes, frames },
            _file: file,
        }))
    }
}

/// Pointers borrow this read-only mapping only during the dispatch stack below.
pub(super) struct MappedSources {
    pub frames: Vec<NativeSourceFrame>,
    _mapping: Option<memmap2::Mmap>,
}

impl MappedSources {
    pub fn open(
        manifest: Option<&Manifest>,
        prepare: &Prepare,
        timing: &OpenFxRenderTiming,
        sampling: mondrian_core::ImageSampling,
        checkpoint: &mut dyn FnMut() -> Result<(), OpenFxHostError>,
    ) -> Result<Self, OpenFxHostError> {
        checkpoint()?;
        let Some(manifest) = manifest else {
            return Ok(Self { frames: Vec::new(), _mapping: None });
        };
        if prepare.context != OpenFxContext::Filter
            || manifest.frames.is_empty()
            || manifest.frames.len() > LIMIT
            || manifest.bytes == 0
            || manifest.bytes > MAX_FRAME_BYTES
            || !manifest.path.is_absolute()
        {
            return Err(invalid("invalid Source delivery envelope"));
        }
        let mut offset = 0usize;
        for (i, d) in manifest.frames.iter().enumerate() {
            validate_descriptor(d, timing)?;
            if d.offset != offset
                || manifest.frames[..i].iter().any(|previous| previous.time == d.time)
            {
                return Err(invalid("Source delivery overlaps, gaps or repeats a time"));
            }
            offset = offset
                .checked_add(d.bytes)
                .filter(|n| *n <= manifest.bytes)
                .ok_or_else(|| invalid("Source delivery exceeds its backing"))?;
        }
        if offset != manifest.bytes {
            return Err(invalid("Source delivery has trailing bytes"));
        }
        let file = std::fs::File::open(&manifest.path).map_err(|e| unavailable(e.to_string()))?;
        if file.metadata().map_err(|e| unavailable(e.to_string()))?.len() != manifest.bytes as u64 {
            return Err(invalid("Source delivery backing length differs"));
        }
        // SAFETY: the exclusive parent keeps this immutable file alive until a
        // matching terminal response or process settlement; no native writer.
        let mapping = unsafe { memmap2::MmapOptions::new().len(manifest.bytes).map(&file) }
            .map_err(|e| unavailable(e.to_string()))?;
        let mut frames = Vec::new();
        frames.try_reserve_exact(LIMIT).map_err(|e| unavailable(e.to_string()))?;
        if frames.capacity() > LIMIT {
            return Err(unavailable("native Source span capacity exceeds policy"));
        }
        let alpha = match prepare.input_alpha {
            mondrian_core::ColorFrameAlpha::StraightFloat => 0,
            mondrian_core::ColorFrameAlpha::PremultipliedFloat => 1,
            mondrian_core::ColorFrameAlpha::Opaque => 2,
            _ => return Err(invalid("Source delivery requires negotiated Float alpha")),
        };
        for d in &manifest.frames {
            let pixels =
                bytemuck::try_cast_slice::<u8, [f32; 4]>(&mapping[d.offset..d.offset + d.bytes])
                    .map_err(|_| invalid("unaligned Source delivery pixels"))?;
            // Reject untrusted pixels before even constructing a native plugin.
            for chunk in pixels.chunks(FRAME_TRANSFER_PIXELS) {
                checkpoint()?;
                if chunk
                    .iter()
                    .any(|p| p.iter().any(|c| !c.is_finite()) || (alpha == 2 && p[3] != 1.0))
                {
                    return Err(invalid(
                        "Source delivery violates Float image representation",
                    ));
                }
            }
            frames.push(NativeSourceFrame {
                time: d.time,
                pixels: pixels.as_ptr(),
                bytes: d.bytes,
                x: d.x,
                y: d.y,
                width: d.width as c_int,
                height: d.height as c_int,
                alpha,
                pixel_aspect_ratio: sampling.sample_aspect_ratio().to_f64(),
            });
        }
        checkpoint()?;
        Ok(Self { frames, _mapping: Some(mapping) })
    }
}
