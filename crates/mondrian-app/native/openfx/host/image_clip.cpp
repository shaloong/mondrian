// Copyright OpenFX and contributors to the OpenFX project.
// SPDX-License-Identifier: BSD-3-Clause
#include <iostream>
#include <fstream>
#include <cassert>
#include <algorithm>
#include <cmath>
#include <ctime>

// ofx
#include "ofxCore.h"
#include "ofxImageEffect.h"
#include "ofxPixels.h"

// ofx host
#include "ofxhBinary.h"
#include "ofxhPropertySuite.h"
#include "ofxhClip.h"
#include "ofxhParam.h"
#include "ofxhMemory.h"
#include "ofxhImageEffect.h"
#include "ofxhPluginAPICache.h"
#include "ofxhPluginCache.h"
#include "ofxhHost.h"
#include "ofxhImageEffectAPI.h"

// Mondrian's filter host
#include "host_descriptor.h"
#include "effect_instance.h"
#include "image_clip.h"

namespace MondrianOpenFx {
  FrameImage::FrameImage(FilterClipInstance &clip, OfxTime time, int view)
    : OFX::Host::ImageEffect::Image(clip) /// this ctor will set basic props on the image
    , _frame(clip.frameState())
    , _data(NULL)
  {
    const auto& state = clip.frameState();
    SourceFrameView source{};
    size_t sourceIndex = 0;
    if (!clip.isOutput() && (!state.sourceFrame(time, source, sourceIndex) || !source.pixels)) {
      throw OFX::Host::Property::Exception(kOfxStatErrUnsupported);
    }
    const OfxRectI bounds = clip.isOutput() ? state.outputPixelRod :
        OfxRectI{source.x, source.y, source.x + source.width, source.y + source.height};
    const int width = bounds.x2 - bounds.x1;
    const int height = bounds.y2 - bounds.y1;
    // Each fetch owns an independent property handle; both clips borrow the
    // active frame mapping, which outlives every legal action-scoped handle.
    _data = clip.isOutput() ? state.output : const_cast<OfxRGBAColourF*>(source.pixels);

    // Image-owned metadata identifies this binding, not its reusable address.
    if (clip.isOutput()) { setStringProperty(kOfxImagePropUniqueIdentifier, state.outputIdentifier); }
    else if (sourceIndex == 0) { setStringProperty(kOfxImagePropUniqueIdentifier, state.sourceIdentifier); }
    else { setStringProperty(kOfxImagePropUniqueIdentifier, state.sourceIdentifier + ":set:" +
        std::to_string(state.sourceBindingRevision) + ":sample:" + std::to_string(sourceIndex)); }
    setStringProperty(kOfxImagePropField, kOfxImageFieldNone);

    setDoublePropertyN(kOfxImageEffectPropRenderScale, state.info.render_scale, 2);

    // data ptr
    setPointerProperty(kOfxImagePropData, _data + (height - 1) * width);

    // bounds and rod
    setIntPropertyN(kOfxImagePropBounds, &bounds.x1, 4);
    setIntPropertyN(kOfxImagePropRegionOfDefinition, &bounds.x1, 4);

    // row bytes
    setIntProperty(kOfxImagePropRowBytes, -width * sizeof(OfxRGBAColourF));
    _frame.imageHandles.fetch_add(1);
  }

  FrameImage::~FrameImage() { _frame.imageHandles.fetch_sub(1); }

  FilterClipInstance::FilterClipInstance(FilterEffectInstance* effect, OFX::Host::ImageEffect::ClipDescriptor *desc)
    : OFX::Host::ImageEffect::ClipInstance(effect, *desc)
    , _effect(effect)
    , _name(desc->getName())
    , _outputFetched(false)
  {
  }

  const FrameState& FilterClipInstance::frameState() const
  {
    return _effect->frameState();
  }

  void FilterClipInstance::beginFrame()
  {
    _outputFetched = false;
    // The caller supplied a new Output address for this frame. No image
    // wrapper or native raster survives an action or owns mapped storage.
  }

  FilterClipInstance::~FilterClipInstance() = default;

  /// Connected clips admit Float32; disconnected clips have no source depth.
  const std::string &FilterClipInstance::getUnmappedBitDepth() const
  {
    static const std::string connected(kOfxBitDepthFloat);
    static const std::string disconnected(kOfxBitDepthNone);
    return getConnected() ? connected : disconnected;
  }

  /// Connected clips admit RGBA; disconnected clips have no source components.
  const std::string &FilterClipInstance::getUnmappedComponents() const
  {
    static const std::string connected(kOfxImageComponentRGBA);
    static const std::string disconnected(kOfxImageComponentNone);
    return getConnected() ? connected : disconnected;
  }

  // PreMultiplication -
  //
  //  kOfxImageOpaque - the image is opaque and so has no premultiplication state
  //  kOfxImagePreMultiplied - the image is premultiplied by it's alpha
  //  kOfxImageUnPreMultiplied - the image is unpremultiplied
  const std::string &FilterClipInstance::getPremult() const
  {
    if (isOutput()) { return _effect->getOutputPreMultiplication(); }
    static const std::string straight(kOfxImageUnPreMultiplied);
    static const std::string associated(kOfxImagePreMultiplied);
    static const std::string opaque(kOfxImageOpaque);
    switch (frameState().info.source_alpha) {
      case 1: return associated;
      case 2: return opaque;
      default: return straight; // loadFrame admitted exactly one known ABI value.
    }
  }

  // Pixel Aspect Ratio -
  //
  //  The pixel aspect ratio of a clip or image.
  double FilterClipInstance::getAspectRatio() const
  {
    return frameState().info.pixel_aspect_ratio;
  }

  // Frame Rate -
  double FilterClipInstance::getFrameRate() const
  {
    return frameState().info.frame_rate;
  }

  // Frame Range (startFrame, endFrame) -
  //
  //  The frame range over which a clip has images.
  void FilterClipInstance::getFrameRange(double &startFrame, double &endFrame) const
  {
    startFrame = frameState().info.first_frame;
    endFrame = frameState().info.last_frame;
  }

  /// Field Order - Which spatial field occurs temporally first in a frame.
  /// \returns
  ///  - kOfxImageFieldNone - the clip material is unfielded
  ///  - kOfxImageFieldLower - the clip material is fielded, with image rows 0,2,4.... occurring first in a frame
  ///  - kOfxImageFieldUpper - the clip material is fielded, with image rows line 1,3,5.... occurring first in a frame
  const std::string &FilterClipInstance::getFieldOrder() const
  {
    static const std::string v(kOfxImageFieldNone);
    return v;
  }

  // Connected -
  //
  //  Says whether the clip is actually connected at the moment.
  bool FilterClipInstance::getConnected() const
  {
    // Connection belongs to the selected owner context, not a transient frame
    // borrow. Generator inputs remain disconnected even when named Source.
    return _name == kOfxImageEffectOutputClipName ||
        (_name == kOfxImageEffectSimpleSourceClipName &&
         _effect->getContext() == kOfxImageEffectContextFilter);
  }

  // Unmapped Frame Rate -
  //
  //  The unmaped frame range over which an output clip has images.
  double FilterClipInstance::getUnmappedFrameRate() const
  {
    return frameState().info.frame_rate;
  }

  // Unmapped Frame Range -
  //
  //  The unmaped frame range over which an output clip has images.
  // this is applicable only to hosts and plugins that allow a plugin to change frame rates
  void FilterClipInstance::getUnmappedFrameRange(double &unmappedStartFrame, double &unmappedEndFrame) const
  {
    unmappedStartFrame = frameState().info.first_frame;
    unmappedEndFrame = frameState().info.last_frame;
  }

  // Continuous Samples -
  //
  //  0 if the images can only be sampled at discrete times (eg: the clip is a sequence of frames),
  //  1 if the images can only be sampled continuously (eg: the clip is in fact an animating roto spline and can be rendered anywhen).
  bool FilterClipInstance::getContinuousSamples() const
  {
    return false;
  }


  /// override this to return the rod on the clip canonical coords!
  OfxRectD FilterClipInstance::getRegionOfDefinition(OfxTime time) const
  {
    if (isOutput()) { return _effect->outputRegionOfDefinition(time); }
    // This input has no image outside its declared temporal domain. Its RoD is
    // known to be empty; no Source fetch or pixel/time substitution is needed.
    // Preserve failures for unknown in-range inputs, invalid time and memory.
    if (getConnected() && std::isfinite(time) &&
        (time < frameState().info.first_frame || time > frameState().info.last_frame)) {
      return OfxRectD{0, 0, 0, 0};
    }
    SourceFrameView source{};
    size_t index = 0;
    int failure = kOfxStatFailed;
    if (!getConnected() || !frameState().sourceFrame(time, source, index, &failure)) {
      throw OFX::Host::Property::Exception(failure);
    }
    OfxRectD canonical{};
    if (!canonicalRegionOfDefinition({source.x, source.y, source.x + source.width, source.y + source.height},
        source.pixelAspectRatio, frameState().renderScale(), canonical)) {
      throw OFX::Host::Property::Exception(kOfxStatErrUnsupported);
    }
    return canonical;
  }

  /// override this to fill in the image at the given time.
  /// The bounds of the image on the image plane should be
  /// 'appropriate', typically the value returned in getRegionsOfInterest
  /// on the effect instance. Outside a render call, the optionalBounds should
  /// be 'appropriate' for the.
  /// If bounds is not null, fetch the indicated section of the canonical image plane.
  OFX::Host::ImageEffect::Image* FilterClipInstance::getImage(OfxTime time, const OfxRectD *optionalBounds)
  {
    const auto& frame = frameState();
    if (!getConnected() || !frame.imagesAvailable || (!isOutput() && !frame.input) ||
        (isOutput() && (!frame.output || !frame.outputAvailable || !frame.outputRodReady || frame.outputPixelCount == 0 || time != frame.info.frame)) || !std::isfinite(time)) {
      return nullptr;
    }
    if (!isOutput() && time != frame.info.frame &&
        (!_effect->temporalAccess() || !temporalAccess())) { return nullptr; }
    if (!isOutput()) {
      SourceFrameView source{};
      size_t index = 0;
      int failure = kOfxStatFailed;
      if (!frame.sourceFrame(time, source, index, &failure) || !source.pixels) {
        if (failure == kOfxStatErrMemory) { throw OFX::Host::Property::Exception(failure); }
        return nullptr;
      }
    }
    if (isOutput()) {
      std::lock_guard<std::mutex> lock(_outputFetchMutex);
      auto* image = new FrameImage(*this, time);
      // Initialize only when the current render actually requests Output.
      // An unfetched/denied frame never writes the caller's staging buffer.
      if (!_outputFetched) {
        auto* diagnostic = frame.renderDiagnostic;
        const auto started = diagnostic ? DiagnosticClock::now() : DiagnosticClock::time_point{};
        std::fill(frame.output, frame.output + frame.outputPixelCount, OfxRGBAColourF{0, 0, 0, 0});
        if (diagnostic) {
          diagnostic->output_clear_calls = 1;
          diagnostic->output_clear_bytes = frame.outputPixelCount * sizeof(OfxRGBAColourF);
          if (!diagnosticNanoseconds(started, diagnostic->output_clear_ns)) { diagnostic->counter_overflowed = 1; }
        }
        _outputFetched = true;
      }
      return image;
    }
    return new FrameImage(*this, time);
  }

} // MondrianOpenFx
