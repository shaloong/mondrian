#include "videotoolbox_prores.h"
#include <CoreMedia/CoreMedia.h>
#include <CoreVideo/CoreVideo.h>
#include <VideoToolbox/VideoToolbox.h>
#include <limits.h>
#include <pthread.h>
#include <stdlib.h>
#include <string.h>

struct MdnProResDecoder {
    VTDecompressionSessionRef session;
    CMVideoFormatDescriptionRef format;
    pthread_mutex_t lock;
    CVPixelBufferRef output;
    CMTime timestamp;
    OSStatus callback_status;
    uint32_t callbacks;
    int failed;
    MdnProResContract contract;
    CFStringRef primaries, transfer, matrix;
};

static int fail(MdnVtError *error, uint32_t stage, OSStatus status) {
    if (error) *error = (MdnVtError){status ? status : -70001, stage};
    return 0;
}

static void decoded(void *opaque, void *source, OSStatus status,
                    VTDecodeInfoFlags flags, CVImageBufferRef image,
                    CMTime timestamp, CMTime duration) {
    (void)source; (void)duration;
    MdnProResDecoder *decoder = opaque;
    pthread_mutex_lock(&decoder->lock);
    if (decoder->callbacks != UINT32_MAX) decoder->callbacks++;
    if (decoder->callbacks == 1) {
        decoder->callback_status = status;
        decoder->timestamp = timestamp;
        if (!status && !(flags & kVTDecodeInfo_FrameDropped) && image)
            decoder->output = CVPixelBufferRetain(image);
    }
    pthread_mutex_unlock(&decoder->lock);
}

static CFMutableDictionaryRef dictionary(void) {
    return CFDictionaryCreateMutable(kCFAllocatorDefault, 0,
        &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
}

MdnProResDecoder *mdn_prores_create(const MdnProResContract *contract, MdnVtError *error) {
    if (error) *error = (MdnVtError){0};
    if (!contract || !contract->width || !contract->height ||
        contract->width > INT32_MAX || contract->height > INT32_MAX ||
        (contract->codec != kCMVideoCodecType_AppleProRes4444 &&
         contract->codec != kCMVideoCodecType_AppleProRes4444XQ)) {
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
    MdnProResDecoder *decoder = calloc(1, sizeof(*decoder));
    if (!decoder) { fail(error, MDN_VT_SESSION, -70002); return NULL; }
    if (pthread_mutex_init(&decoder->lock, NULL)) {
        free(decoder); fail(error, MDN_VT_SESSION, -70002); return NULL;
    }
    decoder->contract = *contract;
    decoder->primaries = pri; decoder->transfer = trc; decoder->matrix = matrix;
    CFMutableDictionaryRef extensions = dictionary();
    CFMutableDictionaryRef attributes = dictionary();
    CFMutableDictionaryRef iosurface = dictionary();
    CFMutableDictionaryRef specification = dictionary();
    int32_t pixel_format = kCVPixelFormatType_444YpCbCr16VideoRange_16A_TriPlanar;
    CFNumberRef pixel = CFNumberCreate(NULL, kCFNumberSInt32Type, &pixel_format);
    OSStatus status = -70002;
    uint32_t stage = MDN_VT_FORMAT;
    if (!extensions || !attributes || !iosurface || !specification || !pixel) goto done;
    CFDictionarySetValue(extensions, kCMFormatDescriptionExtension_ColorPrimaries, pri);
    CFDictionarySetValue(extensions, kCMFormatDescriptionExtension_TransferFunction, trc);
    CFDictionarySetValue(extensions, kCMFormatDescriptionExtension_YCbCrMatrix, matrix);
    CFDictionarySetValue(extensions, kCMFormatDescriptionExtension_FullRangeVideo, kCFBooleanFalse);
    status = CMVideoFormatDescriptionCreate(NULL, contract->codec,
        (int32_t)contract->width, (int32_t)contract->height, extensions, &decoder->format);
    if (status) goto done;
    CFDictionarySetValue(attributes, kCVPixelBufferPixelFormatTypeKey, pixel);
    CFDictionarySetValue(attributes, kCVPixelBufferIOSurfacePropertiesKey, iosurface);
    CFDictionarySetValue(attributes, kCVPixelBufferMetalCompatibilityKey, kCFBooleanTrue);
    CFDictionarySetValue(specification,
        kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder, kCFBooleanTrue);
    VTDecompressionOutputCallbackRecord callback = {decoded, decoder};
    stage = MDN_VT_SESSION;
    status = VTDecompressionSessionCreate(NULL, decoder->format, specification,
        attributes, &callback, &decoder->session);
done:
    if (pixel) CFRelease(pixel);
    if (specification) CFRelease(specification);
    if (iosurface) CFRelease(iosurface);
    if (attributes) CFRelease(attributes);
    if (extensions) CFRelease(extensions);
    if (status) {
        fail(error, stage, status);
        mdn_prores_close(decoder, NULL);
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
static int validate_packet_contract(const MdnProResContract *contract,
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
    if ((header[12] & 0xc0) != 0xc0)
        return fail(error, MDN_VT_METADATA, MDN_VT_PACKET_CHROMA_UNSUPPORTED);
    if ((header[17] & 0x0f) > 2)
        return fail(error, MDN_VT_METADATA, MDN_VT_PACKET_ALPHA_UNSUPPORTED);
    if (header[14] != contract->primaries || header[15] != contract->transfer ||
        header[16] != contract->matrix)
        return fail(error, MDN_VT_METADATA, MDN_VT_PACKET_COLOR_CHANGED);
    return 1;
}

int mdn_prores_decode(MdnProResDecoder *decoder, const uint8_t *bytes, size_t size,
                     int64_t pts_value, int32_t pts_scale,
                     MdnProResOutput *output, MdnVtError *error) {
    if (error) *error = (MdnVtError){0};
    if (output) *output = (MdnProResOutput){0};
    if (!decoder || !output || !bytes || !size || pts_scale <= 0 || decoder->failed)
        return fail(error, MDN_VT_ARGUMENT, 0);
    if (!validate_packet_contract(&decoder->contract, bytes, size, error)) {
        decoder->failed = 1;
        return 0;
    }
    pthread_mutex_lock(&decoder->lock);
    decoder->callbacks = 0; decoder->callback_status = 0;
    decoder->timestamp = kCMTimeInvalid;
    pthread_mutex_unlock(&decoder->lock);
    CMBlockBufferRef block = NULL;
    CMSampleBufferRef sample = NULL;
    /* Own compressed bytes through completion; never lock or copy pixel data. */
    OSStatus status = CMBlockBufferCreateWithMemoryBlock(NULL, NULL, size,
        kCFAllocatorDefault, NULL, 0, size, 0, &block);
    uint32_t stage = MDN_VT_PACKET;
    if (status) goto done;
    status = CMBlockBufferReplaceDataBytes(bytes, block, 0, size);
    if (status) goto done;
    CMSampleTimingInfo timing = {kCMTimeInvalid, CMTimeMake(pts_value, pts_scale), kCMTimeInvalid};
    status = CMSampleBufferCreateReady(NULL, block, decoder->format,
        1, 1, &timing, 1, &size, &sample);
    if (status) goto done;
    stage = MDN_VT_DECODE;
    status = VTDecompressionSessionDecodeFrame(decoder->session, sample, 0, NULL, NULL);
    /* Join submitted work even when submission reports an error. Preserve the
     * first failure while completing callback ownership before returning. */
    OSStatus finished = VTDecompressionSessionFinishDelayedFrames(decoder->session);
    OSStatus waited = VTDecompressionSessionWaitForAsynchronousFrames(decoder->session);
    if (status) goto done;
    stage = MDN_VT_FINISH;
    status = finished;
    if (status) goto done;
    stage = MDN_VT_WAIT;
    status = waited;
    if (status) goto done;
    pthread_mutex_lock(&decoder->lock);
    CVPixelBufferRef image = decoder->output;
    status = decoder->callback_status;
    uint32_t callbacks = decoder->callbacks;
    CMTime timestamp = decoder->timestamp;
    pthread_mutex_unlock(&decoder->lock);
    stage = MDN_VT_CALLBACK;
    if (!status && (callbacks != 1 || !image || !CMTIME_IS_NUMERIC(timestamp) ||
        CMTimeCompare(timestamp, timing.presentationTimeStamp))) status = -70001;
    if (status) goto done;
    stage = MDN_VT_SURFACE;
    if (CVPixelBufferGetPixelFormatType(image) != kCVPixelFormatType_444YpCbCr16VideoRange_16A_TriPlanar ||
        CVPixelBufferGetWidth(image) != decoder->contract.width ||
        CVPixelBufferGetHeight(image) != decoder->contract.height ||
        CVPixelBufferGetPlaneCount(image) != 3 || !CVPixelBufferGetIOSurface(image)) {
        status = -70001; goto done;
    }
    for (size_t plane = 0; plane < 3; plane++) {
        if (CVPixelBufferGetWidthOfPlane(image, plane) != decoder->contract.width ||
            CVPixelBufferGetHeightOfPlane(image, plane) != decoder->contract.height) {
            status = -70001; goto done;
        }
    }
    stage = MDN_VT_METADATA;
    if (!attachment_matches(image, kCVImageBufferColorPrimariesKey, decoder->primaries) ||
        !attachment_matches(image, kCVImageBufferTransferFunctionKey, decoder->transfer) ||
        !attachment_matches(image, kCVImageBufferYCbCrMatrixKey, decoder->matrix)) {
        status = -70001; goto done;
    }
    stage = MDN_VT_HARDWARE;
    CFTypeRef hardware = NULL;
    status = VTSessionCopyProperty(decoder->session,
        kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder, NULL, &hardware);
    int observed = !status && hardware && CFGetTypeID(hardware) == CFBooleanGetTypeID() &&
        CFBooleanGetValue((CFBooleanRef)hardware);
    if (hardware) CFRelease(hardware);
    if (!observed) { if (!status) status = -70001; goto done; }
    *output = (MdnProResOutput){image, CVPixelBufferGetDataSize(image),
        decoder->contract.width, decoder->contract.height,
        CVPixelBufferGetPixelFormatType(image), callbacks, 1};
    pthread_mutex_lock(&decoder->lock);
    decoder->output = NULL; /* transfer the callback's +1 reference */
    pthread_mutex_unlock(&decoder->lock);
done:
    if (status) decoder->failed = 1;
    if (sample) CFRelease(sample);
    if (block) CFRelease(block);
    if (status) return fail(error, stage, status);
    return 1;
}

int mdn_prores_close(MdnProResDecoder *decoder, MdnVtError *error) {
    if (error) *error = (MdnVtError){0};
    if (!decoder) return 1;
    OSStatus finish = 0, wait = 0;
    if (decoder->session) {
        finish = VTDecompressionSessionFinishDelayedFrames(decoder->session);
        wait = VTDecompressionSessionWaitForAsynchronousFrames(decoder->session);
        VTDecompressionSessionInvalidate(decoder->session);
        CFRelease(decoder->session);
    }
    if (decoder->output) CVPixelBufferRelease(decoder->output);
    if (decoder->format) CFRelease(decoder->format);
    pthread_mutex_destroy(&decoder->lock);
    free(decoder);
    if (finish) return fail(error, MDN_VT_FINISH, finish);
    if (wait) return fail(error, MDN_VT_WAIT, wait);
    return 1;
}

void mdn_prores_surface_release(void *surface) {
    if (surface) CVPixelBufferRelease((CVPixelBufferRef)surface);
}
