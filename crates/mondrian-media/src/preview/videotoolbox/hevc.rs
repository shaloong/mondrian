//! Encoded HEVC facts needed by the native packet adapter, not color math.
//!
//! H.265 7.3.2.2 / profile_tier_level prefix and ISO/IEC 14496-15 hvcC framing.
//! The bounded SPS prefix supplies reorder capacity; CoreMedia validates the
//! complete parameter sets and output attachments supply the color contract.

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum HevcContractError {
    #[error("malformed HEVC configuration or access unit")]
    Malformed,
    #[error("native HEVC requires single-layer progressive ten-bit Main10 or Range Extensions")]
    UnsupportedSampling,
    #[error("HEVC SPS visible extent conflicts with source metadata")]
    Extent,
    #[error("HEVC SPS has an invalid decoded-picture/reorder bound")]
    Reorder,
    #[error("in-band HEVC parameter set differs from the owning session")]
    ParameterChanged,
}

#[derive(Debug)]
pub(crate) struct HevcConfiguration {
    pub parameter_sets: Vec<Vec<u8>>,
    pub length_size: usize,
    pub reorder_depth: usize,
    pub chroma_format_idc: u32,
    max_temporal_layers: u32,
}

/// Source metadata admission; the coded SPS must independently agree below.
pub(crate) fn source_chroma_format(profile: i32, pixel_format: i32) -> Option<u32> {
    use ffmpeg_next::ffi;
    let chroma = match pixel_format {
        value if value == ffi::AVPixelFormat::AV_PIX_FMT_YUV420P10LE as i32 => 1,
        value if value == ffi::AVPixelFormat::AV_PIX_FMT_YUV422P10LE as i32 => 2,
        value if value == ffi::AVPixelFormat::AV_PIX_FMT_YUV444P10LE as i32 => 3,
        _ => return None,
    };
    ((profile == ffi::FF_PROFILE_HEVC_MAIN_10 && chroma == 1)
        || profile == ffi::FF_PROFILE_HEVC_REXT)
        .then_some(chroma)
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct HevcPacketFacts {
    pub key: bool,
    pub reference: bool,
}

impl HevcConfiguration {
    pub fn parse(bytes: &[u8], width: u32, height: u32) -> Result<Self, HevcContractError> {
        use HevcContractError as E;
        if bytes.len() < 23 || bytes[0] != 1 {
            return Err(E::Malformed);
        }
        let length_size = usize::from(bytes[21] & 3) + 1;
        if !matches!(length_size, 1 | 2 | 4) {
            return Err(E::Malformed);
        }
        let mut position = 23;
        let mut sets = Vec::new();
        let mut kinds = 0;
        let mut reorder_depth = 0;
        let mut max_temporal_layers = 0;
        let mut chroma_format_idc = None;
        for _ in 0..bytes[22] {
            let kind = *bytes.get(position).ok_or(E::Malformed)? & 63;
            position += 1;
            let count = read_length(bytes, &mut position, 2)?;
            if !matches!(kind, 32..=34 | 39 | 40) {
                return Err(E::Malformed);
            }
            for _ in 0..count {
                let length = read_length(bytes, &mut position, 2)?;
                let end = position.checked_add(length).ok_or(E::Malformed)?;
                let nal = bytes.get(position..end).ok_or(E::Malformed)?;
                let (actual, _) = nal_header(nal)?;
                if actual != kind || sets.len() >= 64 {
                    return Err(E::Malformed);
                }
                if kind == 33 {
                    let facts = sps_prefix(nal, width, height)?;
                    if chroma_format_idc.is_some_and(|chroma| chroma != facts.chroma_format_idc) {
                        return Err(E::UnsupportedSampling);
                    }
                    chroma_format_idc = Some(facts.chroma_format_idc);
                    reorder_depth = reorder_depth.max(facts.reorder_depth);
                    max_temporal_layers = max_temporal_layers.max(facts.max_temporal_layers);
                }
                if (32..=34).contains(&kind) {
                    kinds |= 1 << (kind - 32);
                }
                sets.push(nal.to_vec());
                position = end;
            }
        }
        if position != bytes.len() || kinds != 7 || max_temporal_layers == 0 {
            return Err(E::Malformed);
        }
        Ok(Self {
            parameter_sets: sets,
            length_size,
            reorder_depth,
            chroma_format_idc: chroma_format_idc.ok_or(E::Malformed)?,
            max_temporal_layers,
        })
    }

    pub fn validate_packet(&self, bytes: &[u8]) -> Result<HevcPacketFacts, HevcContractError> {
        use HevcContractError as E;
        let mut position = 0;
        let mut picture = None;
        while position < bytes.len() {
            let length = read_length(bytes, &mut position, self.length_size)?;
            let end = position.checked_add(length).ok_or(E::Malformed)?;
            let nal = bytes.get(position..end).ok_or(E::Malformed)?;
            let (kind, temporal) = nal_header(nal)?;
            if temporal > self.max_temporal_layers {
                return Err(E::UnsupportedSampling);
            }
            if (32..=34).contains(&kind)
                && !self.parameter_sets.iter().any(|set| set.as_slice() == nal)
            {
                return Err(E::ParameterChanged);
            }
            if kind <= 31 {
                // first_slice_segment_in_pic_flag is the first RBSP bit, so
                // no emulation-prevention removal is needed for this byte.
                let first = nal.get(2).ok_or(E::Malformed)? & 0x80 != 0;
                if first {
                    if picture.is_some() {
                        return Err(E::Malformed);
                    }
                    picture = Some(HevcPacketFacts {
                        key: (16..=23).contains(&kind),
                        reference: kind >= 16 || kind & 1 != 0,
                    });
                } else if picture.is_none() {
                    return Err(E::Malformed);
                }
            }
            position = end;
        }
        picture.ok_or(E::Malformed)
    }
}

fn read_length(
    bytes: &[u8],
    position: &mut usize,
    size: usize,
) -> Result<usize, HevcContractError> {
    let end = position.checked_add(size).ok_or(HevcContractError::Malformed)?;
    let slice = bytes.get(*position..end).ok_or(HevcContractError::Malformed)?;
    *position = end;
    Ok(slice.iter().fold(0, |value, byte| (value << 8) | usize::from(*byte)))
}

fn nal_header(nal: &[u8]) -> Result<(u8, u32), HevcContractError> {
    if nal.len() < 2 || nal[0] & 0x80 != 0 || nal[1] & 7 == 0 {
        return Err(HevcContractError::Malformed);
    }
    if nal[0] & 1 != 0 || nal[1] & 0xf8 != 0 {
        return Err(HevcContractError::UnsupportedSampling);
    }
    Ok(((nal[0] >> 1) & 63, u32::from(nal[1] & 7)))
}

struct Bits {
    bytes: Vec<u8>,
    position: usize,
}

impl Bits {
    fn rbsp(nal: &[u8]) -> Result<Self, HevcContractError> {
        let mut bytes = Vec::with_capacity(nal.len());
        let mut zeros = 0;
        for (index, &byte) in nal[2..].iter().enumerate() {
            if zeros == 2 && byte == 3 {
                if nal.get(index + 3).is_none_or(|next| *next > 3) {
                    return Err(HevcContractError::Malformed);
                }
                zeros = 0;
                continue;
            }
            bytes.push(byte);
            zeros = if byte == 0 { (zeros + 1).min(2) } else { 0 };
        }
        Ok(Self { bytes, position: 0 })
    }

    fn read(&mut self, count: usize) -> Result<u32, HevcContractError> {
        if count > 32
            || self.position.checked_add(count).is_none_or(|end| end > self.bytes.len() * 8)
        {
            return Err(HevcContractError::Malformed);
        }
        let mut value = 0;
        for _ in 0..count {
            value = (value << 1)
                | u32::from((self.bytes[self.position / 8] >> (7 - self.position % 8)) & 1);
            self.position += 1;
        }
        Ok(value)
    }

    fn skip(&mut self, count: usize) -> Result<(), HevcContractError> {
        for _ in 0..count {
            self.read(1)?;
        }
        Ok(())
    }

    fn ue(&mut self) -> Result<u32, HevcContractError> {
        let mut zeros = 0;
        while self.read(1)? == 0 {
            zeros += 1;
            if zeros >= 32 {
                return Err(HevcContractError::Malformed);
            }
        }
        Ok(((1u32 << zeros) - 1) + self.read(zeros)?)
    }
}

struct HevcSpsFacts {
    reorder_depth: usize,
    max_temporal_layers: u32,
    chroma_format_idc: u32,
}

fn sps_prefix(nal: &[u8], width: u32, height: u32) -> Result<HevcSpsFacts, HevcContractError> {
    use HevcContractError as E;
    let mut bits = Bits::rbsp(nal)?;
    bits.skip(4)?; // sps_video_parameter_set_id
    let sublayers = bits.read(3)? as usize;
    if sublayers > 6 || bits.read(1)? != 1 {
        return Err(E::UnsupportedSampling);
    }
    // Profile identifiers below belong to the standard namespace.
    if bits.read(2)? != 0 {
        return Err(E::UnsupportedSampling);
    }
    bits.skip(1)?; // general_tier_flag
    let profile = bits.read(5)?;
    if !matches!(profile, 2 | 4) {
        return Err(E::UnsupportedSampling);
    }
    bits.skip(32)?; // general_profile_compatibility_flags
    let progressive = bits.read(1)?;
    let interlaced = bits.read(1)?;
    bits.skip(1)?; // general_non_packed_constraint_flag
    let frame_only = bits.read(1)?;
    if progressive != 1 || interlaced != 0 || frame_only != 1 {
        return Err(E::UnsupportedSampling);
    }
    bits.skip(44 + 8)?; // remaining constraints and general_level_idc
    let mut flags = Vec::with_capacity(sublayers);
    for _ in 0..sublayers {
        flags.push((bits.read(1)?, bits.read(1)?));
    }
    if sublayers > 0 {
        bits.skip((8 - sublayers) * 2)?;
    }
    for (profile, level) in flags {
        if profile != 0 {
            bits.skip(88)?;
        }
        if level != 0 {
            bits.skip(8)?;
        }
    }
    if bits.ue()? > 15 {
        return Err(E::UnsupportedSampling);
    }
    let chroma_format_idc = bits.ue()?;
    if !matches!(chroma_format_idc, 1..=3)
        || (profile == 2 && chroma_format_idc != 1)
        || (chroma_format_idc == 3 && bits.read(1)? != 0)
    {
        return Err(E::UnsupportedSampling);
    }
    let sub_width = if chroma_format_idc == 3 { 1 } else { 2 };
    let sub_height = if chroma_format_idc == 1 { 2 } else { 1 };
    let coded_width = bits.ue()?;
    let coded_height = bits.ue()?;
    let mut crop_width = 0u32;
    let mut crop_height = 0u32;
    if bits.read(1)? != 0 {
        crop_width = bits
            .ue()?
            .checked_add(bits.ue()?)
            .and_then(|v| v.checked_mul(sub_width))
            .ok_or(E::Extent)?;
        crop_height = bits
            .ue()?
            .checked_add(bits.ue()?)
            .and_then(|v| v.checked_mul(sub_height))
            .ok_or(E::Extent)?;
    }
    if coded_width.checked_sub(crop_width) != Some(width)
        || coded_height.checked_sub(crop_height) != Some(height)
    {
        return Err(E::Extent);
    }
    if bits.ue()? != 2 || bits.ue()? != 2 || bits.ue()? > 12 {
        return Err(E::UnsupportedSampling);
    }
    let all_layers = bits.read(1)? != 0;
    let mut reorder = 0;
    for _ in if all_layers { 0 } else { sublayers }..=sublayers {
        let dpb_minus_one = bits.ue()?;
        let layer_reorder = bits.ue()?;
        bits.ue()?; // sps_max_latency_increase_plus1
        if dpb_minus_one > 15 || layer_reorder > dpb_minus_one {
            return Err(E::Reorder);
        }
        reorder = reorder.max(layer_reorder as usize);
    }
    Ok(HevcSpsFacts {
        reorder_depth: reorder,
        max_temporal_layers: sublayers as u32 + 1,
        chroma_format_idc,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real libx265 Main10 progressive 1920x1080 motion/B stream: frozen coded
    // SPS bound is two, independent of the reorder seen in a short sample.
    const VPS: &str = "40010c01ffff022000000300900000030000030078959409";
    const SPS: &str = "420101022000000300900000030000030078a003c08010e4d96565924caf016a12201208000003000800000300f040";
    const PPS: &str = "4401c172b46240";
    const SPS_422: &str = "4201010408000003009d08000003000078b003c08010e4d96565924caf016a12201208000003000800000300f040";
    const SPS_444: &str = "4201010408000003009c0800000300007890007810021c9b2cacb24995e02d424402410000030001000003001e08";

    fn nal(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    fn configuration() -> Vec<u8> {
        let mut bytes = vec![0; 23];
        bytes[0] = 1;
        bytes[21] = 3; // four-byte access-unit lengths
        bytes[22] = 3;
        for (kind, hex) in [(32, VPS), (33, SPS), (34, PPS)] {
            let set = nal(hex);
            bytes.extend([kind, 0, 1]);
            bytes.extend((set.len() as u16).to_be_bytes());
            bytes.extend(set);
        }
        bytes
    }

    fn packet(nals: &[&[u8]]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for nal in nals {
            bytes.extend((nal.len() as u32).to_be_bytes());
            bytes.extend_from_slice(nal);
        }
        bytes
    }

    #[test]
    fn real_sps_bound_is_source_derived_and_configuration_is_bounded() {
        let bytes = configuration();
        let config = HevcConfiguration::parse(&bytes, 1920, 1080).unwrap();
        assert_eq!(config.reorder_depth, 2);
        assert_eq!(config.chroma_format_idc, 1);
        assert_eq!(config.max_temporal_layers, 1);
        for length in 0..bytes.len() {
            assert!(HevcConfiguration::parse(&bytes[..length], 1920, 1080).is_err());
        }
        assert!(matches!(
            HevcConfiguration::parse(&bytes, 1920, 1088),
            Err(HevcContractError::Extent)
        ));
        let mut extra = bytes.clone();
        extra.push(0);
        assert!(HevcConfiguration::parse(&extra, 1920, 1080).is_err());
        let mut sps = nal(SPS);
        sps[3] = 1; // Main rather than Main10 cannot use the ten-bit contract.
        assert!(matches!(
            sps_prefix(&sps, 1920, 1080),
            Err(HevcContractError::UnsupportedSampling)
        ));
        let mut sps = nal(SPS);
        sps[2] &= !1; // temporal nesting proof is required.
        assert!(sps_prefix(&sps, 1920, 1080).is_err());
    }

    #[test]
    fn range_extensions_use_coded_chroma_and_crop_units() {
        for (sps, chroma) in [(SPS_422, 2), (SPS_444, 3)] {
            let bytes = nal(sps);
            let facts = sps_prefix(&bytes, 1920, 1080).unwrap();
            assert_eq!(facts.chroma_format_idc, chroma);
            assert_eq!(facts.reorder_depth, 2);
            assert_eq!(facts.max_temporal_layers, 1);
            let mut nonstandard_namespace = bytes.clone();
            nonstandard_namespace[3] |= 0x40;
            assert!(matches!(
                sps_prefix(&nonstandard_namespace, 1920, 1080),
                Err(HevcContractError::UnsupportedSampling)
            ));
            assert!(matches!(
                sps_prefix(&bytes, 1920, 1088),
                Err(HevcContractError::Extent)
            ));
        }
        let mut config = configuration();
        config[22] += 2;
        for sps in [SPS_422, SPS_444] {
            let bytes = nal(sps);
            config.extend([33, 0, 1]);
            config.extend((bytes.len() as u16).to_be_bytes());
            config.extend(bytes);
        }
        assert!(matches!(
            HevcConfiguration::parse(&config, 1920, 1080),
            Err(HevcContractError::UnsupportedSampling)
        ));
    }

    #[test]
    fn source_metadata_admission_requires_a_ten_bit_profile_and_sampling_pair() {
        use ffmpeg_next::ffi;
        for (pixel, chroma) in [
            (ffi::AVPixelFormat::AV_PIX_FMT_YUV420P10LE, 1),
            (ffi::AVPixelFormat::AV_PIX_FMT_YUV422P10LE, 2),
            (ffi::AVPixelFormat::AV_PIX_FMT_YUV444P10LE, 3),
        ] {
            assert_eq!(
                source_chroma_format(ffi::FF_PROFILE_HEVC_REXT, pixel as i32),
                Some(chroma)
            );
            assert_eq!(
                source_chroma_format(ffi::FF_PROFILE_HEVC_MAIN_10, pixel as i32),
                (chroma == 1).then_some(1)
            );
            assert_eq!(
                source_chroma_format(ffi::FF_PROFILE_UNKNOWN, pixel as i32),
                None
            );
        }
        for pixel in [
            ffi::AVPixelFormat::AV_PIX_FMT_YUV420P12LE,
            ffi::AVPixelFormat::AV_PIX_FMT_YUV422P12LE,
            ffi::AVPixelFormat::AV_PIX_FMT_YUV444P12LE,
            ffi::AVPixelFormat::AV_PIX_FMT_YUV420P,
        ] {
            assert_eq!(
                source_chroma_format(ffi::FF_PROFILE_HEVC_REXT, pixel as i32),
                None
            );
        }
    }

    #[test]
    fn access_units_reject_parameter_changes_multiple_pictures_and_invalid_headers() {
        let config = HevcConfiguration::parse(&configuration(), 1920, 1080).unwrap();
        let idr = [19 << 1, 1, 0x80];
        let nonreference = [0, 1, 0x80];
        let facts = config.validate_packet(&packet(&[&idr])).unwrap();
        assert!(facts.key && facts.reference);
        let facts = config.validate_packet(&packet(&[&nonreference])).unwrap();
        assert!(!facts.key && !facts.reference);
        assert!(config.validate_packet(&packet(&[&idr, &idr])).is_err());
        assert!(config.validate_packet(&packet(&[&[19 << 1, 0, 0x80]])).is_err());
        assert!(config.validate_packet(&packet(&[&[19 << 1, 2, 0x80]])).is_err());
        assert!(config.validate_packet(&packet(&[&[19 << 1 | 1, 1, 0x80]])).is_err());
        let mut changed = nal(SPS);
        *changed.last_mut().unwrap() ^= 1;
        assert!(matches!(
            config.validate_packet(&packet(&[&changed, &idr])),
            Err(HevcContractError::ParameterChanged)
        ));
        let frozen = nal(SPS);
        assert!(config.validate_packet(&packet(&[&frozen, &idr])).is_ok());
        let bytes = packet(&[&idr]);
        for length in 0..bytes.len() {
            assert!(config.validate_packet(&bytes[..length]).is_err());
        }
        assert!(config.validate_packet(&[255, 255, 255, 255]).is_err());
    }
}
