#ifndef MONDRIAN_VIDEOTOOLBOX_H
#define MONDRIAN_VIDEOTOOLBOX_H
#include <stddef.h>
#include <stdint.h>

/* One worker exclusively owns a decoder. Calls must not overlap. The adapter
 * owns no demux, seek, cache, playback clock, or color interpretation policy. */
typedef struct MdnVtDecoder MdnVtDecoder;
typedef struct {
    int32_t status;
    uint32_t stage;
} MdnVtError;
enum {
    MDN_VT_ARGUMENT = 1, MDN_VT_FORMAT, MDN_VT_SESSION, MDN_VT_PACKET,
    MDN_VT_DECODE, MDN_VT_FINISH, MDN_VT_WAIT, MDN_VT_CALLBACK,
    MDN_VT_SURFACE, MDN_VT_HARDWARE, MDN_VT_METADATA, MDN_VT_ENCODE
};
/* Adapter contract failures, distinct from OSStatus returned by VideoToolbox. */
enum {
    MDN_VT_PACKET_EXTENT_CHANGED = -70010,
    MDN_VT_PACKET_SCAN_UNSUPPORTED = -70011,
    MDN_VT_PACKET_CHROMA_UNSUPPORTED = -70012,
    MDN_VT_PACKET_COLOR_CHANGED = -70013,
    MDN_VT_PACKET_HEADER_INVALID = -70014,
    MDN_VT_PACKET_ALPHA_UNSUPPORTED = -70015
};
typedef struct {
    uint32_t width, height, codec;
    uint32_t primaries, transfer, matrix;
    /* HEVC SPS chroma_format_idc (1/2/3); ProRes 422 uses 2, 4444 uses 3. */
    uint32_t chroma_format_idc;
    /* Coded HEVC component depth (8/10); zero for the ProRes adapter. */
    uint32_t hevc_bit_depth;
} MdnVtContract;
typedef struct {
    /* +1 CVPixelBuffer reference; release with mdn_vt_surface_release. */
    void *pixel_buffer;
    size_t allocation_bytes;
    uint32_t width, height, pixel_format, callback_count;
    uint32_t hardware_observed;
} MdnVtOutput;

typedef struct {
    const uint8_t *bytes;
    size_t size;
} MdnVtParameterSet;

/* Explicit CICP and limited-range native output. Hardware is required;
 * successful output additionally carries the observed property, not intent. */
MdnVtDecoder *mdn_vt_create(const MdnVtContract *,
    const MdnVtParameterSet *, size_t count, int nal_length, MdnVtError *);
int mdn_vt_finish(MdnVtDecoder *, MdnVtError *);
int mdn_vt_reset(MdnVtDecoder *, MdnVtError *);
/* Packet PTS is an exact rational, not a floating-point timestamp. */
int mdn_vt_decode(MdnVtDecoder *, const uint8_t *, size_t,
                     int64_t pts_value, int32_t pts_scale,
                     MdnVtOutput *, MdnVtError *);
/* Consumes the decoder even on error. No callback state survives this call. */
int mdn_vt_close(MdnVtDecoder *, MdnVtError *);
void mdn_vt_surface_release(void *);
#endif
