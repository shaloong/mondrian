// Copyright OpenFX and contributors to the OpenFX project.
// SPDX-License-Identifier: BSD-3-Clause


#include <iostream>
#include <fstream>
#include <cmath>
#include <filesystem>
#include <set>
#include <memory>

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

static int run(const char* binary, const char* bundle, const char* identifier,
               const char* inputPath, const char* outputPath,
               const MondrianOpenFxFrameInfo& info,
               const MondrianOpenFxParameter* parameters, int parameterCount)
{
  MondrianOpenFx::gWidth = info.width;
  MondrianOpenFx::gHeight = info.height;
  if (MondrianOpenFx::gWidth <= 0 || MondrianOpenFx::gHeight <= 0 || MondrianOpenFx::gWidth > 8192 || MondrianOpenFx::gHeight > 8192) {
    return 2;
  }
  if (!std::isfinite(info.frame) || !std::isfinite(info.frame_rate) ||
      !std::isfinite(info.first_frame) || !std::isfinite(info.last_frame) ||
      !std::isfinite(info.pixel_aspect_ratio) || info.frame_rate <= 0.0 ||
      info.pixel_aspect_ratio <= 0.0 || info.first_frame > info.frame ||
      info.frame > info.last_frame || parameterCount < 0 || parameterCount > 4096 ||
      (parameterCount != 0 && parameters == nullptr)) { return 2; }
  MondrianOpenFx::gFrame = info.frame;
  MondrianOpenFx::gFrameRate = info.frame_rate;
  MondrianOpenFx::gFirstFrame = info.first_frame;
  MondrianOpenFx::gLastFrame = info.last_frame;
  MondrianOpenFx::gPixelAspectRatio = info.pixel_aspect_ratio;
  const double frame = info.frame;
  const size_t pixels = size_t(MondrianOpenFx::gWidth) * size_t(MondrianOpenFx::gHeight);
  MondrianOpenFx::gInput.resize(pixels);
  std::ifstream input(std::filesystem::u8path(inputPath), std::ios::binary | std::ios::ate);
  if (!input || input.tellg() != std::streamoff(pixels * sizeof(OfxRGBAColourF))) {
    std::cerr << "invalid input size" << std::endl;
    return 2;
  }
  input.seekg(0);
  input.read(reinterpret_cast<char*>(MondrianOpenFx::gInput.data()), pixels * sizeof(OfxRGBAColourF));
  if (!input) { return 2; }
  for (const auto& pixel : MondrianOpenFx::gInput) {
    if (!std::isfinite(pixel.r) || !std::isfinite(pixel.g) ||
        !std::isfinite(pixel.b) || !std::isfinite(pixel.a)) { return 2; }
  }
  OFX::Host::PluginCache::getPluginCache()->setCacheVersion("mondrian-openfx-v1");
  MondrianOpenFx::Host host;
  OFX::Host::ImageEffect::PluginCache imageEffectPluginCache(host);
  imageEffectPluginCache.registerInCache(*OFX::Host::PluginCache::getPluginCache());
  OFX::Host::PluginBinary selectedBinary(binary, bundle, OFX::Host::PluginCache::getPluginCache());
  auto* plugin = selectFilter(imageEffectPluginCache, selectedBinary, identifier);
  if (!plugin) { return 3; }
  std::unique_ptr<OFX::Host::ImageEffect::Instance> instance(
      plugin->createInstance(kOfxImageEffectContextFilter, nullptr));
  if (!instance) { return 4; }

  std::set<std::string> authoredNames;
  for (int index = 0; index < parameterCount; ++index) {
    const auto& authored = parameters[index];
    if (!authored.name || !std::isfinite(authored.value) ||
        !authoredNames.insert(authored.name).second) { return 4; }
    auto* parameter = instance->getParam(authored.name);
    if (!parameter) { return 4; }
    if (authored.kind == 1) {
      auto* value = dynamic_cast<OFX::Host::Param::DoubleInstance*>(parameter);
      if (!value) { return 4; }
      const auto& properties = value->getProperties();
      if (authored.value < properties.getDoubleProperty(kOfxParamPropMin) ||
          authored.value > properties.getDoubleProperty(kOfxParamPropMax) ||
          value->set(authored.value) != kOfxStatOK) { return 4; }
    } else if (authored.kind == 2) {
      auto* value = dynamic_cast<OFX::Host::Param::BooleanInstance*>(parameter);
      if (!value || (authored.value != 0.0 && authored.value != 1.0) ||
          value->set(authored.value == 1.0) != kOfxStatOK) { return 4; }
    } else { return 4; }
  }
  for (const auto& entry : instance->getParams()) {
    auto* parameter = entry.second;
    const std::string& type = parameter->getType();
    if (type == kOfxParamTypeGroup || type == kOfxParamTypePage ||
        type == kOfxParamTypePushButton) { continue; }
    if (!dynamic_cast<OFX::Host::Param::DoubleInstance*>(parameter) &&
        !dynamic_cast<OFX::Host::Param::BooleanInstance*>(parameter)) {
      std::cerr << "unsupported parameter type: " << type << std::endl;
      return 7;
    }
  }

  // Authored values must be set before the plugin's CreateInstance action.
  OfxStatus status = instance->createInstanceAction();
  if (status != kOfxStatOK && status != kOfxStatReplyDefault) { return 4; }
  if (!instance->getClipPreferences()) { return 4; }
  for (const char* name : {"Source", "Output"}) {
    auto* clip = instance->getClip(name);
    if (!clip || clip->getPixelDepth() != kOfxBitDepthFloat ||
        clip->getComponents() != kOfxImageComponentRGBA) { return 7; }
  }
  auto* outputClip = dynamic_cast<MondrianOpenFx::FilterClipInstance*>(instance->getClip("Output"));
  if (!outputClip) { return 5; }

  const OfxPointD renderScale{1.0, 1.0};
  const OfxRectI renderWindow{0, 0, info.width, info.height};
  const OfxRectD regionOfInterest{0.0, 0.0,
      info.width * info.pixel_aspect_ratio, static_cast<double>(info.height)};
  status = instance->beginRenderAction(frame, frame, 1.0, false, renderScale,
                                       true, false);
  if (status != kOfxStatOK && status != kOfxStatReplyDefault) { return 5; }

  std::map<OFX::Host::ImageEffect::ClipInstance*, OfxRectD> rois;
  status = instance->getRegionOfInterestAction(frame, renderScale,
                                               regionOfInterest, rois);
  if (status == kOfxStatOK || status == kOfxStatReplyDefault) {
    status = instance->renderAction(frame, kOfxImageFieldBoth, renderWindow,
                                    renderScale, true, false, false);
  }
  bool outputWritten = false;
  if (status == kOfxStatOK) {
    if (auto* image = outputClip->getOutputImage()) {
      std::ofstream output(std::filesystem::u8path(outputPath), std::ios::binary);
      output.write(reinterpret_cast<const char*>(image->rawData()),
                   pixels * sizeof(OfxRGBAColourF));
      outputWritten = static_cast<bool>(output);
    }
  }
  const OfxStatus endStatus = instance->endRenderAction(frame, frame, 1.0,
      false, renderScale, true, false);
  if (status != kOfxStatOK || !outputWritten ||
      (endStatus != kOfxStatOK && endStatus != kOfxStatReplyDefault)) { return 5; }
  return 0;
}

static int describe(const char* binary, const char* bundle,
                    const char* identifier,
                    MondrianOpenFxParameterCallback callback, void* context)
{
  OFX::Host::PluginCache::getPluginCache()->setCacheVersion("mondrian-openfx-v1");
  MondrianOpenFx::Host host;
  OFX::Host::ImageEffect::PluginCache imageEffectPluginCache(host);
  imageEffectPluginCache.registerInCache(*OFX::Host::PluginCache::getPluginCache());
  OFX::Host::PluginBinary selectedBinary(binary, bundle,
                                         OFX::Host::PluginCache::getPluginCache());
  auto* plugin = selectFilter(imageEffectPluginCache, selectedBinary, identifier);
  if (!plugin) { return 3; }
  auto* descriptor = plugin->getContext(kOfxImageEffectContextFilter);
  if (!descriptor) { return 7; }
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
    if (callback(context, &info) != 0) { return 8; }
  }
  return 0;
}

extern "C" int mondrian_openfx_describe(
    const char* binary, const char* bundle, const char* identifier,
    MondrianOpenFxParameterCallback callback, void* context) noexcept
{
  try {
    if (!binary || !bundle || !identifier || !callback || !context) { return 2; }
    return describe(binary, bundle, identifier, callback, context);
  } catch (const std::exception& error) {
    std::cerr << "OpenFX description failed: " << error.what() << std::endl;
    return 6;
  } catch (...) {
    return 6;
  }
}

extern "C" int mondrian_openfx_render(
  const char* binary,
  const char* bundle,
  const char* identifier,
  const char* input,
  const char* output,
  const MondrianOpenFxFrameInfo* frameInfo,
  const MondrianOpenFxParameter* parameters,
  int parameterCount) noexcept
{
  try {
    if (!binary || !bundle || !identifier || !input || !output || !frameInfo) { return 2; }
    return run(binary, bundle, identifier, input, output, *frameInfo,
               parameters, parameterCount);
  } catch (const std::exception& error) {
    std::cerr << "OpenFX host exception: " << error.what() << std::endl;
    return 6;
  } catch (...) {
    return 6;
  }
}
