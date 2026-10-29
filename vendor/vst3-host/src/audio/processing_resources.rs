//! Pure geometry and bounded construction for the known audio-buffer family.
//! This is not a grant ledger, authority, full SDK heap or plugin allocation bound.
use super::{
    AudioBusBuffer, AudioBusConfig, AudioBusLayout, AudioLevels, BusAudioBuffers, ChannelLevel,
};
use crate::error::{Error, ProcessingResourceError as Refusal, Result};
use std::alloc::Layout;

/// Immutable opt-in limits, installed before native default activation and setup.
/// The byte quote covers two ProcessData generations, audio activation overlap,
/// owned/raw arrangements, one caller buffer set, two layouts and meter channels.
/// COM payloads, the meter Arc wrapper, parameter storage, metadata text, plugin
/// allocations and allocator bookkeeping are outside this family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessingResourceBounds {
    max_audio_buses_per_direction: usize,
    max_channels_per_bus: usize,
    max_total_declared_channels: usize,
    max_block_frames: usize,
}
fn refused(reason: Refusal) -> Error {
    Error::ProcessingResources(reason)
}
pub(crate) fn add(a: usize, b: usize) -> Result<usize> {
    a.checked_add(b)
        .ok_or_else(|| refused(Refusal::ExtentOverflow))
}
fn mul(a: usize, b: usize) -> Result<usize> {
    a.checked_mul(b)
        .ok_or_else(|| refused(Refusal::ExtentOverflow))
}
fn array<T>(count: usize) -> Result<usize> {
    Layout::array::<T>(count)
        .map(|v| v.size())
        .map_err(|_| refused(Refusal::ExtentOverflow))
}
impl ProcessingResourceBounds {
    /// Validate explicit geometry and its complete checked family byte quote.
    /// Zero buses/channels are allowed; a process block is positive and i32-sized.
    pub fn new(
        max_audio_buses_per_direction: usize,
        max_channels_per_bus: usize,
        max_total_declared_channels: usize,
        max_block_frames: usize,
    ) -> Result<Self> {
        if max_audio_buses_per_direction > i32::MAX as usize
            || max_channels_per_bus > i32::MAX as usize
            || max_block_frames == 0
            || max_block_frames > i32::MAX as usize
        {
            return Err(refused(Refusal::InvalidGeometry));
        }
        let value = Self {
            max_audio_buses_per_direction,
            max_channels_per_bus,
            max_total_declared_channels,
            max_block_frames,
        };
        value.host_buffer_peak_bytes()?;
        Ok(value)
    }
    /// Conservative allocator-requested byte upper bound U=2P+2A+Q+W+2L+M.
    /// P uses f64, W uses f32 including inactive samples, and M includes stereo
    /// fallback. This is one consumer's W, not arbitrary caller allocations.
    pub fn host_buffer_peak_bytes(self) -> Result<usize> {
        use vst3::Steinberg::Vst::AudioBusBuffers;
        let buses = mul(2, self.max_audio_buses_per_direction)?;
        let channels = self
            .max_total_declared_channels
            .min(mul(buses, self.max_channels_per_bus)?);
        let samples = mul(channels, self.max_block_frames)?;
        let p = add(
            crate::internal::plugin_impl::host_process_data_layout().size(),
            add(
                array::<AudioBusBuffers>(buses)?,
                add(
                    array::<Vec<f64>>(channels)?,
                    add(
                        array::<f64>(samples)?,
                        add(array::<Vec<*mut f64>>(buses)?, array::<*mut f64>(channels)?)?,
                    )?,
                )?,
            )?,
        )?;
        let a = array::<bool>(buses)?;
        let q = mul(2, array::<u64>(buses)?)?;
        let w = add(
            array::<AudioBusBuffer>(buses)?,
            add(array::<Vec<f32>>(channels)?, array::<f32>(samples)?)?,
        )?;
        let l = array::<AudioBusConfig>(buses)?;
        let m = array::<ChannelLevel>(self.max_total_declared_channels.max(2))?;
        add(
            mul(2, p)?,
            add(mul(2, a)?, add(q, add(w, add(mul(2, l)?, m)?)?)?)?,
        )
    }
    pub(crate) fn meter_capacity(self) -> usize {
        self.max_total_declared_channels.max(2)
    }
    pub(crate) fn buses(self, actual: usize) -> Result<()> {
        if actual > self.max_audio_buses_per_direction {
            Err(refused(Refusal::BusCount))
        } else {
            Ok(())
        }
    }
    pub(crate) fn native_buses(self, actual: i32) -> Result<usize> {
        let actual = usize::try_from(actual).map_err(|_| refused(Refusal::BusCount))?;
        self.buses(actual)?;
        Ok(actual)
    }
    pub(crate) fn channels(self, actual: usize) -> Result<()> {
        if actual > self.max_channels_per_bus {
            Err(refused(Refusal::ChannelCount))
        } else {
            Ok(())
        }
    }
    pub(crate) fn total(self, actual: usize) -> Result<()> {
        if actual > self.max_total_declared_channels {
            Err(refused(Refusal::TotalChannels))
        } else {
            Ok(())
        }
    }
    pub(crate) fn frames(self, actual: usize) -> Result<()> {
        if actual == 0 || actual > self.max_block_frames {
            Err(refused(Refusal::BlockFrames))
        } else {
            Ok(())
        }
    }
    pub(crate) fn layout(self, value: &AudioBusLayout) -> Result<()> {
        self.buses(value.inputs.len())?;
        self.buses(value.outputs.len())?;
        let mut total = 0;
        for bus in value.inputs.iter().chain(&value.outputs) {
            self.channels(bus.channel_count)?;
            total = add(total, bus.channel_count)?;
            self.total(total)?;
        }
        Ok(())
    }
}
pub(crate) fn vector<T>(count: usize) -> Result<Vec<T>> {
    Layout::array::<T>(count).map_err(|_| refused(Refusal::ExtentOverflow))?;
    let mut value = Vec::new();
    value
        .try_reserve_exact(count)
        .map_err(|_| refused(Refusal::AllocationFailed))?;
    if std::mem::size_of::<T>() != 0 && value.capacity() > count {
        return Err(refused(Refusal::AllocationExtent));
    }
    Ok(value)
}
pub(crate) fn samples<T: Default>(channels: usize, frames: usize) -> Result<Vec<Vec<T>>> {
    let mut result = vector(channels)?;
    for _ in 0..channels {
        let mut samples = vector(frames)?;
        for _ in 0..frames {
            samples.push(T::default());
        }
        result.push(samples);
    }
    Ok(result)
}
pub(crate) fn caller_buffers(
    bounds: ProcessingResourceBounds,
    layout: &AudioBusLayout,
    frames: usize,
    rate: f64,
) -> Result<BusAudioBuffers> {
    bounds.frames(frames)?;
    bounds.layout(layout)?;
    let side = |buses: &[AudioBusConfig]| -> Result<Vec<AudioBusBuffer>> {
        let mut result = vector(buses.len())?;
        for bus in buses {
            result.push(AudioBusBuffer {
                active: bus.active,
                channels: samples(bus.channel_count, frames)?,
            });
        }
        Ok(result)
    };
    Ok(BusAudioBuffers {
        inputs: side(&layout.inputs)?,
        outputs: side(&layout.outputs)?,
        sample_rate: rate,
        block_size: frames,
    })
}
pub(crate) fn reserved_levels(bounds: ProcessingResourceBounds) -> Result<AudioLevels> {
    Ok(AudioLevels {
        channels: vector(bounds.meter_capacity())?,
    })
}
