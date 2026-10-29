// Copyright OpenFX and contributors to the OpenFX project.
// SPDX-License-Identifier: BSD-3-Clause

#include <iostream>
#include <fstream>
#include <cmath>
#include <stdexcept>
#include <type_traits>
#include <limits>

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
#include "render.h"

namespace MondrianOpenFx {

  template<class T, size_t N>
  OfxStatus curveNumbers(const FilterEffectInstance& effect, const std::string& name,
                        int operation, double start, double end, const std::array<T*, N>& outputs,
                        const OFX::Host::Property::Set* bounds = nullptr) {
    MondrianOpenFxQueryResult result{};
    const auto status = effect.frameState().queryParameter(name, operation, start, end, 0, 0, result);
    if (status != kOfxStatOK) { return status; }
    std::array<T, N> values{};
    for (size_t index = 0; index < N; ++index) {
      const double value = result.value[index];
      if (!std::isfinite(value)) { return kOfxStatErrValue; }
      if constexpr (std::is_integral_v<T>) {
        if (std::trunc(value) != value || value < std::numeric_limits<T>::lowest() ||
            value > std::numeric_limits<T>::max()) { return kOfxStatErrValue; }
      }
      if (bounds) {
        const double minimum = std::is_integral_v<T> ? bounds->getIntProperty(kOfxParamPropMin, index)
                                                     : bounds->getDoubleProperty(kOfxParamPropMin, index);
        const double maximum = std::is_integral_v<T> ? bounds->getIntProperty(kOfxParamPropMax, index)
                                                     : bounds->getDoubleProperty(kOfxParamPropMax, index);
        if (value < minimum || value > maximum) { return kOfxStatErrValue; }
      }
      values[index] = static_cast<T>(value);
    }
    // Complete result admission precedes every caller-owned component write.
    for (size_t index = 0; index < N; ++index) { *outputs[index] = values[index]; }
    return kOfxStatOK;
  }

  OfxStatus parameterKeyCount(const FilterEffectInstance& effect, const std::string& name, unsigned int& count) {
    if (effect.frameState().curveParameter(name)) {
      MondrianOpenFxQueryResult result{};
      const auto status = effect.frameState().queryParameter(name, 3, 0, 0, 0, 0, result);
      if (status == kOfxStatOK) { count = result.key_count; }
      return status;
    }
    if (!effect.frameState().constantParameter(name)) { return kOfxStatErrMissingHostFeature; }
    count = 0;
    return kOfxStatOK;
  }

  OfxStatus parameterKeyTime(const FilterEffectInstance& effect, const std::string& name, int index, OfxTime& time) {
    if (effect.frameState().curveParameter(name)) {
      MondrianOpenFxQueryResult result{};
      const auto status = effect.frameState().queryParameter(name, 4, 0, 0, index, 0, result);
      if (status == kOfxStatOK) {
        if (!std::isfinite(result.key_time)) { return kOfxStatErrValue; }
        time = result.key_time;
      }
      return status;
    }
    return effect.frameState().constantParameter(name) ? kOfxStatErrBadIndex : kOfxStatErrMissingHostFeature;
  }

  OfxStatus parameterKeyIndex(const FilterEffectInstance& effect, const std::string& name, OfxTime time, int direction, int& index) {
    if (!std::isfinite(time)) { return kOfxStatErrValue; }
    if (effect.frameState().curveParameter(name)) {
      MondrianOpenFxQueryResult result{};
      const auto status = effect.frameState().queryParameter(name, 5, time, 0, 0, direction, result);
      if (status == kOfxStatOK || status == kOfxStatFailed) { index = result.key_index; }
      return status;
    }
    if (!effect.frameState().constantParameter(name)) { return kOfxStatErrMissingHostFeature; }
    index = -1;
    return kOfxStatFailed;
  }

  //
  // IntegerParameter
  //

  IntegerParameter::IntegerParameter(FilterEffectInstance* effect,
                                       const std::string& name,
                                       OFX::Host::Param::Descriptor& descriptor)
    : ParameterQueryInstance<OFX::Host::Param::IntegerInstance>(descriptor, effect), _descriptor(descriptor), _value(descriptor.getProperties().getIntProperty(kOfxParamPropDefault))
  {
  }

  OfxStatus IntegerParameter::get(int& value)
  {
    value = _value;
    return kOfxStatOK;
  }

  OfxStatus IntegerParameter::get(OfxTime time, int& value)
  {
    if (_effect->frameState().curveParameter(getName())) {
      return curveNumbers<int, 1>(*_effect, getName(), 0, time, 0, {&value}, &_descriptor.getProperties());
    }
    if (!std::isfinite(time)) { return kOfxStatErrValue; }
    return _effect->frameState().permitsParameterTime(getName(), time)
        ? get(value) : kOfxStatErrMissingHostFeature;
  }

  OfxStatus IntegerParameter::set(int value)
  {
    if (!_effect->frameState().permitsParameterWrite(getName())) { return kOfxStatErrUnsupported; }
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
    : ParameterQueryInstance<OFX::Host::Param::DoubleInstance>(descriptor, effect), _descriptor(descriptor), _value(descriptor.getProperties().getDoubleProperty(kOfxParamPropDefault))
  {
  }

  OfxStatus DoubleParameter::get(double& d)
  {
    d = _value;
    return kOfxStatOK;
  }

  OfxStatus DoubleParameter::get(OfxTime time, double& d)
  {
    if (_effect->frameState().curveParameter(getName())) {
      return curveNumbers<double, 1>(*_effect, getName(), 0, time, 0, {&d}, &_descriptor.getProperties());
    }
    if (!std::isfinite(time)) { return kOfxStatErrValue; }
    return _effect->frameState().permitsParameterTime(getName(), time)
        ? get(d) : kOfxStatErrMissingHostFeature;
  }

  OfxStatus DoubleParameter::set(double value)
  {
    if (!_effect->frameState().permitsParameterWrite(getName())) { return kOfxStatErrUnsupported; }
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
    if (_effect->frameState().curveParameter(getName())) {
      return curveNumbers<double, 1>(*_effect, getName(), 1, time, 0, {&value});
    }
    if (!std::isfinite(time)) { return kOfxStatErrValue; }
    if (!_effect->frameState().constantParameter(getName())) { return kOfxStatErrMissingHostFeature; }
    value = 0.0;
    return kOfxStatOK;
  }

  OfxStatus DoubleParameter::integrate(OfxTime time1, OfxTime time2, double& value)
  {
    if (_effect->frameState().curveParameter(getName())) {
      return curveNumbers<double, 1>(*_effect, getName(), 2, time1, time2, {&value});
    }
    if (!std::isfinite(time1) || !std::isfinite(time2)) { return kOfxStatErrValue; }
    if (time1 == time2) { value = 0.0; return kOfxStatOK; }
    if (!_effect->frameState().constantParameter(getName())) { return kOfxStatErrMissingHostFeature; }
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
    : ParameterQueryInstance<OFX::Host::Param::BooleanInstance>(descriptor, effect), _descriptor(descriptor), _value(descriptor.getProperties().getIntProperty(kOfxParamPropDefault) != 0)
  {
  }

  OfxStatus BooleanParameter::get(bool& b)
  {
    b = _value;
    return kOfxStatOK;
  }

  OfxStatus BooleanParameter::get(OfxTime time, bool& b)
  {
    if (_effect->frameState().curveParameter(getName())) {
      return curveNumbers<bool, 1>(*_effect, getName(), 0, time, 0, {&b}, nullptr);
    }
    if (!std::isfinite(time)) { return kOfxStatErrValue; }
    return _effect->frameState().permitsParameterTime(getName(), time)
        ? get(b) : kOfxStatErrMissingHostFeature;
  }

  OfxStatus BooleanParameter::set(bool value)
  {
    if (!_effect->frameState().permitsParameterWrite(getName())) { return kOfxStatErrUnsupported; }
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
    : ParameterQueryInstance<OFX::Host::Param::ChoiceInstance>(descriptor, effect),
      _value(descriptor.getProperties().getIntProperty(kOfxParamPropDefault)),
      _ordinalCount(descriptor.getProperties().getDimension(kOfxParamPropChoiceOption))
  {
  }

  void ChoiceParameter::setOption(int /*index*/) {
    // The SDK's existing ChoiceOption notify hook calls this after every
    // property mutation, including reset and a change later restored in-place.
    const int count = getProperties().getDimension(kOfxParamPropChoiceOption);
    if (count != _ordinalCount) { _effect->frameState().invalidChoiceDomain.store(true, std::memory_order_relaxed); }
    _effect->frameState().admitsChoiceDomain(getName(), count);
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
    if (!_effect->frameState().admitsChoiceDomain(getName(),
        getProperties().getDimension(kOfxParamPropChoiceOption))) { return kOfxStatErrValue; }
    if (_effect->frameState().curveParameter(getName())) {
      int result = 0;
      const auto status = curveNumbers<int, 1>(*_effect, getName(), 0, time, 0, {&result});
      if (status != kOfxStatOK) { return status; }
      if (result < 0 || result >= getProperties().getDimension(kOfxParamPropChoiceOption)) { return kOfxStatErrValue; }
      value = result;
      return kOfxStatOK;
    }
    if (!std::isfinite(time)) { return kOfxStatErrValue; }
    return _effect->frameState().permitsParameterTime(getName(), time)
        ? get(value) : kOfxStatErrMissingHostFeature;
  }

  OfxStatus ChoiceParameter::set(int value)
  {
    if (!_effect->frameState().permitsParameterWrite(getName())) { return kOfxStatErrUnsupported; }
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
    : ParameterQueryInstance<OFX::Host::Param::StringInstance>(descriptor, effect),
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
    if (_effect->frameState().curveParameter(getName())) {
      MondrianOpenFxQueryResult result{};
      const auto status = _effect->frameState().queryParameter(getName(), 0, time, 0, 0, 0, result);
      if (status != kOfxStatOK) { return status; }
      if (!result.string_value || result.string_length > 256) { return kOfxStatErrValue; }
      std::string text(result.string_value, result.string_length);
      if (!contains(text)) { return kOfxStatErrValue; }
      value = std::move(text);
      return kOfxStatOK;
    }
    if (!std::isfinite(time)) { return kOfxStatErrValue; }
    return _effect->frameState().permitsParameterTime(getName(), time)
        ? get(value) : kOfxStatErrMissingHostFeature;
  }
  OfxStatus StrChoiceParameter::set(const char* value) {
    if (!_effect->frameState().permitsParameterWrite(getName())) { return kOfxStatErrUnsupported; }
    if (!value || !contains(value)) { return kOfxStatErrValue; }
    _value = value;
    return kOfxStatOK;
  }
  OfxStatus StrChoiceParameter::set(OfxTime time, const char* value) {
    return std::isfinite(time) && time == _effect->frameState().info.frame
        ? set(value) : kOfxStatErrMissingHostFeature;
  }

  StringParameter::StringParameter(FilterEffectInstance* effect,
      OFX::Host::Param::Descriptor& descriptor)
    : ParameterQueryInstance<OFX::Host::Param::StringInstance>(descriptor, effect),
      _value(descriptor.getProperties().getStringProperty(kOfxParamPropDefault)) {}

  OfxStatus StringParameter::get(std::string& value) {
    if (_value.size() > MONDRIAN_OPENFX_MAX_STRING_BYTES) { return kOfxStatErrValue; }
    value = _value;
    return kOfxStatOK;
  }
  OfxStatus StringParameter::get(OfxTime time, std::string& value) {
    if (!std::isfinite(time)) { return kOfxStatErrValue; }
    return _effect->frameState().permitsParameterTime(getName(), time)
        ? get(value) : kOfxStatErrMissingHostFeature;
  }
  OfxStatus StringParameter::set(const char* value) {
    if (!value) { return kOfxStatErrValue; }
    size_t length = 0;
    while (length <= MONDRIAN_OPENFX_MAX_STRING_BYTES && value[length] != '\0') { ++length; }
    if (length > MONDRIAN_OPENFX_MAX_STRING_BYTES) { return kOfxStatErrValue; }
    if (!_effect->frameState().permitsParameterWrite(getName())) { return kOfxStatErrUnsupported; }
    _value.assign(value, length);
    return kOfxStatOK;
  }
  OfxStatus StringParameter::set(OfxTime time, const char* value) {
    if (!std::isfinite(time)) { return kOfxStatErrValue; }
    return time == _effect->frameState().info.frame ? set(value) : kOfxStatErrMissingHostFeature;
  }

  template<class T, size_t N>
  T NumericTupleState<T, N>::property(const char* name, size_t index) const {
    const auto& properties = _descriptor.getProperties();
    if constexpr (std::is_same_v<T, double>) {
      return properties.getDoubleProperty(name, static_cast<int>(index));
    } else {
      return properties.getIntProperty(name, static_cast<int>(index));
    }
  }

  template<class T, size_t N>
  bool NumericTupleState<T, N>::admits(const std::array<T, N>& values) const {
    for (size_t index = 0; index < N; ++index) {
      if (!std::isfinite(values[index]) || values[index] < property(kOfxParamPropMin, index) ||
          values[index] > property(kOfxParamPropMax, index)) { return false; }
    }
    return true;
  }

  template<class T, size_t N>
  NumericTupleState<T, N>::NumericTupleState(FilterEffectInstance* effect, OFX::Host::Param::Descriptor& descriptor)
      : _effect(effect), _descriptor(descriptor) {
    for (size_t index = 0; index < N; ++index) { _values[index] = property(kOfxParamPropDefault, index); }
    if constexpr (std::is_same_v<T, double> && N == 2) {
      if (descriptor.getType() == kOfxParamTypeDouble2D &&
          (descriptor.getDoubleType() == kOfxParamDoubleTypeXY ||
           descriptor.getDoubleType() == kOfxParamDoubleTypeXYAbsolute)) {
        const auto& coordinates = descriptor.getProperties().getStringProperty(kOfxParamPropDefaultCoordinateSystem);
        if (coordinates == kOfxParamCoordinatesNormalised) {
          // Only the provider seed is projected, once, before range admission
          // and CreateInstance. The owner has an immutable zero-offset window.
          const auto& info = effect->frameState().info;
          _values[0] *= info.project_width * info.project_pixel_aspect_ratio;
          _values[1] *= info.project_height;
        } else if (coordinates != kOfxParamCoordinatesCanonical) {
          throw std::runtime_error("unsupported OpenFX spatial default coordinates");
        }
      }
    }
    if (!admits(_values)) { throw std::runtime_error("invalid OpenFX compound default"); }
    _defaultValues = _values;
  }

  template<class T, size_t N>
  OfxStatus NumericTupleState<T, N>::read(const std::array<T*, N>& outputs) const {
    for (size_t index = 0; index < N; ++index) { *outputs[index] = _values[index]; }
    return kOfxStatOK;
  }

  template<class T, size_t N>
  OfxStatus NumericTupleState<T, N>::read(OfxTime time, const std::array<T*, N>& outputs) const {
    if (_effect->frameState().curveParameter(_descriptor.getName())) {
      return curveNumbers<T, N>(*_effect, _descriptor.getName(), 0, time, 0, outputs, &_descriptor.getProperties());
    }
    if (!std::isfinite(time)) { return kOfxStatErrValue; }
    if (!_effect->frameState().permitsParameterTime(_descriptor.getName(), time)) {
      return kOfxStatErrMissingHostFeature;
    }
    return read(outputs);
  }

  template<class T, size_t N>
  OfxStatus NumericTupleState<T, N>::write(const std::array<T, N>& values) {
    if (!_effect->frameState().permitsParameterWrite(_descriptor.getName())) { return kOfxStatErrUnsupported; }
    if (!admits(values)) { return kOfxStatErrValue; }
    _values = values;
    return kOfxStatOK;
  }

  template<class T, size_t N>
  OfxStatus NumericTupleState<T, N>::write(OfxTime time, const std::array<T, N>& values) {
    if (!std::isfinite(time)) { return kOfxStatErrValue; }
    if (_descriptor.getCanAnimate() && time != _effect->frameState().info.frame) {
      return kOfxStatErrMissingHostFeature;
    }
    return write(values);
  }

  template<class T, size_t N>
  OfxStatus NumericTupleState<T, N>::derive(OfxTime time, const std::array<T*, N>& outputs) const {
    if constexpr (!std::is_same_v<T, double>) {
      return kOfxStatErrUnsupported;
    } else {
      if (_effect->frameState().curveParameter(_descriptor.getName())) {
        return curveNumbers<T, N>(*_effect, _descriptor.getName(), 1, time, 0, outputs);
      }
      if (!std::isfinite(time)) { return kOfxStatErrValue; }
      if (!_effect->frameState().constantParameter(_descriptor.getName())) { return kOfxStatErrMissingHostFeature; }
      for (auto* output : outputs) { *output = 0; }
      return kOfxStatOK;
    }
  }

  template<class T, size_t N>
  OfxStatus NumericTupleState<T, N>::integrate(OfxTime start, OfxTime end, const std::array<T*, N>& outputs) const {
    if constexpr (!std::is_same_v<T, double>) {
      return kOfxStatErrUnsupported;
    } else {
      if (_effect->frameState().curveParameter(_descriptor.getName())) {
        return curveNumbers<T, N>(*_effect, _descriptor.getName(), 2, start, end, outputs);
      }
      if (!std::isfinite(start) || !std::isfinite(end)) { return kOfxStatErrValue; }
      if (start == end) {
        for (auto* output : outputs) { *output = 0; }
        return kOfxStatOK;
      }
      if (!_effect->frameState().constantParameter(_descriptor.getName())) { return kOfxStatErrMissingHostFeature; }
      std::array<T, N> result{};
      const double interval = end - start;
      for (size_t index = 0; index < N; ++index) {
        if (_values[index] != 0 && start != end) {
          result[index] = std::isfinite(interval) ? _values[index] * interval
              : (_values[index] * (end * 0.5 - start * 0.5)) * 2.0;
          if (!std::isfinite(result[index])) { return kOfxStatErrValue; }
        }
      }
      for (size_t index = 0; index < N; ++index) { *outputs[index] = result[index]; }
      return kOfxStatOK;
    }
  }

  template class NumericTupleState<double, 2>;
  template class NumericTupleState<double, 3>;
  template class NumericTupleState<double, 4>;
  template class NumericTupleState<int, 2>;
  template class NumericTupleState<int, 3>;

  //

  PushbuttonParameter::PushbuttonParameter(FilterEffectInstance* effect,
                                             const std::string& name,
                                             OFX::Host::Param::Descriptor& descriptor)
    : OFX::Host::Param::PushbuttonInstance(descriptor, effect), _effect(effect), _descriptor(descriptor)
  {
  }

}
