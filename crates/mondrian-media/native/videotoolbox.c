#include "videotoolbox.h"
#include <CoreMedia/CoreMedia.h>
#include <CoreVideo/CoreVideo.h>
#include <VideoToolbox/VideoToolbox.h>
#include <limits.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>

/* The accepted SPS bounds reorder depth to at most 15. Stable slot addresses
 * outlive every native callback, including unexpected late callbacks. */
#define MDN_VT_MAX_PENDING 16
typedef struct {
    int active;
    int64_t token;
    CMSampleBufferRef sample;
    CVPixelBufferRef output;
    CMTime expected, timestamp;
    OSStatus status;
    uint32_t callbacks;
} MdnVtPending;

struct MdnVtDecoder {
    VTDecompressionSessionRef session;
    CMVideoFormatDescriptionRef format;
    pthread_mutex_t lock;
    pthread_cond_t changed;
    atomic_int failed;
    uint32_t pending_limit;
    MdnVtPending pending[MDN_VT_MAX_PENDING];
    MdnVtContract contract;
    CFStringRef primaries, transfer, matrix;
};

static int fail(MdnVtError *error, uint32_t stage, OSStatus status) {
    if (error) *error = (MdnVtError){status ? status : -70001, stage};
    return 0;
}

static void decoded(void *opaque, void *source, OSStatus status,
                    VTDecodeInfoFlags flags, CVImageBufferRef image,
                    CMTime timestamp, CMTime duration) {
    (void)duration;
    MdnVtDecoder *decoder = opaque;
    pthread_mutex_lock(&decoder->lock);
    MdnVtPending *pending = source;
    if (!pending || !pending->active || pending->callbacks != 0 ||
        (!status && (!CMTIME_IS_NUMERIC(timestamp) ||
            CMTimeCompare(timestamp, pending->expected)))) {
        decoder->failed = 1;
    } else {
        pending->callbacks = 1;
        pending->status = status;
        pending->timestamp = timestamp;
        if (!status && !(flags & kVTDecodeInfo_FrameDropped) && image)
            pending->output = CVPixelBufferRetain(image);
    }
    pthread_cond_broadcast(&decoder->changed);
    pthread_mutex_unlock(&decoder->lock);
}

static CFMutableDictionaryRef dictionary(void) {
    return CFDictionaryCreateMutable(kCFAllocatorDefault, 0,
        &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
}

static int prores422(uint32_t codec) {
    return codec == kCMVideoCodecType_AppleProRes422Proxy ||
        codec == kCMVideoCodecType_AppleProRes422LT ||
        codec == kCMVideoCodecType_AppleProRes422 ||
        codec == kCMVideoCodecType_AppleProRes422HQ;
}

static OSType output_pixel(const MdnVtContract *contract) {
    if (prores422(contract->codec))
        return kCVPixelFormatType_422YpCbCr16BiPlanarVideoRange;
    if (contract->codec != kCMVideoCodecType_HEVC)
        return kCVPixelFormatType_444YpCbCr16VideoRange_16A_TriPlanar;
    if (contract->hevc_bit_depth == 8)
        return contract->chroma_format_idc == 1 ? kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange : 0;
    switch (contract->chroma_format_idc) {
        case 1: return kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange;
        case 2: return kCVPixelFormatType_422YpCbCr10BiPlanarVideoRange;
        case 3: return kCVPixelFormatType_444YpCbCr10BiPlanarVideoRange;
        default: return 0;
    }
}

static OSStatus create_session(MdnVtDecoder *decoder) {
    CFMutableDictionaryRef attributes = dictionary();
    CFMutableDictionaryRef iosurface = dictionary();
    CFMutableDictionaryRef specification = dictionary();
    int32_t pixel_format = (int32_t)output_pixel(&decoder->contract);
    CFNumberRef pixel = CFNumberCreate(NULL, kCFNumberSInt32Type, &pixel_format);
    OSStatus status = -70002;
    if (!attributes || !iosurface || !specification || !pixel) goto done;
    CFDictionarySetValue(attributes, kCVPixelBufferPixelFormatTypeKey, pixel);
    CFDictionarySetValue(attributes, kCVPixelBufferIOSurfacePropertiesKey, iosurface);
    CFDictionarySetValue(attributes, kCVPixelBufferMetalCompatibilityKey, kCFBooleanTrue);
    CFDictionarySetValue(specification,
        kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder, kCFBooleanTrue);
    VTDecompressionOutputCallbackRecord callback = {decoded, decoder};
    status = VTDecompressionSessionCreate(NULL, decoder->format, specification,
        attributes, &callback, &decoder->session);
done:
    if (pixel) CFRelease(pixel);
    if (specification) CFRelease(specification);
    if (iosurface) CFRelease(iosurface);
    if (attributes) CFRelease(attributes);
    return status;
}

MdnVtDecoder *mdn_vt_create(const MdnVtContract *contract,
    const MdnVtParameterSet *sets, size_t count, int nal_length,
    uint32_t pending_limit, MdnVtError *error) {
    if (error) *error = (MdnVtError){0};
    if (!contract || !pending_limit || pending_limit > MDN_VT_MAX_PENDING ||
        (contract->codec != kCMVideoCodecType_HEVC && pending_limit != 1) ||
        !contract->width || !contract->height ||
        contract->width > INT32_MAX || contract->height > INT32_MAX ||
        contract->chroma_format_idc < 1 || contract->chroma_format_idc > 3 ||
        (contract->codec == kCMVideoCodecType_HEVC ?
         ((contract->hevc_bit_depth != 8 && contract->hevc_bit_depth != 10) ||
          (contract->hevc_bit_depth == 8 && contract->chroma_format_idc != 1)) :
         contract->hevc_bit_depth != 0) ||
        (prores422(contract->codec) && contract->chroma_format_idc != 2) ||
        ((contract->codec == kCMVideoCodecType_AppleProRes4444 ||
          contract->codec == kCMVideoCodecType_AppleProRes4444XQ) &&
         contract->chroma_format_idc != 3) ||
        (!prores422(contract->codec) &&
         contract->codec != kCMVideoCodecType_AppleProRes4444 &&
         contract->codec != kCMVideoCodecType_AppleProRes4444XQ &&
         contract->codec != kCMVideoCodecType_HEVC)) {
        fail(error, MDN_VT_ARGUMENT, 0); return NULL;
    }
    CFStringRef pri = contract->primaries == 1 ? kCVImageBufferColorPrimaries_ITU_R_709_2 :
        contract->primaries == 9 ? kCVImageBufferColorPrimaries_ITU_R_2020 : NULL;
    CFStringRef trc = contract->transfer == 1 ? kCVImageBufferTransferFunction_ITU_R_709_2 :
        contract->transfer == 16 ? kCVImageBufferTransferFunction_SMPTE_ST_2084_PQ :
        contract->transfer == 18 ? kCVImageBufferTransferFunction_ITU_R_2100_HLG : NULL;
    CFStringRef matrix = contract->matrix == 1 ? kCVImageBufferYCbCrMatrix_ITU_R_709_2 :
        contract->matrix == 9 ? kCVImageBufferYCbCrMatrix_ITU_R_2020 : NULL;
    if (!pri || !trc || !matrix) { fail(error, MDN_VT_METADATA, 0); return NULL; }
    MdnVtDecoder *decoder = calloc(1, sizeof(*decoder));
    if (!decoder) { fail(error, MDN_VT_SESSION, -70002); return NULL; }
    if (pthread_mutex_init(&decoder->lock, NULL)) {
        free(decoder); fail(error, MDN_VT_SESSION, -70002); return NULL;
    }
    if (pthread_cond_init(&decoder->changed, NULL)) {
        pthread_mutex_destroy(&decoder->lock);
        free(decoder); fail(error, MDN_VT_SESSION, -70002); return NULL;
    }
    atomic_init(&decoder->failed, 0);
    decoder->pending_limit = pending_limit;
    decoder->contract = *contract;
    decoder->primaries = pri; decoder->transfer = trc; decoder->matrix = matrix;
    OSStatus status = -70001;
    uint32_t stage = MDN_VT_FORMAT;
    if (contract->codec == kCMVideoCodecType_HEVC) {
        if (!sets || count < 3 || count > 64 ||
            (nal_length != 1 && nal_length != 2 && nal_length != 4)) goto done;
        const uint8_t *pointers[64];
        size_t sizes[64];
        for (size_t i = 0; i < count; i++) {
            if (!sets[i].bytes || sets[i].size < 2) goto done;
            pointers[i] = sets[i].bytes; sizes[i] = sets[i].size;
        }
        /* Parse real coded dimensions/metadata. Do not override source VUI
         * with the caller's expected CICP contract. Every output is also checked. */
        status = CMVideoFormatDescriptionCreateFromHEVCParameterSets(NULL,
            count, pointers, sizes, nal_length, NULL, &decoder->format);
        if (status) goto done;
        CMVideoDimensions dimensions = CMVideoFormatDescriptionGetDimensions(decoder->format);
        CFTypeRef full = CMFormatDescriptionGetExtension(decoder->format,
            kCMFormatDescriptionExtension_FullRangeVideo);
        if (dimensions.width != (int32_t)contract->width ||
            dimensions.height != (int32_t)contract->height ||
            (full && CFEqual(full, kCFBooleanTrue))) {
            status = -70001; goto done;
        }
        stage = MDN_VT_METADATA;
        CFTypeRef coded_pri = CMFormatDescriptionGetExtension(decoder->format,
            kCMFormatDescriptionExtension_ColorPrimaries);
        CFTypeRef coded_trc = CMFormatDescriptionGetExtension(decoder->format,
            kCMFormatDescriptionExtension_TransferFunction);
        CFTypeRef coded_matrix = CMFormatDescriptionGetExtension(decoder->format,
            kCMFormatDescriptionExtension_YCbCrMatrix);
        if (!coded_pri || !CFEqual(coded_pri, pri) ||
            !coded_trc || !CFEqual(coded_trc, trc) ||
            !coded_matrix || !CFEqual(coded_matrix, matrix)) {
            status = -70001; goto done;
        }
    } else {
        CFMutableDictionaryRef extensions = dictionary();
        if (!extensions) { status = -70002; goto done; }
        CFDictionarySetValue(extensions, kCMFormatDescriptionExtension_ColorPrimaries, pri);
        CFDictionarySetValue(extensions, kCMFormatDescriptionExtension_TransferFunction, trc);
        CFDictionarySetValue(extensions, kCMFormatDescriptionExtension_YCbCrMatrix, matrix);
        CFDictionarySetValue(extensions, kCMFormatDescriptionExtension_FullRangeVideo, kCFBooleanFalse);
        status = CMVideoFormatDescriptionCreate(NULL, contract->codec,
            (int32_t)contract->width, (int32_t)contract->height, extensions, &decoder->format);
        CFRelease(extensions);
        if (status) goto done;
    }
    stage = MDN_VT_SESSION;
    status = create_session(decoder);
done:
    if (status) {
        fail(error, stage, status);
        mdn_vt_close(decoder, NULL);
        return NULL;
    }
    return decoder;
}

static int attachment_matches(CVPixelBufferRef buffer, CFStringRef key, CFStringRef expected) {
    CFTypeRef value = CVBufferCopyAttachment(buffer, key, NULL);
    int equal = value && CFEqual(value, expected);
    if (value) CFRelease(value);
    return equal;
}

static uint16_t read_be16(const uint8_t *bytes) {
    return (uint16_t)(((uint16_t)bytes[0] << 8) | bytes[1]);
}

/* Validate compressed-frame facts before a fixed CMFormatDescription can mask
 * them in output attachments. These are codec header fields, not color math.
 * Field layout: FFmpeg n8.1.2 libavcodec/proresdec.c decode_frame_header and
 * decode_frame (https://github.com/FFmpeg/FFmpeg/blob/n8.1.2/libavcodec/proresdec.c).
 */
static int validate_packet_contract(const MdnVtContract *contract,
                                    const uint8_t *packet, size_t size, MdnVtError *error) {
    if (size < 28 || memcmp(packet + 4, "icpf", 4))
        return fail(error, MDN_VT_PACKET, MDN_VT_PACKET_HEADER_INVALID);
    uint32_t declared_size = ((uint32_t)packet[0] << 24) | ((uint32_t)packet[1] << 16) |
                             ((uint32_t)packet[2] << 8) | packet[3];
    const uint8_t *header = packet + 8;
    size_t header_size = read_be16(header);
    size_t required_header = 20 + ((header[19] & 1) ? 64 : 0) + ((header[19] & 2) ? 64 : 0);
    if (declared_size != size || header_size < required_header || header_size > size - 8 ||
        read_be16(header + 2) > 1)
        return fail(error, MDN_VT_PACKET, MDN_VT_PACKET_HEADER_INVALID);
    if (read_be16(header + 8) != contract->width || read_be16(header + 10) != contract->height)
        return fail(error, MDN_VT_METADATA, MDN_VT_PACKET_EXTENT_CHANGED);
    if ((header[12] >> 2) & 3)
        return fail(error, MDN_VT_METADATA, MDN_VT_PACKET_SCAN_UNSUPPORTED);
    if ((header[12] & 0xc0) != (prores422(contract->codec) ? 0x80 : 0xc0))
        return fail(error, MDN_VT_METADATA, MDN_VT_PACKET_CHROMA_UNSUPPORTED);
    if ((header[17] & 0x0f) > (prores422(contract->codec) ? 0 : 2))
        return fail(error, MDN_VT_METADATA, MDN_VT_PACKET_ALPHA_UNSUPPORTED);
    if (header[14] != contract->primaries || header[15] != contract->transfer ||
        header[16] != contract->matrix)
        return fail(error, MDN_VT_METADATA, MDN_VT_PACKET_COLOR_CHANGED);
    return 1;
}

/* Every codec uses this one physical surface/attachment contract. The caller
 * retains image ownership on error; success transfers that same +1 reference. */
static int validate_output(MdnVtDecoder *decoder, CVPixelBufferRef image,
    OSStatus status, uint32_t callbacks, CMTime timestamp, CMTime expected,
    MdnVtOutput *output, MdnVtError *error) {
    if (status || callbacks != 1 || !image || !CMTIME_IS_NUMERIC(timestamp) ||
        CMTimeCompare(timestamp, expected))
        return fail(error, MDN_VT_CALLBACK, status);
    int hevc = decoder->contract.codec == kCMVideoCodecType_HEVC;
    int is422 = prores422(decoder->contract.codec);
    OSType expected_pixel = output_pixel(&decoder->contract);
    size_t planes = hevc || is422 ? 2 : 3;
    if (CVPixelBufferGetPixelFormatType(image) != expected_pixel ||
        CVPixelBufferGetWidth(image) != decoder->contract.width ||
        CVPixelBufferGetHeight(image) != decoder->contract.height ||
        CVPixelBufferGetPlaneCount(image) != planes || !CVPixelBufferGetIOSurface(image))
        return fail(error, MDN_VT_SURFACE, 0);
    for (size_t plane = 0; plane < planes; plane++) {
        size_t width = (hevc || is422) && plane && decoder->contract.chroma_format_idc != 3 ?
            (decoder->contract.width + 1) / 2 : decoder->contract.width;
        size_t height = hevc && plane && decoder->contract.chroma_format_idc == 1 ?
            (decoder->contract.height + 1) / 2 : decoder->contract.height;
        size_t component_bytes = hevc && decoder->contract.hevc_bit_depth == 8 ? 1 : 2;
        size_t bytes_per_pixel = component_bytes * (plane == 1 ? 2 : 1);
        if (CVPixelBufferGetWidthOfPlane(image, plane) != width ||
            CVPixelBufferGetHeightOfPlane(image, plane) != height ||
            CVPixelBufferGetBytesPerRowOfPlane(image, plane) < width * bytes_per_pixel)
            return fail(error, MDN_VT_SURFACE, 0);
    }
    if (!attachment_matches(image, kCVImageBufferColorPrimariesKey, decoder->primaries) ||
        !attachment_matches(image, kCVImageBufferTransferFunctionKey, decoder->transfer) ||
        !attachment_matches(image, kCVImageBufferYCbCrMatrixKey, decoder->matrix))
        return fail(error, MDN_VT_METADATA, 0);
    CFTypeRef hardware = NULL;
    status = VTSessionCopyProperty(decoder->session,
        kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder, NULL, &hardware);
    int observed = !status && hardware && CFGetTypeID(hardware) == CFBooleanGetTypeID() &&
        CFBooleanGetValue((CFBooleanRef)hardware);
    if (hardware) CFRelease(hardware);
    if (!observed) return fail(error, MDN_VT_HARDWARE, status);
    *output = (MdnVtOutput){image, CVPixelBufferGetDataSize(image),
        decoder->contract.width, decoder->contract.height,
        CVPixelBufferGetPixelFormatType(image), callbacks, 1};
    return 1;
}

int mdn_vt_submit(MdnVtDecoder *decoder, const uint8_t *bytes, size_t size,
    int64_t pts_value, int32_t pts_scale, int64_t token, MdnVtError *error) {
    if (error) *error = (MdnVtError){0};
    if (!decoder || decoder->failed || !bytes || !size || pts_scale <= 0)
        return fail(error, MDN_VT_ARGUMENT, 0);
    if (decoder->contract.codec != kCMVideoCodecType_HEVC &&
        !validate_packet_contract(&decoder->contract, bytes, size, error)) {
        decoder->failed = 1;
        return 0;
    }
    pthread_mutex_lock(&decoder->lock);
    MdnVtPending *pending = NULL;
    size_t active = 0;
    for (size_t i = 0; i < decoder->pending_limit; i++) {
        MdnVtPending *slot = &decoder->pending[i];
        if (slot->active) {
            active++;
            if (slot->token == token) {
                pthread_mutex_unlock(&decoder->lock);
                return fail(error, MDN_VT_ARGUMENT, 0);
            }
        } else if (!pending) pending = slot;
    }
    if (!pending || active >= decoder->pending_limit) {
        pthread_mutex_unlock(&decoder->lock);
        return fail(error, MDN_VT_ARGUMENT, 0);
    }
    /* Retain the previous compressed sample until slot reuse. Callback context
     * storage itself stays alive through consuming session shutdown. */
    if (pending->sample) CFRelease(pending->sample);
    *pending = (MdnVtPending){.active = 1, .token = token,
        .expected = CMTimeMake(pts_value, pts_scale), .timestamp = kCMTimeInvalid};
    pthread_mutex_unlock(&decoder->lock);
    CMBlockBufferRef block = NULL;
    OSStatus status = CMBlockBufferCreateWithMemoryBlock(NULL, NULL, size,
        kCFAllocatorDefault, NULL, 0, size, 0, &block);
    uint32_t stage = MDN_VT_PACKET;
    if (status) goto done;
    status = CMBlockBufferReplaceDataBytes(bytes, block, 0, size);
    if (status) goto done;
    CMSampleTimingInfo timing = {kCMTimeInvalid, pending->expected, kCMTimeInvalid};
    status = CMSampleBufferCreateReady(NULL, block, decoder->format,
        1, 1, &timing, 1, &size, &pending->sample);
    if (status) goto done;
    stage = MDN_VT_DECODE;
    VTDecodeInfoFlags flags = 0;
    status = VTDecompressionSessionDecodeFrame(decoder->session, pending->sample,
        kVTDecodeFrame_EnableAsynchronousDecompression, pending, &flags);
    if (decoder->contract.codec != kCMVideoCodecType_HEVC) {
        OSStatus finished = VTDecompressionSessionFinishDelayedFrames(decoder->session);
        if (!status && finished) { stage = MDN_VT_FINISH; status = finished; }
    }
    if (!status && (flags & kVTDecodeInfo_FrameDropped)) {
        stage = MDN_VT_CALLBACK; status = -70001;
    }
done:
    if (block) CFRelease(block);
    if (status) {
        /* Failed submission still retains stable slots until close joins every
         * other submitted callback. No partial continuation after a failure. */
        decoder->failed = 1;
        return fail(error, stage, status);
    }
    return 1;
}

int mdn_vt_take(MdnVtDecoder *decoder, int64_t token,
    MdnVtOutput *output, MdnVtError *error) {
    if (error) *error = (MdnVtError){0};
    if (output) *output = (MdnVtOutput){0};
    if (!decoder || !output) {
        return fail(error, MDN_VT_ARGUMENT, 0);
    }
    pthread_mutex_lock(&decoder->lock);
    MdnVtPending *pending = NULL;
    for (size_t i = 0; i < decoder->pending_limit; i++) {
        if (decoder->pending[i].active && decoder->pending[i].token == token) {
            pending = &decoder->pending[i]; break;
        }
    }
    if (!pending) {
        pthread_mutex_unlock(&decoder->lock);
        return fail(error, MDN_VT_ARGUMENT, 0);
    }
    while (!pending->callbacks && !decoder->failed)
        pthread_cond_wait(&decoder->changed, &decoder->lock);
    if (decoder->failed) {
        pthread_mutex_unlock(&decoder->lock);
        return fail(error, MDN_VT_CALLBACK, 0);
    }
    CVPixelBufferRef image = pending->output;
    OSStatus status = pending->status;
    uint32_t callbacks = pending->callbacks;
    CMTime timestamp = pending->timestamp, expected = pending->expected;
    pending->output = NULL;
    pending->active = 0;
    pthread_mutex_unlock(&decoder->lock);
    if (!validate_output(decoder, image, status, callbacks, timestamp, expected, output, error)) {
        if (image) CVPixelBufferRelease(image);
        decoder->failed = 1;
        return 0;
    }
    if (decoder->failed) {
        CVPixelBufferRelease(image);
        *output = (MdnVtOutput){0};
        return fail(error, MDN_VT_CALLBACK, 0);
    }
    return 1;
}

/* Only after the session-wide native join, never while callbacks can run. */
static void clear_pending(MdnVtDecoder *decoder) {
    for (size_t i = 0; i < decoder->pending_limit; i++) {
        MdnVtPending *pending = &decoder->pending[i];
        if (pending->output) CVPixelBufferRelease(pending->output);
        if (pending->sample) CFRelease(pending->sample);
        *pending = (MdnVtPending){0};
    }
}

int mdn_vt_finish(MdnVtDecoder *decoder, MdnVtError *error) {
    if (error) *error = (MdnVtError){0};
    if (!decoder || decoder->failed) return fail(error, MDN_VT_ARGUMENT, 0);
    OSStatus finish = VTDecompressionSessionFinishDelayedFrames(decoder->session);
    OSStatus wait = VTDecompressionSessionWaitForAsynchronousFrames(decoder->session);
    if (finish || wait) {
        decoder->failed = 1;
        return fail(error, finish ? MDN_VT_FINISH : MDN_VT_WAIT, finish ? finish : wait);
    }
    if (decoder->failed) return fail(error, MDN_VT_CALLBACK, 0);
    return 1;
}

int mdn_vt_reset(MdnVtDecoder *decoder, MdnVtError *error) {
    if (!mdn_vt_finish(decoder, error)) return 0;
    clear_pending(decoder);
    if (decoder->contract.codec != kCMVideoCodecType_HEVC) return 1;
    /* Join and invalidate the old temporal owner before constructing another.
     * Retained output buffers are independent of this session and remain live. */
    VTDecompressionSessionInvalidate(decoder->session);
    CFRelease(decoder->session); decoder->session = NULL;
    OSStatus status = create_session(decoder);
    if (status) { decoder->failed = 1; return fail(error, MDN_VT_SESSION, status); }
    return 1;
}

int mdn_vt_close(MdnVtDecoder *decoder, MdnVtError *error) {
    if (error) *error = (MdnVtError){0};
    if (!decoder) return 1;
    OSStatus finish = 0, wait = 0;
    if (decoder->session) {
        finish = VTDecompressionSessionFinishDelayedFrames(decoder->session);
        wait = VTDecompressionSessionWaitForAsynchronousFrames(decoder->session);
        VTDecompressionSessionInvalidate(decoder->session);
        CFRelease(decoder->session);
    }
    clear_pending(decoder);
    if (decoder->format) CFRelease(decoder->format);
    pthread_cond_destroy(&decoder->changed);
    pthread_mutex_destroy(&decoder->lock);
    free(decoder);
    if (finish) return fail(error, MDN_VT_FINISH, finish);
    if (wait) return fail(error, MDN_VT_WAIT, wait);
    return 1;
}

void mdn_vt_surface_release(void *surface) {
    if (surface) CVPixelBufferRelease((CVPixelBufferRef)surface);
}
