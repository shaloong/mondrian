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
