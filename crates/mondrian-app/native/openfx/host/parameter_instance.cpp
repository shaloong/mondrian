// Copyright OpenFX and contributors to the OpenFX project.
// SPDX-License-Identifier: BSD-3-Clause

#include <iostream>
#include <fstream>
#include <cmath>

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

namespace MondrianOpenFx {

  //
  // IntegerParameter
  //

  IntegerParameter::IntegerParameter(FilterEffectInstance* effect,
                                       const std::string& name,
                                       OFX::Host::Param::Descriptor& descriptor)
    : _effect(effect), _descriptor(descriptor), _value(descriptor.getProperties().getIntProperty(kOfxParamPropDefault)), OFX::Host::Param::IntegerInstance(descriptor)
  {
  }

  OfxStatus IntegerParameter::get(int& value)
  {
    value = _value;
    return kOfxStatOK;
  }

  OfxStatus IntegerParameter::get(OfxTime time, int& value)
  {
    if (!std::isfinite(time)) { return kOfxStatErrValue; }
    return !getCanAnimate() || time == _effect->frameState().info.frame
        ? get(value) : kOfxStatErrMissingHostFeature;
  }

  OfxStatus IntegerParameter::set(int value)
  {
    const auto& properties = _descriptor.getProperties();
    if (value < properties.getIntProperty(kOfxParamPropMin) || value > properties.getIntProperty(kOfxParamPropMax)) { return kOfxStatErrValue; }
    _value = value;
    return kOfxStatOK;
  }

  OfxStatus IntegerParameter::set(OfxTime time, int value) {
    return std::isfinite(time) && time == _effect->frameState().info.frame ? set(value) : kOfxStatErrMissingHostFeature;
  }

  //
  // DoubleParameter
  //

  DoubleParameter::DoubleParameter(FilterEffectInstance* effect,
                                     const std::string& name,
                                     OFX::Host::Param::Descriptor& descriptor)
    : _effect(effect), _descriptor(descriptor), _value(descriptor.getProperties().getDoubleProperty(kOfxParamPropDefault)), OFX::Host::Param::DoubleInstance(descriptor)
  {
  }

  OfxStatus DoubleParameter::get(double& d)
  {
    d = _value;
    return kOfxStatOK;
  }

  OfxStatus DoubleParameter::get(OfxTime time, double& d)
  {
    if (!std::isfinite(time)) { return kOfxStatErrValue; }
    return !getCanAnimate() || time == _effect->frameState().info.frame
        ? get(d) : kOfxStatErrMissingHostFeature;
  }

  OfxStatus DoubleParameter::set(double value)
  {
    const auto& properties = _descriptor.getProperties();
    if (!std::isfinite(value) ||
        value < properties.getDoubleProperty(kOfxParamPropMin) ||
        value > properties.getDoubleProperty(kOfxParamPropMax)) {
      return kOfxStatErrValue;
    }
    _value = value;
    return kOfxStatOK;
  }

  OfxStatus DoubleParameter::set(OfxTime time, double value)
  {
    return std::isfinite(time) && time == _effect->frameState().info.frame
        ? set(value) : kOfxStatErrMissingHostFeature;
  }

  OfxStatus DoubleParameter::derive(OfxTime time, double& value)
  {
    if (!std::isfinite(time)) { return kOfxStatErrValue; }
    if (getCanAnimate()) { return kOfxStatErrMissingHostFeature; }
    value = 0.0;
    return kOfxStatOK;
  }

  OfxStatus DoubleParameter::integrate(OfxTime time1, OfxTime time2, double& value)
  {
    if (!std::isfinite(time1) || !std::isfinite(time2)) { return kOfxStatErrValue; }
    if (getCanAnimate()) { return kOfxStatErrMissingHostFeature; }
    double result = 0.0;
    if (_value != 0.0 && time1 != time2) {
      const double interval = time2 - time1;
      // Finite endpoints can have an unrepresentable difference while the
      // scaled integral is finite. Halve only that large interval; halving
      // tiny intervals would lose subnormal precision. Windows long double
      // has no extra range, so do not rely on an extended intermediate type.
      result = std::isfinite(interval) ? _value * interval
          : (_value * (time2 * 0.5 - time1 * 0.5)) * 2.0;
      if (!std::isfinite(result)) { return kOfxStatErrValue; }
    }
    value = result;
    return kOfxStatOK;
  }

  //
  // BooleanParameter
  //

  BooleanParameter::BooleanParameter(FilterEffectInstance* effect,
                                       const std::string& name,
                                       OFX::Host::Param::Descriptor& descriptor)
    : _effect(effect), _descriptor(descriptor), _value(descriptor.getProperties().getIntProperty(kOfxParamPropDefault) != 0), OFX::Host::Param::BooleanInstance(descriptor)
  {
  }

  OfxStatus BooleanParameter::get(bool& b)
  {
    b = _value;
    return kOfxStatOK;
  }

  OfxStatus BooleanParameter::get(OfxTime time, bool& b)
  {
    if (!std::isfinite(time)) { return kOfxStatErrValue; }
    return !getCanAnimate() || time == _effect->frameState().info.frame
        ? get(b) : kOfxStatErrMissingHostFeature;
  }

  OfxStatus BooleanParameter::set(bool value)
  {
    _value = value;
    return kOfxStatOK;
  }

  OfxStatus BooleanParameter::set(OfxTime time, bool value) {
    return std::isfinite(time) && time == _effect->frameState().info.frame
        ? set(value) : kOfxStatErrMissingHostFeature;
  }

  //
  // ChoiceParameter
  //

  ChoiceParameter::ChoiceParameter(FilterEffectInstance* effect,
                                     const std::string& name,
                                     OFX::Host::Param::Descriptor& descriptor)
    : OFX::Host::Param::ChoiceInstance(descriptor), _effect(effect),
      _value(descriptor.getProperties().getIntProperty(kOfxParamPropDefault))
  {
  }

  OfxStatus ChoiceParameter::get(int& value)
  {
    const int count = getProperties().getDimension(kOfxParamPropChoiceOption);
    if (_value < 0 || _value >= count) { return kOfxStatErrValue; }
    value = _value;
    return kOfxStatOK;
  }

  OfxStatus ChoiceParameter::get(OfxTime time, int& value)
  {
    if (!std::isfinite(time)) { return kOfxStatErrValue; }
    return !getCanAnimate() || time == _effect->frameState().info.frame
        ? get(value) : kOfxStatErrMissingHostFeature;
  }

  OfxStatus ChoiceParameter::set(int value)
  {
    const int count = getProperties().getDimension(kOfxParamPropChoiceOption);
    if (value < 0 || value >= count) { return kOfxStatErrValue; }
    _value = value;
    return kOfxStatOK;
  }

  OfxStatus ChoiceParameter::set(OfxTime time, int value)
  {
    return std::isfinite(time) && time == _effect->frameState().info.frame
        ? set(value) : kOfxStatErrMissingHostFeature;
  }

  StrChoiceParameter::StrChoiceParameter(FilterEffectInstance* effect,
      OFX::Host::Param::Descriptor& descriptor)
    : OFX::Host::Param::StringInstance(descriptor), _effect(effect),
      _value(descriptor.getProperties().getStringProperty(kOfxParamPropDefault)) {
    if (_value.empty() && getProperties().getDimension(kOfxParamPropChoiceEnum) > 0) {
      _value = getProperties().getStringProperty(kOfxParamPropChoiceEnum, 0);
    }
  }

  bool StrChoiceParameter::contains(const std::string& value) const {
    const auto& properties = getProperties();
    const int count = properties.getDimension(kOfxParamPropChoiceEnum);
    if (count != properties.getDimension(kOfxParamPropChoiceOption)) { return false; }
    for (int index = 0; index < count; ++index) {
      if (value == properties.getStringProperty(kOfxParamPropChoiceEnum, index)) { return true; }
    }
    return false;
  }
  OfxStatus StrChoiceParameter::get(std::string& value) {
    if (!contains(_value)) { return kOfxStatErrValue; }
    value = _value;
    return kOfxStatOK;
  }
  OfxStatus StrChoiceParameter::get(OfxTime time, std::string& value) {
    if (!std::isfinite(time)) { return kOfxStatErrValue; }
    return !getCanAnimate() || time == _effect->frameState().info.frame
        ? get(value) : kOfxStatErrMissingHostFeature;
  }
  OfxStatus StrChoiceParameter::set(const char* value) {
    if (!value || !contains(value)) { return kOfxStatErrValue; }
    _value = value;
    return kOfxStatOK;
  }
  OfxStatus StrChoiceParameter::set(OfxTime time, const char* value) {
    return std::isfinite(time) && time == _effect->frameState().info.frame
        ? set(value) : kOfxStatErrMissingHostFeature;
  }

  //
  // RgbaParameter
  //

  RgbaParameter::RgbaParameter(FilterEffectInstance* effect,
                                 const std::string& name,
                                 OFX::Host::Param::Descriptor& descriptor)
    : _effect(effect), _descriptor(descriptor), OFX::Host::Param::RGBAInstance(descriptor)
  {
  }

  OfxStatus RgbaParameter::get(double&,double&,double&,double&)
  {
    return kOfxStatErrMissingHostFeature;
  }

  OfxStatus RgbaParameter::get(OfxTime time, double&,double&,double&,double&)
  {
    return kOfxStatErrMissingHostFeature;
  }

  OfxStatus RgbaParameter::set(double,double,double,double)
  {
    return kOfxStatErrMissingHostFeature;
  }

  OfxStatus RgbaParameter::set(OfxTime time, double,double,double,double)
  {
    return kOfxStatErrMissingHostFeature;
  }

  //
  // RgbParameter
  //

  RgbParameter::RgbParameter(FilterEffectInstance* effect,
                               const std::string& name,
                               OFX::Host::Param::Descriptor& descriptor)
    : _effect(effect), _descriptor(descriptor), OFX::Host::Param::RGBInstance(descriptor)
  {
  }

  OfxStatus RgbParameter::get(double&,double&,double&)
  {
    return kOfxStatErrMissingHostFeature;
  }

  OfxStatus RgbParameter::get(OfxTime time, double&,double&,double&)
  {
    return kOfxStatErrMissingHostFeature;
  }

  OfxStatus RgbParameter::set(double,double,double)
  {
    return kOfxStatErrMissingHostFeature;
  }

  OfxStatus RgbParameter::set(OfxTime time, double,double,double)
  {
    return kOfxStatErrMissingHostFeature;
  }

  //
  // Double2DParameter
  //

  Double2DParameter::Double2DParameter(FilterEffectInstance* effect,
                                         const std::string& name,
                                         OFX::Host::Param::Descriptor& descriptor)
    : _effect(effect), _descriptor(descriptor), OFX::Host::Param::Double2DInstance(descriptor)
  {
  }

  OfxStatus Double2DParameter::get(double&,double&)
  {
    return kOfxStatErrMissingHostFeature;
  }

  OfxStatus Double2DParameter::get(OfxTime time,double&,double&)
  {
    return kOfxStatErrMissingHostFeature;
  }

  OfxStatus Double2DParameter::set(double,double)
  {
    return kOfxStatErrMissingHostFeature;
  }

  OfxStatus Double2DParameter::set(OfxTime time,double,double)
  {
    return kOfxStatErrMissingHostFeature;
  }

  //
  // Integer2DParameter
  //

  Integer2DParameter::Integer2DParameter(FilterEffectInstance* effect,
                                           const std::string& name,
                                           OFX::Host::Param::Descriptor& descriptor)
    : _effect(effect), _descriptor(descriptor), OFX::Host::Param::Integer2DInstance(descriptor)
  {
  }

  OfxStatus Integer2DParameter::get(int&,int&)
  {
    return kOfxStatErrMissingHostFeature;
  }

  OfxStatus Integer2DParameter::get(OfxTime time,int&,int&)
  {
    return kOfxStatErrMissingHostFeature;
  }

  OfxStatus Integer2DParameter::set(int,int)
  {
    return kOfxStatErrMissingHostFeature;
  }

  OfxStatus Integer2DParameter::set(OfxTime time,int,int)
  {
    return kOfxStatErrMissingHostFeature;
  }

  //
  // Integer2DParameter
  //

  PushbuttonParameter::PushbuttonParameter(FilterEffectInstance* effect,
                                             const std::string& name,
                                             OFX::Host::Param::Descriptor& descriptor)
    : _effect(effect), _descriptor(descriptor), OFX::Host::Param::PushbuttonInstance(descriptor)
  {
  }

}
