// Copyright Mondrian contributors.
// SPDX-License-Identifier: BSD-3-Clause
// Independent, test-only Generator fixture; no official example implementation
// is included. SDK contracts: ofxCore.h setHost; ofxImageEffect.h Generator,
// Render, ClipPreferences, ProjectSize/Offset/PAR, Bounds and signed RowBytes.
#include "ofxImageEffect.h"
#include "ofxPixels.h"

#include <cmath>
#include <cstddef>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <limits>

namespace {
OfxHost* host = nullptr;
const OfxPropertySuiteV1* properties = nullptr;
const OfxImageEffectSuiteV1* images = nullptr;
const OfxParameterSuiteV1* parameters = nullptr;

bool stringIs(OfxPropertySetHandle props, const char* name, const char* expected) {
  char* value = nullptr;
  return properties->propGetString(props, name, 0, &value) == kOfxStatOK &&
      value && std::strcmp(value, expected) == 0;
}

bool generatorInstance(OfxImageEffectHandle effect) {
  OfxPropertySetHandle props = nullptr;
  OfxImageClipHandle source = nullptr;
  return images->getPropertySet(effect, &props) == kOfxStatOK && props &&
      stringIs(props, kOfxImageEffectPropContext, kOfxImageEffectContextGenerator) &&
      images->clipGetHandle(effect, kOfxImageEffectSimpleSourceClipName,
                            &source, nullptr) != kOfxStatOK && !source;
}

OfxStatus load() {
  if (!host || !host->host || !host->fetchSuite) { return kOfxStatErrMissingHostFeature; }
  properties = static_cast<const OfxPropertySuiteV1*>(
      host->fetchSuite(host->host, kOfxPropertySuite, 1));
  images = static_cast<const OfxImageEffectSuiteV1*>(
      host->fetchSuite(host->host, kOfxImageEffectSuite, 1));
  parameters = static_cast<const OfxParameterSuiteV1*>(
      host->fetchSuite(host->host, kOfxParameterSuite, 1));
  return properties && images && parameters ? kOfxStatOK : kOfxStatErrMissingHostFeature;
}

OfxStatus describe(OfxImageEffectHandle effect) {
  OfxPropertySetHandle props = nullptr;
  if (images->getPropertySet(effect, &props) != kOfxStatOK || !props) { return kOfxStatFailed; }
  const bool valid =
      properties->propSetString(props, kOfxPropLabel, 0, "Mondrian Generator Probe") == kOfxStatOK &&
      properties->propSetString(props, kOfxImageEffectPropSupportedContexts, 0,
                                kOfxImageEffectContextGenerator) == kOfxStatOK &&
      properties->propSetString(props, kOfxImageEffectPropSupportedPixelDepths, 0,
                                kOfxBitDepthFloat) == kOfxStatOK &&
      properties->propSetString(props, kOfxImageEffectPluginRenderThreadSafety, 0,
                                kOfxImageEffectRenderUnsafe) == kOfxStatOK &&
      properties->propSetInt(props, kOfxImageEffectPluginPropHostFrameThreading, 0, 0) == kOfxStatOK &&
      properties->propSetInt(props, kOfxImageEffectPropSupportsMultiResolution, 0, 0) == kOfxStatOK &&
      properties->propSetInt(props, kOfxImageEffectPropSupportsTiles, 0, 0) == kOfxStatOK &&
      properties->propSetInt(props, kOfxImageEffectPropTemporalClipAccess, 0, 0) == kOfxStatOK;
  return valid ? kOfxStatOK : kOfxStatFailed;
}

OfxStatus describeInContext(OfxImageEffectHandle effect, OfxPropertySetHandle in) {
  if (!in || !stringIs(in, kOfxImageEffectPropContext, kOfxImageEffectContextGenerator)) {
    return kOfxStatErrUnsupported;
  }
  OfxPropertySetHandle output = nullptr;
  OfxParamSetHandle params = nullptr;
  OfxPropertySetHandle gain = nullptr;
  const bool valid =
      images->clipDefine(effect, kOfxImageEffectOutputClipName, &output) == kOfxStatOK && output &&
      properties->propSetString(output, kOfxImageEffectPropSupportedComponents, 0,
                                kOfxImageComponentRGBA) == kOfxStatOK &&
      properties->propSetInt(output, kOfxImageEffectPropSupportsTiles, 0, 0) == kOfxStatOK &&
      images->getParamSet(effect, &params) == kOfxStatOK && params &&
      parameters->paramDefine(params, kOfxParamTypeDouble, "gain", &gain) == kOfxStatOK && gain &&
      properties->propSetString(gain, kOfxPropLabel, 0, "Gain") == kOfxStatOK &&
      properties->propSetString(gain, kOfxParamPropDoubleType, 0, kOfxParamDoubleTypePlain) == kOfxStatOK &&
      properties->propSetDouble(gain, kOfxParamPropDefault, 0, 0.5) == kOfxStatOK &&
      properties->propSetDouble(gain, kOfxParamPropMin, 0, 0.0) == kOfxStatOK &&
      properties->propSetDouble(gain, kOfxParamPropMax, 0, 1.0) == kOfxStatOK &&
      properties->propSetDouble(gain, kOfxParamPropDisplayMin, 0, 0.0) == kOfxStatOK &&
      properties->propSetDouble(gain, kOfxParamPropDisplayMax, 0, 1.0) == kOfxStatOK;
  return valid ? kOfxStatOK : kOfxStatFailed;
}

OfxStatus clipPreferences(OfxPropertySetHandle out) {
  if (!out) { return kOfxStatFailed; }
  return properties->propSetString(out, kOfxImageEffectPropPreMultiplication, 0,
                                   kOfxImageUnPreMultiplied) == kOfxStatOK &&
      properties->propSetInt(out, kOfxImageEffectFrameVarying, 0, 1) == kOfxStatOK
      ? kOfxStatOK : kOfxStatFailed;
}

// Own only the action-scoped image handle, never the host's pixel allocation.
class ImageLease {
 public:
  explicit ImageLease(OfxPropertySetHandle value) : value_(value) {}
  ~ImageLease() { if (value_) { images->clipReleaseImage(value_); } }
  ImageLease(const ImageLease&) = delete;
  ImageLease& operator=(const ImageLease&) = delete;
  OfxStatus release() {
    const auto value = value_;
    value_ = nullptr;
    return images->clipReleaseImage(value);
  }
 private:
  OfxPropertySetHandle value_;
};

OfxStatus render(OfxImageEffectHandle effect, OfxPropertySetHandle in) {
  double time = 0.0, scale[2] = {}, gain = 0.0;
  double projectSize[2] = {}, projectOffset[2] = {}, projectExtent[2] = {}, projectPar = 0.0;
  int window[4] = {};
  OfxPropertySetHandle effectProps = nullptr;
  OfxParamSetHandle params = nullptr;
  OfxParamHandle gainParam = nullptr;
  OfxImageClipHandle output = nullptr;
  if (!in || !generatorInstance(effect) ||
      properties->propGetDouble(in, kOfxPropTime, 0, &time) != kOfxStatOK || !std::isfinite(time) ||
      properties->propGetDoubleN(in, kOfxImageEffectPropRenderScale, 2, scale) != kOfxStatOK ||
      scale[0] != 1.0 || scale[1] != 1.0 ||
      !stringIs(in, kOfxImageEffectPropFieldToRender, kOfxImageFieldNone) ||
      properties->propGetIntN(in, kOfxImageEffectPropRenderWindow, 4, window) != kOfxStatOK ||
      window[0] != 0 || window[1] != 0 || window[2] <= 0 || window[3] <= 0 ||
      images->getPropertySet(effect, &effectProps) != kOfxStatOK || !effectProps ||
      properties->propGetDoubleN(effectProps, kOfxImageEffectPropProjectSize, 2, projectSize) != kOfxStatOK ||
      properties->propGetDoubleN(effectProps, kOfxImageEffectPropProjectOffset, 2, projectOffset) != kOfxStatOK ||
      properties->propGetDoubleN(effectProps, kOfxImageEffectPropProjectExtent, 2, projectExtent) != kOfxStatOK ||
      properties->propGetDouble(effectProps, kOfxImageEffectPropProjectPixelAspectRatio, 0, &projectPar) != kOfxStatOK ||
      !std::isfinite(projectPar) || projectPar <= 0.0 ||
      // Project coordinates are canonical: 3x2 pixels at PAR=2 means 6x2,
      // with a 3x2 render window. No input image supplies this geometry.
      projectSize[0] != static_cast<double>(window[2]) * projectPar ||
      projectSize[1] != static_cast<double>(window[3]) ||
      projectOffset[0] != 0.0 || projectOffset[1] != 0.0 ||
      projectExtent[0] != projectSize[0] || projectExtent[1] != projectSize[1] ||
      images->getParamSet(effect, &params) != kOfxStatOK || !params ||
      parameters->paramGetHandle(params, "gain", &gainParam, nullptr) != kOfxStatOK || !gainParam ||
      parameters->paramGetValueAtTime(gainParam, time, &gain) != kOfxStatOK ||
      !std::isfinite(gain) || gain < 0.0 || gain > 1.0 ||
      images->clipGetHandle(effect, kOfxImageEffectOutputClipName, &output, nullptr) != kOfxStatOK || !output) {
    return kOfxStatFailed;
  }

  // Deliberate fixture fault: return success without ever fetching Output.
  // The host must refuse publication, including after a successful prior frame.
  // This environment is test infrastructure, not a parameter or product policy.
  const char* mode = std::getenv("MONDRIAN_OPENFX_GENERATOR_MODE");
  if (mode && std::strcmp(mode, "no_output_fetch") == 0) { return kOfxStatOK; }
  if (mode && *mode) { return kOfxStatErrValue; }

  OfxPropertySetHandle image = nullptr;
  if (images->clipGetImage(output, time, nullptr, &image) != kOfxStatOK || !image) {
    return kOfxStatFailed;
  }
  ImageLease lease(image);
  void* data = nullptr;
  int bounds[4] = {}, rod[4] = {}, rowBytes = 0;
  double imageScale[2] = {}, imagePar = 0.0;
  if (properties->propGetPointer(image, kOfxImagePropData, 0, &data) != kOfxStatOK || !data ||
      properties->propGetIntN(image, kOfxImagePropBounds, 4, bounds) != kOfxStatOK ||
      properties->propGetIntN(image, kOfxImagePropRegionOfDefinition, 4, rod) != kOfxStatOK ||
      properties->propGetInt(image, kOfxImagePropRowBytes, 0, &rowBytes) != kOfxStatOK ||
      properties->propGetDoubleN(image, kOfxImageEffectPropRenderScale, 2, imageScale) != kOfxStatOK ||
      imageScale[0] != 1.0 || imageScale[1] != 1.0 ||
      properties->propGetDouble(image, kOfxImagePropPixelAspectRatio, 0, &imagePar) != kOfxStatOK ||
      imagePar != projectPar ||
      !stringIs(image, kOfxImageEffectPropPixelDepth, kOfxBitDepthFloat) ||
      !stringIs(image, kOfxImageEffectPropComponents, kOfxImageComponentRGBA) ||
      !stringIs(image, kOfxImageEffectPropPreMultiplication, kOfxImageUnPreMultiplied) ||
      !stringIs(image, kOfxImagePropField, kOfxImageFieldNone)) { return kOfxStatFailed; }
  for (int index = 0; index < 4; ++index) {
    if (bounds[index] != window[index] || rod[index] != window[index]) { return kOfxStatFailed; }
  }
  const std::int64_t width = static_cast<std::int64_t>(bounds[2]) - bounds[0];
  const std::int64_t height = static_cast<std::int64_t>(bounds[3]) - bounds[1];
  const std::int64_t stride = rowBytes;
  const std::int64_t absoluteStride = stride < 0 ? -stride : stride;
  const auto maxOffset = std::numeric_limits<std::ptrdiff_t>::max();
  if (width <= 0 || height <= 0 ||
      width > maxOffset / static_cast<std::int64_t>(sizeof(OfxRGBAColourF)) ||
      absoluteStride < width * static_cast<std::int64_t>(sizeof(OfxRGBAColourF)) ||
      absoluteStride > maxOffset / height) { return kOfxStatFailed; }
  const float frame = static_cast<float>(time);
  if (!std::isfinite(frame)) { return kOfxStatFailed; }
  for (int y = bounds[1]; y < bounds[3]; ++y) {
    auto* row = static_cast<char*>(data) +
        static_cast<std::ptrdiff_t>((static_cast<std::int64_t>(y) - bounds[1]) * stride);
    for (int x = bounds[0]; x < bounds[2]; ++x) {
      const OfxRGBAColourF pixel = {
          static_cast<float>(gain),
          static_cast<float>(static_cast<std::int64_t>(x) + 1) / 16.0f,
          static_cast<float>(static_cast<std::int64_t>(y) + 1) / 8.0f + frame / 64.0f,
          0.75f};
      const auto column = static_cast<std::ptrdiff_t>(
          (static_cast<std::int64_t>(x) - bounds[0]) * sizeof(OfxRGBAColourF));
      std::memcpy(row + column, &pixel, sizeof(pixel));
    }
  }
  return lease.release() == kOfxStatOK ? kOfxStatOK : kOfxStatFailed;
}

OfxStatus dispatch(const char* action, const void* handle,
                    OfxPropertySetHandle in, OfxPropertySetHandle out) {
  if (!action) { return kOfxStatErrValue; }
  if (std::strcmp(action, kOfxActionLoad) == 0) { return load(); }
  if (!properties || !images || !parameters) { return kOfxStatErrMissingHostFeature; }
  const auto effect = static_cast<OfxImageEffectHandle>(const_cast<void*>(handle));
  if (std::strcmp(action, kOfxActionDescribe) == 0) { return describe(effect); }
  if (std::strcmp(action, kOfxImageEffectActionDescribeInContext) == 0) {
    return describeInContext(effect, in);
  }
  if (std::strcmp(action, kOfxActionCreateInstance) == 0) {
    return generatorInstance(effect) ? kOfxStatOK : kOfxStatFailed;
  }
  if (std::strcmp(action, kOfxImageEffectActionGetClipPreferences) == 0) {
    return clipPreferences(out);
  }
  if (std::strcmp(action, kOfxImageEffectActionRender) == 0) { return render(effect, in); }
  if (std::strcmp(action, kOfxActionDestroyInstance) == 0) { return kOfxStatOK; }
  if (std::strcmp(action, kOfxActionUnload) == 0) {
    properties = nullptr;
    images = nullptr;
    parameters = nullptr;
    return kOfxStatOK;
  }
  return kOfxStatReplyDefault;
}

// No C++ exception may escape the plug-in entry point into the host.
OfxStatus mainEntry(const char* action, const void* handle,
                    OfxPropertySetHandle in, OfxPropertySetHandle out) {
  try { return dispatch(action, handle, in, out); }
  catch (...) { return kOfxStatErrUnknown; }
}

void setHost(OfxHost* value) { host = value; }
OfxPlugin plugin = {kOfxImageEffectPluginApi, kOfxImageEffectPluginApiVersion,
    "org.mondrian.fixture.ofx-generator", 1, 0, setHost, mainEntry};
} // namespace

extern "C" {
OfxExport OfxPlugin* OfxGetPlugin(int index) { return index == 0 ? &plugin : nullptr; }
OfxExport int OfxGetNumberOfPlugins() { return 1; }
}
