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
    const int width = state.info.width;
    const int height = state.info.height;
    // Each fetch owns an independent property handle; both clips borrow the
    // active frame mapping, which outlives every legal action-scoped handle.
    _data = clip.isOutput() ? state.output : const_cast<OfxRGBAColourF*>(state.input);

    // render scale x and y of 1.0
    setDoubleProperty(kOfxImageEffectPropRenderScale, 1.0, 0);
    setDoubleProperty(kOfxImageEffectPropRenderScale, 1.0, 1);

    // data ptr
    setPointerProperty(kOfxImagePropData, _data + (height - 1) * width);

    // bounds and rod
    setIntProperty(kOfxImagePropBounds, 0, 0);
    setIntProperty(kOfxImagePropBounds, 0, 1);
    setIntProperty(kOfxImagePropBounds, width, 2);
    setIntProperty(kOfxImagePropBounds, height, 3);

    setIntProperty(kOfxImagePropRegionOfDefinition, 0, 0);
    setIntProperty(kOfxImagePropRegionOfDefinition, 0, 1);
    setIntProperty(kOfxImagePropRegionOfDefinition, width, 2);
    setIntProperty(kOfxImagePropRegionOfDefinition, height, 3);

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

  /// Float32 is the only admitted image depth.
  const std::string &FilterClipInstance::getUnmappedBitDepth() const
  {
    static const std::string v(kOfxBitDepthFloat);
    return v;
  }

  /// RGBA is the only admitted component layout.
  const std::string &FilterClipInstance::getUnmappedComponents() const
  {
    static const std::string v(kOfxImageComponentRGBA);
    return v;
  }

  // PreMultiplication -
  //
  //  kOfxImageOpaque - the image is opaque and so has no premultiplication state
  //  kOfxImagePreMultiplied - the image is premultiplied by it's alpha
  //  kOfxImageUnPreMultiplied - the image is unpremultiplied
  const std::string &FilterClipInstance::getPremult() const
  {
    if (isOutput()) { return _effect->getOutputPreMultiplication(); }
    static const std::string v(kOfxImageUnPreMultiplied);
    return v;
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
    return _name == "Source" || _name == "Output";
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
    OfxRectD v;
    v.x1 = v.y1 = 0;
    v.x2 = frameState().info.width * frameState().info.pixel_aspect_ratio;
    v.y2 = frameState().info.height;
    return v;
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
    if (!getConnected() || !frame.imagesAvailable || !frame.input ||
        (isOutput() && (!frame.output || !frame.outputAvailable)) || !std::isfinite(time) || time != frame.info.frame) {
      return nullptr;
    }
    if (isOutput()) {
      std::lock_guard<std::mutex> lock(_outputFetchMutex);
      auto* image = new FrameImage(*this, time);
      // Initialize only when the current render actually requests Output.
      // An unfetched/denied frame never writes the caller's staging buffer.
      if (!_outputFetched) {
        std::fill(frame.output, frame.output + frame.pixelCount, OfxRGBAColourF{0, 0, 0, 0});
        _outputFetched = true;
      }
      return image;
    }
    return new FrameImage(*this, time);
  }

} // MondrianOpenFx
