#include <fstream>
#include <filesystem>
#ifdef _WIN32
#include <process.h>
#else
#include <unistd.h>
#endif
// Copyright Mondrian contributors.
// SPDX-License-Identifier: BSD-3-Clause
// Test-only contract probe around the externally checked-out official example.
#define OfxGetPlugin MondrianReferenceGetPlugin
#define OfxGetNumberOfPlugins MondrianReferenceGetNumberOfPlugins
#include "Examples/Basic/basic.cpp"
#undef OfxGetPlugin
#undef OfxGetNumberOfPlugins
#include <cstdlib>
#include <atomic>
#include "ofxMultiThread.h"
#include <algorithm>
#include <chrono>
#include <thread>
#include <map>
#include <string>
#include <limits>
#include <cmath>
#include <array>
#include <mutex>
#include <cstring>
#include <cstdint>
#include <utility>
#include <type_traits>
#include "ofxGPURender.h"
#include "ofxProgress.h"
#include "ofxMessage.h"
#include "ofxTimeLine.h"

namespace {
int bootstrapPhase = 0;
bool cpuDisabled = false;

bool honestHostCapabilities(const OfxHost* host) {
  if (!host || !host->host || !host->fetchSuite) { return false; }
  const auto* properties = static_cast<const OfxPropertySuiteV1*>(
      host->fetchSuite(host->host, kOfxPropertySuite, 1));
  if (!properties) { return false; }
  const char* names[] = {kOfxImageEffectPropSupportedPixelDepths,
                        kOfxImageEffectPropSupportedComponents,
                        kOfxImageEffectPropSupportedContexts,
                        kOfxImageEffectPropCPURenderSupported};
  const char* expected[] = {kOfxBitDepthFloat, kOfxImageComponentRGBA,
                           kOfxImageEffectContextFilter, "true"};
  for (int index = 0; index < 4; ++index) {
    int dimension = -1;
    char* value = nullptr;
    if (properties->propGetDimension(host->host, names[index], &dimension) != kOfxStatOK ||
        dimension != 1 ||
        properties->propGetString(host->host, names[index], 0, &value) != kOfxStatOK ||
        !value || std::string(value) != expected[index]) { return false; }
  }
  for (const char* name : {kOfxImageEffectPropSupportsOverlays,
                           kOfxImageEffectPropSupportsMultiResolution,
                           kOfxImageEffectPropSupportsTiles,
                           kOfxImageEffectPropTemporalClipAccess,
                           kOfxImageEffectPropSupportsMultipleClipDepths,
                           kOfxImageEffectPropSupportsMultipleClipPARs,
                           kOfxParamHostPropSupportsCustomInteract}) {
    int value = -1;
    if (properties->propGetInt(host->host, name, 0, &value) != kOfxStatOK || value != 0) {
      return false;
    }
  }
  return true;
}

// Host-only property contracts. Always use the cached suite while probing the
// private Host pointer: a legacy reset clears it, so restore before any fetchSuite.
bool readonlyHostIntegers(const OfxPropertySuiteV1* properties, OfxPropertySetHandle host,
                          const char* name, const std::array<int, 2>& expected) {
  int dimension = 0;
  std::array<int, 2> before{};
  if (properties->propGetDimension(host, name, &dimension) != kOfxStatOK || dimension != 2 ||
      properties->propGetIntN(host, name, 2, before.data()) != kOfxStatOK || before != expected) { return false; }
  const auto check = [&](OfxStatus status) {
    std::array<int, 2> after{};
    int size = 0;
    const bool unchanged = properties->propGetDimension(host, name, &size) == kOfxStatOK && size == 2 &&
        properties->propGetIntN(host, name, 2, after.data()) == kOfxStatOK && after == before;
    if (!unchanged && properties->propSetIntN(host, name, 2, before.data()) != kOfxStatOK) { return false; }
    return status == kOfxStatErrValue && unchanged;
  };
  const int replacement[2] = {91, 92};
  bool valid = check(properties->propSetInt(host, name, 0, replacement[0]));
  valid = check(properties->propSetIntN(host, name, 2, replacement)) && valid;
  valid = check(properties->propReset(host, name)) && valid;
  return valid;
}

bool readonlyHostString(const OfxPropertySuiteV1* properties, OfxPropertySetHandle host,
                        const char* name, const char* expected) {
  char* value = nullptr;
  int dimension = 0;
  if (properties->propGetDimension(host, name, &dimension) != kOfxStatOK || dimension != 1 ||
      properties->propGetString(host, name, 0, &value) != kOfxStatOK || !value || std::string(value) != expected) { return false; }
  const std::string before(value);
  const auto check = [&](OfxStatus status) {
    char* after = nullptr;
    int size = 0;
    const bool unchanged = properties->propGetDimension(host, name, &size) == kOfxStatOK && size == 1 &&
        properties->propGetString(host, name, 0, &after) == kOfxStatOK && after && before == after;
    const char* original[] = {before.c_str()};
    if (!unchanged && properties->propSetStringN(host, name, 1, original) != kOfxStatOK) { return false; }
    return status == kOfxStatErrValue && unchanged;
  };
  const char* replacement[] = {"B06 must not replace a Host property"};
  bool valid = check(properties->propSetString(host, name, 0, replacement[0]));
  valid = check(properties->propSetStringN(host, name, 1, replacement)) && valid;
  valid = check(properties->propReset(host, name)) && valid;
  return valid;
}

bool readonlyHostPointer(const OfxPropertySuiteV1* properties, OfxPropertySetHandle host) {
  constexpr const char* name = "sf.openfx.net.OfxHostSupportHostPointer";
  void* before = nullptr;
  int dimension = 0;
  if (properties->propGetDimension(host, name, &dimension) != kOfxStatOK || dimension != 1 ||
      properties->propGetPointer(host, name, 0, &before) != kOfxStatOK || !before) { return false; }
  const auto check = [&](OfxStatus status) {
    void* after = nullptr;
    int size = 0;
    const bool unchanged = properties->propGetDimension(host, name, &size) == kOfxStatOK && size == 1 &&
        properties->propGetPointer(host, name, 0, &after) == kOfxStatOK && after == before;
    if (!unchanged) {
      // Never call host->fetchSuite between reset and this same-suite restore.
      if (properties->propSetPointer(host, name, 0, before) != kOfxStatOK ||
          properties->propGetPointer(host, name, 0, &after) != kOfxStatOK || after != before) { std::abort(); }
    }
    return status == kOfxStatErrValue && unchanged;
  };
  void* original[] = {before};
  bool valid = check(properties->propSetPointer(host, name, 0, before));
  valid = check(properties->propSetPointerN(host, name, 1, original)) && valid;
  valid = check(properties->propReset(host, name)) && valid;
  return valid;
}

bool readonlyHostProperties(const OfxHost* host) {
  const auto* properties = host ? static_cast<const OfxPropertySuiteV1*>(
      host->fetchSuite(host->host, kOfxPropertySuite, 1)) : nullptr;
  if (!properties) { return false; }
  bool valid = readonlyHostPointer(properties, host->host);
  valid = (host->fetchSuite(host->host, kOfxPropertySuite, 1) == properties) && valid;
  valid = readonlyHostIntegers(properties, host->host, kOfxPropAPIVersion, {1, 4}) && valid;
  valid = readonlyHostIntegers(properties, host->host, kOfxPropVersion, {1, 0}) && valid;
  valid = readonlyHostString(properties, host->host, kOfxPropType, kOfxTypeImageEffectHost) && valid;
  valid = readonlyHostString(properties, host->host, kOfxPropName, "Mondrian") && valid;
  valid = readonlyHostString(properties, host->host, kOfxPropLabel, "Mondrian") && valid;
  valid = readonlyHostString(properties, host->host, kOfxPropVersionLabel, "1.0") && valid;
  return valid;
}

bool hostParameterLimit(const OfxHost* host) {
  const auto* properties = host ? static_cast<const OfxPropertySuiteV1*>(
      host->fetchSuite(host->host, kOfxPropertySuite, 1)) : nullptr;
  int limit = -2;
  return properties && properties->propGetInt(host->host, kOfxParamHostPropMaxParameters, 0, &limit) == kOfxStatOK && limit == 4096;
}

bool addHostParameterCountProbe(const void* handle, bool overflow) {
  OfxParamSetHandle parameters = nullptr;
  if (gEffectHost->getParamSet((OfxImageEffectHandle)handle, &parameters) != kOfxStatOK) { return false; }
  // The untouched pinned Basic defines six value parameters, one Group, one Page.
  // All eight are descriptor parameters, even though only six enter author values.
  for (const char* name : {"scale", "scaleComponents", "componentScales", "scaleR", "scaleG", "scaleB", "scaleA", "Main"}) {
    OfxParamHandle parameter = nullptr;
    if (gParamHost->paramGetHandle(parameters, name, &parameter, nullptr) != kOfxStatOK || !parameter) { return false; }
  }
  const int total = overflow ? 4097 : 4096;
  for (int index = 8; index < total; ++index) {
    const std::string name = "host_limit_group_" + std::to_string(index);
    OfxPropertySetHandle properties = nullptr;
    if (gParamHost->paramDefine(parameters, kOfxParamTypeGroup, name.c_str(), &properties) != kOfxStatOK || !properties) { return false; }
  }
  return true;
}

// Describe-only boundary: the untouched Basic context also contains Group/Page,
// so define this context independently to exercise all 4096 exported values.
bool defineHostNumericParameterCountProbe(const void* handle, bool overflow) {
  const auto effect = (OfxImageEffectHandle)handle;
  for (const char* name : {kOfxImageEffectSimpleSourceClipName, kOfxImageEffectOutputClipName}) {
    OfxPropertySetHandle properties = nullptr;
    if (gEffectHost->clipDefine(effect, name, &properties) != kOfxStatOK || !properties ||
        gPropHost->propSetString(properties, kOfxImageEffectPropSupportedComponents, 0,
                                kOfxImageComponentRGBA) != kOfxStatOK) { return false; }
  }
  OfxParamSetHandle parameters = nullptr;
  if (gEffectHost->getParamSet(effect, &parameters) != kOfxStatOK) { return false; }
  for (int index = 0; index < 4096; ++index) {
    const std::string name = "host_value_" + std::to_string(index);
    OfxPropertySetHandle properties = nullptr;
    if (gParamHost->paramDefine(parameters, kOfxParamTypeDouble, name.c_str(), &properties) != kOfxStatOK || !properties ||
        gPropHost->propSetString(properties, kOfxPropLabel, 0, name.c_str()) != kOfxStatOK ||
        gPropHost->propSetString(properties, kOfxParamPropDoubleType, 0, kOfxParamDoubleTypePlain) != kOfxStatOK ||
        gPropHost->propSetDouble(properties, kOfxParamPropDefault, 0, 0.5) != kOfxStatOK ||
        gPropHost->propSetDouble(properties, kOfxParamPropMin, 0, 0.0) != kOfxStatOK ||
        gPropHost->propSetDouble(properties, kOfxParamPropMax, 0, 1.0) != kOfxStatOK ||
        gPropHost->propSetDouble(properties, kOfxParamPropDisplayMin, 0, 0.0) != kOfxStatOK ||
        gPropHost->propSetDouble(properties, kOfxParamPropDisplayMax, 0, 1.0) != kOfxStatOK) { return false; }
  }
  if (overflow) {
    OfxPropertySetHandle properties = nullptr;
    if (gParamHost->paramDefine(parameters, kOfxParamTypeGroup, "host_extra_group", &properties) != kOfxStatOK || !properties) { return false; }
  }
  return true;
}

// Describe-only temporal admission controls. Define an independent zero-parameter
// context through legal descriptor APIs; never fetch an instance Clip handle here.
bool temporalClipAdmissionMode(const std::string& selected) {
  return selected == "clip_temporal_source" || selected == "clip_temporal_output" ||
      selected == "clip_temporal_optional";
}
bool defineTemporalClipAdmissionProbe(const void* handle, const std::string& selected) {
  const auto effect = (OfxImageEffectHandle)handle;
  OfxPropertySetHandle properties = nullptr;
  int temporal = -1;
  if (gEffectHost->getPropertySet(effect, &properties) != kOfxStatOK || !properties ||
      gPropHost->propGetInt(properties, kOfxImageEffectPropTemporalClipAccess, 0, &temporal) != kOfxStatOK ||
      temporal != 0 ||
      gPropHost->propGetInt(gHost->host, kOfxImageEffectPropTemporalClipAccess, 0, &temporal) != kOfxStatOK ||
      temporal != 0) { return false; }
  for (const char* name : {kOfxImageEffectSimpleSourceClipName, kOfxImageEffectOutputClipName,
                           "TemporalOptional"}) {
    const bool optional = std::strcmp(name, "TemporalOptional") == 0;
    if (optional && selected != "clip_temporal_optional") { continue; }
    const int expected = optional ||
        (selected == "clip_temporal_source" && std::strcmp(name, kOfxImageEffectSimpleSourceClipName) == 0) ||
        (selected == "clip_temporal_output" && std::strcmp(name, kOfxImageEffectOutputClipName) == 0);
    properties = nullptr;
    int observed = -1, dimension = 0;
    if (gEffectHost->clipDefine(effect, name, &properties) != kOfxStatOK || !properties ||
        gPropHost->propSetString(properties, kOfxImageEffectPropSupportedComponents, 0, kOfxImageComponentRGBA) != kOfxStatOK ||
        gPropHost->propSetInt(properties, kOfxImageClipPropOptional, 0, optional ? 1 : 0) != kOfxStatOK ||
        gPropHost->propSetInt(properties, kOfxImageEffectPropTemporalClipAccess, 0, expected) != kOfxStatOK ||
        gPropHost->propGetDimension(properties, kOfxImageEffectPropTemporalClipAccess, &dimension) != kOfxStatOK || dimension != 1 ||
        gPropHost->propGetInt(properties, kOfxImageEffectPropTemporalClipAccess, 0, &observed) != kOfxStatOK || observed != expected ||
        gPropHost->propGetInt(properties, kOfxImageClipPropOptional, 0, &observed) != kOfxStatOK || observed != (optional ? 1 : 0)) { return false; }
  }
  return true;
}

bool writablePluginLabels(const void* handle, bool parameter) {
  OfxPropertySetHandle properties = nullptr;
  if (parameter) {
    OfxParamSetHandle parameters = nullptr;
    OfxParamHandle scale = nullptr;
    if (gEffectHost->getParamSet((OfxImageEffectHandle)handle, &parameters) != kOfxStatOK ||
        gParamHost->paramGetHandle(parameters, "scale", &scale, &properties) != kOfxStatOK) { return false; }
  } else if (gEffectHost->getPropertySet((OfxImageEffectHandle)handle, &properties) != kOfxStatOK) { return false; }
  const char* label = parameter ? "Writable parameter label" : "Writable plugin descriptor label";
  char* observed = nullptr;
  return gPropHost->propSetString(properties, kOfxPropLabel, 0, label) == kOfxStatOK &&
      gPropHost->propGetString(properties, kOfxPropLabel, 0, &observed) == kOfxStatOK && observed && std::string(observed) == label;
}

bool canonicalProgressiveContract(const void* handle, OfxPropertySetHandle in) {
  OfxPropertySetHandle properties = nullptr;
  OfxImageClipHandle source = nullptr;
  OfxRectD rod{};
  double par = 0.0, size[2]{}, extent[2]{}, offset[2]{}, time = 0.0;
  const auto* timeline = static_cast<const OfxTimeLineSuiteV1*>(
      gHost->fetchSuite(gHost->host, kOfxTimeLineSuite, 1));
  if (!timeline || timeline->getTime(const_cast<void*>(handle), &time) != kOfxStatOK ||
      gEffectHost->getPropertySet((OfxImageEffectHandle)handle, &properties) != kOfxStatOK ||
      gPropHost->propGetDouble(properties, kOfxImageEffectPropProjectPixelAspectRatio, 0, &par) != kOfxStatOK ||
      gPropHost->propGetDoubleN(properties, kOfxImageEffectPropProjectSize, 2, size) != kOfxStatOK ||
      gPropHost->propGetDoubleN(properties, kOfxImageEffectPropProjectExtent, 2, extent) != kOfxStatOK ||
      gPropHost->propGetDoubleN(properties, kOfxImageEffectPropProjectOffset, 2, offset) != kOfxStatOK ||
      gEffectHost->clipGetHandle((OfxImageEffectHandle)handle, "Source", &source, nullptr) != kOfxStatOK ||
      gEffectHost->clipGetRegionOfDefinition(source, time, &rod) != kOfxStatOK ||
      !std::isfinite(size[0]) || !std::isfinite(extent[0]) || par <= 0.0 ||
      size[0] != rod.x2 || extent[0] != rod.x2 || size[1] != rod.y2 ||
      extent[1] != rod.y2 || offset[0] != 0.0 || offset[1] != 0.0 ||
      rod.x1 != 0.0 || rod.y1 != 0.0) { return false; }
  if (in) {
    char* field = nullptr;
    int window[4]{};
    if (gPropHost->propGetString(in, kOfxImageEffectPropFieldToRender, 0, &field) != kOfxStatOK ||
        !field || std::string(field) != kOfxImageFieldNone ||
        gPropHost->propGetIntN(in, kOfxImageEffectPropRenderWindow, 4, window) != kOfxStatOK ||
        window[0] != 0 || window[1] != 0 ||
        window[2] * par != rod.x2 || window[3] != rod.y2) { return false; }
  }
  return true;
}

bool canonicalCoordinateOutput(const void* handle, OfxPropertySetHandle in) {
  OfxImageClipHandle output = nullptr;
  OfxPropertySetHandle image = nullptr;
  double time = 0.0;
  if (gPropHost->propGetDouble(in, kOfxPropTime, 0, &time) != kOfxStatOK ||
      gEffectHost->clipGetHandle((OfxImageEffectHandle)handle, "Output", &output, nullptr) != kOfxStatOK ||
      gEffectHost->clipGetImage(output, time, nullptr, &image) != kOfxStatOK) { return false; }
  void* data = nullptr;
  int bounds[4]{}, stride = 0;
  double par = 0.0;
  bool valid = gPropHost->propGetPointer(image, kOfxImagePropData, 0, &data) == kOfxStatOK && data &&
      gPropHost->propGetIntN(image, kOfxImagePropBounds, 4, bounds) == kOfxStatOK &&
      gPropHost->propGetInt(image, kOfxImagePropRowBytes, 0, &stride) == kOfxStatOK &&
      gPropHost->propGetDouble(image, kOfxImagePropPixelAspectRatio, 0, &par) == kOfxStatOK &&
      bounds[0] == 0 && bounds[1] == 0 && bounds[2] > 0 && bounds[3] > 0;
  if (valid) {
    for (int y = bounds[1]; y < bounds[3]; ++y) {
      auto* row = reinterpret_cast<OfxRGBAColourF*>(static_cast<char*>(data) + y * stride);
      for (int x = bounds[0]; x < bounds[2]; ++x) {
        row[x].r += static_cast<float>((x + 0.5) * par / 32.0);
        row[x].g += static_cast<float>((y + 0.5) / 64.0);
      }
    }
  }
  return gEffectHost->clipReleaseImage(image) == kOfxStatOK && valid;
}

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
bool isCurveLifecycleMode(const std::string& selected) {
  return selected == "curve_lifecycle" || selected == "curve_animation_state";
}
void describeActionReceipt(const char* action, const char* point,
                           unsigned attempted, unsigned accepted, int status) noexcept;

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
  if (mode() == "curve_animation_state") {
    OfxPropertySetHandle properties = nullptr;
    int animating = -1;
    if (gParamHost->paramGetPropertySet(scale, &properties) != kOfxStatOK || !properties) { return false; }
    const auto status = gPropHost->propGetInt(properties, kOfxParamPropIsAnimating, 0, &animating);
    const unsigned expected = constant ? 0 : 1;
    if (status != kOfxStatOK || animating != static_cast<int>(expected)) {
      // Only a mismatch adds a row: keep the ordinary lifecycle action receipt
      // bounded while identifying a real property error rather than a timeout.
      describeActionReceipt(operation.c_str(), "is-animating", expected,
                            static_cast<unsigned>(animating), status);
      return false;
    }
  }
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
enum class AuthorHistoryEdit { Time, Value, Transform, Interpolation, Reset };

bool isAuthorHistoryMode(const std::string& selected) {
  return selected == "curve_group_authoring" || selected == "curve_group_value_authoring" ||
         selected == "curve_group_transform_authoring" ||
         selected == "curve_group_interpolation_authoring" || selected == "curve_reset_authoring";
}

bool authorHistoryCurveQueries(const void* handle, AuthorHistoryEdit edit) {
  const bool valueEdit = edit == AuthorHistoryEdit::Value || edit == AuthorHistoryEdit::Transform;
  const bool timeEdit = edit == AuthorHistoryEdit::Time || edit == AuthorHistoryEdit::Transform;
  const bool interpolationEdit = edit == AuthorHistoryEdit::Interpolation;
  OfxParamSetHandle params = nullptr;
  OfxParamHandle scale = nullptr;
  unsigned count = 999;
  double current = -999, middle = -999, end = -999;
  if (gEffectHost->getParamSet((OfxImageEffectHandle)handle, &params) != kOfxStatOK ||
      gParamHost->paramGetHandle(params, "scale", &scale, nullptr) != kOfxStatOK ||
      gParamHost->paramGetNumKeys(scale, &count) != kOfxStatOK) { return false; }
  if (edit == AuthorHistoryEdit::Reset && count == 0) {
    double value = -999, derivative = -999, integral = -999, keyTime = -999;
    int keyIndex = -999;
    if (gParamHost->paramGetValue(scale, &value) != kOfxStatOK || value != 1.0 ||
        gParamHost->paramGetKeyTime(scale, 0, &keyTime) != kOfxStatErrBadIndex || keyTime != -999 ||
        gParamHost->paramGetDerivative(scale, 3.0, &derivative) != kOfxStatOK || derivative != 0.0 ||
        gParamHost->paramGetIntegral(scale, 0.0, 18.0, &integral) != kOfxStatOK || integral != 18.0) { return false; }
    for (int direction : {-1, 0, 1}) {
      keyIndex = -999;
      if (gParamHost->paramGetKeyIndex(scale, 3.0, direction, &keyIndex) != kOfxStatFailed || keyIndex != -1) { return false; }
    }
    for (double time : {-3.0, 0.0, 3.0, 6.0, 12.0, 18.0, 24.0}) {
      if (gParamHost->paramGetValueAtTime(scale, time, &value) != kOfxStatOK || value != 1.0) { return false; }
    }
    return true;
  }
  if (count != 3 ||
      gParamHost->paramGetKeyTime(scale, 1, &middle) != kOfxStatOK ||
      gParamHost->paramGetKeyTime(scale, 2, &end) != kOfxStatOK ||
      !((middle == 6.0 && end == 12.0) || (timeEdit && middle == 12.0 && end == 18.0)) ||
      gParamHost->paramGetValue(scale, &current) != kOfxStatOK || current != 1.0) { return false; }
  double offset = 0.0;
  if (valueEdit) {
    double middleValue = -999;
    if (gParamHost->paramGetValueAtTime(scale, middle, &middleValue) != kOfxStatOK ||
        (middleValue != 2.0 && middleValue != 2.5)) { return false; }
    offset = middleValue - 2.0;
  }
  // Joint edits publish both axes together. Reject a partially translated snapshot.
  if (edit == AuthorHistoryEdit::Transform &&
      ((middle == 6.0 && offset != 0.0) || (middle == 12.0 && offset != 0.5))) { return false; }
  const double times[] = {0.0, middle, end};
  for (unsigned index = 0; index < 3; ++index) {
    double time = -999, value = -999;
    int keyIndex = -999;
    if (gParamHost->paramGetKeyTime(scale, index, &time) != kOfxStatOK || time != times[index] ||
        gParamHost->paramGetKeyIndex(scale, time, 0, &keyIndex) != kOfxStatOK || keyIndex != static_cast<int>(index) ||
        gParamHost->paramGetValueAtTime(scale, time, &value) != kOfxStatOK || value != (interpolationEdit && index == 1 ? 2.5 : 1.0 + index + (index > 0 ? offset : 0.0))) { return false; }
  }
  double sentinel = -999;
  if (gParamHost->paramGetKeyTime(scale, 3, &sentinel) != kOfxStatErrBadIndex || sentinel != -999) { return false; }
  // Check the derivative independently by a symmetric value query inside a segment.
  double lower = -999, upper = -999, derivative = -999;
  const double epsilon = 1e-3;
  if (gParamHost->paramGetValueAtTime(scale, 3.0 - epsilon, &lower) != kOfxStatOK ||
      gParamHost->paramGetValueAtTime(scale, 3.0 + epsilon, &upper) != kOfxStatOK ||
      gParamHost->paramGetDerivative(scale, 3.0, &derivative) != kOfxStatOK ||
      !std::isfinite(derivative) || std::abs(derivative - (upper - lower)/(2.0*epsilon)) > 1e-6) { return false; }
  // Independent Simpson quadrature, split at exact keys to include clamped tails.
  double expectedIntegral = 0.0;
  const double boundaries[] = {0.0, middle, end, 18.0};
  constexpr int subdivisions = 384;
  for (int segment = 0; segment < 3; ++segment) {
    const double width = boundaries[segment + 1] - boundaries[segment];
    if (width == 0.0) { continue; }
    double sum = 0.0;
    for (int sample = 0; sample <= subdivisions; ++sample) {
      const double time = boundaries[segment] + width*sample/subdivisions;
      double value = -999;
      if (gParamHost->paramGetValueAtTime(scale, time, &value) != kOfxStatOK || !std::isfinite(value)) { return false; }
      sum += value*((sample == 0 || sample == subdivisions) ? 1.0 : sample % 2 ? 4.0 : 2.0);
    }
    expectedIntegral += width*sum/(3.0*subdivisions);
  }
  double integral = -999;
  return gParamHost->paramGetIntegral(scale, 0.0, 18.0, &integral) == kOfxStatOK &&
         std::isfinite(integral) && std::abs(integral - expectedIntegral) < 1e-6;
}
bool lifecycleOutput(const void* handle, OfxPropertySetHandle in) {
  OfxParamSetHandle params = nullptr;
  OfxParamHandle scale = nullptr;
  const int phase = actions.at(handle).renders - 1;
  double gain = -999;
  if (gEffectHost->getParamSet((OfxImageEffectHandle)handle, &params) != kOfxStatOK ||
      gParamHost->paramGetHandle(params, "scale", &scale, nullptr) != kOfxStatOK ||
      gParamHost->paramGetValueAtTime(scale, isAuthorHistoryMode(mode()) ? 3.0 : isCurveLifecycleMode(mode()) && (phase == 6 || phase == 7) ? 0.0 : 24.0, &gain) != kOfxStatOK) { return false; }
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

bool defineOutputAlpha(const void* handle) {
  OfxParamSetHandle params = nullptr;
  OfxPropertySetHandle props = nullptr, effect = nullptr;
  if (gEffectHost->getParamSet((OfxImageEffectHandle)handle, &params) != kOfxStatOK ||
      gParamHost->paramDefine(params, kOfxParamTypeChoice, "alphaMode", &props) != kOfxStatOK ||
      gEffectHost->getPropertySet((OfxImageEffectHandle)handle, &effect) != kOfxStatOK ||
      gPropHost->propSetString(effect, kOfxImageEffectPropClipPreferencesSlaveParam, 0, "alphaMode") != kOfxStatOK ||
      gPropHost->propSetString(props, kOfxPropLabel, 0, "Output alpha") != kOfxStatOK ||
      gPropHost->propSetInt(props, kOfxParamPropAnimates, 0, 0) != kOfxStatOK ||
      gPropHost->propSetInt(props, kOfxParamPropDefault, 0, 0) != kOfxStatOK) { return false; }
  const char* captions[] = {"Straight", "Premultiplied", "Opaque", "Invalid metadata",
      "Nonfinite", "Division overflow", "Signed alpha", "Alpha above one", "Zero-alpha emission"};
  for (int index = 0; index < 9; ++index) {
    if (gPropHost->propSetString(props, kOfxParamPropChoiceOption, index, captions[index]) != kOfxStatOK) { return false; }
  }
  return true;
}

int outputAlphaMode(const void* handle, OfxPropertySetHandle in) {
  OfxParamSetHandle params = nullptr;
  OfxParamHandle parameter = nullptr;
  int value = -1;
  if (gEffectHost->getParamSet((OfxImageEffectHandle)handle, &params) != kOfxStatOK ||
      gParamHost->paramGetHandle(params, "alphaMode", &parameter, nullptr) != kOfxStatOK) { return -1; }
  if (in) {
    double time = 0.0;
    if (gPropHost->propGetDouble(in, kOfxPropTime, 0, &time) != kOfxStatOK ||
        gParamHost->paramGetValueAtTime(parameter, time, &value) != kOfxStatOK) { return -1; }
  } else if (gParamHost->paramGetValue(parameter, &value) != kOfxStatOK) { return -1; }
  return value;
}

const char* outputAlphaRepresentation(int choice) {
  if (choice == 0) { return kOfxImageUnPreMultiplied; }
  if (choice == 2) { return kOfxImageOpaque; }
  if (choice == 3) { return "org.mondrian.test.InvalidAlpha"; }
  return kOfxImagePreMultiplied;
}

bool negotiatedAlphaOutput(const void* handle, OfxPropertySetHandle in, bool write) {
  const int choice = outputAlphaMode(handle, in);
  if (choice < 0 || choice > 8) { return false; }
  // An invalid preference must be rejected before any Render action.
  if (choice == 3) { std::abort(); }
  double time = 0.0;
  if (gPropHost->propGetDouble(in, kOfxPropTime, 0, &time) != kOfxStatOK) { return false; }
  for (const char* name : {"Source", "Output"}) {
    OfxImageClipHandle clip = nullptr;
    OfxPropertySetHandle clipProps = nullptr, image = nullptr;
    char* clipAlpha = nullptr;
    char* imageAlpha = nullptr;
    if (gEffectHost->clipGetHandle((OfxImageEffectHandle)handle, name, &clip, &clipProps) != kOfxStatOK ||
        gPropHost->propGetString(clipProps, kOfxImageEffectPropPreMultiplication, 0, &clipAlpha) != kOfxStatOK ||
        gEffectHost->clipGetImage(clip, time, nullptr, &image) != kOfxStatOK) { return false; }
    const char* expected = std::string(name) == "Source" ? kOfxImageUnPreMultiplied : outputAlphaRepresentation(choice);
    bool valid = clipAlpha && std::string(clipAlpha) == expected &&
        gPropHost->propGetString(image, kOfxImageEffectPropPreMultiplication, 0, &imageAlpha) == kOfxStatOK &&
        imageAlpha && std::string(imageAlpha) == expected;
    if (valid && write && std::string(name) == "Output") {
      void* data = nullptr;
      int bounds[4]{}, stride = 0;
      valid = gPropHost->propGetPointer(image, kOfxImagePropData, 0, &data) == kOfxStatOK && data &&
          gPropHost->propGetIntN(image, kOfxImagePropBounds, 4, bounds) == kOfxStatOK &&
          gPropHost->propGetInt(image, kOfxImagePropRowBytes, 0, &stride) == kOfxStatOK;
      if (valid) {
        for (int y = bounds[1]; y < bounds[3]; ++y) {
          auto* row = reinterpret_cast<OfxRGBAColourF*>(static_cast<char*>(data) + (y - bounds[1]) * stride);
          for (int x = 0; x < bounds[2] - bounds[0]; ++x) {
            auto& pixel = row[x];
            if (choice == 2) { pixel.a = 123.0f; }
            else if (choice != 0) {
              pixel.r *= pixel.a; pixel.g *= pixel.a; pixel.b *= pixel.a;
              if (choice == 8 && pixel.a == 0.0f) { pixel.r = 7.0f; pixel.g = -3.0f; pixel.b = 11.0f; }
            }
          }
        }
        auto& first = *static_cast<OfxRGBAColourF*>(data);
        if (choice == 4) { first.r = std::numeric_limits<float>::quiet_NaN(); }
        if (choice == 5) { first.r = std::numeric_limits<float>::max(); first.a = std::numeric_limits<float>::min(); }
        if (choice == 6) { first.a = -0.5f; }
        if (choice == 7) { first.a = 1.5f; }
      }
    }
    if (gEffectHost->clipReleaseImage(image) != kOfxStatOK || !valid) { return false; }
  }
  return true;
}

bool sourceAssociationMatches(const void* handle, OfxPropertySetHandle in) {
  const int choice = outputAlphaMode(handle, in);
  const char* expected = choice == 0 ? kOfxImageUnPreMultiplied : choice == 1 ? kOfxImagePreMultiplied : choice == 2 ? kOfxImageOpaque : nullptr;
  OfxImageClipHandle clip = nullptr;
  OfxPropertySetHandle props = nullptr, image = nullptr;
  double time = 0.0;
  char* clipAlpha = nullptr;
  char* imageAlpha = nullptr;
  if (!expected || gPropHost->propGetDouble(in, kOfxPropTime, 0, &time) != kOfxStatOK ||
      gEffectHost->clipGetHandle((OfxImageEffectHandle)handle, "Source", &clip, &props) != kOfxStatOK ||
      gPropHost->propGetString(props, kOfxImageEffectPropPreMultiplication, 0, &clipAlpha) != kOfxStatOK ||
      gEffectHost->clipGetImage(clip, time, nullptr, &image) != kOfxStatOK) { return false; }
  const bool valid = clipAlpha && std::string(clipAlpha) == expected &&
      gPropHost->propGetString(image, kOfxImageEffectPropPreMultiplication, 0, &imageAlpha) == kOfxStatOK &&
      imageAlpha && std::string(imageAlpha) == expected;
  return gEffectHost->clipReleaseImage(image) == kOfxStatOK && valid;
}

// Owned fixture metadata/analysis oracle; it never hashes image pixels.
std::string capturedSourceIdentifier, capturedOutputIdentifier;
std::map<std::string, std::array<float, 3>> imageAnalysis;

bool imageMetadata(const void* handle, OfxPropertySetHandle in, bool identifier) {
  double time = 0.0;
  if (gPropHost->propGetDouble(in, kOfxPropTime, 0, &time) != kOfxStatOK) { return false; }
  std::string labels[2];
  for (int index = 0; index < 2; ++index) {
    OfxImageClipHandle clip = nullptr;
    OfxPropertySetHandle first = nullptr, second = nullptr;
    if (gEffectHost->clipGetHandle((OfxImageEffectHandle)handle, index == 0 ? "Source" : "Output", &clip, nullptr) != kOfxStatOK ||
        gEffectHost->clipGetImage(clip, time, nullptr, &first) != kOfxStatOK) { return false; }
    bool valid = gEffectHost->clipGetImage(clip, time, nullptr, &second) == kOfxStatOK;
    for (auto image : {first, second}) {
      if (!image) { valid = false; continue; }
      char* text = nullptr;
      valid = valid && gPropHost->propGetString(image, identifier ? kOfxImagePropUniqueIdentifier : kOfxImagePropField, 0, &text) == kOfxStatOK && text;
      if (valid) {
        const std::string actual(text);
        if (identifier) {
          valid = !actual.empty() && std::all_of(actual.begin(), actual.end(), [](unsigned char byte) { return byte >= 0x20 && byte <= 0x7e; });
          if (labels[index].empty()) { labels[index] = actual; }
          else { valid = valid && labels[index] == actual; }
        } else { valid = actual == kOfxImageFieldNone; }
      }
    }
    if (valid && identifier && index == 0) {
      void* data = nullptr;
      if (gPropHost->propGetPointer(first, kOfxImagePropData, 0, &data) != kOfxStatOK || !data) { valid = false; }
      else {
        const auto& pixel = *static_cast<const OfxRGBAColourF*>(data);
        imageAnalysis.try_emplace(labels[index], std::array<float, 3>{pixel.r, pixel.g, pixel.b});
      }
    }
    const bool releasedSecond = !second || gEffectHost->clipReleaseImage(second) == kOfxStatOK;
    const bool releasedFirst = gEffectHost->clipReleaseImage(first) == kOfxStatOK;
    if (!valid || !releasedFirst || !releasedSecond) { return false; }
  }
  if (identifier) {
    if (labels[0] == labels[1]) { return false; }
    capturedSourceIdentifier = labels[0];
    capturedOutputIdentifier = labels[1];
  }
  return true;
}

// Legal plugin-private analysis cache keyed solely by the host image UID.
// The exact small binary record survives host child/process replacement.
bool restoreImageAnalysis() {
  const char* path = std::getenv("MONDRIAN_OPENFX_IMAGE_ANALYSIS_FILE");
  if (!path || !*path) { return false; }
  std::ifstream file(path, std::ios::binary);
  if (!file) { return true; }
  uint32_t length = 0;
  file.read(reinterpret_cast<char*>(&length), sizeof(length));
  if (!file || length == 0 || length > 256) { return false; }
  std::string identifier(length, '\0');
  std::array<float,3> rgb{};
  file.read(identifier.data(), length);
  file.read(reinterpret_cast<char*>(rgb.data()), sizeof(rgb));
  if (!file || file.peek() != std::char_traits<char>::eof()) { return false; }
  imageAnalysis.try_emplace(identifier, rgb);
  return true;
}
bool saveImageAnalysis() {
  const char* path = std::getenv("MONDRIAN_OPENFX_IMAGE_ANALYSIS_FILE");
  const auto found = imageAnalysis.find(capturedSourceIdentifier);
  if (!path || !*path || found == imageAnalysis.end() || capturedSourceIdentifier.size() > 256) { return false; }
  std::ofstream file(path, std::ios::binary | std::ios::trunc);
  const auto length = static_cast<uint32_t>(capturedSourceIdentifier.size());
  file.write(reinterpret_cast<const char*>(&length), sizeof(length));
  file.write(capturedSourceIdentifier.data(), length);
  file.write(reinterpret_cast<const char*>(found->second.data()), sizeof(found->second));
  file.flush();
  return static_cast<bool>(file);
}

bool imageAnalysisOutput(const void* handle, OfxPropertySetHandle in) {
  const auto cached = imageAnalysis.find(capturedSourceIdentifier);
  if (cached == imageAnalysis.end()) { return false; }
  OfxImageClipHandle output = nullptr;
  OfxPropertySetHandle image = nullptr;
  OfxParamSetHandle parameters = nullptr;
  OfxParamHandle scale = nullptr;
  double time = 0.0, gain = 0.0;
  if (gPropHost->propGetDouble(in, kOfxPropTime, 0, &time) != kOfxStatOK ||
      gEffectHost->getParamSet((OfxImageEffectHandle)handle, &parameters) != kOfxStatOK ||
      gParamHost->paramGetHandle(parameters, "scale", &scale, nullptr) != kOfxStatOK ||
      gParamHost->paramGetValueAtTime(scale, time, &gain) != kOfxStatOK ||
      gEffectHost->clipGetHandle((OfxImageEffectHandle)handle, "Output", &output, nullptr) != kOfxStatOK ||
      gEffectHost->clipGetImage(output, time, nullptr, &image) != kOfxStatOK) { return false; }
  void* data = nullptr;
  bool valid = gPropHost->propGetPointer(image, kOfxImagePropData, 0, &data) == kOfxStatOK && data;
  if (valid) {
    auto& pixel = *static_cast<OfxRGBAColourF*>(data);
    pixel.r = cached->second[0] * static_cast<float>(gain);
    pixel.g = cached->second[1] * static_cast<float>(gain);
    pixel.b = cached->second[2] * static_cast<float>(gain);
  }
  return gEffectHost->clipReleaseImage(image) == kOfxStatOK && valid;
}

// Independent suite oracle: no production implementation is included here.
const OfxMultiThreadSuiteV1* threadSuite() {
  return static_cast<const OfxMultiThreadSuiteV1*>(gHost->fetchSuite(gHost->host, kOfxMultiThreadSuite, 1));
}

bool mutexContract() {
  const auto* suite = threadSuite();
  if (!suite) { return false; }
  OfxMutexHandle first = nullptr, second = nullptr;
  if (suite->mutexCreate(&first, 2) != kOfxStatOK || !first ||
      suite->mutexCreate(&second, 0) != kOfxStatOK || !second || first == second) { return false; }
  auto foreignTry = [&](OfxMutexHandle mutex) {
    OfxStatus result = kOfxStatErrUnknown;
    std::thread thread([&] {
      result = suite->mutexTryLock(mutex);
      if (result == kOfxStatOK && suite->mutexUnLock(mutex) != kOfxStatOK) { result = kOfxStatFailed; }
    });
    thread.join();
    return result;
  };
  if (foreignTry(first) != kOfxStatFailed || foreignTry(second) != kOfxStatOK ||
      suite->mutexLock(first) != kOfxStatOK || suite->mutexTryLock(first) != kOfxStatOK) { return false; }
  // The creator started with two locks; successful recursive lock and trylock
  // add two. Failed foreign trylock cannot change that depth.
  for (int remaining = 3; remaining > 0; --remaining) {
    if (suite->mutexUnLock(first) != kOfxStatOK || foreignTry(first) != kOfxStatFailed) { return false; }
  }
  if (suite->mutexUnLock(first) != kOfxStatOK || foreignTry(first) != kOfxStatOK) { return false; }

  // Self-created plugin threads require real exclusion even when NumCPUs=1.
  std::atomic<int> counter{0};
  std::atomic<bool> valid{true};
  auto increments = [&] {
    for (int index = 0; index < 1000; ++index) {
      if (suite->mutexLock(second) != kOfxStatOK) { valid = false; return; }
      const int before = counter.load(std::memory_order_relaxed);
      std::this_thread::yield();
      counter.store(before + 1, std::memory_order_relaxed);
      if (suite->mutexUnLock(second) != kOfxStatOK) { valid = false; return; }
    }
  };
  std::thread one(increments), two(increments);
  one.join(); two.join();
  if (!valid || counter.load(std::memory_order_relaxed) != 2000) { return false; }
  const auto retired = first;
  if (suite->mutexDestroy(first) != kOfxStatOK || suite->mutexDestroy(second) != kOfxStatOK ||
      suite->mutexDestroy(retired) != kOfxStatErrBadHandle ||
      suite->mutexLock(retired) != kOfxStatErrBadHandle ||
      suite->mutexTryLock(retired) != kOfxStatErrBadHandle ||
      suite->mutexUnLock(retired) != kOfxStatErrBadHandle) { return false; }
  // Reuse all slot positions repeatedly; a stale generation must never bind to
  // a replacement, and no tombstone heap can grow per create/destroy cycle.
  for (int iteration = 0; iteration < 2048; ++iteration) {
    OfxMutexHandle replacement = nullptr;
    if (suite->mutexCreate(&replacement, 0) != kOfxStatOK || !replacement || replacement == retired ||
        suite->mutexTryLock(retired) != kOfxStatErrBadHandle ||
        suite->mutexDestroy(replacement) != kOfxStatOK) { return false; }
  }
  return true;
}

bool negativeMutexContract() {
  const auto* suite = threadSuite();
  if (!suite) { return false; }
  for (int initial : {-3, std::numeric_limits<int>::min()}) {
    OfxMutexHandle mutex = nullptr;
    if (suite->mutexCreate(&mutex, initial) != kOfxStatOK || !mutex ||
        suite->mutexLock(mutex) != kOfxStatOK) { return false; }
    OfxStatus competing = kOfxStatErrUnknown;
    std::thread thread([&] { competing = suite->mutexTryLock(mutex); });
    thread.join();
    if (competing != kOfxStatFailed || suite->mutexUnLock(mutex) != kOfxStatOK ||
        suite->mutexTryLock(mutex) != kOfxStatOK || suite->mutexUnLock(mutex) != kOfxStatOK ||
        suite->mutexDestroy(mutex) != kOfxStatOK) { return false; }
  }
  // Large positive initial counts must not cause INT_MAX repeated native lock
  // operations or an unbounded allocation before the mutex is usable.
  OfxMutexHandle large = nullptr;
  if (suite->mutexCreate(&large, std::numeric_limits<int>::max()) != kOfxStatOK || !large) { return false; }
  OfxStatus competing = kOfxStatErrUnknown;
  std::thread thread([&] { competing = suite->mutexTryLock(large); });
  thread.join();
  return competing == kOfxStatFailed && suite->mutexDestroy(large) == kOfxStatOK;
}

bool mutexLifecycleContract() {
  const auto* suite = threadSuite();
  if (!suite) { return false; }
  OfxMutexHandle mutex = nullptr;
  if (suite->mutexCreate(&mutex, 1) != kOfxStatOK || !mutex) { return false; }
  std::atomic<bool> submitted{false};
  OfxStatus result = kOfxStatErrUnknown;
  std::thread waiter([&] {
    submitted = true;
    result = suite->mutexLock(mutex);
    if (result == kOfxStatOK) { suite->mutexUnLock(mutex); }
  });
  while (!submitted) { std::this_thread::yield(); }
  const auto destroyed = suite->mutexDestroy(mutex);
  waiter.join();
  return destroyed == kOfxStatOK && result == kOfxStatErrBadHandle;
}

struct ThreadProbe {
  const OfxMultiThreadSuiteV1* suite;
  std::thread::id caller;
  OfxParamHandle parameter;
  double time, value;
  std::atomic<unsigned> calls{0};
  std::atomic<bool> valid{true};
  unsigned expected;
};
void nestedNoop(unsigned, unsigned, void*) {}
void suiteCallback(unsigned index, unsigned maximum, void* opaque) {
  auto& probe = *static_cast<ThreadProbe*>(opaque);
  unsigned reported = std::numeric_limits<unsigned>::max();
  double value = 0.0;
  if (index >= maximum || maximum != probe.expected || std::this_thread::get_id() == probe.caller ||
      probe.suite->multiThreadIndex(&reported) != kOfxStatOK || reported != index ||
      probe.suite->multiThreadIsSpawnedThread() != 1 ||
      gParamHost->paramGetValueAtTime(probe.parameter, probe.time, &value) != kOfxStatOK || value != probe.value ||
      probe.suite->multiThread(nestedNoop, 1, nullptr) != kOfxStatErrExists) { probe.valid = false; }
  // A plugin-created thread is not mislabeled as a suite-spawned thread, even
  // while the callback is active. Thread-local context cannot leak across it.
  std::thread foreign([&] {
    unsigned foreignIndex = 999;
    if (probe.suite->multiThreadIsSpawnedThread() != 0 ||
        probe.suite->multiThreadIndex(&foreignIndex) != kOfxStatOK || foreignIndex != 0) { probe.valid = false; }
  });
  foreign.join();
  ++probe.calls;
}
bool threadContract(const void* handle, OfxPropertySetHandle arguments) {
  const auto* suite = threadSuite();
  if (!suite) { return false; }
  OfxParamSetHandle parameters = nullptr;
  OfxParamHandle parameter = nullptr;
  double time = 0.0, value = 0.0;
  if (gEffectHost->getParamSet((OfxImageEffectHandle)handle, &parameters) != kOfxStatOK ||
      gParamHost->paramGetHandle(parameters, "scale", &parameter, nullptr) != kOfxStatOK ||
      gPropHost->propGetDouble(arguments, kOfxPropTime, 0, &time) != kOfxStatOK ||
      gParamHost->paramGetValueAtTime(parameter, time, &value) != kOfxStatOK) { return false; }
  unsigned cpus = 0, index = 999;
  if (suite->multiThreadNumCPUs(&cpus) != kOfxStatOK || cpus != 1 ||
      suite->multiThreadIndex(&index) != kOfxStatOK || index != 0 ||
      suite->multiThreadIsSpawnedThread() != 0) { return false; }
  for (unsigned request : {1U, std::numeric_limits<unsigned>::max()}) {
    ThreadProbe probe{suite, std::this_thread::get_id(), parameter, time, value, 0, true, cpus};
    if (suite->multiThread(suiteCallback, request, &probe) != kOfxStatOK || !probe.valid ||
        probe.calls != cpus || suite->multiThreadIsSpawnedThread() != 0) { return false; }
  }
  return true;
}

struct ConcurrentThreadProbe {
  std::atomic<bool> firstCallbackEnded{false}, firstCallerReturned{false}, abort{false};
  std::atomic<unsigned> secondCalls{0};
};
void firstConcurrentCallback(unsigned, unsigned, void* opaque) {
  auto& probe = *static_cast<ConcurrentThreadProbe*>(opaque);
  probe.firstCallbackEnded.store(true, std::memory_order_release);
}
void secondConcurrentCallback(unsigned, unsigned, void* opaque) {
  auto& probe = *static_cast<ConcurrentThreadProbe*>(opaque);
  // This dependency is legal once the first accepted batch has completed. Its
  // caller must consume its own completion before another accepted batch can
  // overwrite that result. No callback throws and no sleep widens a threshold.
  while (!probe.firstCallerReturned.load(std::memory_order_acquire)) { std::this_thread::yield(); }
  ++probe.secondCalls;
}
bool concurrentThreadContract() {
  const auto* suite = threadSuite();
  if (!suite) { return false; }
  // Scheduling determines whether the old result-publication race triggers in
  // one round. Repetition is a stress control, not a deterministic RED claim.
  for (int round = 0; round < 64; ++round) {
    ConcurrentThreadProbe probe;
    OfxStatus secondResult = kOfxStatErrUnknown;
    std::thread second([&] {
      while (!probe.firstCallbackEnded.load(std::memory_order_acquire) &&
             !probe.abort.load(std::memory_order_acquire)) { std::this_thread::yield(); }
      if (probe.abort.load(std::memory_order_acquire)) { return; }
      do {
        secondResult = suite->multiThread(secondConcurrentCallback, 1, &probe);
        if (secondResult == kOfxStatFailed) { std::this_thread::yield(); }
      } while (secondResult == kOfxStatFailed && !probe.abort.load(std::memory_order_acquire));
    });
    const auto firstResult = suite->multiThread(firstConcurrentCallback, 1, &probe);
    if (firstResult != kOfxStatOK) { probe.abort.store(true, std::memory_order_release); }
    probe.firstCallerReturned.store(true, std::memory_order_release);
    second.join();
    if (firstResult != kOfxStatOK || secondResult != kOfxStatOK || probe.secondCalls != 1) { return false; }
  }
  return true;
}

bool mutexCapacityContract() {
  const auto* suite = threadSuite();
  if (!suite) { return false; }
  std::array<OfxMutexHandle, 1024> handles{};
  for (auto& handle : handles) {
    if (suite->mutexCreate(&handle, 0) != kOfxStatOK || !handle) { return false; }
  }
  OfxMutexHandle overflow = nullptr;
  if (suite->mutexCreate(&overflow, 0) != kOfxStatErrMemory || overflow != nullptr) { return false; }
  for (auto handle : handles) { if (suite->mutexDestroy(handle) != kOfxStatOK) { return false; } }
  return true;
}

// Independent test-only observation of the actual plugin worker and Source image.
bool appendResidentOwnerReceipt(const void* handle, OfxPropertySetHandle in) {
  const char* path = std::getenv("MONDRIAN_OPENFX_PROCESSOR_OWNER_RECEIPT");
  double time = 0.0;
  OfxImageClipHandle source = nullptr;
  OfxPropertySetHandle image = nullptr;
  if (!path || !*path ||
      gPropHost->propGetDouble(in, kOfxPropTime, 0, &time) != kOfxStatOK ||
      gEffectHost->clipGetHandle((OfxImageEffectHandle)handle, "Source", &source, nullptr) != kOfxStatOK ||
      gEffectHost->clipGetImage(source, time, nullptr, &image) != kOfxStatOK) { return false; }
  char* identifier = nullptr;
  bool valid = gPropHost->propGetString(image, kOfxImagePropUniqueIdentifier, 0, &identifier) == kOfxStatOK &&
      identifier && *identifier;
  if (valid) {
    // u8path supplies the actual UTF-8 fixture path on Windows as well as POSIX.
    std::ofstream file(std::filesystem::u8path(path), std::ios::binary | std::ios::app);
#ifdef _WIN32
    const auto process = _getpid();
#else
    const auto process = getpid();
#endif
    file << process << '\t' << identifier << '\n';
    file.flush();
    valid = process > 0 && file.good();
  }
  const auto released = gEffectHost->clipReleaseImage(image);
  return released == kOfxStatOK && valid;
}

// Independent legal suite/status oracles, called by the owned SessionProbe.
// All callback handles are SDK handles supplied by the actual host.
const OfxMessageSuiteV2* auxMessageSuite() {
  return static_cast<const OfxMessageSuiteV2*>(gHost->fetchSuite(gHost->host, kOfxMessageSuite, 2));
}
const OfxProgressSuiteV2* auxProgressSuite() {
  return static_cast<const OfxProgressSuiteV2*>(gHost->fetchSuite(gHost->host, kOfxProgressSuite, 2));
}
const OfxTimeLineSuiteV1* auxTimelineSuite() {
  return static_cast<const OfxTimeLineSuiteV1*>(gHost->fetchSuite(gHost->host, kOfxTimeLineSuite, 1));
}
// Default-off, fixture-only action receipt. Fixed ASCII rows, <=32 per PID.
// ENV presence identifies the actual helper role, not an emitter/request ID.
void describeActionReceipt(const char* action, const char* point,
                           unsigned attempted = 0, unsigned accepted = 0,
                           int status = 0) noexcept {
  const char* directory = std::getenv("MONDRIAN_OPENFX_DESCRIBE_ACTION_RECEIPT_DIRECTORY");
  if (!directory || !*directory) { return; }
  static std::mutex gate;
  static std::atomic<bool> incomplete{false};
  try {
    std::lock_guard<std::mutex> lock(gate);
#ifdef _WIN32
    const auto pid = _getpid();
#else
    const auto pid = getpid();
#endif
    const auto path = std::filesystem::u8path(directory) / ("actions-" + std::to_string(pid) + ".txt");
    std::ifstream previous(path, std::ios::binary);
    if (std::filesystem::exists(path) && !previous.is_open()) { incomplete = true; }
    unsigned lines = 0, bytes = 0;
    char byte;
    while (previous.get(byte)) {
      if (++bytes > 4096) { incomplete = true; return; }
      if (byte == '\n') { ++lines; }
    }
    if (previous.bad()) { incomplete = true; }
    previous.close();
    if (lines >= 32) { return; }
    if (lines == 31) { incomplete = true; action = "overflow"; point = "incomplete"; }
    std::ofstream file(path, std::ios::binary | std::ios::app);
    file << lines + 1 << " D=" << (std::getenv("MONDRIAN_INTERNAL_OPENFX_DESCRIBE_REQUEST") != nullptr)
         << " I=" << (std::getenv("MONDRIAN_INTERNAL_OPENFX_DISCOVERY_REQUEST") != nullptr)
         << ' ' << action << ' ' << point << " attempted=" << attempted << " accepted=" << accepted
         << " status=" << status << " complete=" << !incomplete << '\n';
    file.flush();
    if (!file) { incomplete = true; }
  } catch (...) { incomplete = true; }
}

// The disconnected clip has no actual source format. Observe through the real
// Property/Image suites during Render, after the ordinary clip-preference action.
bool optionalUnmappedFormat(const void* handle, OfxPropertySetHandle in) {
  OfxImageClipHandle optional = nullptr;
  OfxPropertySetHandle properties = nullptr;
  double time = 0.0;
  if (gEffectHost->clipGetHandle((OfxImageEffectHandle)handle, "DisconnectedOptional", &optional,
                                &properties) != kOfxStatOK || !optional || !properties ||
      gPropHost->propGetDouble(in, kOfxPropTime, 0, &time) != kOfxStatOK) { return false; }
  const auto stringIs = [](OfxPropertySetHandle set, const char* name, const char* expected) {
    char* value = nullptr;
    int dimension = 0;
    return gPropHost->propGetDimension(set, name, &dimension) == kOfxStatOK && dimension == 1 &&
        gPropHost->propGetString(set, name, 0, &value) == kOfxStatOK && value &&
        std::strcmp(value, expected) == 0;
  };
  const auto record = [](const char* point, bool valid, OfxStatus status = kOfxStatOK) {
    describeActionReceipt("optional-unmapped", point, 1, valid ? 1 : 0, status);
    return valid;
  };
  int connected = -1;
  bool valid = record("connected-zero",
      gPropHost->propGetInt(properties, kOfxImageClipPropConnected, 0, &connected) == kOfxStatOK && connected == 0);
  valid = record("unmapped-depth-none",
      stringIs(properties, kOfxImageClipPropUnmappedPixelDepth, kOfxBitDepthNone)) && valid;
  valid = record("unmapped-components-none",
      stringIs(properties, kOfxImageClipPropUnmappedComponents, kOfxImageComponentNone)) && valid;
  OfxPropertySetHandle image = nullptr;
  const auto fetched = gEffectHost->clipGetImage(optional, time, nullptr, &image);
  valid = record("image-unavailable", fetched == kOfxStatFailed && !image, fetched) && valid;
  if (fetched == kOfxStatOK && image && gEffectHost->clipReleaseImage(image) != kOfxStatOK) { return false; }
  for (const char* name : {kOfxImageEffectSimpleSourceClipName, kOfxImageEffectOutputClipName}) {
    OfxImageClipHandle clip = nullptr;
    properties = nullptr;
    connected = -1;
    const bool format = gEffectHost->clipGetHandle((OfxImageEffectHandle)handle, name, &clip,
                                                  &properties) == kOfxStatOK && clip && properties &&
        gPropHost->propGetInt(properties, kOfxImageClipPropConnected, 0, &connected) == kOfxStatOK && connected == 1 &&
        stringIs(properties, kOfxImageEffectPropPixelDepth, kOfxBitDepthFloat) &&
        stringIs(properties, kOfxImageEffectPropComponents, kOfxImageComponentRGBA) &&
        stringIs(properties, kOfxImageClipPropUnmappedPixelDepth, kOfxBitDepthFloat) &&
        stringIs(properties, kOfxImageClipPropUnmappedComponents, kOfxImageComponentRGBA);
    valid = record(name, format) && valid;
  }
  return valid;
}

// Fixed-canvas oracle only: time selects independent output, never invocation
// history. Rust checks separate literal tables in top-down storage order.
OfxStatus fixedCanvasPartialOutput(const void* handle, OfxPropertySetHandle in) {
  OfxPropertySetHandle sourceImage = nullptr, outputImage = nullptr, repeatedImage = nullptr;
  struct View { void* data = nullptr; int rowBytes = 0; } source, output, repeated;
  unsigned sourceAttempts = 0, sourceAccepted = 0, outputAttempts = 0, outputAccepted = 0;
  int phase = -1;
  const bool valid = [&]() {
    double time = -1.0, renderScale[2]{};
    if (!honestHostCapabilities(gHost) || !canonicalProgressiveContract(handle, in) ||
        gPropHost->propGetDoubleN(in, kOfxImageEffectPropRenderScale, 2, renderScale) != kOfxStatOK ||
        renderScale[0] != 1.0 || renderScale[1] != 1.0 ||
        gPropHost->propGetDouble(in, kOfxPropTime, 0, &time) != kOfxStatOK ||
        (time != 0.0 && time != 1.0 && time != 2.0)) { return false; }
    phase = static_cast<int>(time);
    OfxImageClipHandle clips[2]{};
    for (int index = 0; index < 2; ++index) {
      OfxPropertySetHandle properties = nullptr;
      OfxRectD rod{};
      char* association = nullptr;
      if (gEffectHost->clipGetHandle((OfxImageEffectHandle)handle,
              index == 0 ? "Source" : "Output", &clips[index], &properties) != kOfxStatOK ||
          gEffectHost->clipGetRegionOfDefinition(clips[index], time, &rod) != kOfxStatOK ||
          rod.x1 != 0.0 || rod.y1 != 0.0 || rod.x2 != 6.0 || rod.y2 != 5.0 ||
          gPropHost->propGetString(properties, kOfxImageEffectPropPreMultiplication,
              0, &association) != kOfxStatOK || !association ||
          std::strcmp(association, kOfxImageUnPreMultiplied) != 0) { return false; }
    }
    const auto fetch = [&](OfxImageClipHandle clip, OfxPropertySetHandle& image,
                           View& view, unsigned& attempts, unsigned& accepted) {
      ++attempts;
      if (gEffectHost->clipGetImage(clip, time, nullptr, &image) != kOfxStatOK || !image) { return false; }
      ++accepted;
      int bounds[4]{}, rod[4]{};
      double par = 0.0, imageScale[2]{};
      char *depth = nullptr, *components = nullptr, *association = nullptr, *field = nullptr;
      if (gPropHost->propGetIntN(image, kOfxImagePropBounds, 4, bounds) != kOfxStatOK ||
          gPropHost->propGetIntN(image, kOfxImagePropRegionOfDefinition, 4, rod) != kOfxStatOK ||
          gPropHost->propGetDouble(image, kOfxImagePropPixelAspectRatio, 0, &par) != kOfxStatOK || par != 2.0 ||
          gPropHost->propGetDoubleN(image, kOfxImageEffectPropRenderScale, 2, imageScale) != kOfxStatOK ||
          imageScale[0] != 1.0 || imageScale[1] != 1.0 ||
          gPropHost->propGetPointer(image, kOfxImagePropData, 0, &view.data) != kOfxStatOK || !view.data ||
          gPropHost->propGetInt(image, kOfxImagePropRowBytes, 0, &view.rowBytes) != kOfxStatOK ||
          view.rowBytes != -static_cast<int>(3 * sizeof(OfxRGBAColourF)) ||
          reinterpret_cast<std::uintptr_t>(view.data) % alignof(OfxRGBAColourF) != 0 ||
          gPropHost->propGetString(image, kOfxImageEffectPropPixelDepth, 0, &depth) != kOfxStatOK ||
          !depth || std::strcmp(depth, kOfxBitDepthFloat) != 0 ||
          gPropHost->propGetString(image, kOfxImageEffectPropComponents, 0, &components) != kOfxStatOK ||
          !components || std::strcmp(components, kOfxImageComponentRGBA) != 0 ||
          gPropHost->propGetString(image, kOfxImageEffectPropPreMultiplication, 0, &association) != kOfxStatOK ||
          !association || std::strcmp(association, kOfxImageUnPreMultiplied) != 0 ||
          gPropHost->propGetString(image, kOfxImagePropField, 0, &field) != kOfxStatOK ||
          !field || std::strcmp(field, kOfxImageFieldNone) != 0) { return false; }
      const int expected[4] = {0, 0, 3, 5};
      for (int index = 0; index < 4; ++index) {
        if (bounds[index] != expected[index] || rod[index] != expected[index]) { return false; }
      }
      return true;
    };
    if (!fetch(clips[0], sourceImage, source, sourceAttempts, sourceAccepted) ||
        !fetch(clips[1], outputImage, output, outputAttempts, outputAccepted)) { return false; }
    const auto pixel = [](const View& view, int x, int y) -> OfxRGBAColourF& {
      return reinterpret_cast<OfxRGBAColourF*>(static_cast<char*>(view.data) + y * view.rowBytes)[x];
    };
    const auto expected = [&](int x, int y) {
      if (phase == 0) {
        return OfxRGBAColourF{-0.5f + x / 8.0f + y / 16.0f,
            2.0f + x / 4.0f + y / 8.0f, 0.25f + x / 16.0f - y / 32.0f, 0.5f};
      }
      if (phase == 1 && x >= 1 && y >= 1 && y < 3) {
        return OfxRGBAColourF{1.5f + x / 8.0f, -0.25f - y / 16.0f,
            0.5f + x / 32.0f + y / 64.0f, 0.25f};
      }
      return OfxRGBAColourF{0, 0, 0, 0};
    };
    for (int y = 0; y < 5; ++y) {
      for (int x = 0; x < 3; ++x) {
        if (phase == 0 || (phase == 1 && x >= 1 && y >= 1 && y < 3)) {
          pixel(output, x, y) = expected(x, y);
        }
      }
    }
    // A second image fetch must not clear pixels already written this action.
    // Phase two really fetches/releases Output but never writes a pixel.
    if (phase != 2 &&
        !fetch(clips[1], repeatedImage, repeated, outputAttempts, outputAccepted)) { return false; }
    const View& observed = phase == 2 ? output : repeated;
    for (int y = 0; y < 5; ++y) {
      for (int x = 0; x < 3; ++x) {
        const auto actual = pixel(observed, x, y);
        const auto wanted = expected(x, y);
        if (actual.r != wanted.r || actual.g != wanted.g ||
            actual.b != wanted.b || actual.a != wanted.a) { return false; }
      }
    }
    return true;
  }();
  bool released = true;
  for (auto image : {repeatedImage, outputImage, sourceImage}) {
    if (image && gEffectHost->clipReleaseImage(image) != kOfxStatOK) { released = false; }
  }
  const auto status = valid && released ? kOfxStatOK : kOfxStatFailed;
  const char* names[] = {"fixed-canvas-0", "fixed-canvas-1", "fixed-canvas-2"};
  const char* name = phase >= 0 ? names[phase] : "fixed-canvas-invalid";
  describeActionReceipt(name, "source-fetch", sourceAttempts, sourceAccepted, status);
  describeActionReceipt(name, "output-fetch", outputAttempts, outputAccepted, status);
  return status;
}

bool auxTransientMessages(void* handle) {
  const auto* v1 = static_cast<const OfxMessageSuiteV1*>(gHost->fetchSuite(gHost->host, kOfxMessageSuite, 1));
  const auto* v2 = auxMessageSuite();
  if (!v1 || !v2) { return false; }
  unsigned attempted = 0, accepted = 0;
  const auto finish = [&](bool result) {
    describeActionReceipt("transient", "batch", attempted, accepted);
    return result;
  };
  for (const char* type : {kOfxMessageMessage, kOfxMessageWarning, kOfxMessageLog}) {
    const auto first = v1->message(handle, type, nullptr, "%s %d %.3f", u8"说明🙂", 7, 1.25);
    ++attempted;
    accepted += first == kOfxStatOK;
    if (first != kOfxStatOK) { return finish(false); }
    const auto second = v2->message(handle, type, "", "%s", u8"第二条通知✓");
    ++attempted;
    accepted += second == kOfxStatOK;
    if (second != kOfxStatOK) { return finish(false); }
  }
  return finish(true);
}

bool auxQuestionRefusal(void* handle) {
  const auto* messages = auxMessageSuite();
  OfxPropertySetHandle properties = nullptr;
  int interactive = 1;
  return messages && gEffectHost->getPropertySet((OfxImageEffectHandle)handle, &properties) == kOfxStatOK &&
      gPropHost->propGetInt(properties, kOfxPropIsInteractive, 0, &interactive) == kOfxStatOK && interactive == 0 &&
      messages->message(handle, kOfxMessageQuestion, "noninteractive.question", "%s", "Continue?") == kOfxStatFailed;
}
bool auxInvalidHandles(void* descriptor) {
  const auto* messages = auxMessageSuite();
  const auto* progress = auxProgressSuite();
  const auto* timeline = auxTimelineSuite();
  if (!messages || !progress || !timeline) { return false; }
  OfxParamSetHandle parameters = nullptr;
  if (gEffectHost->getParamSet((OfxImageEffectHandle)descriptor, &parameters) != kOfxStatOK || !parameters) { return false; }
  // A real descriptor is a valid transient target and an invalid persistent,
  // Progress/Timeline instance target. A real ParameterSet is another live
  // object; neither it nor a rubbish token may be dereferenced as an Instance.
  for (void* handle : std::array<void*, 4>{nullptr, descriptor, static_cast<void*>(parameters), reinterpret_cast<void*>(uintptr_t(1))}) {
    double time = 31.0, first = 41.0, last = 59.0;
    if (messages->setPersistentMessage(handle, kOfxMessageWarning, nullptr, "%s", "not an instance") != kOfxStatErrBadHandle ||
        messages->clearPersistentMessage(handle) != kOfxStatErrBadHandle ||
        progress->progressStart(handle, "not an instance", nullptr) != kOfxStatErrBadHandle ||
        progress->progressUpdate(handle, 0.5) != kOfxStatErrBadHandle ||
        progress->progressEnd(handle) != kOfxStatErrBadHandle ||
        timeline->getTime(handle, &time) != kOfxStatErrBadHandle || time != 31.0 ||
        timeline->getTimeBounds(handle, &first, &last) != kOfxStatErrBadHandle || first != 41.0 || last != 59.0 ||
        timeline->gotoTime(handle, 0.0) != kOfxStatErrBadHandle) { return false; }
  }
  return true;
}

bool auxPersistentWarning(void* handle) {
  const auto* messages = auxMessageSuite();
  return messages &&
      messages->setPersistentMessage(handle, kOfxMessageWarning, nullptr, "%s", u8"可继续的警告🙂") == kOfxStatOK &&
      messages->clearPersistentMessage(handle) == kOfxStatOK &&
      messages->setPersistentMessage(handle, kOfxMessageMessage, "information", "%s", "Information continues") == kOfxStatOK &&
      messages->clearPersistentMessage(handle) == kOfxStatOK;
}
bool auxDestroyTarget(void* handle) {
  const auto* messages = auxMessageSuite();
  if (!messages) { return false; }
  // DestroyInstance is a legal actual instance action. A bounded consumer may
  // refuse posting during teardown; it may not treat the still-live handle as
  // rubbish or call a destroyed Filter virtual object.
  const auto status = messages->message(handle, kOfxMessageMessage, nullptr, "%s", "DestroyInstance callback");
  return status == kOfxStatOK || status == kOfxStatFailed;
}
bool auxiliaryErrorPending = false;
int auxiliaryErrorRenderCount = 0;
bool auxiliaryScale(void* handle, double& value) {
  OfxParamSetHandle parameters = nullptr;
  OfxParamHandle scale = nullptr;
  return gEffectHost->getParamSet((OfxImageEffectHandle)handle, &parameters) == kOfxStatOK &&
      gParamHost->paramGetHandle(parameters, "scale", &scale, nullptr) == kOfxStatOK &&
      gParamHost->paramGetValue(scale, &value) == kOfxStatOK;
}
bool auxPersistentError(void* handle, const std::string& action, OfxPropertySetHandle arguments) {
  const auto* messages = auxMessageSuite();
  if (!messages) { return false; }
  if (action == kOfxActionCreateInstance) {
    auxiliaryErrorRenderCount = 0;
    double value = 0.0;
    if (!auxiliaryScale(handle, value)) { return false; }
    // A genuinely new owner prepared from repaired author state is already
    // valid. It does not fabricate a clear of an evicted owner's private Error.
    if (value == 2.0) { auxiliaryErrorPending = false; return true; }
    auxiliaryErrorPending = true;
    return messages->setPersistentMessage(handle, kOfxMessageError, "user.repair", "%s", "Change scale to recover") == kOfxStatOK;
  }
  if (action == kOfxActionInstanceChanged) {
    char* name = nullptr;
    char* type = nullptr;
    if (gPropHost->propGetString(arguments, kOfxPropType, 0, &type) != kOfxStatOK || !type ||
        std::string(type) != kOfxTypeParameter) { return true; }
    if (gPropHost->propGetString(arguments, kOfxPropName, 0, &name) != kOfxStatOK || !name ||
        std::string(name) != "scale") { return true; }
    double value = 0.0;
    if (!auxiliaryScale(handle, value)) { return false; }
    if (value == 2.0) {
      if (messages->clearPersistentMessage(handle) != kOfxStatOK) { return false; }
      auxiliaryErrorPending = false;
    }
  }
  return true;
}
bool auxProgressLifetime(void* handle) {
  const auto* v1 = static_cast<const OfxProgressSuiteV1*>(gHost->fetchSuite(gHost->host, kOfxProgressSuite, 1));
  const auto* v2 = auxProgressSuite();
  if (!v1 || !v2) { return false; }
  if (v2->progressUpdate(handle, 0.0) != kOfxStatErrBadHandle ||
      v2->progressEnd(handle) != kOfxStatErrBadHandle ||
      v1->progressStart(handle, u8"分析🙂") != kOfxStatOK ||
      v2->progressStart(handle, "already active", nullptr) != kOfxStatFailed) { return false; }
  for (double value : {0.0, 0.5, 1.0}) {
    if (v2->progressUpdate(handle, value) != kOfxStatOK) { return false; }
  }
  for (double value : {-0.125, 1.125, std::numeric_limits<double>::infinity(), std::numeric_limits<double>::quiet_NaN()}) {
    if (v1->progressUpdate(handle, value) != kOfxStatErrValue) { return false; }
  }
  return v1->progressEnd(handle) == kOfxStatOK &&
      v2->progressUpdate(handle, 0.5) == kOfxStatErrBadHandle &&
      v2->progressEnd(handle) == kOfxStatErrBadHandle &&
      v2->progressStart(handle, "second task", "optional.progress.id") == kOfxStatOK &&
      v2->progressEnd(handle) == kOfxStatOK;
}
bool auxTimelineReadAndRefusal(void* handle, OfxPropertySetHandle arguments) {
  const auto* timeline = auxTimelineSuite();
  double time = 0.0, expected = 0.0, first = 0.0, last = 0.0;
  if (!timeline || gPropHost->propGetDouble(arguments, kOfxPropTime, 0, &expected) != kOfxStatOK ||
      timeline->getTime(handle, &time) != kOfxStatOK || time != expected ||
      timeline->getTimeBounds(handle, &first, &last) != kOfxStatOK || first != 0.0 || last != 100.0 ||
      timeline->gotoTime(handle, std::numeric_limits<double>::quiet_NaN()) != kOfxStatErrValue ||
      timeline->gotoTime(handle, std::numeric_limits<double>::infinity()) != kOfxStatErrValue ||
      timeline->gotoTime(handle, time + 1.0) != kOfxStatFailed) { return false; }
  double unchanged = 0.0;
  return timeline->getTime(handle, &unchanged) == kOfxStatOK && unchanged == expected &&
      timeline->getTime(handle, nullptr) == kOfxStatErrValue &&
      timeline->getTimeBounds(handle, nullptr, &last) == kOfxStatErrValue &&
      timeline->getTimeBounds(handle, &first, nullptr) == kOfxStatErrValue;
}

// Independent observation of a real in-flight native abort callback. The
// supervisor requests cancellation only after the first complete receipt.
bool cooperativeReceipt(const void* handle, OfxPropertySetHandle in, const char* environment) {
  const char* path = std::getenv(environment);
  double time = 0.0;
  OfxImageClipHandle source = nullptr;
  OfxPropertySetHandle image = nullptr;
  if (!path || !*path ||
      gPropHost->propGetDouble(in, kOfxPropTime, 0, &time) != kOfxStatOK ||
      gEffectHost->clipGetHandle((OfxImageEffectHandle)handle, "Source", &source, nullptr) != kOfxStatOK ||
      gEffectHost->clipGetImage(source, time, nullptr, &image) != kOfxStatOK) { return false; }
  char* identifier = nullptr;
  bool valid = gPropHost->propGetString(image, kOfxImagePropUniqueIdentifier, 0, &identifier) == kOfxStatOK && identifier;
  if (valid) {
    std::ofstream output(std::filesystem::u8path(path), std::ios::binary | std::ios::app);
#ifdef _WIN32
    const auto process = _getpid();
#else
    const auto process = getpid();
#endif
    output << process << '\t' << identifier << '\n';
    output.flush();
    valid = process > 0 && output.good();
  }
  const auto released = gEffectHost->clipReleaseImage(image);
  return released == kOfxStatOK && valid;
}
OfxStatus cooperativeProgressRender(const void* handle, OfxPropertySetHandle in) {
  const auto* progress = static_cast<const OfxProgressSuiteV2*>(gHost->fetchSuite(gHost->host, kOfxProgressSuite, 2));
  const char* release = std::getenv("MONDRIAN_OPENFX_COOPERATIVE_RELEASE");
  if (!progress || !release || !*release ||
      !cooperativeReceipt(handle, in, "MONDRIAN_OPENFX_COOPERATIVE_STARTED") ||
      progress->progressStart(const_cast<void*>(handle), "Live native render", "live-native") != kOfxStatOK ||
      progress->progressUpdate(const_cast<void*>(handle), 0.375) != kOfxStatOK) { return kOfxStatFailed; }
  while (!std::filesystem::exists(std::filesystem::u8path(release))) {
    if (gEffectHost->abort((OfxImageEffectHandle)handle)) { return kOfxStatFailed; }
    std::this_thread::yield();
  }
  // Only the actual parent live observation releases this in-flight action.
  // No timeout/sleep automatically completes it, and no output is fabricated.
  return progress->progressEnd(const_cast<void*>(handle));
}

OfxStatus cooperativeAbortRender(const void* handle, OfxPropertySetHandle in) {
  if (!gEffectHost->abort ||
      !cooperativeReceipt(handle, in, "MONDRIAN_OPENFX_COOPERATIVE_STARTED")) { return kOfxStatFailed; }
  const auto* progress = auxProgressSuite();
  if (!progress || progress->progressStart(const_cast<void*>(handle), "Cancelable native render", "cancel-native") != kOfxStatOK) { return kOfxStatFailed; }
  while (gEffectHost->abort((OfxImageEffectHandle)handle) == 0) {
    const auto status = progress->progressUpdate(const_cast<void*>(handle), 0.25);
    if (status != kOfxStatOK && status != kOfxStatReplyNo) { return kOfxStatFailed; }
    std::this_thread::yield();
  }
  if (progress->progressUpdate(const_cast<void*>(handle), 0.25) != kOfxStatReplyNo ||
      progress->progressEnd(const_cast<void*>(handle)) != kOfxStatOK) { return kOfxStatFailed; }
  if (!cooperativeReceipt(handle, in, "MONDRIAN_OPENFX_COOPERATIVE_ABORTED")) { return kOfxStatFailed; }
  // Rendering genuinely stopped, so no output is supplied. The supervisor's
  // exact current cancellation has precedence over this native failed status.
  return kOfxStatFailed;
}


// Independent actual native action receipts, not host-derived render counts.
bool constructorActionReceipt(const char* environment) {
  const char* path = std::getenv(environment);
  if (!path || !*path) { return false; }
  std::ofstream file(std::filesystem::u8path(path), std::ios::binary | std::ios::app);
#ifdef _WIN32
  const auto process = _getpid();
#else
  const auto process = getpid();
#endif
  file << process << '\n';
  file.flush();
  return process > 0 && file.good();
}

bool blockCatalogCallback(const OfxHost* host) {
  if (!honestHostCapabilities(host) ||
      !constructorActionReceipt("MONDRIAN_OPENFX_CATALOG_NATIVE_STARTED")) {
    return false;
  }
  // The host must terminate this actual callback's isolated native process.
  // No timer or fabricated output completes the callback automatically.
  for (;;) { std::this_thread::sleep_for(std::chrono::milliseconds(5)); }
}

struct GenericGuiReceipt { int begun = 0, ended = 0, rendered = 0; bool open = false; };
GenericGuiReceipt genericGuiReceipt;
OfxStatus genericGuiProbe(const std::string& action, const void* handle,
                          OfxPropertySetHandle in, const std::string& selected) {
  if (action == kOfxActionCreateInstance) { genericGuiReceipt = {}; return kOfxStatOK; }
  if (action != kOfxActionBeginInstanceEdit && action != kOfxActionEndInstanceEdit &&
      action != kOfxImageEffectActionRender) { return kOfxStatOK; }
  OfxPropertySetHandle properties = nullptr;
  int interactive = -1;
  if (gEffectHost->getPropertySet((OfxImageEffectHandle)handle, &properties) != kOfxStatOK ||
      gPropHost->propGetInt(properties, kOfxPropIsInteractive, 0, &interactive) != kOfxStatOK) { return kOfxStatFailed; }
  if (action == kOfxActionBeginInstanceEdit) {
    if (interactive != 1 || genericGuiReceipt.open) { return kOfxStatErrValue; }
    genericGuiReceipt.open = true;
    ++genericGuiReceipt.begun;
    return selected == "gui_failed_begin" ? kOfxStatFailed : kOfxStatOK;
  }
  if (action == kOfxActionEndInstanceEdit) {
    if (interactive != 0 || !genericGuiReceipt.open) { return kOfxStatErrValue; }
    genericGuiReceipt.open = false;
    ++genericGuiReceipt.ended;
    if (selected == "gui_cycle") {
      const auto* messages = static_cast<const OfxMessageSuiteV2*>(gHost->fetchSuite(gHost->host, kOfxMessageSuite, 2));
      if (!messages || messages->message(const_cast<void*>(handle), kOfxMessageWarning,
          "generic.gui.end", "Actual End %d", genericGuiReceipt.ended) != kOfxStatOK) { return kOfxStatErrValue; }
    }
    return kOfxStatFailed; // Legal private UI teardown failure still pairs End.
  }
  if (interactive != 1 || !genericGuiReceipt.open || genericGuiReceipt.begun != genericGuiReceipt.ended + 1) {
    return kOfxStatErrValue;
  }
  int interactiveRender = -1;
  if (gPropHost->propGetInt(in, kOfxImageEffectPropInteractiveRenderStatus, 0, &interactiveRender) != kOfxStatOK ||
      interactiveRender != 0) { return kOfxStatErrValue; }
  ++genericGuiReceipt.rendered;
  const auto* messages = static_cast<const OfxMessageSuiteV2*>(gHost->fetchSuite(gHost->host, kOfxMessageSuite, 2));
  if (!messages) { return kOfxStatFailed; }
  OfxStatus response = kOfxStatFailed;
  const auto ask = [&]() { response = messages->message(const_cast<void*>(handle), kOfxMessageQuestion,
      "generic.gui.question", "%s", "Original plugin Question: continue?"); };
  if (selected == "gui_question_foreign") { std::thread foreign(ask); foreign.join(); }
  else { ask(); }
  const OfxStatus expected = selected == "gui_question_no" ? kOfxStatReplyNo :
      (selected == "gui_failed_begin" || selected == "gui_question_unavailable" || selected == "gui_cycle" ? kOfxStatFailed : kOfxStatReplyYes);
  return response == expected ? kOfxStatOK : kOfxStatErrValue;
}

// Fixed test-only receipt from the actual CABI callbacks. No author authority.
struct StringGuiPermissionPhase {
  int action = 0, interactive = -1;
  int status[2]{};
  char before[2][128]{}, after[2][128]{};
};
struct StringGuiPermissionReceipt {
  int created = 0, destroyed = 0;
  int createStatus[2]{}, invalidStatus[4]{};
  int phaseCount = 0;
  StringGuiPermissionPhase phases[4]{};
};
StringGuiPermissionReceipt stringGuiPermission;
static_assert(std::is_trivially_copyable_v<StringGuiPermissionReceipt>);
bool copyStringGuiValue(OfxParamHandle parameter, double time, char (&output)[128]) {
  char* value = nullptr;
  if (gParamHost->paramGetValueAtTime(parameter, time, &value) != kOfxStatOK || !value) { return false; }
  size_t length = 0;
  while (length < sizeof(output) && value[length] != '\0') { ++length; }
  if (length == sizeof(output)) { return false; }
  std::memcpy(output, value, length + 1);
  return true;
}
bool stringGuiPermissionProbe(const std::string& action, const void* handle) {
  if (action == kOfxActionDestroyInstance) { ++stringGuiPermission.destroyed; return true; }
  const bool creating = action == kOfxActionCreateInstance;
  const bool beginning = action == kOfxActionBeginInstanceEdit;
  const bool rendering = action == kOfxImageEffectActionRender;
  const bool ending = action == kOfxActionEndInstanceEdit;
  if (!creating && !beginning && !rendering && !ending) { return true; }
  if (creating) { stringGuiPermission = {}; ++stringGuiPermission.created; }
  OfxParamSetHandle parameters = nullptr;
  OfxParamHandle plain = nullptr, timed = nullptr;
  double time = 0.0;
  const auto* timeline = auxTimelineSuite();
  if (!timeline || timeline->getTime(const_cast<void*>(handle), &time) != kOfxStatOK || !std::isfinite(time) ||
      gEffectHost->getParamSet((OfxImageEffectHandle)handle, &parameters) != kOfxStatOK ||
      gParamHost->paramGetHandle(parameters, "single", &plain, nullptr) != kOfxStatOK ||
      gParamHost->paramGetHandle(parameters, "multi", &timed, nullptr) != kOfxStatOK) { return false; }
  if (creating) {
    char first[128]{}, second[128]{};
    if (!copyStringGuiValue(plain, time, first) || !copyStringGuiValue(timed, time, second)) { return false; }
    stringGuiPermission.createStatus[0] = gParamHost->paramSetValue(plain, first);
    stringGuiPermission.createStatus[1] = gParamHost->paramSetValueAtTime(timed, time, second);
    return true;
  }
  if (stringGuiPermission.phaseCount >= 4) { return false; }
  auto& phase = stringGuiPermission.phases[stringGuiPermission.phaseCount];
  phase.action = beginning ? 1 : rendering ? 2 : 3;
  OfxPropertySetHandle properties = nullptr;
  if (gEffectHost->getPropertySet((OfxImageEffectHandle)handle, &properties) != kOfxStatOK ||
      gPropHost->propGetInt(properties, kOfxPropIsInteractive, 0, &phase.interactive) != kOfxStatOK ||
      !copyStringGuiValue(plain, time, phase.before[0]) || !copyStringGuiValue(timed, time, phase.before[1])) { return false; }
  if (beginning) {
    const std::string oversized(64 * 1024 + 1, 'x');
    stringGuiPermission.invalidStatus[0] = gParamHost->paramSetValue(plain, static_cast<const char*>(nullptr));
    stringGuiPermission.invalidStatus[1] = gParamHost->paramSetValue(plain, oversized.c_str());
    stringGuiPermission.invalidStatus[2] = gParamHost->paramSetValueAtTime(timed, std::numeric_limits<double>::quiet_NaN(), "invalid");
    stringGuiPermission.invalidStatus[3] = gParamHost->paramSetValueAtTime(timed, time + 1.0, "off-time");
  }
  // Distinct parameters prevent the ordinary setter's old mutation from
  // contaminating the timed setter's target. Freeze facts before host reapply.
  phase.status[0] = gParamHost->paramSetValue(plain, "plugin/plain");
  phase.status[1] = gParamHost->paramSetValueAtTime(timed, time, "plugin/timed");
  if (!copyStringGuiValue(plain, time, phase.after[0]) || !copyStringGuiValue(timed, time, phase.after[1])) { return false; }
  ++stringGuiPermission.phaseCount;
  // Record the old permission violation without failing Render/End prematurely.
  return true;
}

// Test-only conjunction guards; no exported counter or product protocol.
struct LegacyPriorityState { bool posted = false, repaired = false; };
std::map<const void*, LegacyPriorityState> legacyPriorityStates;
bool legacyPriorityMode(const std::string& selected) {
  return selected == "legacy_priority_render_fatal_error" || selected == "legacy_priority_roi_failed_error" ||
      selected == "legacy_priority_end_failed_error" || selected == "legacy_priority_error_only";
}
void legacyPriorityError(const void* handle, LegacyPriorityState& state) {
  const auto* messages = auxMessageSuite();
  // Posting failure must not alias the expected generic CABI5 and falsely pass.
  if (!messages || messages->setPersistentMessage(const_cast<void*>(handle), kOfxMessageError,
      "render.repair", "%s", "Change scale to recover render") != kOfxStatOK) { std::abort(); }
  state.posted = true;
}

// This mode delegates the identity decision to the untouched official Basic
// implementation. It never fetches Output or manufactures a Render receipt.
OfxStatus currentSourceIdentity(const void* handle, OfxPropertySetHandle in,
                                OfxPropertySetHandle out) {
  double time = -1.0, currentTime = -1.0, renderScale[2]{};
  const auto* timeline = auxTimelineSuite();
  bool valid = timeline && canonicalProgressiveContract(handle, in) &&
      timeline->getTime(const_cast<void*>(handle), &currentTime) == kOfxStatOK &&
      gPropHost->propGetDouble(in, kOfxPropTime, 0, &time) == kOfxStatOK &&
      (time == 0.0 || time == 1.0 || time == 2.0) && currentTime == time &&
      gPropHost->propGetDoubleN(in, kOfxImageEffectPropRenderScale, 2, renderScale) == kOfxStatOK &&
      renderScale[0] == 1.0 && renderScale[1] == 1.0;
  const auto stringIs = [](OfxPropertySetHandle properties, const char* name, const char* expected) {
    char* value = nullptr;
    return gPropHost->propGetString(properties, name, 0, &value) == kOfxStatOK && value &&
        std::strcmp(value, expected) == 0;
  };
  for (const char* name : {kOfxImageEffectSimpleSourceClipName, kOfxImageEffectOutputClipName}) {
    OfxImageClipHandle clip = nullptr;
    OfxPropertySetHandle properties = nullptr;
    OfxRectD rod{};
    double par = 0.0;
    valid = valid && gEffectHost->clipGetHandle((OfxImageEffectHandle)handle, name, &clip,
        &properties) == kOfxStatOK && clip && properties &&
        gEffectHost->clipGetRegionOfDefinition(clip, time, &rod) == kOfxStatOK &&
        rod.x1 == 0.0 && rod.y1 == 0.0 && rod.x2 == 6.0 && rod.y2 == 2.0 &&
        gPropHost->propGetDouble(properties, kOfxImagePropPixelAspectRatio, 0, &par) == kOfxStatOK &&
        par == 2.0 && stringIs(properties, kOfxImageEffectPropPixelDepth, kOfxBitDepthFloat) &&
        stringIs(properties, kOfxImageEffectPropComponents, kOfxImageComponentRGBA) &&
        stringIs(properties, kOfxImageEffectPropPreMultiplication, kOfxImageUnPreMultiplied);
  }
  const auto result = valid ? pluginMain(kOfxImageEffectActionIsIdentity, handle, in, out) : kOfxStatFailed;
  valid = valid && result == (time == 1.0 ? kOfxStatReplyDefault : kOfxStatOK);
  double outputTime = -1.0;
  valid = valid && gPropHost->propGetDouble(out, kOfxPropTime, 0, &outputTime) == kOfxStatOK &&
      outputTime == time;
  if (result == kOfxStatOK) {
    valid = valid && stringIs(out, kOfxPropName, kOfxImageEffectSimpleSourceClipName);
  }
  describeActionReceipt("identity-current", "contract", 1, valid ? 1 : 0, result);
  return valid ? result : kOfxStatFailed;
}

bool nonCurrentIdentityMode(const std::string& selected) {
  return selected == "identity_output_clip" || selected == "identity_other_time";
}

// Identity selections outside this host's current-Source/current-time
// optimization. Keep the original Basic decision and change only its outArgs.
OfxStatus nonCurrentIdentity(const void* handle, OfxPropertySetHandle in,
                             OfxPropertySetHandle out, const std::string& selected) {
  const auto original = currentSourceIdentity(handle, in, out);
  double time = -1.0;
  bool valid = original == kOfxStatOK &&
      gPropHost->propGetDouble(in, kOfxPropTime, 0, &time) == kOfxStatOK;
  if (valid && selected == "identity_output_clip") {
    valid = gPropHost->propSetString(out, kOfxPropName, 0,
        kOfxImageEffectOutputClipName) == kOfxStatOK;
  } else if (valid && selected == "identity_other_time") {
    valid = gPropHost->propSetDouble(out, kOfxPropTime, 0, time + 1.0) == kOfxStatOK;
  } else { valid = false; }
  const auto result = valid ? kOfxStatOK : kOfxStatFailed;
  describeActionReceipt(selected.c_str(), "contract", 1, valid ? 1 : 0, result);
  return result;
}

OfxStatus probeAction(const char* action, const void* handle,
                OfxPropertySetHandle in, OfxPropertySetHandle out) {
  const std::string operation(action);
  const std::string selected = mode();
  if (operation == kOfxImageEffectActionIsIdentity) {
    // The other fixture modes test their real Render work, including pixel
    // overrides after Basic. Opt out of this optional optimization explicitly.
    if (nonCurrentIdentityMode(selected)) { return nonCurrentIdentity(handle, in, out, selected); }
    return selected == "identity_current" ? currentSourceIdentity(handle, in, out) : kOfxStatReplyDefault;
  }
  if (temporalClipAdmissionMode(selected)) {
    if (operation == kOfxActionCreateInstance || operation == kOfxImageEffectActionRender) { std::abort(); }
    if (operation == kOfxImageEffectActionDescribeInContext) {
      return defineTemporalClipAdmissionProbe(handle, selected) ? kOfxStatOK : kOfxStatFailed;
    }
  }
  if (selected == "aux_describe_load_failure" && operation == kOfxActionLoad) { return kOfxStatFailed; }
  if (selected == "host_properties_readonly" && operation == kOfxActionDescribe &&
      !readonlyHostProperties(gHost)) { return kOfxStatFailed; }
  if (selected == "host_parameter_limit" && operation == kOfxActionDescribe &&
      !hostParameterLimit(gHost)) { return kOfxStatFailed; }
  // A count refusal must happen before constructing or processing an instance.
  if ((selected == "host_parameters_4097" || selected == "host_numeric_parameters_4096" ||
       selected == "host_numeric_parameters_4096_plus_group") &&
      (operation == kOfxActionCreateInstance || operation == kOfxImageEffectActionRender)) { std::abort(); }
  if ((selected == "host_numeric_parameters_4096" || selected == "host_numeric_parameters_4096_plus_group") &&
      operation == kOfxImageEffectActionDescribeInContext) {
    return defineHostNumericParameterCountProbe(handle, selected == "host_numeric_parameters_4096_plus_group")
        ? kOfxStatOK : kOfxStatFailed;
  }
  if (legacyPriorityMode(selected) &&
      (operation == kOfxActionInstanceChanged || operation == kOfxImageEffectActionGetRegionsOfInterest ||
       operation == kOfxImageEffectActionRender)) {
    auto found = legacyPriorityStates.find(handle);
    if (found == legacyPriorityStates.end()) { std::abort(); }
    auto& state = found->second;
    if (operation == kOfxActionInstanceChanged && selected == "legacy_priority_error_only") {
      char* type = nullptr;
      char* name = nullptr;
      if (gPropHost->propGetString(in, kOfxPropType, 0, &type) != kOfxStatOK || !type) { std::abort(); }
      if (std::string(type) != kOfxTypeParameter) { /* clip changes never repair private Error */ }
      else {
        if (gPropHost->propGetString(in, kOfxPropName, 0, &name) != kOfxStatOK || !name ||
            std::string(name) != "scale") { std::abort(); }
        double gain = 0.0;
        if (!auxiliaryScale(const_cast<void*>(handle), gain)) { std::abort(); }
        if (gain == 2.0) {
        const auto* messages = auxMessageSuite();
        if (!state.posted || !messages || messages->clearPersistentMessage(const_cast<void*>(handle)) != kOfxStatOK) { std::abort(); }
          state.posted = false;
          state.repaired = true;
        }
      }
    } else if (operation == kOfxImageEffectActionGetRegionsOfInterest && selected == "legacy_priority_roi_failed_error") {
      if (!canonicalProgressiveContract(handle, nullptr)) { std::abort(); }
      legacyPriorityError(handle, state);
      return kOfxStatFailed;
    } else if (operation == kOfxImageEffectActionRender) {
      if (state.posted || !canonicalProgressiveContract(handle, in) ||
          selected == "legacy_priority_roi_failed_error") { std::abort(); }
      if (selected == "legacy_priority_render_fatal_error") {
        legacyPriorityError(handle, state);
        return kOfxStatErrFatal;
      }
    }
  }
  if (selected == "catalog_describe_block" && operation == kOfxActionDescribe) {
    return blockCatalogCallback(gHost) ? kOfxStatOK : kOfxStatFailed;
  }
  if (selected.find("gui_") == 0) {
    const OfxStatus gui = genericGuiProbe(operation, handle, in, selected);
    if (operation == kOfxActionBeginInstanceEdit || operation == kOfxActionEndInstanceEdit || gui != kOfxStatOK) { return gui; }
  }
  if (selected == "string_gui_permission" && !stringGuiPermissionProbe(operation, handle)) { return kOfxStatFailed; }
  if (selected == "cancel_after_create" && operation == kOfxImageEffectActionRender &&
      !constructorActionReceipt("MONDRIAN_OPENFX_ACTUAL_RENDER_ENTERED")) { return kOfxStatFailed; }
  if (selected == "cooperative_abort" && operation == kOfxImageEffectActionRender) { return cooperativeAbortRender(handle, in); }
  if (selected == "cooperative_progress" && operation == kOfxImageEffectActionRender &&
      cooperativeProgressRender(handle, in) != kOfxStatOK) { return kOfxStatFailed; }
  if (operation == kOfxImageEffectActionRender &&
      ((selected == "aux_transient_instance" && !auxTransientMessages(const_cast<void*>(handle))) ||
       (selected == "aux_question" && !auxQuestionRefusal(const_cast<void*>(handle))) ||
       (selected == "aux_progress" && !auxProgressLifetime(const_cast<void*>(handle))) ||
       (selected == "aux_timeline" && !auxTimelineReadAndRefusal(const_cast<void*>(handle), in)) ||
       (selected == "aux_warning" && !auxPersistentWarning(const_cast<void*>(handle))))) { return kOfxStatFailed; }
  if (selected == "aux_error" && operation == kOfxImageEffectActionRender) {
    // A persistent Error blocks processing until authored InstanceChanged
    // explicitly clears it. Entering Render while it remains is behavioral RED.
    if (auxiliaryErrorPending) { std::abort(); }
    ++auxiliaryErrorRenderCount;
  }
  if (selected == "aux_transient_destroy" && operation == kOfxActionDestroyInstance &&
      !auxDestroyTarget(const_cast<void*>(handle))) { return kOfxStatFailed; }
  if (selected == "source_alpha" && operation == kOfxImageEffectActionRender && !sourceAssociationMatches(handle, in)) { return kOfxStatFailed; }
  if (selected == "image_uid_persisted" && operation == kOfxImageEffectActionRender && !restoreImageAnalysis()) { return kOfxStatFailed; }
  if ((selected == "image_uid" || selected == "image_uid_persisted" || selected == "image_field") && operation == kOfxImageEffectActionRender &&
      !imageMetadata(handle, in, selected != "image_field")) { return kOfxStatFailed; }
  if (operation == kOfxImageEffectActionRender &&
      ((selected == "mutex_contract" && !mutexContract()) ||
       (selected == "mutex_negative" && !negativeMutexContract()) ||
       (selected == "mutex_lifecycle" && !mutexLifecycleContract()) ||
       (selected == "thread_contract" && !threadContract(handle, in)) ||
       (selected == "thread_concurrent" && !concurrentThreadContract()) ||
       (selected == "mutex_capacity" && !mutexCapacityContract()))) { return kOfxStatFailed; }
  if (selected == "thread_hang" && operation == kOfxImageEffectActionRender) {
    const auto* suite = threadSuite();
    if (!suite || suite->multiThread([](unsigned, unsigned, void*) {
        std::this_thread::sleep_for(std::chrono::seconds(5));
      }, 1, nullptr) != kOfxStatOK) { return kOfxStatFailed; }
  }
  if (selected == "output_alpha" && operation == kOfxImageEffectActionRender &&
      !negotiatedAlphaOutput(handle, in, false)) { return kOfxStatFailed; }
  if (selected == "bootstrap_skip") { std::abort(); }
  if ((selected == "bootstrap_required" || selected == "host_capabilities") &&
      operation == kOfxActionDescribe && !honestHostCapabilities(gHost)) { return kOfxStatFailed; }
  if (selected == "canonical_progressive" &&
      (operation == kOfxActionCreateInstance || operation == kOfxImageEffectActionRender) &&
      !canonicalProgressiveContract(handle, in)) { return kOfxStatFailed; }
  if (selected.find("cpu_unsupported_") == 0 && operation == kOfxImageEffectActionRender &&
      selected != "cpu_unsupported_change") { std::abort(); }
  if (selected == "cpu_unsupported_change" && operation == kOfxImageEffectActionRender &&
      cpuDisabled) { std::abort(); }
  if (selected == "curve_authoring" &&
      (operation == kOfxActionCreateInstance || operation == kOfxActionBeginInstanceChanged ||
       operation == kOfxActionInstanceChanged || operation == kOfxActionEndInstanceChanged ||
       operation == kOfxImageEffectActionRender || operation == kOfxActionDestroyInstance) &&
      !authorCurveQueries(handle)) { return kOfxStatFailed; }
  if (isAuthorHistoryMode(selected) &&
      (operation == kOfxActionCreateInstance || operation == kOfxActionBeginInstanceChanged ||
       operation == kOfxActionInstanceChanged || operation == kOfxActionEndInstanceChanged ||
       operation == kOfxImageEffectActionRender || operation == kOfxActionDestroyInstance) &&
      !authorHistoryCurveQueries(handle,
          selected == "curve_group_value_authoring" ? AuthorHistoryEdit::Value :
          selected == "curve_group_transform_authoring" ? AuthorHistoryEdit::Transform :
          selected == "curve_group_interpolation_authoring" ? AuthorHistoryEdit::Interpolation :
          selected == "curve_reset_authoring" ? AuthorHistoryEdit::Reset :
          AuthorHistoryEdit::Time)) { return kOfxStatFailed; }
  if (isCurveLifecycleMode(selected) &&
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
        gParamHost->paramGetValue(toggle, &enabled) != kOfxStatOK) { return kOfxStatFailed; }
    unsigned keyCount = 999;
    if (selected == "curve_reset_authoring" &&
        gParamHost->paramGetNumKeys(scalar, &keyCount) != kOfxStatOK) { return kOfxStatFailed; }
    // Canonical curves reject SDK author writes. A reset constant retains the
    // existing instance-local initialization contract; it does not edit Project.
    const bool authorCurve = (selected == "canonical_curves" || isCurveLifecycleMode(selected) ||
                              selected == "curve_authoring" || isAuthorHistoryMode(selected)) &&
                             !(selected == "curve_reset_authoring" && keyCount == 0);
    if (gParamHost->paramSetValue(scalar, scale) != (authorCurve ? kOfxStatErrUnsupported : kOfxStatOK) ||
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
  if (selected == "optional_unmapped" && operation == kOfxImageEffectActionRender &&
      !optionalUnmappedFormat(handle, in)) { return kOfxStatFailed; }
  const OfxStatus result = selected == "fixed_canvas_partial_output" && operation == kOfxImageEffectActionRender
      ? fixedCanvasPartialOutput(handle, in) : pluginMain(action, handle, in, out);
  if (result == kOfxStatOK || result == kOfxStatReplyDefault) {
    if (selected == "optional_unmapped" && operation == kOfxImageEffectActionDescribeInContext) {
      OfxPropertySetHandle clip = nullptr;
      if (gEffectHost->clipDefine((OfxImageEffectHandle)handle, "DisconnectedOptional", &clip) != kOfxStatOK || !clip ||
          gPropHost->propSetString(clip, kOfxImageEffectPropSupportedComponents, 0, kOfxImageComponentRGBA) != kOfxStatOK ||
          gPropHost->propSetInt(clip, kOfxImageClipPropOptional, 0, 1) != kOfxStatOK) { return kOfxStatFailed; }
    }
    if (selected == "host_properties_readonly" &&
        ((operation == kOfxActionDescribe && !writablePluginLabels(handle, false)) ||
         (operation == kOfxImageEffectActionDescribeInContext && !writablePluginLabels(handle, true)))) { return kOfxStatFailed; }
    if ((selected == "host_parameters_4096" || selected == "host_parameters_4097") &&
        operation == kOfxImageEffectActionDescribeInContext &&
        !addHostParameterCountProbe(handle, selected == "host_parameters_4097")) { return kOfxStatFailed; }
  }
  if ((result == kOfxStatOK || result == kOfxStatReplyDefault) &&
      ((selected == "aux_transient_null" && operation == kOfxActionLoad && !auxTransientMessages(nullptr)) ||
       (selected == "aux_transient_descriptor" && operation == kOfxActionDescribe && !auxTransientMessages(const_cast<void*>(handle))) ||
       (selected == "aux_invalid_handles" && operation == kOfxActionDescribe && !auxInvalidHandles(const_cast<void*>(handle))) ||
       (selected == "aux_error" && !auxPersistentError(const_cast<void*>(handle), operation, in)))) { return kOfxStatFailed; }

  // Describe-only notice controls. Deliberately ignore a recording refusal
  // after the retained prefix, so the host must enforce its own completeness.
  if (operation == kOfxActionDescribe &&
      (result == kOfxStatOK || result == kOfxStatReplyDefault) &&
      (selected == "aux_describe_32" || selected == "aux_describe_33" ||
       selected == "aux_describe_fatal" || selected == "aux_describe_invalid_utf8" ||
       selected == "aux_describe_native_failure")) {
    const auto* messages = auxMessageSuite();
    if (!messages) { return kOfxStatFailed; }
    void* descriptor = const_cast<void*>(handle);
    if (selected == "aux_describe_32" || selected == "aux_describe_33") {
      for (int index = 0; index < 32; ++index) {
        if (messages->message(descriptor, kOfxMessageMessage, "bounded", "notice %d", index) != kOfxStatOK) {
          return kOfxStatFailed;
        }
      }
      if (selected == "aux_describe_33") {
        (void)messages->message(descriptor, kOfxMessageMessage, "overflow", "ignored 33rd notice");
      }
    } else if (selected == "aux_describe_fatal") {
      if (messages->message(descriptor, kOfxMessageFatal, "fatal", "metadata fatal") != kOfxStatOK) { return kOfxStatFailed; }
    } else {
      if (messages->message(descriptor, kOfxMessageWarning, "before-refusal", "retained prefix") != kOfxStatOK) { return kOfxStatFailed; }
      if (selected == "aux_describe_invalid_utf8") {
        const char invalid[] = {static_cast<char>(0xff), '\0'};
        (void)messages->message(descriptor, kOfxMessageMessage, "invalid", "%s", invalid);
      } else { return kOfxStatFailed; }
    }
  }

  if (legacyPriorityMode(selected)) {
    if (operation == kOfxActionCreateInstance) {
      if ((result != kOfxStatOK && result != kOfxStatReplyDefault) ||
          !legacyPriorityStates.emplace(handle, LegacyPriorityState{}).second) { std::abort(); }
    } else if (operation == kOfxActionDestroyInstance) {
      if (legacyPriorityStates.erase(handle) != 1) { std::abort(); }
    } else if (operation == kOfxImageEffectActionEndSequenceRender && selected == "legacy_priority_end_failed_error") {
      if ((result != kOfxStatOK && result != kOfxStatReplyDefault) ||
          !canonicalProgressiveContract(handle, nullptr)) { std::abort(); }
      legacyPriorityError(handle, legacyPriorityStates.at(handle));
      return kOfxStatFailed;
    } else if (operation == kOfxImageEffectActionRender && selected == "legacy_priority_error_only") {
      auto& state = legacyPriorityStates.at(handle);
      double gain = 0.0;
      if (result != kOfxStatOK || !auxiliaryScale(const_cast<void*>(handle), gain)) { std::abort(); }
      if (gain == 1.5 && !state.repaired) { legacyPriorityError(handle, state); }
      else if (gain != 2.0 || !state.repaired || state.posted) { std::abort(); }
    }
  }

  if (selected == "source_alpha" && operation == kOfxImageEffectActionDescribeInContext && result == kOfxStatOK && !defineOutputAlpha(handle)) { return kOfxStatFailed; }
  if (selected == "output_alpha") {
    if (operation == kOfxImageEffectActionDescribeInContext && result == kOfxStatOK &&
        !defineOutputAlpha(handle)) { return kOfxStatFailed; }
    if (operation == kOfxImageEffectActionGetClipPreferences &&
        (result == kOfxStatOK || result == kOfxStatReplyDefault)) {
      const int choice = outputAlphaMode(handle, nullptr);
      if (choice < 0 || choice > 8 || gPropHost->propSetString(out,
          kOfxImageEffectPropPreMultiplication, 0, outputAlphaRepresentation(choice)) != kOfxStatOK) {
        return kOfxStatFailed;
      }
      return kOfxStatOK;
    }
    if (operation == kOfxImageEffectActionRender && result == kOfxStatOK &&
        !negotiatedAlphaOutput(handle, in, true)) { return kOfxStatFailed; }
  }
  if ((selected == "cpu_unsupported_descriptor" && operation == kOfxActionDescribe) ||
      (selected == "cpu_unsupported_create" && operation == kOfxActionCreateInstance) ||
      (selected == "cpu_unsupported_change" && operation == kOfxActionInstanceChanged)) {
    OfxPropertySetHandle props = nullptr;
    if (gEffectHost->getPropertySet((OfxImageEffectHandle)handle, &props) != kOfxStatOK ||
        gPropHost->propSetString(props, kOfxImageEffectPropCPURenderSupported, 0, "false") != kOfxStatOK) {
      return kOfxStatFailed;
    }
    cpuDisabled = true;
  }
  if (selected == "resident_owner_receipt" && operation == kOfxImageEffectActionRender && result == kOfxStatOK &&
      !appendResidentOwnerReceipt(handle, in)) { return kOfxStatFailed; }
  if ((selected == "image_uid" || selected == "image_uid_persisted") && operation == kOfxImageEffectActionRender && result == kOfxStatOK &&
      (!imageAnalysisOutput(handle, in) || (selected == "image_uid_persisted" && !saveImageAnalysis()))) { return kOfxStatFailed; }
  if (selected == "canonical_progressive" && operation == kOfxImageEffectActionRender &&
      result == kOfxStatOK && !canonicalCoordinateOutput(handle, in)) { return kOfxStatFailed; }
  if ((isCurveLifecycleMode(selected) || selected == "curve_authoring" || isAuthorHistoryMode(selected)) && result == kOfxStatOK &&
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

  if ((selected.find("strings") == 0 || selected == "string_gui_permission") && result == kOfxStatOK &&
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
    const int count = (selected == "strings_text" || selected == "string_gui_permission") ? 2 : selected == "strings_paths" ? 6 : 8;
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
    if (selected == "cancel_after_create" &&
        !constructorActionReceipt("MONDRIAN_OPENFX_ACTUAL_CONSTRUCTOR_COMPLETE")) { return kOfxStatFailed; }
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
OfxStatus probe(const char* action, const void* handle,
                OfxPropertySetHandle in, OfxPropertySetHandle out) {
  const char* observed = nullptr;
  for (const char* known : {kOfxActionLoad, kOfxActionDescribe,
       kOfxImageEffectActionDescribeInContext, kOfxActionUnload, kOfxImageEffectActionRender}) {
    if (std::strcmp(action, known) == 0) { observed = known; break; }
  }
  // These Describe-only modes must prove that Create was never entered. Keep
  // existing modes' receipt sequences unchanged.
  if (temporalClipAdmissionMode(mode()) && std::strcmp(action, kOfxActionCreateInstance) == 0) {
    observed = kOfxActionCreateInstance;
  }
  if (mode() == "optional_unmapped" &&
      (std::strcmp(action, kOfxActionCreateInstance) == 0 ||
       std::strcmp(action, kOfxImageEffectActionGetClipPreferences) == 0)) { observed = action; }
  if ((mode() == "identity_current" || nonCurrentIdentityMode(mode())) &&
      std::strcmp(action, kOfxImageEffectActionIsIdentity) == 0) {
    observed = action;
  }
  const char* directory = std::getenv("MONDRIAN_OPENFX_DESCRIBE_ACTION_RECEIPT_DIRECTORY");
  const bool observing = directory && *directory;
  static std::atomic<const void*> describedBase{nullptr}, describedContext{nullptr};
  if (observing && std::strcmp(action, kOfxActionDescribe) == 0) { describedBase = handle; }
  if (observing && std::strcmp(action, kOfxImageEffectActionDescribeInContext) == 0) { describedContext = handle; }
  if (observed) { describeActionReceipt(observed, "enter"); }
  if (observing && std::strcmp(action, kOfxActionUnload) == 0) {
    const auto* messages = auxMessageSuite();
    for (const auto& entry : {std::make_pair("descriptor-base", describedBase.load()),
                             std::make_pair("descriptor-context", describedContext.load())}) {
      if (!entry.second) { continue; }
      // Unknown kind is a pure handle check: live -> Failed, dead -> BadHandle.
      // Neither the fixture nor the host dereferences the borrowed handle here.
      const auto status = messages ? messages->message(const_cast<void*>(entry.second),
          "mondrian.fixture.unknown-message-kind", nullptr, "%s", "handle check") : kOfxStatErrMissingHostFeature;
      describeActionReceipt(entry.first, "check", 0, 0, status);
    }
  }
  const auto result = probeAction(action, handle, in, out);
  if (observed) { describeActionReceipt(observed, "return", 0, 0, result); }
  if (observing && std::strcmp(action, kOfxActionUnload) == 0) { describedBase = nullptr; describedContext = nullptr; }
  return result;
}

bool bootstrapImageMemory(const OfxHost* host) {
  if (!host || !host->host || !host->fetchSuite) { return false; }
  const auto* images = static_cast<const OfxImageEffectSuiteV1*>(
      host->fetchSuite(host->host, kOfxImageEffectSuite, 1));
  if (!images) { return false; }
  OfxImageMemoryHandle memory = nullptr;
  if (images->imageMemoryAlloc(nullptr, 256, &memory) != kOfxStatOK || !memory) { return false; }
  void* storage = nullptr;
  bool valid = images->imageMemoryLock(memory, &storage) == kOfxStatOK && storage;
  if (valid) {
    auto* bytes = static_cast<unsigned char*>(storage);
    for (int index = 0; index < 256; ++index) { bytes[index] = static_cast<unsigned char>(index); }
    for (int index = 0; index < 256; ++index) {
      valid = valid && bytes[index] == static_cast<unsigned char>(index);
    }
    valid = images->imageMemoryUnlock(memory) == kOfxStatOK && valid;
  }
  return images->imageMemoryFree(memory) == kOfxStatOK && valid;
}

bool mixedApiMode() { return mode() == "mixed_api" || mode() == "mixed_api_long"; }

OfxPlugin probePlugin = basicPlugin;
void ignoredApiSetHost(OfxHost*) { std::abort(); }
OfxStatus ignoredApiAction(const char*, const void*, OfxPropertySetHandle,
                          OfxPropertySetHandle) { std::abort(); }
OfxPlugin ignoredApiPlugin = {"org.mondrian.test.UnsupportedAPI", 1,
    "org.mondrian.test.UnsupportedAPI.plugin", 1, 0, ignoredApiSetHost, ignoredApiAction};
OfxPlugin longApiPlugin = {
    "org.mondrian.test.UnsupportedAPI.mixedBundleCapability.withExtendedVendorVersionNamespace",
    1, "org.mondrian.test.UnsupportedAPI.longPlugin", 1, 0, ignoredApiSetHost, ignoredApiAction};
OfxPlugin otherApiPlugin = {"org.mondrian.test.UnsupportedAPI", 1,
    "org.mondrian.test.UnsupportedAPI.otherPlugin", 1, 0, ignoredApiSetHost, ignoredApiAction};
OfxPlugin newerImageApiPlugin = {kOfxImageEffectPluginApi, 2,
    "org.mondrian.test.UnsupportedImageVersion", 1, 0, ignoredApiSetHost, ignoredApiAction};
}
extern "C" {
OfxExport int SessionProbeImageIdentifier(int output, char* buffer, size_t capacity, size_t* length) {
  if (!buffer || !length || (output != 0 && output != 1)) { return 2; }
  const auto& value = output == 0 ? capturedSourceIdentifier : capturedOutputIdentifier;
  if (value.size() > capacity) { return 2; }
  std::copy(value.begin(), value.end(), buffer);
  *length = value.size();
  return 0;
}

OfxExport OfxStatus OfxSetHost(const OfxHost* host) {
  describeActionReceipt("OfxSetHost", "enter");
  if (mode() == "catalog_discovery_block") {
    return blockCatalogCallback(host) ? kOfxStatOK : kOfxStatFailed;
  }
  if (mode() == "bootstrap_skip") { return kOfxStatFailed; }
  if (mode() == "bootstrap_error") { return kOfxStatErrMissingHostFeature; }
  if (mode() == "bootstrap_memory" && !bootstrapImageMemory(host)) { return kOfxStatErrMissingHostFeature; }
  if (mode() != "bootstrap_required" && mode() != "bootstrap_memory" && !mixedApiMode() &&
      mode() != "unsupported_api") { return kOfxStatOK; }
  if (bootstrapPhase != 0 || !honestHostCapabilities(host)) { return kOfxStatErrUnknown; }
  bootstrapPhase = 1;
  return kOfxStatOK;
}
OfxExport OfxPlugin* OfxGetPlugin(int index) {
  if (mode() == "bootstrap_skip") { std::abort(); }
  if (mode() == "bootstrap_required" || mode() == "bootstrap_memory" || mixedApiMode() ||
      mode() == "unsupported_api") {
    if (bootstrapPhase < 2) { return nullptr; }
    bootstrapPhase = 3;
  }
  if (mixedApiMode() || mode() == "unsupported_api") {
    if (index == 0) { return mode() == "mixed_api_long" ? &longApiPlugin : &ignoredApiPlugin; }
    if (index == 1) { return &newerImageApiPlugin; }
    if (mixedApiMode() && index == 3) { return &otherApiPlugin; }
    if (!mixedApiMode() || index != 2) { return nullptr; }
  } else if (index != 0) { return nullptr; }
  probePlugin.mainEntry = probe;
  return &probePlugin;
}
OfxExport int OfxGetNumberOfPlugins() {
  if (mode() == "bootstrap_skip") { std::abort(); }
  if (mode() == "bootstrap_required" || mode() == "bootstrap_memory" || mixedApiMode() ||
      mode() == "unsupported_api") {
    if (bootstrapPhase != 1) { return 0; }
    bootstrapPhase = 2;
  }
  if (mixedApiMode()) { return 4; }
  if (mode() == "unsupported_api") { return 2; }
  return 1;
}
// Inside the existing extern "C" block: Windows OfxExport already contains extern.
OfxExport int SessionProbeGuiReceipt(int coordinate) {
  switch (coordinate) { case 0: return genericGuiReceipt.begun; case 1: return genericGuiReceipt.ended;
    case 2: return genericGuiReceipt.rendered; case 3: return genericGuiReceipt.open ? 1 : 0; default: return -1; }
}
OfxExport int SessionProbeAuxRenderCount() { return auxiliaryErrorRenderCount; }
OfxExport int SessionProbeStringGuiPermissionReceipt(void* output, size_t bytes) {
  if (!output || bytes != sizeof(stringGuiPermission)) { return 2; }
  std::memcpy(output, &stringGuiPermission, bytes);
  return 0;
}
}
