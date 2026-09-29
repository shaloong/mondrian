#ifndef MONDRIAN_VIDEOTOOLBOX_PRORES_H
#define MONDRIAN_VIDEOTOOLBOX_PRORES_H
#include <stddef.h>
#include <stdint.h>

/* One worker exclusively owns a decoder. Calls must not overlap. The adapter
 * owns no demux, seek, cache, playback clock, or color interpretation policy. */
typedef struct MdnProResDecoder MdnProResDecoder;
typedef struct {
    int32_t status;
    uint32_t stage;
} MdnVtError;
enum {
    MDN_VT_ARGUMENT = 1, MDN_VT_FORMAT, MDN_VT_SESSION, MDN_VT_PACKET,
    MDN_VT_DECODE, MDN_VT_FINISH, MDN_VT_WAIT, MDN_VT_CALLBACK,
    MDN_VT_SURFACE, MDN_VT_HARDWARE, MDN_VT_METADATA
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
} MdnProResContract;
typedef struct {
    /* +1 CVPixelBuffer reference; release with mdn_prores_surface_release. */
    void *pixel_buffer;
    size_t allocation_bytes;
    uint32_t width, height, pixel_format, callback_count;
    uint32_t hardware_observed;
} MdnProResOutput;

/* Explicit CICP and limited-range 16-bit 4444 output. Hardware is required;
 * successful output additionally carries the observed property, not intent. */
MdnProResDecoder *mdn_prores_create(const MdnProResContract *, MdnVtError *);
/* Packet PTS is an exact rational, not a floating-point timestamp. */
int mdn_prores_decode(MdnProResDecoder *, const uint8_t *, size_t,
                     int64_t pts_value, int32_t pts_scale,
                     MdnProResOutput *, MdnVtError *);
/* Consumes the decoder even on error. No callback state survives this call. */
int mdn_prores_close(MdnProResDecoder *, MdnVtError *);
void mdn_prores_surface_release(void *);
#endif
