#include "videotoolbox_encode.h"
#include <CoreVideo/CoreVideo.h>
#include <VideoToolbox/VideoToolbox.h>
#include <pthread.h>
#include <limits.h>
#include <stdlib.h>
#include <string.h>

struct MdnVtEncoder {
    VTCompressionSessionRef session;
    CVPixelBufferPoolRef pool;
    MdnVtEncodeContract contract;
    pthread_mutex_t mutex;
    uint8_t *packet;
    size_t size;
    uint32_t callbacks, key;
    OSStatus callback_error;
    CMTime expected_pts;
};
static int fail(MdnVtError *e, uint32_t stage, int32_t status) {
    if (e) *e = (MdnVtError){status,stage};
    return 0;
}
static CFNumberRef number(int64_t value) {
    return CFNumberCreate(NULL,kCFNumberSInt64Type,&value);
}
static OSStatus set_number(VTCompressionSessionRef s, CFStringRef key, int64_t n) {
    CFNumberRef v=number(n);
    if (!v) return memFullErr;
    OSStatus r=VTSessionSetProperty(s,key,v);CFRelease(v);return r;
}
static int check_number(VTCompressionSessionRef s, CFStringRef key, int64_t n) {
    CFTypeRef value=NULL;
    OSStatus r=VTSessionCopyProperty(s,key,NULL,&value);
    int64_t actual=0;
    int ok=!r && value && CFGetTypeID(value)==CFNumberGetTypeID()
        && CFNumberGetValue(value,kCFNumberSInt64Type,&actual) && actual==n;
    if(value) CFRelease(value);
    return ok;
}
static CFStringRef primaries(uint32_t n) {
    return n==1 ? kCVImageBufferColorPrimaries_ITU_R_709_2 :
           n==9 ? kCVImageBufferColorPrimaries_ITU_R_2020 : NULL;
}
static CFStringRef transfer(uint32_t n) {
    return n==1 ? kCVImageBufferTransferFunction_ITU_R_709_2 :
           n==16 ? kCVImageBufferTransferFunction_SMPTE_ST_2084_PQ :
           n==18 ? kCVImageBufferTransferFunction_ITU_R_2100_HLG : NULL;
}
static CFStringRef matrix(uint32_t n) {
    return n==1 ? kCVImageBufferYCbCrMatrix_ITU_R_709_2 :
           n==9 ? kCVImageBufferYCbCrMatrix_ITU_R_2020 : NULL;
}
static OSType pixel_format(const MdnVtEncodeContract *c) {
    return c->ten_bit ? (c->full_range ? kCVPixelFormatType_420YpCbCr10BiPlanarFullRange : kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange)
        : (c->full_range ? kCVPixelFormatType_420YpCbCr8BiPlanarFullRange : kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange);
}
static OSStatus parameter_set(const MdnVtEncoder *s, CMFormatDescriptionRef format,
                              size_t index, const uint8_t **data, size_t *bytes,
                              size_t *count, int *nal_size) {
    return s->contract.codec==2
        ? CMVideoFormatDescriptionGetH264ParameterSetAtIndex(format,index,data,bytes,count,nal_size)
        : CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(format,index,data,bytes,count,nal_size);
}
static void callback(void *context, void *frame, OSStatus status,
                     VTEncodeInfoFlags flags, CMSampleBufferRef sample) {
    (void)frame;
    MdnVtEncoder *s=context;
    pthread_mutex_lock(&s->mutex);
    s->callbacks++;
    if(status || !sample || !CMSampleBufferDataIsReady(sample)
       || (flags & kVTEncodeInfo_FrameDropped) || s->callbacks!=1
       || CMTimeCompare(CMSampleBufferGetPresentationTimeStamp(sample),s->expected_pts)!=0) {
        s->callback_error=status ? status : -70100;goto end;
    }
    CFArrayRef a=CMSampleBufferGetSampleAttachmentsArray(sample,false);
    s->key=1;
    if(a && CFArrayGetCount(a)) s->key=CFDictionaryGetValue(CFArrayGetValueAtIndex(a,0),kCMSampleAttachmentKey_NotSync)!=kCFBooleanTrue;
    CMFormatDescriptionRef format=CMSampleBufferGetFormatDescription(sample);
    if(CMFormatDescriptionGetMediaSubType(format)!=(s->contract.codec==2?kCMVideoCodecType_H264:kCMVideoCodecType_HEVC)) {s->callback_error=-70118;goto end;}
    size_t count=0,bytes=0,parameter_bytes=0;int nal_size=0;const uint8_t *data=NULL;
    status=parameter_set(s,format,0,&data,&bytes,&count,&nal_size);
    if(status || nal_size!=4) {s->callback_error=status ? status : -70101;goto end;}
    if(s->key) for(size_t i=0;i<count;i++) {
        status=parameter_set(s,format,i,&data,&bytes,NULL,NULL);
        if(status || parameter_bytes>SIZE_MAX-4 || bytes>SIZE_MAX-parameter_bytes-4) {s->callback_error=status ? status : -70102;goto end;}
        parameter_bytes+=bytes+4;
    }
    CMBlockBufferRef block=CMSampleBufferGetDataBuffer(sample);
    if(!block) {s->callback_error=-70103;goto end;}
    size_t length=CMBlockBufferGetDataLength(block);
    if(length>SIZE_MAX-parameter_bytes) {s->callback_error=-70104;goto end;}
    s->packet=malloc(parameter_bytes+length);
    if(!s->packet) {s->callback_error=memFullErr;goto end;}
    size_t offset=0;
    const uint8_t start[]={0,0,0,1};
    if(s->key) for(size_t i=0;i<count;i++) {
        status=parameter_set(s,format,i,&data,&bytes,NULL,NULL);
        if(status) {s->callback_error=status;goto end;}
        memcpy(s->packet+offset,start,4);offset+=4;memcpy(s->packet+offset,data,bytes);offset+=bytes;
    }
    status=CMBlockBufferCopyDataBytes(block,0,length,s->packet+offset);
    if(status) {s->callback_error=status;goto end;}
    size_t pos=offset;
    int h264_idr=0;
    while(offset+length-pos>=4) {
        uint8_t *p=s->packet+pos;
        uint32_t n=((uint32_t)p[0]<<24)|((uint32_t)p[1]<<16)|((uint32_t)p[2]<<8)|p[3];
        pos+=4;
        if(!n || n>offset+length-pos) {s->callback_error=-70105;goto end;}
        if(s->contract.codec==2 && (s->packet[pos]&31)==5) h264_idr=1;
        memcpy(p,start,4);pos+=n;
    }
    if(pos!=offset+length) {s->callback_error=-70106;goto end;}
    if(s->contract.codec==2 && h264_idr!=(int)s->key) {s->callback_error=-70119;goto end;}
    s->size=pos;
end:
    pthread_mutex_unlock(&s->mutex);
}
void mdn_vt_encoder_close(MdnVtEncoder *s) {
    if(!s)return;
    if(s->session) {VTCompressionSessionInvalidate(s->session);CFRelease(s->session);}
    if(s->pool)CFRelease(s->pool);
    free(s->packet);pthread_mutex_destroy(&s->mutex);free(s);
}
MdnVtEncoder *mdn_vt_encoder_create(const MdnVtEncodeContract *c, MdnVtError *e) {
    if(!c || (c->codec!=1 && c->codec!=2) || (c->codec==2 && c->ten_bit) || !c->width || !c->height || !c->fps_num || c->fps_num>INT_MAX
       || !c->fps_den || !c->sar_num || !c->sar_den || !c->gop
       || !c->metal_registry_id || c->pool_size<2 || c->quantizer<1 || c->quantizer>51
       || !primaries(c->primaries) || !transfer(c->transfer) || !matrix(c->matrix)) {
        fail(e,MDN_VT_ARGUMENT,-70110);return NULL;
    }
    MdnVtEncoder *s=calloc(1,sizeof(*s));
    if(!s) {fail(e,MDN_VT_SESSION,memFullErr);return NULL;}
    s->contract=*c;
    int mutex_status=pthread_mutex_init(&s->mutex,NULL);
    if(mutex_status) {free(s);fail(e,MDN_VT_SESSION,mutex_status);return NULL;}
    CFMutableDictionaryRef spec=CFDictionaryCreateMutable(NULL,0,&kCFTypeDictionaryKeyCallBacks,&kCFTypeDictionaryValueCallBacks);
    CFMutableDictionaryRef attrs=CFDictionaryCreateMutable(NULL,0,&kCFTypeDictionaryKeyCallBacks,&kCFTypeDictionaryValueCallBacks);
    CFNumberRef w=number(c->width),h=number(c->height),f=number(pixel_format(c));
    CFDictionaryRef empty=CFDictionaryCreate(NULL,NULL,NULL,0,&kCFTypeDictionaryKeyCallBacks,&kCFTypeDictionaryValueCallBacks);
    if(!spec || !attrs || !w || !h || !f || !empty) {
        if(spec)CFRelease(spec);if(attrs)CFRelease(attrs);
        if(w)CFRelease(w);if(h)CFRelease(h);if(f)CFRelease(f);if(empty)CFRelease(empty);
        fail(e,MDN_VT_SESSION,memFullErr);goto reject;
    }
    CFDictionarySetValue(spec,kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,kCFBooleanTrue);
    CFDictionarySetValue(attrs,kCVPixelBufferWidthKey,w);CFDictionarySetValue(attrs,kCVPixelBufferHeightKey,h);
    CFDictionarySetValue(attrs,kCVPixelBufferPixelFormatTypeKey,f);CFDictionarySetValue(attrs,kCVPixelBufferMetalCompatibilityKey,kCFBooleanTrue);
    CFDictionarySetValue(attrs,kCVPixelBufferIOSurfacePropertiesKey,empty);
    OSStatus status=VTCompressionSessionCreate(NULL,c->width,c->height,c->codec==2?kCMVideoCodecType_H264:kCMVideoCodecType_HEVC,spec,attrs,NULL,callback,s,&s->session);
    if(!status)status=CVPixelBufferPoolCreate(NULL,NULL,attrs,&s->pool);
    CFRelease(w);CFRelease(h);CFRelease(f);CFRelease(empty);CFRelease(attrs);CFRelease(spec);
    if(status) {fail(e,MDN_VT_SESSION,status);goto reject;}
#define SET(key,value) do { status=VTSessionSetProperty(s->session,key,value);if(status){fail(e,MDN_VT_METADATA,status);goto reject;} } while(0)
#define NUM(key,value) do { status=set_number(s->session,key,value);if(status){fail(e,MDN_VT_METADATA,status);goto reject;} } while(0)
    SET(kVTCompressionPropertyKey_ProfileLevel,c->codec==2?kVTProfileLevel_H264_High_AutoLevel:(c->ten_bit?kVTProfileLevel_HEVC_Main10_AutoLevel:kVTProfileLevel_HEVC_Main_AutoLevel));
    SET(kVTCompressionPropertyKey_AllowFrameReordering,kCFBooleanFalse);
    SET(kVTCompressionPropertyKey_AllowOpenGOP,kCFBooleanFalse);
    SET(kVTCompressionPropertyKey_RealTime,kCFBooleanFalse);
    NUM(kVTCompressionPropertyKey_MaxKeyFrameInterval,c->gop);
    NUM(kVTCompressionPropertyKey_MinAllowedFrameQP,c->quantizer);
    NUM(kVTCompressionPropertyKey_MaxAllowedFrameQP,c->quantizer);
    SET(kVTCompressionPropertyKey_ColorPrimaries,primaries(c->primaries));
    SET(kVTCompressionPropertyKey_TransferFunction,transfer(c->transfer));
    SET(kVTCompressionPropertyKey_YCbCrMatrix,matrix(c->matrix));
    const void *keys[]={kCVImageBufferPixelAspectRatioHorizontalSpacingKey,kCVImageBufferPixelAspectRatioVerticalSpacingKey};
    CFNumberRef sn=number(c->sar_num),sd=number(c->sar_den);
    if(!sn || !sd) {
        if(sn)CFRelease(sn);if(sd)CFRelease(sd);
        fail(e,MDN_VT_METADATA,memFullErr);goto reject;
    }
    const void *values[]={sn,sd};
    CFDictionaryRef sar=CFDictionaryCreate(NULL,keys,values,2,&kCFTypeDictionaryKeyCallBacks,&kCFTypeDictionaryValueCallBacks);
    if(!sar) {CFRelease(sn);CFRelease(sd);fail(e,MDN_VT_METADATA,memFullErr);goto reject;}
    status=VTSessionSetProperty(s->session,kVTCompressionPropertyKey_PixelAspectRatio,sar);
    CFRelease(sar);CFRelease(sn);CFRelease(sd);
    if(status){fail(e,MDN_VT_METADATA,status);goto reject;}
    double fps=(double)c->fps_num/c->fps_den;
    CFNumberRef rate=CFNumberCreate(NULL,kCFNumberDoubleType,&fps);
    if(!rate) {fail(e,MDN_VT_METADATA,memFullErr);goto reject;}
    status=VTSessionSetProperty(s->session,kVTCompressionPropertyKey_ExpectedFrameRate,rate);CFRelease(rate);
    if(status){fail(e,MDN_VT_METADATA,status);goto reject;}
    if(!check_number(s->session,kVTCompressionPropertyKey_MinAllowedFrameQP,c->quantizer)
       || !check_number(s->session,kVTCompressionPropertyKey_MaxAllowedFrameQP,c->quantizer)) {
        fail(e,MDN_VT_METADATA,-70111);goto reject;
    }
    status=VTCompressionSessionPrepareToEncodeFrames(s->session);
    if(status){fail(e,MDN_VT_SESSION,status);goto reject;}
    return s;
reject:mdn_vt_encoder_close(s);return NULL;
#undef SET
#undef NUM
}
void *mdn_vt_encoder_acquire(MdnVtEncoder *s,MdnVtError *e) {
    CFNumberRef count=number(s->contract.pool_size);
    if(!count) {fail(e,MDN_VT_SURFACE,memFullErr);return NULL;}
    const void *key=kCVPixelBufferPoolAllocationThresholdKey,*value=count;
    CFDictionaryRef attrs=CFDictionaryCreate(NULL,&key,&value,1,&kCFTypeDictionaryKeyCallBacks,&kCFTypeDictionaryValueCallBacks);
    if(!attrs) {CFRelease(count);fail(e,MDN_VT_SURFACE,memFullErr);return NULL;}
    CVPixelBufferRef b=NULL;CVReturn r=CVPixelBufferPoolCreatePixelBufferWithAuxAttributes(NULL,s->pool,attrs,&b);
    CFRelease(attrs);CFRelease(count);
    if(r || !b){fail(e,MDN_VT_SURFACE,r);return NULL;}
    CVBufferSetAttachment(b,kCVImageBufferColorPrimariesKey,primaries(s->contract.primaries),kCVAttachmentMode_ShouldPropagate);
    CVBufferSetAttachment(b,kCVImageBufferTransferFunctionKey,transfer(s->contract.transfer),kCVAttachmentMode_ShouldPropagate);
    CVBufferSetAttachment(b,kCVImageBufferYCbCrMatrixKey,matrix(s->contract.matrix),kCVAttachmentMode_ShouldPropagate);
    CVBufferSetAttachment(b,kCVImageBufferChromaLocationTopFieldKey,s->contract.centered_chroma?kCVImageBufferChromaLocation_Center:kCVImageBufferChromaLocation_Left,kCVAttachmentMode_ShouldPropagate);
    CVBufferSetAttachment(b,kCVImageBufferChromaLocationBottomFieldKey,s->contract.centered_chroma?kCVImageBufferChromaLocation_Center:kCVImageBufferChromaLocation_Left,kCVAttachmentMode_ShouldPropagate);
    return b;
}
int mdn_vt_encoder_submit(MdnVtEncoder *s,void *buffer,uint64_t index,MdnVtEncodePacket *out,MdnVtError *e) {
    if(index>(uint64_t)INT64_MAX/s->contract.fps_den || !buffer || !out) return fail(e,MDN_VT_ARGUMENT,-70112);
    CVPixelBufferRef b=buffer;
    if(CVPixelBufferGetWidth(b)!=s->contract.width || CVPixelBufferGetHeight(b)!=s->contract.height
       || CVPixelBufferGetPixelFormatType(b)!=pixel_format(&s->contract) || CVPixelBufferGetPlaneCount(b)!=2
       || !CVPixelBufferGetIOSurface(b)) return fail(e,MDN_VT_SURFACE,-70113);
    free(s->packet);s->packet=NULL;s->size=0;s->callbacks=0;s->callback_error=0;
    s->expected_pts=CMTimeMake(index*s->contract.fps_den,s->contract.fps_num);
    CFDictionaryRef options=NULL;
    if(index%s->contract.gop==0) {
        const void *key=kVTEncodeFrameOptionKey_ForceKeyFrame,*value=kCFBooleanTrue;
        options=CFDictionaryCreate(NULL,&key,&value,1,&kCFTypeDictionaryKeyCallBacks,&kCFTypeDictionaryValueCallBacks);
        if(!options)return fail(e,MDN_VT_ENCODE,memFullErr);
    }
    VTEncodeInfoFlags flags=0;
    OSStatus r=VTCompressionSessionEncodeFrame(s->session,b,s->expected_pts,CMTimeMake(s->contract.fps_den,s->contract.fps_num),options,NULL,&flags);
    if(options)CFRelease(options);
    if(r || (flags&kVTEncodeInfo_FrameDropped))return fail(e,MDN_VT_ENCODE,r?r:-70114);
    /* Bound the codec's delayed ownership to this one input. This is a drain
     * through an exact PTS, not EOF; the same session continues the closed GOP. */
    r=VTCompressionSessionCompleteFrames(s->session,s->expected_pts);
    if(r)return fail(e,MDN_VT_FINISH,r);
    pthread_mutex_lock(&s->mutex);
    int valid=s->callbacks==1 && !s->callback_error && s->size && s->key==(index%s->contract.gop==0);
    int32_t error=s->callback_error;
    pthread_mutex_unlock(&s->mutex);
    if(!valid)return fail(e,MDN_VT_CALLBACK,error?error:-70115);
    CFTypeRef hardware=NULL;
    r=VTSessionCopyProperty(s->session,kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder,NULL,&hardware);
    int accelerated=!r && hardware==kCFBooleanTrue;if(hardware)CFRelease(hardware);
    if(!accelerated)return fail(e,MDN_VT_HARDWARE,r?r:-70116);
    CFTypeRef gpu=NULL;
    r=VTSessionCopyProperty(s->session,kVTCompressionPropertyKey_UsingGPURegistryID,NULL,&gpu);
    if(!r && gpu) {
        int64_t id=0;
        int matches=CFGetTypeID(gpu)==CFNumberGetTypeID() && CFNumberGetValue(gpu,kCFNumberSInt64Type,&id) && (uint64_t)id==s->contract.metal_registry_id;
        CFRelease(gpu);if(!matches)return fail(e,MDN_VT_HARDWARE,-70117);
    } else if(gpu)CFRelease(gpu);
    *out=(MdnVtEncodePacket){s->packet,s->size,(int64_t)index,s->key,1};
    return 1;
}
