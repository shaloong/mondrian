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

namespace MondrianOpenFx {

  //
  // IntegerParameter
  //

  IntegerParameter::IntegerParameter(FilterEffectInstance* effect,
                                       const std::string& name,
                                       OFX::Host::Param::Descriptor& descriptor)
    : _effect(effect), _descriptor(descriptor), OFX::Host::Param::IntegerInstance(descriptor)
  {
  }

  OfxStatus IntegerParameter::get(int&)
  {
    return kOfxStatErrMissingHostFeature;
  }

  OfxStatus IntegerParameter::get(OfxTime time, int&)
  {
    return kOfxStatErrMissingHostFeature;
  }

  OfxStatus IntegerParameter::set(int)
  {
    return kOfxStatErrMissingHostFeature;
  }

  OfxStatus IntegerParameter::set(OfxTime time, int) {
    return kOfxStatErrMissingHostFeature;
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
    d = _value;
    return kOfxStatOK;
  }

  OfxStatus DoubleParameter::set(double value)
  {
    _value = value;
    return kOfxStatOK;
  }

  OfxStatus DoubleParameter::set(OfxTime time, double value)
  {
    return set(value);
  }

  OfxStatus DoubleParameter::derive(OfxTime time, double&)
  {
    return kOfxStatErrMissingHostFeature;
  }

  OfxStatus DoubleParameter::integrate(OfxTime time1, OfxTime time2, double&)
  {
    return kOfxStatErrMissingHostFeature;
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
    b = _value;
    return kOfxStatOK;
  }

  OfxStatus BooleanParameter::set(bool value)
  {
    _value = value;
    return kOfxStatOK;
  }

  OfxStatus BooleanParameter::set(OfxTime time, bool value) {
    return set(value);
  }

  //
  // MyChoiceInteger
  //

  ChoiceParameter::ChoiceParameter(FilterEffectInstance* effect,
                                     const std::string& name,
                                     OFX::Host::Param::Descriptor& descriptor)
    : _effect(effect), _descriptor(descriptor), OFX::Host::Param::ChoiceInstance(descriptor)
  {
  }

  OfxStatus ChoiceParameter::get(int&)
  {
    return kOfxStatErrMissingHostFeature;
  }

  OfxStatus ChoiceParameter::get(OfxTime time, int&)
  {
    return kOfxStatErrMissingHostFeature;
  }

  OfxStatus ChoiceParameter::set(int)
  {
    return kOfxStatErrMissingHostFeature;
  }

  OfxStatus ChoiceParameter::set(OfxTime time, int)
  {
    return kOfxStatErrMissingHostFeature;
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
