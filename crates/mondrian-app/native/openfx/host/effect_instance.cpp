// Copyright OpenFX and contributors to the OpenFX project.
// SPDX-License-Identifier: BSD-3-Clause

#include <iostream>
#include <fstream>
#include <stdexcept>
#include <cmath>
#include <cstring>

// ofx
#include "ofxCore.h"
#include "ofxImageEffect.h"
#include "ofxGPURender.h"
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
                                     const std::string& context, FrameState& frameState, AuxSuite& auxiliary)
                                     : AuxRegistrationGuard(auxiliary, true), OFX::Host::ImageEffect::Instance(plugin,desc,context,false), _frameState(frameState)
  {
    bindAuxiliary(getHandle());
    if (auxiliary.bindTimeline(getHandle(), frameState.info.frame, frameState.info.first_frame, frameState.info.last_frame) != kOfxStatOK) {
      throw std::runtime_error("OpenFX auxiliary initial timeline could not be bound");
    }
    // An instance starts with its descriptor's capability. Plugin changes must
    // remain instance-local rather than changing the chained descriptor.
    const auto& cpu = desc.getProps().getStringProperty(kOfxImageEffectPropCPURenderSupported);
    getProps().createProperty({kOfxImageEffectPropCPURenderSupported,
                              OFX::Host::Property::eString, 1, false, cpu.c_str()});
  }

  FilterEffectInstance::~FilterEffectInstance() {
    closeInterface();
  }

  void FilterEffectInstance::setCustomOutArgs(const std::string& action, OFX::Host::Property::Set& outArgs) {
    OFX::Host::ImageEffect::Instance::setCustomOutArgs(action, outArgs);
    if (action != kOfxImageEffectActionGetClipPreferences) { return; }
    // The single-PAR host supplies the actual connected grid, including a
    // Generator's output grid, rather than substituting project geometry.
    for (const auto& entry : _clips) {
      const auto* clip = entry.second;
      if (!clip->getConnected()) { continue; }
      outArgs.setDoubleProperty("OfxImageClipPropPAR_" + entry.first, clip->getAspectRatio());
    }
  }

  OfxStatus FilterEffectInstance::mainEntry(const char* action, const void* handle,
                                           OFX::Host::Property::Set* inArgs,
                                           OFX::Host::Property::Set* outArgs) {
    const OfxStatus status = OFX::Host::ImageEffect::Instance::mainEntry(action, handle, inArgs, outArgs);
    // ReplyDefault's residual outArgs do not request preference changes.
    if (status != kOfxStatOK || !outArgs || std::strcmp(action, kOfxImageEffectActionGetClipPreferences) != 0) {
      return status;
    }
    // SetableFielding is false: the actual clip, image and Render raster are
    // progressive. Refuse a handled request that cannot preserve that contract.
    if (outArgs->getDimension(kOfxImageClipPropFieldOrder) != 1 ||
        outArgs->getStringProperty(kOfxImageClipPropFieldOrder) != kOfxImageFieldNone) {
      return kOfxStatFailed;
    }
    // This host cannot remap a connected clip's sampling. Reject before the
    // SDK consumes the preferences and before the native Render action.
    for (const auto& entry : _clips) {
      const auto* clip = entry.second;
      if (!clip->getConnected()) { continue; }
      const std::string property = "OfxImageClipPropPAR_" + entry.first;
      if (outArgs->getDimension(property) != 1) { return kOfxStatFailed; }
      const double par = outArgs->getDoubleProperty(property);
      if (!std::isfinite(par) || par <= 0.0 || par != clip->getAspectRatio()) {
        return kOfxStatFailed;
      }
    }
    return status;
  }

  OfxStatus FilterEffectInstance::setInterfaceOpen(bool open) {
    if (!open) { return closeInterface(); }
    if (_interfaceOpen) { return kOfxStatOK; }
    // The caller has already bound the current parent-controlled invocation.
    // This does not grant local-only author mutation: C2 must own that bridge.
    if (!auxiliary().hasInteractiveControl()) { return kOfxStatFailed; }
    _frameState.readOnlyInteractive = true;
    OFX::Host::ImageEffect::Instance::_interactive = true;
    getProps().setIntProperty(kOfxPropIsInteractive, 1);
    if (auxiliary().bindInteractive(getHandle(), true) != kOfxStatOK) {
      OFX::Host::ImageEffect::Instance::_interactive = false;
      _frameState.readOnlyInteractive = false;
      getProps().setIntProperty(kOfxPropIsInteractive, 0);
      return kOfxStatFailed;
    }
    // The host generic editor is genuinely open before Begin. This records
    // the host lifecycle, not success of the plugin-private UI setup. The
    // published Begin postcondition and End precondition depend on Begin
    // being called, not on an OK reply; Failed keeps this host editor open.
    _interfaceOpen = true;
    const OfxStatus status = beginInstanceEditAction();
    return status;
  }

  OfxStatus FilterEffectInstance::closeInterface() noexcept {
    if (!_interfaceOpen) { return kOfxStatOK; }
    _interfaceOpen = false;
    try {
      // The last real GUI has closed before End enters plugin code. Questions
      // during End may fail honestly; they cannot keep a closed view alive.
      OFX::Host::ImageEffect::Instance::_interactive = false;
      getProps().setIntProperty(kOfxPropIsInteractive, 0);
      if (auxiliary().bindInteractive(getHandle(), false) != kOfxStatOK) {
        std::terminate();
      }
      const OfxStatus status = endInstanceEditAction();
      _frameState.readOnlyInteractive = false;
      return status;
    } catch (...) {
      // No C++ exception can escape a destruction callback, leaving lifetime
      // bookkeeping live after this supervised native owner has gone away.
      std::terminate();
    }
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
    return auxiliary().message(getHandle(), type, id, format, args);
  }

  OfxStatus FilterEffectInstance::setPersistentMessage(const char* type,
                                                   const char* id,
                                                   const char* format,
                                                   va_list args)
  {
    return auxiliary().persistent(getHandle(), type, id, format, args);
  }

  OfxStatus FilterEffectInstance::clearPersistentMessage()
  {
    return auxiliary().clear(getHandle());
  }

  void FilterEffectInstance::getProjectSize(double& xSize, double& ySize) const
  {
    xSize = static_cast<double>(_frameState.info.project_width) * _frameState.info.project_pixel_aspect_ratio;
    ySize = _frameState.info.project_height;
  }

  // get the project offset in CANONICAL pixels, we are at 0,0
  void FilterEffectInstance::getProjectOffset(double& xOffset, double& yOffset) const
  {
    xOffset = 0;
    yOffset = 0;
  }

  void FilterEffectInstance::getProjectExtent(double& xSize, double& ySize) const
  {
    xSize = static_cast<double>(_frameState.info.project_width) * _frameState.info.project_pixel_aspect_ratio;
    ySize = _frameState.info.project_height;
  }

  double FilterEffectInstance::getProjectPixelAspectRatio() const
  {
    return _frameState.info.project_pixel_aspect_ratio;
  }

  // we are only 25 frames
  double FilterEffectInstance::getEffectDuration() const
  {
    return _frameState.info.last_frame - _frameState.info.first_frame;
  }

  double FilterEffectInstance::getFrameRate() const
  {
    return _frameState.info.frame_rate;
  }

  /// This is called whenever a param is changed by the plugin so that
  /// the recursive instanceChangedAction will be fed the correct frame
  double FilterEffectInstance::getFrameRecursive() const
  {
    return _frameState.info.frame;
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
    else if(descriptor.getType()==kOfxParamTypeStrChoice)
      return new StrChoiceParameter(this,descriptor);
    else if(descriptor.getType()==kOfxParamTypeString)
      return new StringParameter(this,descriptor);
    else if(descriptor.getType()==kOfxParamTypeRGBA)
      return new NumericTupleParameter<OFX::Host::Param::RGBAInstance, double, 4>(this,descriptor);
    else if(descriptor.getType()==kOfxParamTypeRGB)
      return new NumericTupleParameter<OFX::Host::Param::RGBInstance, double, 3>(this,descriptor);
    else if(descriptor.getType()==kOfxParamTypeDouble2D)
      return new NumericTupleParameter<OFX::Host::Param::Double2DInstance, double, 2>(this,descriptor);
    else if(descriptor.getType()==kOfxParamTypeInteger2D)
      return new NumericTupleParameter<OFX::Host::Param::Integer2DInstance, int, 2>(this,descriptor);
    else if(descriptor.getType()==kOfxParamTypeDouble3D)
      return new NumericTupleParameter<OFX::Host::Param::Double3DInstance, double, 3>(this,descriptor);
    else if(descriptor.getType()==kOfxParamTypeInteger3D)
      return new NumericTupleParameter<OFX::Host::Param::Integer3DInstance, int, 3>(this,descriptor);
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
    auxiliary().progressStart(getHandle(), message.c_str(), messageid.c_str());
  }

  /// finish yer progress
  void  FilterEffectInstance::progressEnd()
  {
    auxiliary().progressEnd(getHandle());
  }

  /// set the progress to some level of completion, returns
  /// false if you should abandon processing, true to continue
  bool  FilterEffectInstance::progressUpdate(double t)
  {
    return auxiliary().progressUpdate(getHandle(), t) == kOfxStatOK;
  }


  /// get the current time on the timeline. This is not necessarily the same
  /// time as being passed to an action (eg render)
  double  FilterEffectInstance::timeLineGetTime()
  {
    double time = 0.0;
    auxiliary().timelineTime(getHandle(), &time);
    return time;
  }

  /// set the timeline to a specific time
  void  FilterEffectInstance::timeLineGotoTime(double t)
  {
    auxiliary().timelineGoto(getHandle(), t);
  }

  /// get the first and last times available on the effect's timeline
  void  FilterEffectInstance::timeLineGetBounds(double &t1, double &t2)
  {
    auxiliary().timelineBounds(getHandle(), &t1, &t2);
  }

}
