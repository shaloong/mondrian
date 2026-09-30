#include "videotoolbox.h"
#include <CoreMedia/CoreMedia.h>
#include <CoreVideo/CoreVideo.h>
#include <VideoToolbox/VideoToolbox.h>
#include <limits.h>
#include <pthread.h>
#include <stdlib.h>
#include <string.h>

struct MdnVtDecoder {
    VTDecompressionSessionRef session;
    CMVideoFormatDescriptionRef format;
    pthread_mutex_t lock;
    CVPixelBufferRef output;
    CMTime timestamp;
    OSStatus callback_status;
    uint32_t callbacks;
    int failed;
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
    (void)source; (void)duration;
    MdnVtDecoder *decoder = opaque;
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
    const MdnVtParameterSet *sets, size_t count, int nal_length, MdnVtError *error) {
    if (error) *error = (MdnVtError){0};
    if (!contract || !contract->width || !contract->height ||
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
         * with the caller's expected CICP contract. Output is checked below. */
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

int mdn_vt_decode(MdnVtDecoder *decoder, const uint8_t *bytes, size_t size,
                     int64_t pts_value, int32_t pts_scale,
                     MdnVtOutput *output, MdnVtError *error) {
    if (error) *error = (MdnVtError){0};
    if (output) *output = (MdnVtOutput){0};
    if (!decoder || !output || !bytes || !size || pts_scale <= 0 || decoder->failed)
        return fail(error, MDN_VT_ARGUMENT, 0);
    if (decoder->contract.codec != kCMVideoCodecType_HEVC &&
        !validate_packet_contract(&decoder->contract, bytes, size, error)) {
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
    OSStatus finished = decoder->contract.codec == kCMVideoCodecType_HEVC ? 0 :
        VTDecompressionSessionFinishDelayedFrames(decoder->session);
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
    int hevc = decoder->contract.codec == kCMVideoCodecType_HEVC;
    int is422 = prores422(decoder->contract.codec);
    OSType expected_pixel = output_pixel(&decoder->contract);
    size_t planes = hevc || is422 ? 2 : 3;
    if (CVPixelBufferGetPixelFormatType(image) != expected_pixel ||
        CVPixelBufferGetWidth(image) != decoder->contract.width ||
        CVPixelBufferGetHeight(image) != decoder->contract.height ||
        CVPixelBufferGetPlaneCount(image) != planes || !CVPixelBufferGetIOSurface(image)) {
        status = -70001; goto done;
    }
    for (size_t plane = 0; plane < planes; plane++) {
        size_t width = (hevc || is422) && plane && decoder->contract.chroma_format_idc != 3 ?
            (decoder->contract.width + 1) / 2 : decoder->contract.width;
        size_t height = hevc && plane && decoder->contract.chroma_format_idc == 1 ?
            (decoder->contract.height + 1) / 2 : decoder->contract.height;
        size_t component_bytes = hevc && decoder->contract.hevc_bit_depth == 8 ? 1 : 2;
        size_t bytes_per_pixel = component_bytes * (plane == 1 ? 2 : 1);
        if (CVPixelBufferGetWidthOfPlane(image, plane) != width ||
            CVPixelBufferGetHeightOfPlane(image, plane) != height ||
            CVPixelBufferGetBytesPerRowOfPlane(image, plane) < width * bytes_per_pixel) {
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
    *output = (MdnVtOutput){image, CVPixelBufferGetDataSize(image),
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

int mdn_vt_finish(MdnVtDecoder *decoder, MdnVtError *error) {
    if (error) *error = (MdnVtError){0};
    if (!decoder || decoder->failed) return fail(error, MDN_VT_ARGUMENT, 0);
    OSStatus finish = VTDecompressionSessionFinishDelayedFrames(decoder->session);
    OSStatus wait = VTDecompressionSessionWaitForAsynchronousFrames(decoder->session);
    if (finish || wait) {
        decoder->failed = 1;
        return fail(error, finish ? MDN_VT_FINISH : MDN_VT_WAIT, finish ? finish : wait);
    }
    return 1;
}

int mdn_vt_reset(MdnVtDecoder *decoder, MdnVtError *error) {
    if (!mdn_vt_finish(decoder, error)) return 0;
    if (decoder->contract.codec != kCMVideoCodecType_HEVC) return 1;
    /* Join and invalidate the old temporal owner before constructing another.
     * Retained output buffers are independent of this session and remain live. */
    VTDecompressionSessionInvalidate(decoder->session);
    CFRelease(decoder->session); decoder->session = NULL;
    if (decoder->output) { CVPixelBufferRelease(decoder->output); decoder->output = NULL; }
    decoder->callbacks = 0; decoder->callback_status = 0; decoder->timestamp = kCMTimeInvalid;
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
    if (decoder->output) CVPixelBufferRelease(decoder->output);
    if (decoder->format) CFRelease(decoder->format);
    pthread_mutex_destroy(&decoder->lock);
    free(decoder);
    if (finish) return fail(error, MDN_VT_FINISH, finish);
    if (wait) return fail(error, MDN_VT_WAIT, wait);
    return 1;
}

void mdn_vt_surface_release(void *surface) {
    if (surface) CVPixelBufferRelease((CVPixelBufferRef)surface);
}
