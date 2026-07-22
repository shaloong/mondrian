//! Fail-closed decoding of FFmpeg integer enums carried by demux IPC.

use super::demux_protocol::invalid_data;
use ffmpeg_next as ffmpeg;
use std::io;

macro_rules! decode_ffi_enum {
    ($value:expr, $label:literal, $($variant:path),+ $(,)?) => {{
        match $value {
            $(value if value == $variant as i32 => Ok($variant),)+
            value => Err(invalid_data(format!("invalid {} value {value}", $label))),
        }
    }};
}

pub(super) fn decode_field_order(value: i32) -> io::Result<ffmpeg::ffi::AVFieldOrder> {
    use ffmpeg::ffi::AVFieldOrder::*;
    decode_ffi_enum!(
        value,
        "field order",
        AV_FIELD_UNKNOWN,
        AV_FIELD_PROGRESSIVE,
        AV_FIELD_TT,
        AV_FIELD_BB,
        AV_FIELD_TB,
        AV_FIELD_BT,
    )
}

pub(super) fn decode_color_range(value: i32) -> io::Result<ffmpeg::ffi::AVColorRange> {
    use ffmpeg::ffi::AVColorRange::*;
    decode_ffi_enum!(
        value,
        "color range",
        AVCOL_RANGE_UNSPECIFIED,
        AVCOL_RANGE_MPEG,
        AVCOL_RANGE_JPEG
    )
}

pub(super) fn decode_color_primaries(value: i32) -> io::Result<ffmpeg::ffi::AVColorPrimaries> {
    use ffmpeg::ffi::AVColorPrimaries::*;
    decode_ffi_enum!(
        value,
        "color primaries",
        AVCOL_PRI_RESERVED0,
        AVCOL_PRI_BT709,
        AVCOL_PRI_UNSPECIFIED,
        AVCOL_PRI_RESERVED,
        AVCOL_PRI_BT470M,
        AVCOL_PRI_BT470BG,
        AVCOL_PRI_SMPTE170M,
        AVCOL_PRI_SMPTE240M,
        AVCOL_PRI_FILM,
        AVCOL_PRI_BT2020,
        AVCOL_PRI_SMPTE428,
        AVCOL_PRI_SMPTE431,
        AVCOL_PRI_SMPTE432,
        AVCOL_PRI_EBU3213,
    )
}

pub(super) fn decode_color_trc(
    value: i32,
) -> io::Result<ffmpeg::ffi::AVColorTransferCharacteristic> {
    use ffmpeg::ffi::AVColorTransferCharacteristic::*;
    decode_ffi_enum!(
        value,
        "color transfer",
        AVCOL_TRC_RESERVED0,
        AVCOL_TRC_BT709,
        AVCOL_TRC_UNSPECIFIED,
        AVCOL_TRC_RESERVED,
        AVCOL_TRC_GAMMA22,
        AVCOL_TRC_GAMMA28,
        AVCOL_TRC_SMPTE170M,
        AVCOL_TRC_SMPTE240M,
        AVCOL_TRC_LINEAR,
        AVCOL_TRC_LOG,
        AVCOL_TRC_LOG_SQRT,
        AVCOL_TRC_IEC61966_2_4,
        AVCOL_TRC_BT1361_ECG,
        AVCOL_TRC_IEC61966_2_1,
        AVCOL_TRC_BT2020_10,
        AVCOL_TRC_BT2020_12,
        AVCOL_TRC_SMPTE2084,
        AVCOL_TRC_SMPTE428,
        AVCOL_TRC_ARIB_STD_B67,
    )
}

pub(super) fn decode_color_space(value: i32) -> io::Result<ffmpeg::ffi::AVColorSpace> {
    use ffmpeg::ffi::AVColorSpace::*;
    decode_ffi_enum!(
        value,
        "color space",
        AVCOL_SPC_RGB,
        AVCOL_SPC_BT709,
        AVCOL_SPC_UNSPECIFIED,
        AVCOL_SPC_RESERVED,
        AVCOL_SPC_FCC,
        AVCOL_SPC_BT470BG,
        AVCOL_SPC_SMPTE170M,
        AVCOL_SPC_SMPTE240M,
        AVCOL_SPC_YCGCO,
        AVCOL_SPC_BT2020_NCL,
        AVCOL_SPC_BT2020_CL,
        AVCOL_SPC_SMPTE2085,
        AVCOL_SPC_CHROMA_DERIVED_NCL,
        AVCOL_SPC_CHROMA_DERIVED_CL,
        AVCOL_SPC_ICTCP,
        AVCOL_SPC_IPT_C2,
        AVCOL_SPC_YCGCO_RE,
        AVCOL_SPC_YCGCO_RO,
    )
}

pub(super) fn decode_chroma_location(value: i32) -> io::Result<ffmpeg::ffi::AVChromaLocation> {
    use ffmpeg::ffi::AVChromaLocation::*;
    decode_ffi_enum!(
        value,
        "chroma location",
        AVCHROMA_LOC_UNSPECIFIED,
        AVCHROMA_LOC_LEFT,
        AVCHROMA_LOC_CENTER,
        AVCHROMA_LOC_TOPLEFT,
        AVCHROMA_LOC_TOP,
        AVCHROMA_LOC_BOTTOMLEFT,
        AVCHROMA_LOC_BOTTOM,
    )
}

pub(super) fn decode_side_data_type(value: i32) -> io::Result<ffmpeg::ffi::AVPacketSideDataType> {
    use ffmpeg::ffi::AVPacketSideDataType::*;
    decode_ffi_enum!(
        value,
        "packet side-data type",
        AV_PKT_DATA_PALETTE,
        AV_PKT_DATA_NEW_EXTRADATA,
        AV_PKT_DATA_PARAM_CHANGE,
        AV_PKT_DATA_H263_MB_INFO,
        AV_PKT_DATA_REPLAYGAIN,
        AV_PKT_DATA_DISPLAYMATRIX,
        AV_PKT_DATA_STEREO3D,
        AV_PKT_DATA_AUDIO_SERVICE_TYPE,
        AV_PKT_DATA_QUALITY_STATS,
        AV_PKT_DATA_FALLBACK_TRACK,
        AV_PKT_DATA_CPB_PROPERTIES,
        AV_PKT_DATA_SKIP_SAMPLES,
        AV_PKT_DATA_JP_DUALMONO,
        AV_PKT_DATA_STRINGS_METADATA,
        AV_PKT_DATA_SUBTITLE_POSITION,
        AV_PKT_DATA_MATROSKA_BLOCKADDITIONAL,
        AV_PKT_DATA_WEBVTT_IDENTIFIER,
        AV_PKT_DATA_WEBVTT_SETTINGS,
        AV_PKT_DATA_METADATA_UPDATE,
        AV_PKT_DATA_MPEGTS_STREAM_ID,
        AV_PKT_DATA_MASTERING_DISPLAY_METADATA,
        AV_PKT_DATA_SPHERICAL,
        AV_PKT_DATA_CONTENT_LIGHT_LEVEL,
        AV_PKT_DATA_A53_CC,
        AV_PKT_DATA_ENCRYPTION_INIT_INFO,
        AV_PKT_DATA_ENCRYPTION_INFO,
        AV_PKT_DATA_AFD,
        AV_PKT_DATA_PRFT,
        AV_PKT_DATA_ICC_PROFILE,
        AV_PKT_DATA_DOVI_CONF,
        AV_PKT_DATA_S12M_TIMECODE,
        AV_PKT_DATA_DYNAMIC_HDR10_PLUS,
        AV_PKT_DATA_IAMF_MIX_GAIN_PARAM,
        AV_PKT_DATA_IAMF_DEMIXING_INFO_PARAM,
        AV_PKT_DATA_IAMF_RECON_GAIN_INFO_PARAM,
        AV_PKT_DATA_AMBIENT_VIEWING_ENVIRONMENT,
        AV_PKT_DATA_FRAME_CROPPING,
        AV_PKT_DATA_LCEVC,
    )
}
