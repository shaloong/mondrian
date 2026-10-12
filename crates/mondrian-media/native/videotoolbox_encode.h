#ifndef MONDRIAN_VIDEOTOOLBOX_ENCODE_H
#define MONDRIAN_VIDEOTOOLBOX_ENCODE_H
#include <stddef.h>
#include <stdint.h>
#include "videotoolbox.h"
/* Exclusive platform codec worker; no Timeline, color interpretation or Export policy. */
typedef struct MdnVtEncoder MdnVtEncoder;
typedef struct {
    uint32_t codec; /* 1=HEVC, 2=H.264 High; Rust supplies exact typed identity. */
    uint32_t width, height, fps_num, fps_den, sar_num, sar_den;
    uint32_t ten_bit, full_range, primaries, transfer, matrix, centered_chroma;
    uint32_t gop, quantizer, pool_size;
    uint64_t metal_registry_id;
} MdnVtEncodeContract;
typedef struct {
    const uint8_t *bytes;
    size_t size;
    int64_t pts;
    uint32_t key, hardware_observed;
} MdnVtEncodePacket;
MdnVtEncoder *mdn_vt_encoder_create(const MdnVtEncodeContract *, MdnVtError *);
/* Returned surface is +1; release through mdn_vt_surface_release. */
void *mdn_vt_encoder_acquire(MdnVtEncoder *, MdnVtError *);
/* Both GPU plane writes must have completed before submission. Output bytes
 * remain owned by this session until its next submit/close. */
int mdn_vt_encoder_submit(MdnVtEncoder *, void *pixel_buffer, uint64_t index,
                         MdnVtEncodePacket *, MdnVtError *);
/* Consuming shutdown: invalidation completes before callback storage retires. */
void mdn_vt_encoder_close(MdnVtEncoder *);
#endif
