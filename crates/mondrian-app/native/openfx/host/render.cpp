// Copyright OpenFX and contributors to the OpenFX project.
// SPDX-License-Identifier: BSD-3-Clause


#include <iostream>
#include <cmath>
#include <set>
#include <memory>
#include <cstring>
#include <cstdint>
#include <algorithm>

#include "render.h"

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

static OFX::Host::ImageEffect::ImageEffectPlugin* selectFilter(
    OFX::Host::ImageEffect::PluginCache& cache,
    OFX::Host::PluginBinary& binary,
    const char* identifier)
{
  for (int index = 0; index < binary.getNPlugins(); ++index) {
    OFX::Host::Plugin& candidate = binary.getPlugin(index);
    if (candidate.getIdentifier() != identifier) { continue; }
    cache.loadFromPlugin(&candidate);
    std::string reason;
    if (!cache.pluginSupported(&candidate, reason)) {
      std::cerr << reason << std::endl;
      return nullptr;
    }
    cache.confirmPlugin(&candidate);
    return cache.getPluginById(candidate.getIdentifier());
  }
  return nullptr;
}

static bool accepted(OfxStatus status)
{
  return status == kOfxStatOK || status == kOfxStatReplyDefault;
}

static bool admitsIndependentFrames(const OFX::Host::ImageEffect::Base& effect)
{
  const int sequential = effect.getProps().getIntProperty(kOfxImageEffectInstancePropSequentialRender);
  return !effect.temporalAccess() && (sequential == 0 || sequential == 2);
}

static int loadFrame(MondrianOpenFx::FrameState& state, const void* input,
                     size_t frameBytes, const MondrianOpenFxFrameInfo& info)
{
  if (!input || info.width <= 0 || info.height <= 0 ||
      info.width > 8192 || info.height > 8192 ||
      !std::isfinite(info.frame) || !std::isfinite(info.frame_rate) ||
      !std::isfinite(info.first_frame) || !std::isfinite(info.last_frame) ||
      !std::isfinite(info.pixel_aspect_ratio) || info.frame_rate <= 0.0 ||
      info.pixel_aspect_ratio <= 0.0 || info.first_frame > info.frame ||
      info.frame > info.last_frame) { return 2; }
  const size_t pixels = size_t(info.width) * size_t(info.height);
  if (frameBytes != pixels * sizeof(OfxRGBAColourF) ||
      frameBytes > 128u * 1024u * 1024u) { return 2; }
  if (reinterpret_cast<uintptr_t>(input) % alignof(OfxRGBAColourF) != 0) { return 2; }
  const auto* data = static_cast<const OfxRGBAColourF*>(input);
  for (size_t index = 0; index < pixels; ++index) {
    const auto& pixel = data[index];
    if (!std::isfinite(pixel.r) || !std::isfinite(pixel.g) ||
        !std::isfinite(pixel.b) || !std::isfinite(pixel.a)) { return 2; }
  }
  state.input = data;
  state.pixelCount = pixels;
  state.info = info;
  return 0;
}

struct ParameterUpdate {
  OFX::Host::Param::Instance* parameter;
  int kind;
  double value;
  bool changed;
};

// Declared in lifetime order: the instance dies before its plugin binary,
// API handler, cache, host and frame facts. No global cache keeps raw handlers.
struct MondrianOpenFxSession {
  MondrianOpenFx::FrameState frame;
  MondrianOpenFx::Host host;
  OFX::Host::PluginCache cache;
  OFX::Host::ImageEffect::PluginCache imageCache;
  std::unique_ptr<OFX::Host::PluginBinary> binary;
  std::unique_ptr<OFX::Host::ImageEffect::Instance> instance;
  bool failed = false;

  MondrianOpenFxSession() : imageCache(host) {
    cache.setCacheVersion("mondrian-openfx-v1");
    imageCache.registerInCache(cache);
  }

  int applyParameters(const MondrianOpenFxParameter* parameters, int count,
                      bool notify, const char* reason, bool clipsChanged) {
    if (count < 0 || count > 4096 || (count != 0 && !parameters)) { return 2; }
    std::map<std::string, const MondrianOpenFxParameter*> authored;
    for (int index = 0; index < count; ++index) {
      const auto& value = parameters[index];
      if (!value.name || !std::isfinite(value.value) ||
          !authored.emplace(value.name, &value).second ||
          !instance->getParam(value.name)) { return 4; }
    }
    std::vector<ParameterUpdate> updates;
    bool changed = clipsChanged;
    for (const auto& entry : instance->getParams()) {
      auto* parameter = entry.second;
      const auto& type = parameter->getType();
      if (type == kOfxParamTypeGroup || type == kOfxParamTypePage ||
          type == kOfxParamTypePushButton) {
        if (authored.count(entry.first)) { return 4; }
        continue;
      }
      const auto found = authored.find(entry.first);
      const auto* value = found == authored.end() ? nullptr : found->second;
      const auto& properties = parameter->getProperties();
      if (auto* scalar = dynamic_cast<OFX::Host::Param::DoubleInstance*>(parameter)) {
        const double target = value ? value->value :
            properties.getDoubleProperty(kOfxParamPropDefault);
        double current = 0.0;
        if ((value && value->kind != 1) || !std::isfinite(target) ||
            target < properties.getDoubleProperty(kOfxParamPropMin) ||
            target > properties.getDoubleProperty(kOfxParamPropMax) ||
            scalar->get(current) != kOfxStatOK) { return 4; }
        updates.push_back({parameter, 1, target, current != target});
      } else if (auto* scalar = dynamic_cast<OFX::Host::Param::BooleanInstance*>(parameter)) {
        const double target = value ? value->value :
            properties.getIntProperty(kOfxParamPropDefault);
        bool current = false;
        if ((value && value->kind != 2) || (target != 0.0 && target != 1.0) ||
            scalar->get(current) != kOfxStatOK) { return 4; }
        updates.push_back({parameter, 2, target, current != (target == 1.0)});
      } else if (auto* scalar = dynamic_cast<OFX::Host::Param::IntegerInstance*>(parameter)) {
        const double target = value ? value->value : properties.getIntProperty(kOfxParamPropDefault);
        int current = 0;
        if ((value && value->kind != 3) || !std::isfinite(target) || std::trunc(target) != target ||
            target < properties.getIntProperty(kOfxParamPropMin) || target > properties.getIntProperty(kOfxParamPropMax) ||
            scalar->get(current) != kOfxStatOK) { return 4; }
        updates.push_back({parameter, 3, target, current != target});
      } else { return 7; }
      changed = changed || updates.back().changed;
    }
    const bool bracket = notify && changed;
    if (bracket && !accepted(instance->beginInstanceChangedAction(reason))) { return 4; }
    OfxStatus status = kOfxStatOK;
    // Assign the complete authored snapshot before any change callback reads it.
    for (const auto& update : updates) {
      if (update.kind == 1) {
        status = static_cast<OFX::Host::Param::DoubleInstance*>(update.parameter)->set(update.value);
      } else if (update.kind == 3) {
        status = static_cast<OFX::Host::Param::IntegerInstance*>(update.parameter)->set(static_cast<int>(update.value));
      } else {
        status = static_cast<OFX::Host::Param::BooleanInstance*>(update.parameter)->set(update.value == 1.0);
      }
      if (status != kOfxStatOK) { break; }
    }
    const OfxPointD scale{1.0, 1.0};
    if (bracket && status == kOfxStatOK) {
      for (const auto& update : updates) {
        if (!update.changed) { continue; }
        {
          MondrianOpenFx::ImageAccess images(frame, std::strcmp(reason, kOfxChangeUserEdited) == 0);
          status = instance->paramInstanceChangedAction(
              update.parameter->getName(), reason, frame.info.frame, scale);
        }
        if (!accepted(status) || frame.invalidImageLifetime) { break; }
      }
      if (accepted(status) && !frame.invalidImageLifetime && clipsChanged) {
        for (const char* name : {"Source", "Output"}) {
          {
            MondrianOpenFx::ImageAccess images(frame, std::strcmp(reason, kOfxChangeUserEdited) == 0);
            status = instance->clipInstanceChangedAction(name, reason, frame.info.frame, scale);
          }
          if (!accepted(status) || frame.invalidImageLifetime) { break; }
        }
      }
    }
    OfxStatus endStatus = kOfxStatOK;
    if (bracket) {
      MondrianOpenFx::ImageAccess images(frame, !frame.invalidImageLifetime && std::strcmp(reason, kOfxChangeUserEdited) == 0);
      endStatus = instance->endInstanceChangedAction(reason);
    }
    return accepted(status) && accepted(endStatus) && !frame.invalidImageLifetime ? 0 : 4;
  }

  int initialize(const char* path, const char* bundle, const char* identifier,
                 const void* input, size_t bytes, const MondrianOpenFxFrameInfo& info,
                 const MondrianOpenFxParameter* parameters, int count) {
    MondrianOpenFx::FrameBorrow borrowed(frame);
    const int loaded = loadFrame(frame, input, bytes, info);
    if (loaded != 0) { return loaded; }
    binary = std::make_unique<OFX::Host::PluginBinary>(path, bundle, &cache);
    auto* plugin = selectFilter(imageCache, *binary, identifier);
    if (!plugin) { return 3; }
    auto* descriptor = plugin->getContext(kOfxImageEffectContextFilter);
    if (!descriptor || !admitsIndependentFrames(*descriptor)) { return 7; }
    instance.reset(plugin->createInstance(kOfxImageEffectContextFilter, &frame));
    if (!instance) { return 4; }
    const int applied = applyParameters(parameters, count, false, kOfxChangeUserEdited, false);
    if (applied != 0) { return applied; }
    if (!accepted(instance->createInstanceAction())) { return 4; }
    return admitsIndependentFrames(*instance) ? 0 : 7;
  }

  int render(const void* input, void* output, size_t bytes,
             const MondrianOpenFxFrameInfo& info,
             const MondrianOpenFxParameter* parameters, int count) {
    if (failed || !output || info.width != frame.info.width ||
        info.height != frame.info.height) { return 2; }
    const auto previous = frame.info;
    if (reinterpret_cast<uintptr_t>(output) % alignof(OfxRGBAColourF) != 0 || output == input) { return 2; }
    MondrianOpenFx::FrameBorrow borrowed(frame);
    const int loaded = loadFrame(frame, input, bytes, info);
    if (loaded != 0) { return loaded; }
    frame.output = static_cast<OfxRGBAColourF*>(output);
    auto* outputClip = dynamic_cast<MondrianOpenFx::FilterClipInstance*>(instance->getClip("Output"));
    if (!outputClip) { return 5; }
    const bool clipsChanged = previous.frame_rate != info.frame_rate ||
        previous.first_frame != info.first_frame || previous.last_frame != info.last_frame ||
        previous.pixel_aspect_ratio != info.pixel_aspect_ratio;
    const int applied = applyParameters(parameters, count, true,
        previous.frame != info.frame ? kOfxChangeTime : kOfxChangeUserEdited, clipsChanged);
    if (applied != 0) { return applied; }
    if (!instance->getClipPreferences()) { return 4; }
    if (!admitsIndependentFrames(*instance)) { return 7; }
    for (const char* name : {"Source", "Output"}) {
      auto* clip = instance->getClip(name);
      if (!clip || clip->getPixelDepth() != kOfxBitDepthFloat ||
          clip->getComponents() != kOfxImageComponentRGBA) { return 7; }
    }
    // Clear the retained allocation and require a new fetch for this render.
    // Change/preference callbacks cannot make an old output appear completed.
    outputClip->beginFrame();
    const OfxPointD scale{1.0, 1.0};
    const OfxRectI window{0, 0, info.width, info.height};
    const OfxRectD region{0.0, 0.0, info.width * info.pixel_aspect_ratio,
                         static_cast<double>(info.height)};
    OfxStatus status = instance->beginRenderAction(info.frame, info.frame, 1.0,
                                                  false, scale, false, false);
    if (!accepted(status)) { return 5; }
    std::map<OFX::Host::ImageEffect::ClipInstance*, OfxRectD> rois;
    status = instance->getRegionOfInterestAction(info.frame, scale, region, rois);
    if (accepted(status)) {
      MondrianOpenFx::ImageAccess images(frame, true, true);
      status = instance->renderAction(info.frame, kOfxImageFieldBoth, window,
                                     scale, false, false, false);
    }
    bool written = status == kOfxStatOK && !frame.invalidImageLifetime && outputClip->outputFetched();
    if (written) {
      for (size_t index = 0; index < frame.pixelCount; ++index) {
        const auto& pixel = frame.output[index];
        if (!std::isfinite(pixel.r) || !std::isfinite(pixel.g) ||
            !std::isfinite(pixel.b) || !std::isfinite(pixel.a)) { written = false; break; }
      }
    }
    const auto endStatus = instance->endRenderAction(info.frame, info.frame, 1.0,
                                                    false, scale, false, false);
    return status == kOfxStatOK && written && accepted(endStatus) ? 0 : 5;
  }
};

static int describe(const char* binary, const char* bundle,
                    const char* identifier,
                    char* label, size_t labelCapacity, size_t* labelLength,
                    MondrianOpenFxParameterCallback callback, void* context)
{
  OFX::Host::PluginCache cache;
  cache.setCacheVersion("mondrian-openfx-v1");
  MondrianOpenFx::Host host;
  OFX::Host::ImageEffect::PluginCache imageEffectPluginCache(host);
  imageEffectPluginCache.registerInCache(cache);
  OFX::Host::PluginBinary selectedBinary(binary, bundle,
                                         &cache);
  auto* plugin = selectFilter(imageEffectPluginCache, selectedBinary, identifier);
  if (!plugin) { return 3; }
  auto* descriptor = plugin->getContext(kOfxImageEffectContextFilter);
  if (!descriptor || !admitsIndependentFrames(*descriptor)) { return 7; }
  const std::string& displayName = descriptor->getLabel();
  if (displayName.size() > labelCapacity) { return 7; }
  std::memcpy(label, displayName.data(), displayName.size());
  *labelLength = displayName.size();
  const auto& clips = descriptor->getClips();
  if (clips.find("Source") == clips.end() ||
      clips.find("Output") == clips.end()) { return 7; }
  for (const auto& entry : clips) {
    if (entry.first != "Source" && entry.first != "Output" &&
        !entry.second->isOptional()) { return 7; }
  }

  int count = 0;
  for (const auto* parameter : descriptor->getParamList()) {
    const std::string& type = parameter->getType();
    if (type == kOfxParamTypeGroup || type == kOfxParamTypePage ||
        type == kOfxParamTypePushButton) { continue; }
    if (++count > 4096) { return 7; }
    MondrianOpenFxParameterInfo info{};
    const std::string& name = parameter->getName();
    const std::string& label = parameter->getLabel();
    const std::string& hint = parameter->getHint();
    info.name = name.data();
    info.name_length = name.size();
    info.label = label.data();
    info.label_length = label.size();
    info.hint = hint.data();
    info.hint_length = hint.size();
    info.can_animate = parameter->getCanAnimate();
    info.secret = parameter->getSecret();
    info.enabled = parameter->getEnabled();
    const auto& properties = parameter->getProperties();
    if (type == kOfxParamTypeDouble) {
      const std::string& doubleType = parameter->getDoubleType();
      if (doubleType == kOfxParamDoubleTypePlain) {
        info.double_type = 0;
      } else if (doubleType == kOfxParamDoubleTypeScale) {
        info.double_type = 1;
      } else { return 7; }
      info.kind = 1;
      info.default_value = properties.getDoubleProperty(kOfxParamPropDefault);
      info.minimum = properties.getDoubleProperty(kOfxParamPropMin);
      info.maximum = properties.getDoubleProperty(kOfxParamPropMax);
      info.display_minimum = properties.getDoubleProperty(kOfxParamPropDisplayMin);
      info.display_maximum = properties.getDoubleProperty(kOfxParamPropDisplayMax);
      if (!std::isfinite(info.default_value) || !std::isfinite(info.minimum) ||
          !std::isfinite(info.maximum) || !std::isfinite(info.display_minimum) ||
          !std::isfinite(info.display_maximum) || info.minimum > info.maximum ||
          info.display_minimum > info.display_maximum) { return 7; }
    } else if (type == kOfxParamTypeInteger) {
      info.kind = 3;
      info.double_type = -1;
      info.default_value = properties.getIntProperty(kOfxParamPropDefault);
      info.minimum = properties.getIntProperty(kOfxParamPropMin);
      info.maximum = properties.getIntProperty(kOfxParamPropMax);
      info.display_minimum = properties.getIntProperty(kOfxParamPropDisplayMin);
      info.display_maximum = properties.getIntProperty(kOfxParamPropDisplayMax);
      if (info.minimum > info.maximum || info.display_minimum > info.display_maximum || info.default_value < info.minimum || info.default_value > info.maximum) { return 7; }
    } else if (type == kOfxParamTypeBoolean) {
      info.kind = 2;
      info.double_type = -1;
      const int value = properties.getIntProperty(kOfxParamPropDefault);
      if (value != 0 && value != 1) { return 7; }
      info.default_value = value;
      info.minimum = 0.0;
      info.maximum = 1.0;
      info.display_minimum = 0.0;
      info.display_maximum = 1.0;
    } else {
      std::cerr << "unsupported parameter type: " << type << std::endl;
      return 7;
    }
    if (type == kOfxParamTypeDouble || type == kOfxParamTypeInteger) {
      const auto& interpolation = properties.getStringProperty(kOfxParamInterpType);
      if (interpolation == kOfxParamInterpTypeConstantStep) { info.preferred_interpolation = 0; }
      else if (interpolation == kOfxParamInterpTypeLinear) { info.preferred_interpolation = 1; }
      else if (interpolation == kOfxParamInterpTypeSmooth) { info.preferred_interpolation = 2; }
      else { return 7; }
    }
    if (callback(context, &info) != 0) { return 8; }
  }
  return 0;
}

extern "C" int mondrian_openfx_describe(
    const char* binary, const char* bundle, const char* identifier,
    char* label, size_t labelCapacity, size_t* labelLength,
    MondrianOpenFxParameterCallback callback, void* context) noexcept
{
  try {
    if (!binary || !bundle || !identifier || !label || !labelLength ||
        !callback || !context) { return 2; }
    return describe(binary, bundle, identifier, label, labelCapacity,
                    labelLength, callback, context);
  } catch (const std::exception& error) {
    std::cerr << "OpenFX description failed: " << error.what() << std::endl;
    return 6;
  } catch (...) {
    return 6;
  }
}

extern "C" int mondrian_openfx_session_create(
    const char* binary, const char* bundle, const char* identifier,
    const void* input, size_t bytes, const MondrianOpenFxFrameInfo* info,
    const MondrianOpenFxParameter* parameters, int count,
    MondrianOpenFxSession** output) noexcept
{
  if (!output) { return 2; }
  *output = nullptr;
  try {
    if (!binary || !bundle || !identifier || !info) { return 2; }
    auto session = std::make_unique<MondrianOpenFxSession>();
    const int status = session->initialize(binary, bundle, identifier, input, bytes,
                                          *info, parameters, count);
    if (status == 0) { *output = session.release(); }
    return status;
  } catch (const std::exception& error) {
    std::cerr << "OpenFX session creation failed: " << error.what() << std::endl;
    return 6;
  } catch (...) { return 6; }
}

extern "C" int mondrian_openfx_session_render(
    MondrianOpenFxSession* session, const void* input, void* output, size_t bytes,
    const MondrianOpenFxFrameInfo* info, const MondrianOpenFxParameter* parameters,
    int count) noexcept
{
  if (!session) { return 2; }
  try {
    const int status = info ? session->render(input, output, bytes, *info, parameters, count) : 2;
    if (status != 0) { session->failed = true; }
    return status;
  } catch (const std::exception& error) {
    session->failed = true;
    std::cerr << "OpenFX session render failed: " << error.what() << std::endl;
    return 6;
  } catch (...) { session->failed = true; return 6; }
}

extern "C" void mondrian_openfx_session_destroy(MondrianOpenFxSession* session) noexcept
{
  delete session;
}
