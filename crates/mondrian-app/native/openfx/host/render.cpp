// Copyright OpenFX and contributors to the OpenFX project.
// SPDX-License-Identifier: BSD-3-Clause


#include <iostream>
#include <cmath>
#include <set>
#include <memory>
#include <cstring>
#include <cstdint>
#include <algorithm>
#include <array>
#include <limits>
#include <type_traits>

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

static int stringMode(const OFX::Host::Property::Set& properties) {
  const auto& mode = properties.getStringProperty(kOfxParamPropStringMode);
  if (mode == kOfxParamStringIsSingleLine) { return 1; }
  if (mode == kOfxParamStringIsMultiLine) { return 2; }
  if (mode == kOfxParamStringIsFilePath) { return 3; }
  if (mode == kOfxParamStringIsDirectoryPath) { return 4; }
  if (mode == kOfxParamStringIsLabel) { return 5; }
  if (mode == kOfxParamStringIsRichTextFormat) { return 6; }
  return 0;
}

static bool admitsParameterDefaults(const OFX::Host::ImageEffect::Descriptor& descriptor) {
  size_t bytes = 0;
  if (descriptor.getParamList().size() > 4096) { return false; }
  for (const auto* parameter : descriptor.getParamList()) {
    if (parameter->getType() != kOfxParamTypeString && parameter->getType() != kOfxParamTypeStrChoice) { continue; }
    const auto& props = parameter->getProperties();
    if (parameter->getType() == kOfxParamTypeString) {
      const int mode = stringMode(props);
      if (mode == 0) { return false; }
      if (mode == 3 || mode == 4) {
        const int exists = props.getIntProperty(kOfxParamPropStringFilePathExists);
        if (exists != 0 && exists != 1) { return false; }
      }
    }
    const auto& value = props.getStringProperty(kOfxParamPropDefault);
    if (value.size() > MONDRIAN_OPENFX_MAX_STRING_BYTES || value.find('\0') != std::string::npos ||
        value.size() > MONDRIAN_OPENFX_MAX_PARAMETER_STRING_BYTES - bytes) { return false; }
    bytes += value.size();
  }
  return true;
}

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

struct NumericLayout { int kind; int dimensions; bool integer; };
static NumericLayout numericLayout(const std::string& type) {
  if (type == kOfxParamTypeDouble) { return {1, 1, false}; }
  if (type == kOfxParamTypeInteger) { return {3, 1, true}; }
  if (type == kOfxParamTypeDouble2D) { return {7, 2, false}; }
  if (type == kOfxParamTypeDouble3D) { return {8, 3, false}; }
  if (type == kOfxParamTypeRGB) { return {9, 3, false}; }
  if (type == kOfxParamTypeRGBA) { return {10, 4, false}; }
  if (type == kOfxParamTypeInteger2D) { return {11, 2, true}; }
  if (type == kOfxParamTypeInteger3D) { return {12, 3, true}; }
  return {0, 0, false};
}

template<class Base, class T, size_t N>
static OfxStatus readTuple(OFX::Host::Param::Instance* parameter, std::array<double, 4>& output) {
  auto* tuple = dynamic_cast<Base*>(parameter);
  if (!tuple) { return kOfxStatErrValue; }
  std::array<T, N> value{};
  OfxStatus status;
  if constexpr (N == 2) { status = tuple->get(value[0], value[1]); }
  else if constexpr (N == 3) { status = tuple->get(value[0], value[1], value[2]); }
  else { status = tuple->get(value[0], value[1], value[2], value[3]); }
  if (status != kOfxStatOK) { return status; }
  for (size_t index = 0; index < N; ++index) { output[index] = value[index]; }
  return kOfxStatOK;
}

template<class Base, class T, size_t N>
static OfxStatus writeTuple(OFX::Host::Param::Instance* parameter, const std::array<double, 4>& values) {
  auto* tuple = dynamic_cast<Base*>(parameter);
  if (!tuple) { return kOfxStatErrValue; }
  std::array<T, N> value{};
  for (size_t index = 0; index < N; ++index) {
    if (!std::isfinite(values[index])) { return kOfxStatErrValue; }
    if constexpr (std::is_same_v<T, int>) {
      if (std::trunc(values[index]) != values[index] || values[index] < std::numeric_limits<int>::min() ||
          values[index] > std::numeric_limits<int>::max()) { return kOfxStatErrValue; }
    }
    value[index] = static_cast<T>(values[index]);
  }
  if constexpr (N == 2) { return tuple->set(value[0], value[1]); }
  else if constexpr (N == 3) { return tuple->set(value[0], value[1], value[2]); }
  else { return tuple->set(value[0], value[1], value[2], value[3]); }
}

static OfxStatus readCompound(OFX::Host::Param::Instance* parameter, int kind, std::array<double, 4>& values) {
  switch (kind) {
    case 7: return readTuple<OFX::Host::Param::Double2DInstance, double, 2>(parameter, values);
    case 8: return readTuple<OFX::Host::Param::Double3DInstance, double, 3>(parameter, values);
    case 9: return readTuple<OFX::Host::Param::RGBInstance, double, 3>(parameter, values);
    case 10: return readTuple<OFX::Host::Param::RGBAInstance, double, 4>(parameter, values);
    case 11: return readTuple<OFX::Host::Param::Integer2DInstance, int, 2>(parameter, values);
    case 12: return readTuple<OFX::Host::Param::Integer3DInstance, int, 3>(parameter, values);
    default: return kOfxStatErrValue;
  }
}

static OfxStatus writeCompound(OFX::Host::Param::Instance* parameter, int kind, const std::array<double, 4>& values) {
  switch (kind) {
    case 7: return writeTuple<OFX::Host::Param::Double2DInstance, double, 2>(parameter, values);
    case 8: return writeTuple<OFX::Host::Param::Double3DInstance, double, 3>(parameter, values);
    case 9: return writeTuple<OFX::Host::Param::RGBInstance, double, 3>(parameter, values);
    case 10: return writeTuple<OFX::Host::Param::RGBAInstance, double, 4>(parameter, values);
    case 11: return writeTuple<OFX::Host::Param::Integer2DInstance, int, 2>(parameter, values);
    case 12: return writeTuple<OFX::Host::Param::Integer3DInstance, int, 3>(parameter, values);
    default: return kOfxStatErrValue;
  }
}

struct ParameterUpdate {
  OFX::Host::Param::Instance* parameter;
  int kind;
  std::array<double, 4> value;
  bool changed;
  std::string stringValue{};
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
      if (!value.name || (value.query_mode != 0 && value.query_mode != 1) || !std::all_of(value.value, value.value + 4, [](double channel) { return std::isfinite(channel); }) ||
          !authored.emplace(value.name, &value).second ||
          !instance->getParam(value.name)) { return 4; }
    }
    std::set<std::string> frameSamples;
    std::vector<ParameterUpdate> updates;
    size_t stringBytes = 0;
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
        const double target = value ? value->value[0] :
            properties.getDoubleProperty(kOfxParamPropDefault);
        double current = 0.0;
        if ((value && value->kind != 1) || !std::isfinite(target) ||
            target < properties.getDoubleProperty(kOfxParamPropMin) ||
            target > properties.getDoubleProperty(kOfxParamPropMax) ||
            scalar->get(current) != kOfxStatOK) { return 4; }
        updates.push_back({parameter, 1, {target}, current != target});
      } else if (auto* scalar = dynamic_cast<OFX::Host::Param::BooleanInstance*>(parameter)) {
        const double target = value ? value->value[0] :
            properties.getIntProperty(kOfxParamPropDefault);
        bool current = false;
        if ((value && value->kind != 2) || (target != 0.0 && target != 1.0) ||
            scalar->get(current) != kOfxStatOK) { return 4; }
        updates.push_back({parameter, 2, {target}, current != (target == 1.0)});
      } else if (auto* scalar = dynamic_cast<OFX::Host::Param::IntegerInstance*>(parameter)) {
        const double target = value ? value->value[0] : properties.getIntProperty(kOfxParamPropDefault);
        int current = 0;
        if ((value && value->kind != 3) || !std::isfinite(target) || std::trunc(target) != target ||
            target < properties.getIntProperty(kOfxParamPropMin) || target > properties.getIntProperty(kOfxParamPropMax) ||
            scalar->get(current) != kOfxStatOK) { return 4; }
        updates.push_back({parameter, 3, {target}, current != target});
      } else if (auto* choice = dynamic_cast<OFX::Host::Param::ChoiceInstance*>(parameter)) {
        const double target = value ? value->value[0] : properties.getIntProperty(kOfxParamPropDefault);
        const int options = properties.getDimension(kOfxParamPropChoiceOption);
        int current = 0;
        if ((value && value->kind != 4) || !std::isfinite(target) || std::trunc(target) != target ||
            target < 0 || target >= options || choice->get(current) != kOfxStatOK) { return 4; }
        updates.push_back({parameter, 4, {target}, current != target});
      } else if (numericLayout(type).kind >= 7) {
        const auto layout = numericLayout(type);
        std::array<double, 4> target{}, current{};
        if ((value && value->kind != layout.kind) || readCompound(parameter, layout.kind, current) != kOfxStatOK) { return 4; }
        for (int index = 0; index < layout.dimensions; ++index) {
          const double minimum = layout.integer ? properties.getIntProperty(kOfxParamPropMin, index)
                                                : properties.getDoubleProperty(kOfxParamPropMin, index);
          const double maximum = layout.integer ? properties.getIntProperty(kOfxParamPropMax, index)
                                                : properties.getDoubleProperty(kOfxParamPropMax, index);
          target[index] = value ? value->value[index] : layout.integer
              ? properties.getIntProperty(kOfxParamPropDefault, index)
              : properties.getDoubleProperty(kOfxParamPropDefault, index);
          if (!std::isfinite(target[index]) || target[index] < minimum || target[index] > maximum ||
              (layout.integer && std::trunc(target[index]) != target[index])) { return 4; }
        }
        updates.push_back({parameter, layout.kind, target, current != target});
      } else if (type == kOfxParamTypeStrChoice) {
        auto* choice = dynamic_cast<OFX::Host::Param::StringInstance*>(parameter);
        if (!choice || (value && (value->kind != 5 || !value->string_value || value->string_length > 256))) { return 4; }
        std::string target = value ? std::string(value->string_value, value->string_length)
                                  : properties.getStringProperty(kOfxParamPropDefault);
        const int options = properties.getDimension(kOfxParamPropChoiceEnum);
        if (options < 1 || options != properties.getDimension(kOfxParamPropChoiceOption)) { return 4; }
        if (!value && target.empty()) { target = properties.getStringProperty(kOfxParamPropChoiceEnum, 0); }
        bool found = false;
        for (int index = 0; index < options; ++index) {
          found = found || target == properties.getStringProperty(kOfxParamPropChoiceEnum, index);
        }
        std::string current;
        if (!found || target.size() > 256 || choice->get(current) != kOfxStatOK) { return 4; }
        updates.push_back({parameter, 5, {0.0}, current != target, std::move(target)});
      } else if (type == kOfxParamTypeString) {
        auto* text = dynamic_cast<OFX::Host::Param::StringInstance*>(parameter);
        if (!text || (value && (value->kind != 6 || !value->string_value ||
            value->string_length > MONDRIAN_OPENFX_MAX_STRING_BYTES))) { return 4; }
        std::string target = value ? std::string(value->string_value, value->string_length)
                                  : properties.getStringProperty(kOfxParamPropDefault);
        std::string current;
        if (target.size() > MONDRIAN_OPENFX_MAX_STRING_BYTES || target.find('\0') != std::string::npos ||
            text->get(current) != kOfxStatOK) { return 4; }
        updates.push_back({parameter, 6, {0.0}, current != target, std::move(target)});
      } else { return 7; }
      if (updates.back().kind == 5 || updates.back().kind == 6) {
        const size_t bytes = updates.back().stringValue.size();
        if (bytes > MONDRIAN_OPENFX_MAX_PARAMETER_STRING_BYTES - stringBytes) { return 4; }
        stringBytes += bytes;
      }
      const bool frameSample = value && value->query_mode == 1;
      if (frameSample) { frameSamples.insert(entry.first); }
      updates.back().changed = updates.back().changed ||
          (frame.frameSampleParameters.count(entry.first) != 0) != frameSample;
      changed = changed || updates.back().changed;
    }
    // Validation above has neither assigned values nor invoked plugin actions.
    // Once native state can change, retain failure until a complete render
    // succeeds; callback failures cannot safely reuse a partially changed instance.
    if (notify) { failed = true; }
    frame.frameSampleParameters = std::move(frameSamples);
    const bool bracket = notify && changed;
    if (bracket && !accepted(instance->beginInstanceChangedAction(reason))) { return 4; }
    OfxStatus status = kOfxStatOK;
    // Assign the complete authored snapshot before any change callback reads it.
    for (const auto& update : updates) {
      if (update.kind == 1) {
        status = static_cast<OFX::Host::Param::DoubleInstance*>(update.parameter)->set(update.value[0]);
      } else if (update.kind == 3) {
        status = static_cast<OFX::Host::Param::IntegerInstance*>(update.parameter)->set(static_cast<int>(update.value[0]));
      } else if (update.kind == 4) {
        status = static_cast<OFX::Host::Param::ChoiceInstance*>(update.parameter)->set(static_cast<int>(update.value[0]));
      } else if (update.kind == 5 || update.kind == 6) {
        status = static_cast<OFX::Host::Param::StringInstance*>(update.parameter)->set(update.stringValue.c_str());
      } else if (update.kind >= 7) {
        status = writeCompound(update.parameter, update.kind, update.value);
      } else {
        status = static_cast<OFX::Host::Param::BooleanInstance*>(update.parameter)->set(update.value[0] == 1.0);
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
    if (!descriptor || !admitsIndependentFrames(*descriptor) || !admitsParameterDefaults(*descriptor)) { return 7; }
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
    if (failed) { return 2; }
    // A prepared session has a fixed image-storage contract. Violating it is
    // a transport fault, unlike an author snapshot rejected before assignment.
    if (!output || info.width != frame.info.width || info.height != frame.info.height ||
        reinterpret_cast<uintptr_t>(output) % alignof(OfxRGBAColourF) != 0 || output == input) {
      failed = true;
      return 2;
    }
    const auto previous = frame.info;
    MondrianOpenFx::FrameBorrow borrowed(frame);
    const int loaded = loadFrame(frame, input, bytes, info);
    if (loaded != 0) { failed = true; return loaded; }
    frame.output = static_cast<OfxRGBAColourF*>(output);
    auto* outputClip = dynamic_cast<MondrianOpenFx::FilterClipInstance*>(instance->getClip("Output"));
    if (!outputClip) { failed = true; return 5; }
    const bool clipsChanged = previous.frame_rate != info.frame_rate ||
        previous.first_frame != info.first_frame || previous.last_frame != info.last_frame ||
        previous.pixel_aspect_ratio != info.pixel_aspect_ratio;
    const int applied = applyParameters(parameters, count, true,
        previous.frame != info.frame ? kOfxChangeTime : kOfxChangeUserEdited, clipsChanged);
    if (applied != 0) {
      // A rejected snapshot did not reach the commit boundary. Restore the
      // last accepted timing facts so the next request observes the same state.
      if (!failed) { frame.info = previous; }
      return applied;
    }
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
    const bool completed = status == kOfxStatOK && written && accepted(endStatus);
    if (completed) { failed = false; }
    return completed ? 0 : 5;
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
  if (!descriptor || !admitsIndependentFrames(*descriptor) || !admitsParameterDefaults(*descriptor)) { return 7; }
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
    info.string_file_path_exists = -1;
    std::vector<MondrianOpenFxChoiceOption> choices;
    // All borrowed description storage must outlive the callback below.
    std::string stringDefault;
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
    const auto layout = numericLayout(type);
    if (layout.kind != 0) {
      info.kind = layout.kind;
      info.double_type = -1;
      if (layout.kind == 1 || layout.kind == 7 || layout.kind == 8) {
        const auto& doubleType = parameter->getDoubleType();
        if (doubleType == kOfxParamDoubleTypePlain) { info.double_type = 0; }
        else if (doubleType == kOfxParamDoubleTypeScale) { info.double_type = 1; }
        else { return 7; }
      }
      const char* names[] = {kOfxParamPropDefault, kOfxParamPropMin, kOfxParamPropMax,
                             kOfxParamPropDisplayMin, kOfxParamPropDisplayMax};
      double* values[] = {info.default_value, info.minimum, info.maximum, info.display_minimum, info.display_maximum};
      for (int index = 0; index < layout.dimensions; ++index) {
        for (int field = 0; field < 5; ++field) {
          values[field][index] = layout.integer ? properties.getIntProperty(names[field], index)
                                               : properties.getDoubleProperty(names[field], index);
          if (!std::isfinite(values[field][index])) { return 7; }
        }
        if (info.minimum[index] > info.maximum[index] || info.display_minimum[index] > info.display_maximum[index] ||
            info.default_value[index] < info.minimum[index] || info.default_value[index] > info.maximum[index]) { return 7; }
      }
    } else if (type == kOfxParamTypeBoolean) {
      info.kind = 2;
      info.double_type = -1;
      const int value = properties.getIntProperty(kOfxParamPropDefault);
      if (value != 0 && value != 1) { return 7; }
      info.default_value[0] = value;
      info.minimum[0] = 0.0;
      info.maximum[0] = 1.0;
      info.display_minimum[0] = 0.0;
      info.display_maximum[0] = 1.0;
    } else if (type == kOfxParamTypeString) {
      info.kind = 6;
      info.double_type = -1;
      info.string_mode = stringMode(properties);
      if (info.string_mode == 0) { return 7; }
      if (info.string_mode == 3 || info.string_mode == 4) {
        info.string_file_path_exists = properties.getIntProperty(kOfxParamPropStringFilePathExists);
        if (info.string_file_path_exists != 0 && info.string_file_path_exists != 1) { return 7; }
      }
      stringDefault = properties.getStringProperty(kOfxParamPropDefault);
      if (stringDefault.size() > MONDRIAN_OPENFX_MAX_STRING_BYTES ||
          stringDefault.find('\0') != std::string::npos) { return 7; }
      info.string_default = stringDefault.data();
      info.string_default_length = stringDefault.size();
    } else if (type == kOfxParamTypeChoice || type == kOfxParamTypeStrChoice) {
      const bool stringChoice = type == kOfxParamTypeStrChoice;
      info.kind = stringChoice ? 5 : 4;
      info.double_type = -1;
      if (!stringChoice) { info.default_value[0] = properties.getIntProperty(kOfxParamPropDefault); }
      if (stringChoice) { stringDefault = properties.getStringProperty(kOfxParamPropDefault); }
      const int options = properties.getDimension(kOfxParamPropChoiceOption);
      const int ranks = properties.getDimension(kOfxParamPropChoiceOrder);
      if (options < 1 || options > 4096 || (ranks != 0 && ranks != options) ||
          (!stringChoice && (info.default_value[0] < 0 || info.default_value[0] >= options)) ||
          (stringChoice && properties.getDimension(kOfxParamPropChoiceEnum) != options)) { return 7; }
      if (stringChoice && stringDefault.empty()) {
        stringDefault = properties.getStringProperty(kOfxParamPropChoiceEnum, 0);
      }
      bool defaultFound = !stringChoice;
      std::set<std::string> seenKeys;
      std::set<int> seenRanks;
      choices.reserve(options);
      for (int index = 0; index < options; ++index) {
        const auto& caption = properties.getStringProperty(kOfxParamPropChoiceOption, index);
        if (caption.empty() || caption.size() > 256) { return 7; }
        const int rank = ranks ? properties.getIntProperty(kOfxParamPropChoiceOrder, index) : 0;
        if (ranks && !seenRanks.insert(rank).second) { return 7; }
        const auto& key = properties.getStringProperty(stringChoice ? kOfxParamPropChoiceEnum : kOfxParamPropChoiceOption, index);
        if (stringChoice && (key.empty() || key.size() > 256 || !seenKeys.insert(key).second)) { return 7; }
        defaultFound = defaultFound || stringDefault == key;
        choices.push_back({caption.data(), caption.size(), ranks ? 1 : 0, rank,
                           stringChoice ? key.data() : nullptr, stringChoice ? key.size() : 0});
      }
      if (!defaultFound) { return 7; }
      if (stringChoice) {
        info.string_default = stringDefault.data();
        info.string_default_length = stringDefault.size();
      }
      info.choices = choices.data();
      info.choice_count = choices.size();
    } else {
      std::cerr << "unsupported parameter type: " << type << std::endl;
      return 7;
    }
    if (layout.kind != 0) {
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
    if (!info) { session->failed = true; return 2; }
    return session->render(input, output, bytes, *info, parameters, count);
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
