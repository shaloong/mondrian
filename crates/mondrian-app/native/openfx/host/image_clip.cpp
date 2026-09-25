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
  int gWidth = 0;
  int gHeight = 0;
  double gFrame = 0.0;
  double gFrameRate = 24.0;
  double gFirstFrame = 0.0;
  double gLastFrame = 0.0;
  double gPixelAspectRatio = 1.0;
  std::vector<OfxRGBAColourF> gInput;

  FrameImage::FrameImage(FilterClipInstance &clip, OfxTime time, int view)
    : OFX::Host::ImageEffect::Image(clip) /// this ctor will set basic props on the image
    , _data(NULL)
  {
    // make some memory
    _data = new OfxRGBAColourF[gWidth * gHeight];
    std::fill(_data, _data + gWidth * gHeight, OfxRGBAColourF{0, 0, 0, 0});
    if (!clip.isOutput()) {
      std::copy(gInput.begin(), gInput.end(), _data);
    }

    // render scale x and y of 1.0
    setDoubleProperty(kOfxImageEffectPropRenderScale, 1.0, 0);
    setDoubleProperty(kOfxImageEffectPropRenderScale, 1.0, 1);

    // data ptr
    setPointerProperty(kOfxImagePropData, _data + (gHeight - 1) * gWidth);

    // bounds and rod
    setIntProperty(kOfxImagePropBounds, 0, 0);
    setIntProperty(kOfxImagePropBounds, 0, 1);
    setIntProperty(kOfxImagePropBounds, gWidth, 2);
    setIntProperty(kOfxImagePropBounds, gHeight, 3);

    setIntProperty(kOfxImagePropRegionOfDefinition, 0, 0);
    setIntProperty(kOfxImagePropRegionOfDefinition, 0, 1);
    setIntProperty(kOfxImagePropRegionOfDefinition, gWidth, 2);
    setIntProperty(kOfxImagePropRegionOfDefinition, gHeight, 3);

    // row bytes
    setIntProperty(kOfxImagePropRowBytes, -gWidth * sizeof(OfxRGBAColourF));
  }

  OfxRGBAColourF* FrameImage::pixel(int x, int y) const
  {
    OfxRectI bounds = getBounds();
    if ((x >= bounds.x1) && ( x< bounds.x2) && ( y >= bounds.y1) && ( y < bounds.y2) )
    {
      int rowBytes = getIntProperty(kOfxImagePropRowBytes);
      int offset = (y - bounds.y1) * rowBytes + (x - bounds.x1) * sizeof(OfxRGBAColourF);
      char* bottomRow = reinterpret_cast<char*>(_data + (gHeight - 1) * gWidth);
      return reinterpret_cast<OfxRGBAColourF*>(bottomRow + offset);
    }
    return 0;
  }

  FrameImage::~FrameImage()
  {
    delete[] _data;
  }

  FilterClipInstance::FilterClipInstance(FilterEffectInstance* effect, OFX::Host::ImageEffect::ClipDescriptor *desc)
    : OFX::Host::ImageEffect::ClipInstance(effect, *desc)
    , _effect(effect)
    , _name(desc->getName())
    , _outputImage(NULL)
  {
  }

  FilterClipInstance::~FilterClipInstance()
  {
    if(_outputImage)
      _outputImage->releaseReference();
  }

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
    static const std::string v(kOfxImageUnPreMultiplied);
    return v;
  }

  // Pixel Aspect Ratio -
  //
  //  The pixel aspect ratio of a clip or image.
  double FilterClipInstance::getAspectRatio() const
  {
    return gPixelAspectRatio;
  }

  // Frame Rate -
  double FilterClipInstance::getFrameRate() const
  {
    return gFrameRate;
  }

  // Frame Range (startFrame, endFrame) -
  //
  //  The frame range over which a clip has images.
  void FilterClipInstance::getFrameRange(double &startFrame, double &endFrame) const
  {
    startFrame = gFirstFrame;
    endFrame = gLastFrame;
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
    return gFrameRate;
  }

  // Unmapped Frame Range -
  //
  //  The unmaped frame range over which an output clip has images.
  // this is applicable only to hosts and plugins that allow a plugin to change frame rates
  void FilterClipInstance::getUnmappedFrameRange(double &unmappedStartFrame, double &unmappedEndFrame) const
  {
    unmappedStartFrame = gFirstFrame;
    unmappedEndFrame = gLastFrame;
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
    v.x2 = gWidth * gPixelAspectRatio;
    v.y2 = gHeight;
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
    if (!getConnected() || !std::isfinite(time) || time != gFrame) { return nullptr; }
    if(_name == "Output") {
      if(!_outputImage) {
        // make a new ref counted image
        _outputImage = new FrameImage(*this, 0);
      }

      // add another reference to the member image for this fetch
      // as we have a ref count of 1 due to construction, this will
      // cause the output image never to delete by the plugin
      // when it releases the image
      _outputImage->addReference();

      // return it
      return _outputImage;
    }
    else {
      // Fetch on demand for the input clip.
      // It does get deleted after the plugin is done with it as we
      // have not incremented the auto ref
      //
      // You should do somewhat more sophisticated image management
      // than this.
      FrameImage *image = new FrameImage(*this, time);
      return image;
    }
  }

} // MondrianOpenFx
