// Copyright OpenFX and contributors to the OpenFX project.
// SPDX-License-Identifier: BSD-3-Clause


#include <iostream>
#include <fstream>
#include <cmath>
#include <filesystem>
#include <set>

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
  // set the version label in the global cache
  OFX::Host::PluginCache::getPluginCache()->setCacheVersion("mondrian-openfx-v1");

  // create our derived image effect host which provides
  // a factory to make plugin instances and acts
  // as a description of the host application
  MondrianOpenFx::Host myHost;

  // make an image effect plugin cache. This is what knows about
  // all the plugins.
  OFX::Host::ImageEffect::PluginCache imageEffectPluginCache(myHost);

  // register the image effect cache with the global plugin cache
  imageEffectPluginCache.registerInCache(*OFX::Host::PluginCache::getPluginCache());

  OFX::Host::PluginBinary selectedBinary(binary, bundle, OFX::Host::PluginCache::getPluginCache());
  OFX::Host::ImageEffect::ImageEffectPlugin* plugin = nullptr;
  for (int i = 0; i < selectedBinary.getNPlugins(); ++i) {
    OFX::Host::Plugin& candidate = selectedBinary.getPlugin(i);
    if (candidate.getIdentifier() == identifier) {
      imageEffectPluginCache.loadFromPlugin(&candidate);
      std::string reason;
      if (!imageEffectPluginCache.pluginSupported(&candidate, reason)) {
        std::cerr << reason << std::endl;
        return 3;
      }
      imageEffectPluginCache.confirmPlugin(&candidate);
      plugin = imageEffectPluginCache.getPluginById(candidate.getIdentifier());
      break;
    }
  }

  if (!plugin) { return 3; }

  if(plugin) {
    // create an instance of it as a filter
    // the first arg is the context, the second is client data we are allowed to pass down the call chain

    std::unique_ptr<OFX::Host::ImageEffect::Instance> instance(plugin->createInstance(kOfxImageEffectContextFilter, NULL));

    if (!instance) { return 4; }

    if(instance)
    {
        OfxStatus stat;

      std::set<std::string> authoredNames;
      for (int index = 0; index < parameterCount; ++index) {
        const auto& authored = parameters[index];
        if (!authored.name || !std::isfinite(authored.value) ||
            !authoredNames.insert(authored.name).second) { return 4; }
        auto* parameter = instance->getParam(authored.name);
        if (!parameter) { return 4; }
        if (authored.kind == 1) {
          auto* value = dynamic_cast<OFX::Host::Param::DoubleInstance*>(parameter);
          if (!value || value->set(authored.value) != kOfxStatOK) { return 4; }
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

      // now we need to call the create instance action. Only call this once you have initialised all the params
      // and clips to their correct values. So if you are loading a saved plugin state, set up your params from
      // that state, _then_ call create instance.
      stat = instance->createInstanceAction();
      if (stat != kOfxStatOK && stat != kOfxStatReplyDefault) { return 4; }

      // now we need to to call getClipPreferences on the instance so that it does the clip component/depth
      // logic and caches away the components and depth on each clip.
      bool ok = instance->getClipPreferences();
      if (!ok) { return 4; }
      for (const char* name : {"Source", "Output"}) {
        auto* clip = instance->getClip(name);
        if (!clip || clip->getPixelDepth() != kOfxBitDepthFloat ||
            clip->getComponents() != kOfxImageComponentRGBA) {
          return 7;
        }
      }

      // current render scale of 1
      OfxPointD renderScale;
      renderScale.x = renderScale.y = 1.0;

      // The render window is in pixel coordinates
      // ie: render scale and a PAR of not 1
      OfxRectI  renderWindow;
      renderWindow.x1 = renderWindow.y1 = 0;
      renderWindow.x2 = MondrianOpenFx::gWidth;
      renderWindow.y2 = MondrianOpenFx::gHeight;

      /// RoI is in canonical coords,
      OfxRectD  regionOfInterest;
      regionOfInterest.x1 = regionOfInterest.y1 = 0;
      regionOfInterest.x2 = renderWindow.x2 * instance->getProjectPixelAspectRatio();
      regionOfInterest.y2 = MondrianOpenFx::gHeight;

      // say we are about to render a bunch of frames
      stat = instance->beginRenderAction(frame, frame, 1.0, false, renderScale, /*sequential=*/true, /*interactive=*/false
                                         );
      if (stat != kOfxStatOK && stat != kOfxStatReplyDefault) { return 5; }

      // get the output clip
      MondrianOpenFx::FilterClipInstance* outputClip = dynamic_cast<MondrianOpenFx::FilterClipInstance*>(instance->getClip("Output"));
      if (!outputClip) { return 5; }

      {
        // call get region of interest on each of the inputs

        // get the RoI for each input clip
        // the regions of interest for each input clip are returned in a std::map
        // on a real host, these will be the regions of each input clip that the
        // effect needs to render a given frame (clipped to the RoD).
        //
        // In our example we are doing full frame fetches regardless.
        std::map<OFX::Host::ImageEffect::ClipInstance *, OfxRectD> rois;
        stat = instance->getRegionOfInterestAction(frame, renderScale,
                                                   regionOfInterest, rois);
        if (stat != kOfxStatOK && stat != kOfxStatReplyDefault) { return 5; }

        // render a frame
        stat = instance->renderAction(frame,kOfxImageFieldBoth,renderWindow, renderScale, /*sequential=*/true, /*interactive=*/false, /*draft=*/false);
        if (stat != kOfxStatOK) { return 5; }

        // get the output image buffer
        MondrianOpenFx::FrameImage *outputImage = outputClip->getOutputImage();
        if (!outputImage) { return 5; }
        std::ofstream output(std::filesystem::u8path(outputPath), std::ios::binary);
        output.write(reinterpret_cast<const char*>(outputImage->rawData()), pixels * sizeof(OfxRGBAColourF));
        if (!output) { return 5; }
      }

      instance->endRenderAction(frame, frame, 1.0, false, renderScale, /*sequential=*/true, /*interactive=*/false
                                );
    }
  }
  OFX::Host::PluginCache::clearPluginCache();
  return 0;
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
