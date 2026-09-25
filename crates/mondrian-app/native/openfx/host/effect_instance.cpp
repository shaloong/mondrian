// Copyright OpenFX and contributors to the OpenFX project.
// SPDX-License-Identifier: BSD-3-Clause

#include <iostream>
#include <fstream>

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
#include "parameter_instance.h"

// Filter instance implementation
namespace MondrianOpenFx {

  FilterEffectInstance::FilterEffectInstance(OFX::Host::ImageEffect::ImageEffectPlugin* plugin,
                                     OFX::Host::ImageEffect::Descriptor& desc,
                                     const std::string& context)
                                     : OFX::Host::ImageEffect::Instance(plugin,desc,context,false)
  {
  }

  // class member function implementation

  // get a new clip instance
  OFX::Host::ImageEffect::ClipInstance* FilterEffectInstance::newClipInstance(OFX::Host::ImageEffect::Instance* plugin,
                                                                          OFX::Host::ImageEffect::ClipDescriptor* descriptor,
                                                                          int index)
  {
    return new FilterClipInstance(this,descriptor);
  }


  /// get default output fielding. This is passed into the clip prefs action
  /// and  might be mapped (if the host allows such a thing)
  const std::string &FilterEffectInstance::getDefaultOutputFielding() const
  {
    static const std::string v(kOfxImageFieldNone);
    return v;
  }

  // vmessage
  OfxStatus FilterEffectInstance::vmessage(const char* type,
                                       const char* id,
                                       const char* format,
                                       va_list args)
  {
    printf("%s %s ",type,id);
    vprintf(format,args);
    return kOfxStatOK;
  }

  OfxStatus FilterEffectInstance::setPersistentMessage(const char* type,
                                                   const char* id,
                                                   const char* format,
                                                   va_list args)
  {
    return vmessage(type, id, format, args);
  }

  OfxStatus FilterEffectInstance::clearPersistentMessage()
  {
    return kOfxStatOK;
  }

  void FilterEffectInstance::getProjectSize(double& xSize, double& ySize) const
  {
    xSize = gWidth;
    ySize = gHeight;
  }

  // get the project offset in CANONICAL pixels, we are at 0,0
  void FilterEffectInstance::getProjectOffset(double& xOffset, double& yOffset) const
  {
    xOffset = 0;
    yOffset = 0;
  }

  void FilterEffectInstance::getProjectExtent(double& xSize, double& ySize) const
  {
    xSize = gWidth;
    ySize = gHeight;
  }

  double FilterEffectInstance::getProjectPixelAspectRatio() const
  {
    return gPixelAspectRatio;
  }

  // we are only 25 frames
  double FilterEffectInstance::getEffectDuration() const
  {
    return gLastFrame - gFirstFrame;
  }

  double FilterEffectInstance::getFrameRate() const
  {
    return gFrameRate;
  }

  /// This is called whenever a param is changed by the plugin so that
  /// the recursive instanceChangedAction will be fed the correct frame
  double FilterEffectInstance::getFrameRecursive() const
  {
    return gFrame;
  }

  /// This is called whenever a param is changed by the plugin so that
  /// the recursive instanceChangedAction will be fed the correct
  /// renderScale
  void FilterEffectInstance::getRenderScaleRecursive(double &x, double &y) const
  {
    x = y = 1.0;
  }

  // make a parameter instance
  OFX::Host::Param::Instance* FilterEffectInstance::newParam(const std::string& name, OFX::Host::Param::Descriptor& descriptor)
  {
    if(descriptor.getType()==kOfxParamTypeInteger)
      return new IntegerParameter(this,name,descriptor);
    else if(descriptor.getType()==kOfxParamTypeDouble)
      return new DoubleParameter(this,name,descriptor);
    else if(descriptor.getType()==kOfxParamTypeBoolean)
      return new BooleanParameter(this,name,descriptor);
    else if(descriptor.getType()==kOfxParamTypeChoice)
      return new ChoiceParameter(this,name,descriptor);
    else if(descriptor.getType()==kOfxParamTypeRGBA)
      return new RgbaParameter(this,name,descriptor);
    else if(descriptor.getType()==kOfxParamTypeRGB)
      return new RgbParameter(this,name,descriptor);
    else if(descriptor.getType()==kOfxParamTypeDouble2D)
      return new Double2DParameter(this,name,descriptor);
    else if(descriptor.getType()==kOfxParamTypeInteger2D)
      return new Integer2DParameter(this,name,descriptor);
    else if(descriptor.getType()==kOfxParamTypePushButton)
      return new PushbuttonParameter(this,name,descriptor);
    else if(descriptor.getType()==kOfxParamTypeGroup)
      return new OFX::Host::Param::GroupInstance(descriptor,this);
    else if(descriptor.getType()==kOfxParamTypePage)
      return new OFX::Host::Param::PageInstance(descriptor,this);
    else
      return 0;
  }

  OfxStatus FilterEffectInstance::editBegin(const std::string& name)
  {
    return kOfxStatErrMissingHostFeature;
  }

  OfxStatus FilterEffectInstance::editEnd(){
    return kOfxStatErrMissingHostFeature;
  }

  /// Start doing progress.
  void  FilterEffectInstance::progressStart(const std::string &message, const std::string &messageid)
  {
  }

  /// finish yer progress
  void  FilterEffectInstance::progressEnd()
  {
  }

  /// set the progress to some level of completion, returns
  /// false if you should abandon processing, true to continue
  bool  FilterEffectInstance::progressUpdate(double t)
  {
    return true;
  }


  /// get the current time on the timeline. This is not necessarily the same
  /// time as being passed to an action (eg render)
  double  FilterEffectInstance::timeLineGetTime()
  {
    return gFrame;
  }

  /// set the timeline to a specific time
  void  FilterEffectInstance::timeLineGotoTime(double t)
  {
  }

  /// get the first and last times available on the effect's timeline
  void  FilterEffectInstance::timeLineGetBounds(double &t1, double &t2)
  {
    t1 = gFirstFrame;
    t2 = gLastFrame;
  }

}
