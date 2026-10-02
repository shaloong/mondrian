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

namespace {
struct ActionState { bool changing = false; int changes = 0; int renders = 0; };
std::map<const void*, ActionState> actions;
std::string mode() {
  const char* value = std::getenv("MONDRIAN_OPENFX_SESSION_PROBE_MODE");
  return value ? value : "normal";
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

OfxStatus probe(const char* action, const void* handle,
                OfxPropertySetHandle in, OfxPropertySetHandle out) {
  const std::string operation(action);
  const std::string selected = mode();
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
  const OfxStatus result = pluginMain(action, handle, in, out);
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
