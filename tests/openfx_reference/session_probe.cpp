// Copyright Mondrian contributors.
// SPDX-License-Identifier: BSD-3-Clause
// Test-only contract probe around the externally checked-out official example.
#define OfxGetPlugin MondrianReferenceGetPlugin
#define OfxGetNumberOfPlugins MondrianReferenceGetNumberOfPlugins
#include "Examples/Basic/basic.cpp"
#undef OfxGetPlugin
#undef OfxGetNumberOfPlugins
#include <cstdlib>
#include <chrono>
#include <thread>
#include <map>
#include <string>
#include <limits>
#include <cmath>
#include <array>
#include <type_traits>

namespace {
struct ActionState { bool changing = false; int changes = 0; int renders = 0; int scaleChanges = 0; };
std::map<const void*, ActionState> actions;
std::string mode() {
  const char* value = std::getenv("MONDRIAN_OPENFX_SESSION_PROBE_MODE");
  return value ? value : "normal";
}
struct CompoundParameter {
  const char* name;
  const char* type;
  int dimensions;
  bool integer;
  std::array<double, 4> defaults;
};
const CompoundParameter compounds[] = {
  {"point2", kOfxParamTypeDouble2D, 2, false, {0.12500000000000003, 10.000000000000002}},
  {"point3", kOfxParamTypeDouble3D, 3, false, {-0.5000000000000001, 1.0000000000000002, 2.0000000000000004}},
  {"rgb", kOfxParamTypeRGB, 3, false, {0.10000000000000002, 0.20000000000000004, 0.30000000000000004}},
  {"rgba", kOfxParamTypeRGBA, 4, false, {0.12500000000000003, 0.25000000000000006, 0.5000000000000001, 0.7500000000000001}},
  {"integer2", kOfxParamTypeInteger2D, 2, true, {-123456789, 123456789}},
  {"integer3", kOfxParamTypeInteger3D, 3, true, {-2147483648.0, 0, 2147483647.0}}
};
bool defineCompounds(const void* handle) {
  OfxParamSetHandle params = nullptr;
  if (gEffectHost->getParamSet((OfxImageEffectHandle)handle, &params) != kOfxStatOK) { return false; }
  for (const auto& parameter : compounds) {
    OfxPropertySetHandle props = nullptr;
    if (gParamHost->paramDefine(params, parameter.type, parameter.name, &props) != kOfxStatOK ||
        gPropHost->propSetString(props, kOfxPropLabel, 0, parameter.name) != kOfxStatOK ||
        gPropHost->propSetInt(props, kOfxParamPropAnimates, 0, (mode() == "compound_constant_animatable" || mode() == "compound_frame_sample" || mode() == "canonical_curves") ? 1 : 0) != kOfxStatOK) { return false; }
    if ((parameter.type == std::string(kOfxParamTypeDouble2D) || parameter.type == std::string(kOfxParamTypeDouble3D)) &&
        gPropHost->propSetString(props, kOfxParamPropDoubleType, 0, kOfxParamDoubleTypePlain) != kOfxStatOK) { return false; }
    for (int index = 0; index < parameter.dimensions; ++index) {
      if (parameter.integer) {
        const int value = static_cast<int>(parameter.defaults[index]);
        const int minimum = parameter.dimensions == 2 ? (index == 0 ? -200000000 : 0)
            : index == 0 ? std::numeric_limits<int>::min() : index == 1 ? 0 : std::numeric_limits<int>::max() - 32;
        const int maximum = parameter.dimensions == 2 ? (index == 0 ? -100000000 : 200000000)
            : index == 0 ? std::numeric_limits<int>::min() + 32 : index == 1 ? 4 : std::numeric_limits<int>::max();
        if (gPropHost->propSetInt(props, kOfxParamPropDefault, index, value) != kOfxStatOK ||
            gPropHost->propSetInt(props, kOfxParamPropMin, index, minimum) != kOfxStatOK ||
            gPropHost->propSetInt(props, kOfxParamPropMax, index, maximum) != kOfxStatOK ||
            gPropHost->propSetInt(props, kOfxParamPropDisplayMin, index, minimum) != kOfxStatOK ||
            gPropHost->propSetInt(props, kOfxParamPropDisplayMax, index, maximum) != kOfxStatOK) { return false; }
      } else {
        const double low = parameter.defaults[index] - index - 1;
        const double high = parameter.defaults[index] + index + 1;
        if (gPropHost->propSetDouble(props, kOfxParamPropDefault, index, parameter.defaults[index]) != kOfxStatOK ||
            gPropHost->propSetDouble(props, kOfxParamPropMin, index, low) != kOfxStatOK ||
            gPropHost->propSetDouble(props, kOfxParamPropMax, index, high) != kOfxStatOK ||
            gPropHost->propSetDouble(props, kOfxParamPropDisplayMin, index, low) != kOfxStatOK ||
            gPropHost->propSetDouble(props, kOfxParamPropDisplayMax, index, high) != kOfxStatOK) { return false; }
      }
    }
  }
  return true;
}
template<class T, int N>
OfxStatus readCompound(OfxParamHandle parameter, bool timed, double time, std::array<double, 4>& output) {
  std::array<T, N> values{};
  for (int index = 0; index < N; ++index) { values[index] = static_cast<T>(output[index]); }
  OfxStatus status;
  if constexpr (N == 2) {
    status = timed ? gParamHost->paramGetValueAtTime(parameter, time, &values[0], &values[1])
                   : gParamHost->paramGetValue(parameter, &values[0], &values[1]);
  } else if constexpr (N == 3) {
    status = timed ? gParamHost->paramGetValueAtTime(parameter, time, &values[0], &values[1], &values[2])
                   : gParamHost->paramGetValue(parameter, &values[0], &values[1], &values[2]);
  } else {
    status = timed ? gParamHost->paramGetValueAtTime(parameter, time, &values[0], &values[1], &values[2], &values[3])
                   : gParamHost->paramGetValue(parameter, &values[0], &values[1], &values[2], &values[3]);
  }
  for (int index = 0; index < N; ++index) { output[index] = values[index]; }
  return status;
}
template<class T, int N>
OfxStatus writeCompound(OfxParamHandle parameter, bool timed, double time, const std::array<double, 4>& value) {
  std::array<T, N> values{};
  for (int index = 0; index < N; ++index) { values[index] = static_cast<T>(value[index]); }
  if constexpr (N == 2) {
    return timed ? gParamHost->paramSetValueAtTime(parameter, time, values[0], values[1])
                 : gParamHost->paramSetValue(parameter, values[0], values[1]);
  } else if constexpr (N == 3) {
    return timed ? gParamHost->paramSetValueAtTime(parameter, time, values[0], values[1], values[2])
                 : gParamHost->paramSetValue(parameter, values[0], values[1], values[2]);
  } else {
    return timed ? gParamHost->paramSetValueAtTime(parameter, time, values[0], values[1], values[2], values[3])
                 : gParamHost->paramSetValue(parameter, values[0], values[1], values[2], values[3]);
  }
}
template<int N>
OfxStatus compoundCalculus(OfxParamHandle parameter, bool integral, double start, double end, std::array<double, 4>& output) {
  if constexpr (N == 2) {
    return integral ? gParamHost->paramGetIntegral(parameter, start, end, &output[0], &output[1])
                    : gParamHost->paramGetDerivative(parameter, start, &output[0], &output[1]);
  } else if constexpr (N == 3) {
    return integral ? gParamHost->paramGetIntegral(parameter, start, end, &output[0], &output[1], &output[2])
                    : gParamHost->paramGetDerivative(parameter, start, &output[0], &output[1], &output[2]);
  } else {
    return integral ? gParamHost->paramGetIntegral(parameter, start, end, &output[0], &output[1], &output[2], &output[3])
                    : gParamHost->paramGetDerivative(parameter, start, &output[0], &output[1], &output[2], &output[3]);
  }
}
template<class T, int N>
bool compoundSuite(OfxParamHandle parameter, const CompoundParameter& definition, bool creating, bool rendering, double time) {
  if (mode() == "compound_constant_animatable" || mode() == "compound_frame_sample") {
    const bool sampled = mode() == "compound_frame_sample";
    unsigned int keys = 999;
    const auto countStatus = gParamHost->paramGetNumKeys(parameter, &keys);
    if (sampled ? countStatus != kOfxStatErrMissingHostFeature || keys != 999
                : countStatus != kOfxStatOK || keys != 0) { return false; }
    for (unsigned int key : {0U, 1U, std::numeric_limits<unsigned int>::max()}) {
      double keyTime = -999;
      if (gParamHost->paramGetKeyTime(parameter, key, &keyTime) !=
          (sampled ? kOfxStatErrMissingHostFeature : kOfxStatErrBadIndex) || keyTime != -999) { return false; }
    }
    for (int direction : {-2, -1, 0, 1, 2}) {
      int keyIndex = 999;
      const auto status = gParamHost->paramGetKeyIndex(parameter, time, direction, &keyIndex);
      if (sampled ? status != kOfxStatErrMissingHostFeature || keyIndex != 999
                  : status != kOfxStatFailed || keyIndex != -1) { return false; }
    }
    for (double invalid : {std::numeric_limits<double>::quiet_NaN(), std::numeric_limits<double>::infinity()}) {
      int keyIndex = 999;
      if (gParamHost->paramGetKeyIndex(parameter, invalid, 0, &keyIndex) != kOfxStatErrValue || keyIndex != 999) { return false; }
    }
  }
  std::array<double, 4> saved{};
  if (readCompound<T, N>(parameter, rendering, time, saved) != kOfxStatOK ||
      (creating && saved != definition.defaults)) { return false; }
  for (double query : {-3.25, 0.0, 9.5}) {
    std::array<double, 4> actual{};
    if (mode() == "compound_frame_sample" && query != time) { actual = {-999, -999, -999, -999}; }
    const auto sentinel = actual;
    const auto status = readCompound<T, N>(parameter, true, query, actual);
    if (mode() == "compound_frame_sample" && query != time) {
      if (status != kOfxStatErrMissingHostFeature || actual != sentinel) { return false; }
    } else if (status != kOfxStatOK || actual != saved) { return false; }
  }
  for (double query : {std::numeric_limits<double>::quiet_NaN(), std::numeric_limits<double>::infinity()}) {
    std::array<double, 4> actual{-999, -999, -999, -999};
    const auto sentinel = actual;
    if (readCompound<T, N>(parameter, true, query, actual) != kOfxStatErrValue || actual != sentinel) { return false; }
    if (!rendering && writeCompound<T, N>(parameter, true, query, saved) != kOfxStatErrValue) { return false; }
  }
  if constexpr (std::is_same_v<T, double>) {
    for (double boundary : {0.0, std::numeric_limits<double>::max(), -std::numeric_limits<double>::max()}) {
      std::array<double, 4> zero{};
      if (compoundCalculus<N>(parameter, true, boundary, boundary, zero) != kOfxStatOK ||
          zero != std::array<double, 4>{}) { return false; }
    }
    if (mode() == "compound_frame_sample") {
      for (bool integral : {false, true}) {
        std::array<double, 4> actual{-999, -999, -999, -999};
        const auto sentinel = actual;
        if (compoundCalculus<N>(parameter, integral, -3.25, 9.5, actual) != kOfxStatErrMissingHostFeature ||
            actual != sentinel) { return false; }
      }
    } else {
      std::array<double, 4> derivative{};
      if (compoundCalculus<N>(parameter, false, -3.25, 0.0, derivative) != kOfxStatOK ||
          derivative != std::array<double, 4>{}) { return false; }
      for (double interval : {5.0, -5.0}) {
        std::array<double, 4> actual{}, expected{};
        for (int index = 0; index < N; ++index) { expected[index] = saved[index] * interval; }
        if (compoundCalculus<N>(parameter, true, 0.0, interval, actual) != kOfxStatOK || actual != expected) { return false; }
      }
      std::array<double, 4> zero{};
      const double largest = std::numeric_limits<double>::max();
      if (compoundCalculus<N>(parameter, true, largest, largest, zero) != kOfxStatOK ||
          zero != std::array<double, 4>{}) { return false; }
      for (bool integral : {false, true}) {
        std::array<double, 4> actual{-999, -999, -999, -999};
        const auto sentinel = actual;
        if (compoundCalculus<N>(parameter, integral, std::numeric_limits<double>::quiet_NaN(), 1.0, actual) != kOfxStatErrValue ||
            actual != sentinel) { return false; }
      }
      std::array<double, 4> huge{-999, -999, -999, -999};
      const auto sentinel = huge;
      const bool representable = definition.type == std::string(kOfxParamTypeRGB);
      const auto status = compoundCalculus<N>(parameter, true, -largest, largest, huge);
      if (representable) {
        if (status != kOfxStatOK) { return false; }
        for (int index = 0; index < N; ++index) {
          if (!std::isfinite(huge[index]) || huge[index] != (saved[index] * largest) * 2.0) { return false; }
        }
      } else if (status != kOfxStatErrValue || huge != sentinel) { return false; }
    }
  }
  if (!rendering) {
    OfxPropertySetHandle props = nullptr;
    if (gParamHost->paramGetPropertySet(parameter, &props) != kOfxStatOK) { return false; }
    auto rejected = saved;
    rejected[0] = std::is_same_v<T, int> ? saved[0] + 1 : std::nextafter(saved[0], std::numeric_limits<double>::infinity());
    if constexpr (std::is_same_v<T, int>) {
      int minimum = 0;
      if (gPropHost->propGetInt(props, kOfxParamPropMin, 1, &minimum) != kOfxStatOK) { return false; }
      rejected[1] = minimum - 1.0;
    } else {
      double minimum = 0;
      if (gPropHost->propGetDouble(props, kOfxParamPropMin, 1, &minimum) != kOfxStatOK) { return false; }
      rejected[1] = minimum - 1.0;
    }
    std::array<double, 4> after{};
    if (writeCompound<T, N>(parameter, false, 0.0, rejected) != kOfxStatErrValue ||
        readCompound<T, N>(parameter, false, 0.0, after) != kOfxStatOK || after != saved) { return false; }
    if (creating && writeCompound<T, N>(parameter, false, 0.0, saved) != kOfxStatOK) { return false; }
  }
  return true;
}
bool compoundValues(const void* handle, OfxPropertySetHandle in, bool creating, bool rendering) {
  OfxParamSetHandle params = nullptr;
  double time = 0.0;
  if (gEffectHost->getParamSet((OfxImageEffectHandle)handle, &params) != kOfxStatOK) { return false; }
  if (rendering && gPropHost->propGetDouble(in, kOfxPropTime, 0, &time) != kOfxStatOK) { return false; }
  for (const auto& definition : compounds) {
    OfxParamHandle parameter = nullptr;
    if (gParamHost->paramGetHandle(params, definition.name, &parameter, nullptr) != kOfxStatOK) { return false; }
    bool valid;
    if (definition.integer) {
      valid = definition.dimensions == 2 ? compoundSuite<int, 2>(parameter, definition, creating, rendering, time)
                                        : compoundSuite<int, 3>(parameter, definition, creating, rendering, time);
    } else if (definition.dimensions == 2) {
      valid = compoundSuite<double, 2>(parameter, definition, creating, rendering, time);
    } else if (definition.dimensions == 3) {
      valid = compoundSuite<double, 3>(parameter, definition, creating, rendering, time);
    } else {
      valid = compoundSuite<double, 4>(parameter, definition, creating, rendering, time);
    }
    if (!valid) { return false; }
  }
  return true;
}

// Author-workflow probe: every tuple component contributes independently to
// the output. The untouched first component also checks exact SDK precision.
bool compoundAuthorOutput(const void* handle, OfxPropertySetHandle in) {
  OfxParamSetHandle params = nullptr;
  double time = 0.0, gain = 1.0;
  if (gEffectHost->getParamSet((OfxImageEffectHandle)handle, &params) != kOfxStatOK ||
      gPropHost->propGetDouble(in, kOfxPropTime, 0, &time) != kOfxStatOK) { return false; }
  int tuple = 0;
  for (const auto& definition : compounds) {
    OfxParamHandle parameter = nullptr;
    std::array<double, 4> values{};
    if (gParamHost->paramGetHandle(params, definition.name, &parameter, nullptr) != kOfxStatOK) { return false; }
    OfxStatus status;
    if (definition.integer) {
      status = definition.dimensions == 2 ? readCompound<int, 2>(parameter, true, time, values)
                                          : readCompound<int, 3>(parameter, true, time, values);
    } else if (definition.dimensions == 2) {
      status = readCompound<double, 2>(parameter, true, time, values);
    } else if (definition.dimensions == 3) {
      status = readCompound<double, 3>(parameter, true, time, values);
    } else {
      status = readCompound<double, 4>(parameter, true, time, values);
    }
    if (status != kOfxStatOK || values[0] != definition.defaults[0]) { return false; }
    for (int channel = 0; channel < definition.dimensions; ++channel) {
      gain += (values[channel] - definition.defaults[channel]) * (tuple + 1) * (channel + 1) / 128.0;
    }
    ++tuple;
  }
  OfxImageClipHandle clip = nullptr;
  OfxPropertySetHandle image = nullptr;
  if (gEffectHost->clipGetHandle((OfxImageEffectHandle)handle, "Output", &clip, nullptr) != kOfxStatOK ||
      gEffectHost->clipGetImage(clip, time, nullptr, &image) != kOfxStatOK) { return false; }
  void* data = nullptr;
  int bounds[4]{}, stride = 0;
  bool valid = std::isfinite(gain) &&
    gPropHost->propGetPointer(image, kOfxImagePropData, 0, &data) == kOfxStatOK && data &&
    gPropHost->propGetIntN(image, kOfxImagePropBounds, 4, bounds) == kOfxStatOK &&
    gPropHost->propGetInt(image, kOfxImagePropRowBytes, 0, &stride) == kOfxStatOK;
  if (valid) {
    const float multiplier = static_cast<float>(gain);
    for (int y = 0; y < bounds[3] - bounds[1]; ++y) {
      auto* row = reinterpret_cast<OfxRGBAColourF*>(static_cast<char*>(data) + y * stride);
      for (int x = 0; x < bounds[2] - bounds[0]; ++x) {
        row[x].r *= multiplier; row[x].g *= multiplier;
        row[x].b *= multiplier; row[x].a *= multiplier;
      }
    }
  }
  return gEffectHost->clipReleaseImage(image) == kOfxStatOK && valid;
}


bool curveKeys(OfxParamHandle parameter) {
  unsigned int count = 999;
  if (gParamHost->paramGetNumKeys(parameter, &count) != kOfxStatOK || count != 2) { return false; }
  for (int index = 0; index < 2; ++index) {
    double time = -999;
    if (gParamHost->paramGetKeyTime(parameter, index, &time) != kOfxStatOK || time != index * 24.0) { return false; }
  }
  for (int index : {-1, 2}) {
    double time = -999;
    if (gParamHost->paramGetKeyTime(parameter, index, &time) != kOfxStatErrBadIndex || time != -999) { return false; }
  }
  for (double time : {-1.0, -0.0, 0.0, 6.0, 24.0, 25.0}) {
    for (int direction : {-1, 0, 1}) {
      const int expected = direction < 0 ? (time > 24 ? 1 : time > 0 ? 0 : -1)
          : direction > 0 ? (time < 0 ? 0 : time < 24 ? 1 : -1)
          : time == 0 ? 0 : time == 24 ? 1 : -1;
      int index = -999;
      const auto status = gParamHost->paramGetKeyIndex(parameter, time, direction, &index);
      if (status != (expected == -1 ? kOfxStatFailed : kOfxStatOK) || index != expected) { return false; }
    }
  }
  int index = -999;
  return gParamHost->paramGetKeyIndex(parameter, std::numeric_limits<double>::quiet_NaN(), 1, &index)
      == kOfxStatErrValue && index == -999;
}

template<class T, int N>
bool canonicalTuple(OfxParamHandle parameter, const CompoundParameter& definition) {
  if (!curveKeys(parameter)) { return false; }
  for (double frame : {-6.0, 0.0, 6.0, 18.0, 24.0, 30.0}) {
    const double t = std::max(0.0, std::min(1.0, frame / 24.0));
    const double increment = definition.integer ? (frame >= 24 ? 1 : 0) : 3*t*t - 2*t*t*t;
    std::array<double, 4> values{};
    if (readCompound<T, N>(parameter, true, frame, values) != kOfxStatOK) { return false; }
    for (int channel = 0; channel < N; ++channel) {
      const double expected = definition.defaults[channel] + (channel == 1 ? increment : 0);
      if (std::abs(values[channel] - expected) > 2e-12) { return false; }
    }
    if constexpr (std::is_same_v<T, double>) {
      std::array<double, 4> slopes{};
      if (compoundCalculus<N>(parameter, false, frame, 0, slopes) != kOfxStatOK) { return false; }
      for (int channel = 0; channel < N; ++channel) {
        const double expected = channel == 1 ? (6*t - 6*t*t)/24.0 : 0;
        if (std::abs(slopes[channel] - expected) > 2e-12) { return false; }
      }
    }
  }
  std::array<double, 4> invalid{-999,-999,-999,-999};
  if (readCompound<T, N>(parameter, true, std::numeric_limits<double>::infinity(), invalid) != kOfxStatErrValue ||
      invalid != std::array<double,4>{-999,-999,-999,-999}) { return false; }
  if constexpr (std::is_same_v<T, double>) {
    for (bool reverse : {false,true}) {
      std::array<double,4> areas{};
      if (compoundCalculus<N>(parameter, true, reverse ? 24 : 0, reverse ? 0 : 24, areas) != kOfxStatOK) { return false; }
      for (int channel = 0; channel < N; ++channel) {
        const double expected = (24*definition.defaults[channel] + (channel == 1 ? 12 : 0)) * (reverse ? -1 : 1);
        if (std::abs(areas[channel] - expected) > 2e-11) { return false; }
      }
    }
  }
  return writeCompound<T,N>(parameter, false, 0, definition.defaults) == kOfxStatErrUnsupported;
}

bool canonicalCurves(const void* handle) {
  OfxParamSetHandle params = nullptr;
  if (gEffectHost->getParamSet((OfxImageEffectHandle)handle, &params) != kOfxStatOK) { return false; }
  OfxParamHandle scale = nullptr;
  if (gParamHost->paramGetHandle(params, "scale", &scale, nullptr) != kOfxStatOK || !curveKeys(scale)) { return false; }
  for (double frame : {-6.0, 0.0, 6.0, 18.0, 24.0, 30.0}) {
    const double t = std::max(0.0, std::min(1.0, frame/24));
    double value = -999, slope = -999;
    if (gParamHost->paramGetValueAtTime(scale, frame, &value) != kOfxStatOK ||
        std::abs(value - (1+3*t*t-2*t*t*t)) > 2e-12 ||
        gParamHost->paramGetDerivative(scale, frame, &slope) != kOfxStatOK ||
        std::abs(slope - (6*t-6*t*t)/24) > 2e-12) { return false; }
  }
  double area = -999;
  if (gParamHost->paramGetIntegral(scale, 0, 24, &area) != kOfxStatOK || std::abs(area-36) > 2e-12) { return false; }
  for (const auto& definition : compounds) {
    OfxParamHandle parameter = nullptr;
    if (gParamHost->paramGetHandle(params, definition.name, &parameter, nullptr) != kOfxStatOK) { return false; }
    const bool valid = definition.integer
        ? definition.dimensions == 2 ? canonicalTuple<int,2>(parameter, definition) : canonicalTuple<int,3>(parameter, definition)
        : definition.dimensions == 2 ? canonicalTuple<double,2>(parameter, definition)
        : definition.dimensions == 3 ? canonicalTuple<double,3>(parameter, definition) : canonicalTuple<double,4>(parameter, definition);
    if (!valid) { return false; }
  }
  return true;
}

void property(const void* handle, const char* name, int value) {
  OfxPropertySetHandle props = nullptr;
  gEffectHost->getPropertySet((OfxImageEffectHandle)handle, &props);
  gPropHost->propSetInt(props, name, 0, value);
}
// Hold two legal Source image handles simultaneously, release one, then read
// and release the other. Repeated fetch/release must not consume session pixels
// or invalidate an independent handle before the Render action completes.
bool imageLifetimes(const void* handle, OfxPropertySetHandle in, const char* clip) {
  OfxImageClipHandle source = nullptr;
  if (gEffectHost->clipGetHandle((OfxImageEffectHandle)handle, clip, &source, nullptr) != kOfxStatOK) {
    return false;
  }
  double time = 0.0;
  if (gPropHost->propGetDouble(in, kOfxPropTime, 0, &time) != kOfxStatOK) { return false; }
  OfxPropertySetHandle first = nullptr, second = nullptr;
  if (gEffectHost->clipGetImage(source, time, nullptr, &first) != kOfxStatOK) { return false; }
  if (gEffectHost->clipGetImage(source, time, nullptr, &second) != kOfxStatOK) {
    gEffectHost->clipReleaseImage(first);
    return false;
  }
  void* firstData = nullptr;
  void* secondData = nullptr;
  bool valid = first != second && gPropHost->propGetPointer(first, kOfxImagePropData, 0, &firstData) == kOfxStatOK &&
               gPropHost->propGetPointer(second, kOfxImagePropData, 0, &secondData) == kOfxStatOK &&
               firstData && secondData;
  OfxRGBAColourF saved{};
  if (valid) { saved = *static_cast<OfxRGBAColourF*>(firstData); }
  valid = gEffectHost->clipReleaseImage(first) == kOfxStatOK && valid;
  if (valid) {
    const auto& pixel = *static_cast<const OfxRGBAColourF*>(secondData);
    valid = pixel.r == saved.r && pixel.g == saved.g && pixel.b == saved.b && pixel.a == saved.a;
  }
  valid = gEffectHost->clipReleaseImage(second) == kOfxStatOK && valid;
  return valid;
}
bool imageUnavailable(const void* handle, OfxPropertySetHandle in) {
  OfxImageClipHandle clip = nullptr;
  if (gEffectHost->clipGetHandle((OfxImageEffectHandle)handle, "Source", &clip, nullptr) != kOfxStatOK) { return false; }
  double time = 0.0;
  if (in) { gPropHost->propGetDouble(in, kOfxPropTime, 0, &time); }
  OfxPropertySetHandle image = nullptr;
  const auto status = gEffectHost->clipGetImage(clip, time, nullptr, &image);
  if (status == kOfxStatOK) {
    gEffectHost->clipReleaseImage(image);
    return false;
  }
  return status == kOfxStatFailed;
}
bool applyDiscreteGain(const void* handle, OfxPropertySetHandle in, const char* name = "count", bool choice = false) {
  OfxParamSetHandle params = nullptr;
  OfxParamHandle count = nullptr;
  double time = 0.0;
  int value = 0;
  if (gEffectHost->getParamSet((OfxImageEffectHandle)handle, &params) != kOfxStatOK ||
      gParamHost->paramGetHandle(params, name, &count, nullptr) != kOfxStatOK ||
      gPropHost->propGetDouble(in, kOfxPropTime, 0, &time) != kOfxStatOK) { return false; }
  if (mode().find("strchoice") == 0 && choice) {
    char* key = nullptr;
    if (gParamHost->paramGetValueAtTime(count, time, &key) != kOfxStatOK || !key) { return false; }
    const std::string selected(key);
    if (selected == "vendor.red") { value = 0; }
    else if (selected == "vendor.green") { value = 1; }
    else if (selected == "vendor.blue") { value = 2; }
    else { return false; }
  } else if (gParamHost->paramGetValueAtTime(count, time, &value) != kOfxStatOK) { return false; }
  // Render reads use the requested time; getValue/setValue belong to change
  // or interact actions, not Render (OfxParameterSuiteV1 contract).
  if (choice) { ++value; }
  OfxImageClipHandle clip = nullptr;
  OfxPropertySetHandle image = nullptr;
  if (gEffectHost->clipGetHandle((OfxImageEffectHandle)handle, "Output", &clip, nullptr) != kOfxStatOK ||
      gEffectHost->clipGetImage(clip, time, nullptr, &image) != kOfxStatOK) { return false; }
  void* data = nullptr;
  int bounds[4]{}, stride = 0;
  bool valid = gPropHost->propGetPointer(image, kOfxImagePropData, 0, &data) == kOfxStatOK && data &&
      gPropHost->propGetIntN(image, kOfxImagePropBounds, 4, bounds) == kOfxStatOK &&
      gPropHost->propGetInt(image, kOfxImagePropRowBytes, 0, &stride) == kOfxStatOK;
  if (valid) {
    for (int y = 0; y < bounds[3] - bounds[1]; ++y) {
      auto* row = reinterpret_cast<OfxRGBAColourF*>(static_cast<char*>(data) + y * stride);
      for (int x = 0; x < bounds[2] - bounds[0]; ++x) {
        row[x].r *= value; row[x].g *= value; row[x].b *= value; row[x].a *= value;
      }
    }
  }
  return gEffectHost->clipReleaseImage(image) == kOfxStatOK && valid;
}
// Preserve UTF-8 bytes and SDK-owned pointer lifetimes through the actual suite.
bool stringValues(const void* handle, OfxPropertySetHandle in, bool render) {
  OfxParamSetHandle params = nullptr;
  double time = 0.0;
  if (gEffectHost->getParamSet((OfxImageEffectHandle)handle, &params) != kOfxStatOK ||
      gPropHost->propGetDouble(in, kOfxPropTime, 0, &time) != kOfxStatOK) { return false; }
  const char* names[] = {"single", "multi", "existingFile", "newFile",
    "existingDirectory", "newDirectory", "status", "rich"};
  size_t bytes = 0;
  const int count = mode() == "strings_text" ? 2 : mode() == "strings_paths" ? 6 : 8;
  for (int index = 0; index < count; ++index) {
    OfxParamHandle parameter = nullptr;
    char* value = nullptr;
    if (gParamHost->paramGetHandle(params, names[index], &parameter, nullptr) != kOfxStatOK ||
        gParamHost->paramGetValueAtTime(parameter, time, &value) != kOfxStatOK || !value) { return false; }
    const std::string saved(value);
    bytes += saved.size();
    for (double query : {time - 3.25, time + 9.5}) {
      if (gParamHost->paramGetValueAtTime(parameter, query, &value) != kOfxStatOK || !value || saved != value) { return false; }
    }
    char sentinel[] = "unchanged";
    for (double query : {std::numeric_limits<double>::quiet_NaN(), std::numeric_limits<double>::infinity()}) {
      value = sentinel;
      if (gParamHost->paramGetValueAtTime(parameter, query, &value) != kOfxStatErrValue || value != sentinel) { return false; }
      if (!render && gParamHost->paramSetValueAtTime(parameter, query, saved.c_str()) != kOfxStatErrValue) { return false; }
    }
    if (!render) {
      const std::string oversized(64 * 1024 + 1, 'x');
      if (gParamHost->paramSetValue(parameter, static_cast<const char*>(nullptr)) != kOfxStatErrValue ||
          gParamHost->paramSetValue(parameter, oversized.c_str()) != kOfxStatErrValue ||
          gParamHost->paramGetValue(parameter, &value) != kOfxStatOK || !value || saved != value) { return false; }
    }
  }
  if (!render) { return true; }
  OfxImageClipHandle clip = nullptr;
  OfxPropertySetHandle image = nullptr;
  if (gEffectHost->clipGetHandle((OfxImageEffectHandle)handle, "Output", &clip, nullptr) != kOfxStatOK ||
      gEffectHost->clipGetImage(clip, time, nullptr, &image) != kOfxStatOK) { return false; }
  void* data = nullptr;
  int bounds[4]{}, stride = 0;
  bool valid = gPropHost->propGetPointer(image, kOfxImagePropData, 0, &data) == kOfxStatOK && data &&
    gPropHost->propGetIntN(image, kOfxImagePropBounds, 4, bounds) == kOfxStatOK &&
    gPropHost->propGetInt(image, kOfxImagePropRowBytes, 0, &stride) == kOfxStatOK;
  const float gain = 1.0f + static_cast<float>(bytes % 11) / 10.0f;
  if (valid) {
    for (int y = 0; y < bounds[3] - bounds[1]; ++y) {
      auto* row = reinterpret_cast<OfxRGBAColourF*>(static_cast<char*>(data) + y * stride);
      for (int x = 0; x < bounds[2] - bounds[0]; ++x) {
        row[x].r *= gain; row[x].g *= gain; row[x].b *= gain; row[x].a *= gain;
      }
    }
  }
  return gEffectHost->clipReleaseImage(image) == kOfxStatOK && valid;
}

// Exercise the actual varargs suite, including interval overflow whose product
// is still representable. Outputs must survive every rejected request.
bool staticCalculus(const void* handle) {
  OfxParamSetHandle params = nullptr;
  OfxParamHandle parameter = nullptr;
  double current = 0.0;
  if (gEffectHost->getParamSet((OfxImageEffectHandle)handle, &params) != kOfxStatOK ||
      gParamHost->paramGetHandle(params, "calculus", &parameter, nullptr) != kOfxStatOK ||
      gParamHost->paramGetValueAtTime(parameter, 0.0, &current) != kOfxStatOK) { return false; }
  const double limit = std::numeric_limits<double>::max();
  for (double time : {-limit, -3.25, 0.0, 9.5, limit}) {
    double result = -999.0;
    if (gParamHost->paramGetDerivative(parameter, time, &result) != kOfxStatOK || result != 0.0) { return false; }
  }
  for (const auto& interval : {std::pair<double, double>{-1.0, 1.0}, {1.0, -1.0}, {9.5, 9.5}, {-limit, limit}, {limit, -limit}}) {
    double expected = 0.0;
    if (interval.first != interval.second) {
      expected = current * (interval.second == limit || interval.first == limit ? limit : 1.0);
      expected *= interval.second > interval.first ? 2.0 : -2.0;
    }
    double result = -999.0;
    const auto status = gParamHost->paramGetIntegral(parameter, interval.first, interval.second, &result);
    if (std::isfinite(expected)) {
      if (status != kOfxStatOK || result != expected) { return false; }
    } else if (status != kOfxStatErrValue || result != -999.0) { return false; }
  }
  for (double invalid : {std::numeric_limits<double>::quiet_NaN(), std::numeric_limits<double>::infinity(), -std::numeric_limits<double>::infinity()}) {
    double result = -999.0;
    if (gParamHost->paramGetDerivative(parameter, invalid, &result) != kOfxStatErrValue || result != -999.0 ||
        gParamHost->paramGetIntegral(parameter, invalid, 0.0, &result) != kOfxStatErrValue || result != -999.0 ||
        gParamHost->paramGetIntegral(parameter, 0.0, invalid, &result) != kOfxStatErrValue || result != -999.0) { return false; }
  }
  if (gParamHost->paramGetHandle(params, "scaleR", &parameter, nullptr) != kOfxStatOK) { return false; }
  double result = -999.0;
  return gParamHost->paramGetDerivative(parameter, 0.0, &result) == kOfxStatErrMissingHostFeature && result == -999.0 &&
         gParamHost->paramGetIntegral(parameter, 0.0, 1.0, &result) == kOfxStatErrMissingHostFeature && result == -999.0;
}
// These controls are declared non-animatable in the owned Choice fixtures.
// Other finite times must see the same immutable authored snapshot.
bool staticQueries(const void* handle, OfxPropertySetHandle in, bool stringChoice) {
  OfxParamSetHandle params = nullptr;
  double time = 0.0;
  if (gEffectHost->getParamSet((OfxImageEffectHandle)handle, &params) != kOfxStatOK ||
      gPropHost->propGetDouble(in, kOfxPropTime, 0, &time) != kOfxStatOK) { return false; }
  for (const char* name : { "scale", "scaleComponents", "staticCount", "mode" }) {
    OfxParamHandle parameter = nullptr;
    if (gParamHost->paramGetHandle(params, name, &parameter, nullptr) != kOfxStatOK) { return false; }
    if (std::string(name) == "scale") {
      double current = 0.0;
      if (gParamHost->paramGetValueAtTime(parameter, time, &current) != kOfxStatOK) { return false; }
      for (double query : {time - 3.25, time + 9.5}) {
        double value = -999.0;
        if (gParamHost->paramGetValueAtTime(parameter, query, &value) != kOfxStatOK || value != current) { return false; }
      }
      for (double query : {std::numeric_limits<double>::quiet_NaN(), std::numeric_limits<double>::infinity()}) {
        double value = -999.0;
        if (gParamHost->paramGetValueAtTime(parameter, query, &value) != kOfxStatErrValue || value != -999.0) { return false; }
      }
    } else if (std::string(name) == "mode" && stringChoice) {
      char* value = nullptr;
      if (gParamHost->paramGetValueAtTime(parameter, time, &value) != kOfxStatOK || !value) { return false; }
      const std::string current(value);
      for (double query : {time - 3.25, time + 9.5}) {
        if (gParamHost->paramGetValueAtTime(parameter, query, &value) != kOfxStatOK || !value || current != value) { return false; }
      }
      for (double query : {std::numeric_limits<double>::quiet_NaN(), std::numeric_limits<double>::infinity()}) {
        value = nullptr;
        if (gParamHost->paramGetValueAtTime(parameter, query, &value) != kOfxStatErrValue || value) { return false; }
      }
    } else {
      int current = 0;
      if (gParamHost->paramGetValueAtTime(parameter, time, &current) != kOfxStatOK) { return false; }
      for (double query : {time - 3.25, time + 9.5}) {
        int value = -999;
        if (gParamHost->paramGetValueAtTime(parameter, query, &value) != kOfxStatOK || value != current) { return false; }
      }
      for (double query : {std::numeric_limits<double>::quiet_NaN(), std::numeric_limits<double>::infinity()}) {
        int value = -999;
        if (gParamHost->paramGetValueAtTime(parameter, query, &value) != kOfxStatErrValue || value != -999) { return false; }
      }
    }
  }
  return true;
}

// A deterministic sequence replaces the source while the published current
// value remains exactly 1. SDK notifications and queries must see one snapshot.
bool curveSourceLifecycle(const std::string& operation, const void* handle, OfxPropertySetHandle in) {
  const bool creating = operation == kOfxActionCreateInstance;
  auto found = actions.find(handle);
  if (!creating && found == actions.end()) { return false; }
  const int phase = creating ? 0 : std::min(found->second.renders, 8);
  if (operation == kOfxActionInstanceChanged) {
    char* name = nullptr;
    if (gPropHost->propGetString(in, kOfxPropName, 0, &name) != kOfxStatOK || !name) { return false; }
    if (std::string(name) == "scale") { ++found->second.scaleChanges; }
  }
  if (operation == kOfxImageEffectActionRender) {
    const int expectedChanges = phase < 2 ? 0 : phase < 4 ? 1 : phase < 6 ? 2 : phase < 8 ? 3 : 4;
    if (found->second.scaleChanges != expectedChanges) { return false; }
  }
  OfxParamSetHandle params = nullptr;
  OfxParamHandle scale = nullptr;
  double current = -999;
  if (gEffectHost->getParamSet((OfxImageEffectHandle)handle, &params) != kOfxStatOK ||
      gParamHost->paramGetHandle(params, "scale", &scale, nullptr) != kOfxStatOK ||
      gParamHost->paramGetValue(scale, &current) != kOfxStatOK || current != 1.0) { return false; }
  const bool sampled = phase == 6 || phase == 7;
  const bool constant = phase == 4 || phase == 5;
  const double endpoint = phase == 2 || phase == 3 ? 3.0 : constant || sampled ? 1.0 : 2.0;
  double value = -999;
  unsigned int count = 999;
  double derivative = -999;
  const OfxStatus expected = sampled ? kOfxStatErrMissingHostFeature : kOfxStatOK;
  if (gParamHost->paramGetValueAtTime(scale, 24.0, &value) != expected ||
      value != (sampled ? -999.0 : endpoint) ||
      gParamHost->paramGetNumKeys(scale, &count) != expected ||
      count != (sampled ? 999U : constant ? 0U : 2U) ||
      gParamHost->paramGetDerivative(scale, 6.0, &derivative) != expected ||
      (sampled ? derivative != -999.0 : std::abs(derivative - (endpoint - 1.0)/24.0) > 1e-14)) { return false; }
  return true;
}
bool authorCurveQueries(const void* handle) {
  OfxParamSetHandle params = nullptr;
  OfxParamHandle scale = nullptr;
  double current = -999, endpoint = -999, derivative = -999, integral = -999;
  if (gEffectHost->getParamSet((OfxImageEffectHandle)handle, &params) != kOfxStatOK ||
      gParamHost->paramGetHandle(params, "scale", &scale, nullptr) != kOfxStatOK || !curveKeys(scale) ||
      gParamHost->paramGetValue(scale, &current) != kOfxStatOK || current != 1.0 ||
      gParamHost->paramGetValueAtTime(scale, 24.0, &endpoint) != kOfxStatOK || (endpoint != 2.0 && endpoint != 3.0) ||
      gParamHost->paramGetDerivative(scale, 6.0, &derivative) != kOfxStatOK ||
      std::abs(derivative - (endpoint - 1.0)/24.0) > 1e-14 ||
      gParamHost->paramGetIntegral(scale, 0.0, 24.0, &integral) != kOfxStatOK ||
      std::abs(integral - 12.0*(1.0 + endpoint)) > 1e-12) { return false; }
  return true;
}
bool lifecycleOutput(const void* handle, OfxPropertySetHandle in) {
  OfxParamSetHandle params = nullptr;
  OfxParamHandle scale = nullptr;
  const int phase = actions.at(handle).renders - 1;
  double gain = -999;
  if (gEffectHost->getParamSet((OfxImageEffectHandle)handle, &params) != kOfxStatOK ||
      gParamHost->paramGetHandle(params, "scale", &scale, nullptr) != kOfxStatOK ||
      gParamHost->paramGetValueAtTime(scale, mode() == "curve_lifecycle" && (phase == 6 || phase == 7) ? 0.0 : 24.0, &gain) != kOfxStatOK) { return false; }
  OfxImageClipHandle clip = nullptr;
  OfxPropertySetHandle image = nullptr;
  double time = 0;
  if (gPropHost->propGetDouble(in, kOfxPropTime, 0, &time) != kOfxStatOK ||
      gEffectHost->clipGetHandle((OfxImageEffectHandle)handle, "Output", &clip, nullptr) != kOfxStatOK ||
      gEffectHost->clipGetImage(clip, time, nullptr, &image) != kOfxStatOK) { return false; }
  void* data = nullptr;
  int bounds[4] = {}, rowBytes = 0;
  const bool valid = gPropHost->propGetPointer(image, kOfxImagePropData, 0, &data) == kOfxStatOK && data &&
      gPropHost->propGetIntN(image, kOfxImagePropBounds, 4, bounds) == kOfxStatOK &&
      gPropHost->propGetInt(image, kOfxImagePropRowBytes, 0, &rowBytes) == kOfxStatOK;
  if (valid) {
    for (int y = bounds[1]; y < bounds[3]; ++y) {
      auto* row = reinterpret_cast<float*>(static_cast<char*>(data) + (y - bounds[1])*rowBytes);
      for (int x = 0; x < (bounds[2] - bounds[0])*4; ++x) { row[x] *= static_cast<float>(gain); }
    }
  }
  return gEffectHost->clipReleaseImage(image) == kOfxStatOK && valid;
}

OfxStatus probe(const char* action, const void* handle,
                OfxPropertySetHandle in, OfxPropertySetHandle out) {
  const std::string operation(action);
  const std::string selected = mode();
  if (selected == "curve_authoring" &&
      (operation == kOfxActionCreateInstance || operation == kOfxActionBeginInstanceChanged ||
       operation == kOfxActionInstanceChanged || operation == kOfxActionEndInstanceChanged ||
       operation == kOfxImageEffectActionRender || operation == kOfxActionDestroyInstance) &&
      !authorCurveQueries(handle)) { return kOfxStatFailed; }
  if (selected == "curve_lifecycle" &&
      (operation == kOfxActionCreateInstance || operation == kOfxActionBeginInstanceChanged ||
       operation == kOfxActionInstanceChanged || operation == kOfxActionEndInstanceChanged ||
       operation == kOfxImageEffectActionRender || operation == kOfxActionDestroyInstance) &&
      !curveSourceLifecycle(operation, handle, in)) { return kOfxStatFailed; }
  if ((operation == kOfxActionCreateInstance || operation == kOfxActionDestroyInstance ||
       operation == kOfxActionBeginInstanceChanged ||
       operation == kOfxImageEffectActionBeginSequenceRender ||
       operation == kOfxImageEffectActionEndSequenceRender) && !imageUnavailable(handle, in)) {
    return kOfxStatFailed;
  }
  if (operation == kOfxImageEffectActionBeginSequenceRender ||
      operation == kOfxImageEffectActionRender ||
      operation == kOfxImageEffectActionEndSequenceRender) {
    int sequential = -1;
    if (gPropHost->propGetInt(in, kOfxImageEffectPropSequentialRenderStatus, 0,
                             &sequential) != kOfxStatOK || sequential != 0) {
      return kOfxStatFailed;
    }
  }
  if (operation == kOfxActionBeginInstanceChanged) {
    auto& state = actions.at(handle);
    if (state.changing) { return kOfxStatFailed; }
    state.changing = true;
    state.changes = 0;
  } else if (operation == kOfxActionInstanceChanged) {
    auto& state = actions.at(handle);
    if (!state.changing) { return kOfxStatFailed; }
    ++state.changes;
    char* reason = nullptr;
    if (gPropHost->propGetString(in, kOfxPropChangeReason, 0, &reason) == kOfxStatOK &&
        reason && std::string(reason) == kOfxChangeUserEdited &&
        !imageLifetimes(handle, in, "Source")) { return kOfxStatFailed; }
  } else if (operation == kOfxActionEndInstanceChanged) {
    auto& state = actions.at(handle);
    if (!state.changing || state.changes == 0) { return kOfxStatFailed; }
    state.changing = false;
  } else if (operation == kOfxImageEffectActionRender) {
    auto& state = actions.at(handle);
    if (state.changing) { return kOfxStatFailed; }
    if ((selected == "normal" || selected == "optional") && (!imageLifetimes(handle, in, "Source") || !imageLifetimes(handle, in, "Output"))) {
      return kOfxStatFailed;
    }
    if (selected == "leak_source") {
      OfxImageClipHandle source = nullptr;
      OfxPropertySetHandle image = nullptr;
      double time = 0.0;
      gPropHost->propGetDouble(in, kOfxPropTime, 0, &time);
      if (gEffectHost->clipGetHandle((OfxImageEffectHandle)handle, "Source", &source, nullptr) != kOfxStatOK ||
          gEffectHost->clipGetImage(source, time, nullptr, &image) != kOfxStatOK) { return kOfxStatFailed; }
      // Deliberate contract violation: the action must fail before publication.
    }
    ++state.renders;
    if (selected == "hang") { std::this_thread::sleep_for(std::chrono::seconds(5)); }
    if (selected == "unfetched" && state.renders > 1) {
      return kOfxStatOK; // Deliberately do not produce this frame.
    }
  }
  if (selected == "choice" && operation == kOfxActionInstanceChanged) {
    OfxParamSetHandle params = nullptr;
    OfxParamHandle parameter = nullptr;
    int before = -1, after = -1;
    if (gEffectHost->getParamSet((OfxImageEffectHandle)handle, &params) != kOfxStatOK ||
        gParamHost->paramGetHandle(params, "mode", &parameter, nullptr) != kOfxStatOK ||
        gParamHost->paramGetValue(parameter, &before) != kOfxStatOK ||
        gParamHost->paramSetValue(parameter, -1) != kOfxStatErrValue ||
        gParamHost->paramSetValue(parameter, 3) != kOfxStatErrValue ||
        gParamHost->paramGetValue(parameter, &after) != kOfxStatOK || before != after) {
      return kOfxStatFailed;
    }
  }
  if (selected.find("strings") == 0 && operation == kOfxActionInstanceChanged &&
      !stringValues(handle, in, false)) { return kOfxStatFailed; }
  // Successful plugin writes belong here: the instance is not yet created,
  // so the SDK must not notify InstanceChanged recursively for these writes.
  if (selected.find("strings") == 0 && operation == kOfxActionCreateInstance) {
    OfxParamSetHandle params = nullptr;
    if (gEffectHost->getParamSet((OfxImageEffectHandle)handle, &params) != kOfxStatOK) { return kOfxStatFailed; }
    for (const char* name : {"single", "multi"}) {
      OfxParamHandle parameter = nullptr;
      char* value = nullptr;
      if (gParamHost->paramGetHandle(params, name, &parameter, nullptr) != kOfxStatOK ||
          gParamHost->paramGetValue(parameter, &value) != kOfxStatOK || !value) { return kOfxStatFailed; }
      const std::string saved(value);
      if (gParamHost->paramSetValue(parameter, saved.c_str()) != kOfxStatOK ||
          gParamHost->paramGetValue(parameter, &value) != kOfxStatOK || !value || saved != value) { return kOfxStatFailed; }
    }
  }
  // Successful writes notify the owning parameter set even during creation.
  if (operation == kOfxActionCreateInstance) {
    OfxParamSetHandle params = nullptr;
    OfxParamHandle scalar = nullptr, toggle = nullptr;
    double scale = 0.0;
    int enabled = 0;
    if (gEffectHost->getParamSet((OfxImageEffectHandle)handle, &params) != kOfxStatOK ||
        gParamHost->paramGetHandle(params, "scale", &scalar, nullptr) != kOfxStatOK ||
        gParamHost->paramGetHandle(params, "scaleComponents", &toggle, nullptr) != kOfxStatOK ||
        gParamHost->paramGetValue(scalar, &scale) != kOfxStatOK ||
        gParamHost->paramGetValue(toggle, &enabled) != kOfxStatOK ||
        gParamHost->paramSetValue(scalar, scale) != ((selected == "canonical_curves" || selected == "curve_lifecycle" || selected == "curve_authoring") ? kOfxStatErrUnsupported : kOfxStatOK) ||
        gParamHost->paramSetValue(toggle, enabled) != kOfxStatOK) { return kOfxStatFailed; }
    if (selected == "choice" || selected.find("strchoice") == 0) {
      OfxParamHandle parameter = nullptr, integer = nullptr;
      int count = 0;
      if (gParamHost->paramGetHandle(params, "staticCount", &integer, nullptr) != kOfxStatOK ||
          gParamHost->paramGetValue(integer, &count) != kOfxStatOK ||
          gParamHost->paramSetValue(integer, count) != kOfxStatOK ||
          gParamHost->paramGetHandle(params, "mode", &parameter, nullptr) != kOfxStatOK) { return kOfxStatFailed; }
      if (selected == "choice") {
        int value = 0;
        if (gParamHost->paramGetValue(parameter, &value) != kOfxStatOK ||
            gParamHost->paramSetValue(parameter, value) != kOfxStatOK) { return kOfxStatFailed; }
      } else {
        char* value = nullptr;
        if (gParamHost->paramGetValue(parameter, &value) != kOfxStatOK || !value) { return kOfxStatFailed; }
        const std::string saved(value);
        if (gParamHost->paramSetValue(parameter, saved.c_str()) != kOfxStatOK) { return kOfxStatFailed; }
      }
    }
  }
  if ((selected == "compound" || selected == "compound_constant_animatable" || selected == "compound_frame_sample") &&
      (operation == kOfxActionCreateInstance || operation == kOfxActionInstanceChanged || operation == kOfxImageEffectActionRender) &&
      !compoundValues(handle, in, operation == kOfxActionCreateInstance, operation == kOfxImageEffectActionRender)) {
    return kOfxStatFailed;
  }
  if (selected == "canonical_curves" &&
      (operation == kOfxActionCreateInstance || operation == kOfxActionBeginInstanceChanged ||
       operation == kOfxActionInstanceChanged || operation == kOfxImageEffectActionRender ||
       operation == kOfxActionDestroyInstance) && !canonicalCurves(handle)) { return kOfxStatFailed; }
  const OfxStatus result = pluginMain(action, handle, in, out);
  if ((selected == "curve_lifecycle" || selected == "curve_authoring") && result == kOfxStatOK &&
      operation == kOfxImageEffectActionRender && !lifecycleOutput(handle, in)) { return kOfxStatFailed; }
  if ((selected == "compound" || selected == "compound_author" || selected == "compound_constant_animatable" || selected == "compound_frame_sample" || selected == "canonical_curves") && result == kOfxStatOK &&
      operation == kOfxImageEffectActionDescribeInContext && !defineCompounds(handle)) {
    return kOfxStatFailed;
  }
  if ((selected == "compound_author" || selected == "canonical_curves") && result == kOfxStatOK &&
      operation == kOfxImageEffectActionRender && !compoundAuthorOutput(handle, in)) {
    return kOfxStatFailed;
  }
  if (selected.find("strings") == 0 && operation == kOfxImageEffectActionRender &&
      result == kOfxStatOK && !stringValues(handle, in, true)) { return kOfxStatFailed; }

  if (selected.find("strings") == 0 && result == kOfxStatOK &&
      operation == kOfxImageEffectActionDescribeInContext) {
    const char* names[] = {"single", "multi", "existingFile", "newFile",
                           "existingDirectory", "newDirectory", "status", "rich"};
    const char* modes[] = {kOfxParamStringIsSingleLine, kOfxParamStringIsMultiLine,
      kOfxParamStringIsFilePath, kOfxParamStringIsFilePath,
      kOfxParamStringIsDirectoryPath, kOfxParamStringIsDirectoryPath,
      kOfxParamStringIsLabel, kOfxParamStringIsRichTextFormat};
    const char* defaults[] = {u8"汉字🙂", u8"first\n第二行\r\nlast\tvalue",
      u8"C:/媒体/look.cube", u8"C:/输出/new.cube", u8"C:/媒体", u8"C:/输出/new",
      u8"Ready ✓", "{\\rtf1\\ansi hello}"};
    OfxParamSetHandle params = nullptr;
    if (gEffectHost->getParamSet((OfxImageEffectHandle)handle, &params) != kOfxStatOK) { return kOfxStatFailed; }
    const int count = selected == "strings_text" ? 2 : selected == "strings_paths" ? 6 : 8;
    for (int index = 0; index < count; ++index) {
      OfxPropertySetHandle props = nullptr;
      if (gParamHost->paramDefine(params, kOfxParamTypeString, names[index], &props) != kOfxStatOK ||
          gPropHost->propSetString(props, kOfxPropLabel, 0, names[index]) != kOfxStatOK ||
          gPropHost->propSetString(props, kOfxParamPropStringMode, 0, modes[index]) != kOfxStatOK ||
          gPropHost->propSetString(props, kOfxParamPropDefault, 0, defaults[index]) != kOfxStatOK ||
          gPropHost->propSetInt(props, kOfxParamPropAnimates, 0, 0) != kOfxStatOK) { return kOfxStatFailed; }
      if (index >= 2 && index <= 5 &&
          gPropHost->propSetInt(props, kOfxParamPropStringFilePathExists, 0, index % 2 == 0) != kOfxStatOK) { return kOfxStatFailed; }
    }
  }
  if (operation == kOfxActionCreateInstance && result == kOfxStatOK) {
    actions.emplace(handle, ActionState{});
    if (selected == "instance_required") {
      property(handle, kOfxImageEffectInstancePropSequentialRender, 1);
    }
  } else if (operation == kOfxActionDestroyInstance) {
    actions.erase(handle);
  } else if (operation == kOfxImageEffectActionDescribeInContext) {
    if (selected == "required" || selected == "optional") {
      property(handle, kOfxImageEffectInstancePropSequentialRender,
               selected == "required" ? 1 : 2);
    } else if (selected == "temporal") {
      property(handle, kOfxImageEffectPropTemporalClipAccess, 1);
    }
  } else if (operation == kOfxActionInstanceChanged && selected == "become_required") {
    property(handle, kOfxImageEffectInstancePropSequentialRender, 1);
  }
  if ((selected == "integer" || selected == "interpolation" || selected == "bad_interpolation") && result == kOfxStatOK) {
    if (operation == kOfxImageEffectActionDescribeInContext) {
      OfxParamSetHandle params = nullptr;
      OfxPropertySetHandle props = nullptr;
      if (gEffectHost->getParamSet((OfxImageEffectHandle)handle, &params) != kOfxStatOK ||
          gParamHost->paramDefine(params, kOfxParamTypeInteger, "count", &props) != kOfxStatOK) { return kOfxStatFailed; }
      gPropHost->propSetString(props, kOfxPropLabel, 0, "Count");
      gPropHost->propSetInt(props, kOfxParamPropDefault, 0, 2);
      gPropHost->propSetInt(props, kOfxParamPropMin, 0, 0);
      gPropHost->propSetInt(props, kOfxParamPropMax, 0, 9);
      gPropHost->propSetInt(props, kOfxParamPropDisplayMin, 0, 0);
      gPropHost->propSetInt(props, kOfxParamPropDisplayMax, 0, 9);
      gPropHost->propSetInt(props, kOfxParamPropAnimates, 0, 1);
      if (selected != "integer") {
        if (gPropHost->propSetString(props, kOfxParamInterpType, 0, selected == "interpolation" ? kOfxParamInterpTypeConstantStep : "InvalidInterpolation") != kOfxStatOK) { return kOfxStatFailed; }
        OfxParamHandle scale = nullptr;
        OfxPropertySetHandle scaleProps = nullptr;
        if (gParamHost->paramGetHandle(params, "scale", &scale, &scaleProps) != kOfxStatOK ||
            gPropHost->propSetString(scaleProps, kOfxParamInterpType, 0, kOfxParamInterpTypeSmooth) != kOfxStatOK) { return kOfxStatFailed; }
      }
    } else if (operation == kOfxActionCreateInstance && selected == "interpolation") {
      OfxParamSetHandle params = nullptr;
      OfxParamHandle count = nullptr;
      OfxPropertySetHandle props = nullptr;
      char* value = nullptr;
      const char* replacement[] = { kOfxParamInterpTypeLinear };
      if (gEffectHost->getParamSet((OfxImageEffectHandle)handle, &params) != kOfxStatOK ||
          gParamHost->paramGetHandle(params, "count", &count, &props) != kOfxStatOK ||
          gPropHost->propSetString(props, kOfxParamInterpType, 0, replacement[0]) != kOfxStatErrValue ||
          gPropHost->propSetStringN(props, kOfxParamInterpType, 1, replacement) != kOfxStatErrValue ||
          gPropHost->propReset(props, kOfxParamInterpType) != kOfxStatErrValue ||
          gPropHost->propGetString(props, kOfxParamInterpType, 0, &value) != kOfxStatOK ||
          !value || std::string(value) != kOfxParamInterpTypeConstantStep) { return kOfxStatFailed; }
    } else if (operation == kOfxImageEffectActionRender && !applyDiscreteGain(handle, in)) { return kOfxStatFailed; }
  }
  if (selected == "choice" && result == kOfxStatOK) {
    if (operation == kOfxImageEffectActionDescribeInContext) {
      OfxParamSetHandle params = nullptr;
      OfxPropertySetHandle props = nullptr;
      const char* labels[] = { "Same", "Same", "Third" };
      const int ranks[] = { 2147483647, -2147483647 - 1, 0 };
      if (gEffectHost->getParamSet((OfxImageEffectHandle)handle, &params) != kOfxStatOK ||
          gParamHost->paramDefine(params, kOfxParamTypeChoice, "mode", &props) != kOfxStatOK ||
          gPropHost->propSetString(props, kOfxPropLabel, 0, "Mode") != kOfxStatOK ||
          gPropHost->propSetInt(props, kOfxParamPropDefault, 0, 1) != kOfxStatOK ||
          gPropHost->propSetStringN(props, kOfxParamPropChoiceOption, 3, labels) != kOfxStatOK ||
          gPropHost->propSetIntN(props, kOfxParamPropChoiceOrder, 3, ranks) != kOfxStatOK) { return kOfxStatFailed; }
    } else if (operation == kOfxImageEffectActionRender && !applyDiscreteGain(handle, in, "mode", true)) {
      return kOfxStatFailed;
    }
  }
  if (selected.find("strchoice") == 0 && result == kOfxStatOK) {
    if (operation == kOfxImageEffectActionDescribeInContext) {
      OfxParamSetHandle params = nullptr;
      OfxPropertySetHandle props = nullptr;
      const char* labels[] = { "Same", "Same", "Third" };
      const char* keys[] = { "vendor.red", "vendor.green", "vendor.blue" };
      const int ranks[] = { 2147483647, -2147483647 - 1, 0 };
      if (selected == "strchoice_duplicate") { keys[2] = keys[1]; }
      if (selected == "strchoice_gap") { keys[1] = ""; }
      if (gEffectHost->getParamSet((OfxImageEffectHandle)handle, &params) != kOfxStatOK ||
          gParamHost->paramDefine(params, kOfxParamTypeStrChoice, "mode", &props) != kOfxStatOK ||
          gPropHost->propSetString(props, kOfxPropLabel, 0, "Mode") != kOfxStatOK ||
          gPropHost->propSetStringN(props, kOfxParamPropChoiceOption, 3, labels) != kOfxStatOK ||
          gPropHost->propSetStringN(props, kOfxParamPropChoiceEnum, selected == "strchoice_missing_enum" ? 2 : 3, keys) != kOfxStatOK ||
          gPropHost->propSetIntN(props, kOfxParamPropChoiceOrder, 3, ranks) != kOfxStatOK) { return kOfxStatFailed; }
      if (selected != "strchoice_default" &&
          gPropHost->propSetString(props, kOfxParamPropDefault, 0, selected == "strchoice_bad_default" ? "vendor.unknown" : "vendor.green") != kOfxStatOK) { return kOfxStatFailed; }
    } else if (operation == kOfxActionInstanceChanged) {
      OfxParamSetHandle params = nullptr;
      OfxParamHandle parameter = nullptr;
      char* value = nullptr;
      if (gEffectHost->getParamSet((OfxImageEffectHandle)handle, &params) != kOfxStatOK ||
          gParamHost->paramGetHandle(params, "mode", &parameter, nullptr) != kOfxStatOK ||
          gParamHost->paramGetValue(parameter, &value) != kOfxStatOK || !value) { return kOfxStatFailed; }
      const std::string before(value);
      if (gParamHost->paramSetValue(parameter, "vendor.unknown") != kOfxStatErrValue ||
          gParamHost->paramGetValue(parameter, &value) != kOfxStatOK || !value || before != value) { return kOfxStatFailed; }
    } else if (operation == kOfxImageEffectActionRender && !applyDiscreteGain(handle, in, "mode", true)) { return kOfxStatFailed; }
  }
  if ((selected == "choice" || selected.find("strchoice") == 0) && result == kOfxStatOK) {
    if (operation == kOfxImageEffectActionDescribeInContext) {
      OfxParamSetHandle params = nullptr;
      OfxParamHandle scale = nullptr;
      OfxPropertySetHandle props = nullptr;
      if (gEffectHost->getParamSet((OfxImageEffectHandle)handle, &params) != kOfxStatOK ||
          gParamHost->paramGetHandle(params, "scale", &scale, &props) != kOfxStatOK ||
          gPropHost->propSetInt(props, kOfxParamPropAnimates, 0, 0) != kOfxStatOK ||
          gParamHost->paramDefine(params, kOfxParamTypeInteger, "staticCount", &props) != kOfxStatOK) { return kOfxStatFailed; }
      gPropHost->propSetInt(props, kOfxParamPropDefault, 0, 2);
      gPropHost->propSetInt(props, kOfxParamPropMin, 0, 0);
      gPropHost->propSetInt(props, kOfxParamPropMax, 0, 9);
      gPropHost->propSetInt(props, kOfxParamPropDisplayMin, 0, 0);
      gPropHost->propSetInt(props, kOfxParamPropDisplayMax, 0, 9);
      gPropHost->propSetInt(props, kOfxParamPropAnimates, 0, 0);
      if (gParamHost->paramDefine(params, kOfxParamTypeDouble, "calculus", &props) != kOfxStatOK) { return kOfxStatFailed; }
      gPropHost->propSetDouble(props, kOfxParamPropDefault, 0, -0.5);
      gPropHost->propSetDouble(props, kOfxParamPropMin, 0, -std::numeric_limits<double>::max());
      gPropHost->propSetDouble(props, kOfxParamPropMax, 0, std::numeric_limits<double>::max());
      gPropHost->propSetInt(props, kOfxParamPropAnimates, 0, 0);
    } else if (operation == kOfxImageEffectActionRender &&
               (!staticCalculus(handle) || !staticQueries(handle, in, selected != "choice"))) { return kOfxStatFailed; }
  }
  return result;
}
OfxPlugin probePlugin = basicPlugin;
}
extern "C" {
OfxExport OfxPlugin* OfxGetPlugin(int index) {
  if (index != 0) { return nullptr; }
  probePlugin.mainEntry = probe;
  return &probePlugin;
}
OfxExport int OfxGetNumberOfPlugins() { return 1; }
}
