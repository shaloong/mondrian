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
#include <fstream>
#include <filesystem>
#include <initializer_list>
#include <string>
#ifdef _WIN32
#include <process.h>
#else
#include <unistd.h>
#endif

namespace {
OfxHost* host = nullptr;
const OfxPropertySuiteV1* properties = nullptr;
const OfxImageEffectSuiteV1* images = nullptr;
const OfxParameterSuiteV1* parameters = nullptr;

bool stringIs(OfxPropertySetHandle props, const char* name, const char* expected);

bool modeIs(const char* expected) {
  const char* mode = std::getenv("MONDRIAN_OPENFX_GENERATOR_MODE");
  return mode && std::strcmp(mode, expected) == 0;
}

bool declaresInputs() {
  return modeIs("optional_inputs") || modeIs("required_source") || modeIs("required_aux");
}

bool spatialDefaultsMode() { return modeIs("spatial_defaults"); }

bool spatialValues(OfxImageEffectHandle effect, double time, double values[3][2]) {
  OfxParamSetHandle params = nullptr;
  if (images->getParamSet(effect, &params) != kOfxStatOK) { return false; }
  const char* names[] = {"position", "size", "canonical"};
  for (int index=0; index<3; ++index) {
    OfxParamHandle handle = nullptr;
    double atTime[2]{};
    if (parameters->paramGetHandle(params, names[index], &handle, nullptr) != kOfxStatOK || !handle ||
        parameters->paramGetValue(handle, &values[index][0], &values[index][1]) != kOfxStatOK ||
        parameters->paramGetValueAtTime(handle, time, &atTime[0], &atTime[1]) != kOfxStatOK ||
        atTime[0] != values[index][0] || atTime[1] != values[index][1]) { return false; }
  }
  return true;
}

bool spatialInitialDefaults(OfxImageEffectHandle effect) {
  OfxPropertySetHandle props = nullptr;
  double size[2]{}, values[3][2]{};
  return images->getPropertySet(effect, &props) == kOfxStatOK &&
      properties->propGetDoubleN(props, kOfxImageEffectPropProjectSize, 2, size) == kOfxStatOK &&
      spatialValues(effect, 0.0, values) &&
      values[0][0] == size[0]*0.5 && values[0][1] == size[1]*0.25 &&
      values[1][0] == size[0] && values[1][1] == size[1] &&
      values[2][0] == 4.0 && values[2][1] == 5.0;
}

bool disconnectedInputs(OfxImageEffectHandle effect, bool fetch, double time) {
  for (const char* name : {kOfxImageEffectSimpleSourceClipName, "Aux"}) {
    OfxImageClipHandle clip = nullptr;
    OfxPropertySetHandle props = nullptr;
    int connected = -1;
    if (images->clipGetHandle(effect, name, &clip, &props) != kOfxStatOK || !clip || !props ||
        properties->propGetInt(props, kOfxImageClipPropConnected, 0, &connected) != kOfxStatOK || connected != 0 ||
        !stringIs(props, kOfxImageClipPropUnmappedPixelDepth, kOfxBitDepthNone) ||
        !stringIs(props, kOfxImageClipPropUnmappedComponents, kOfxImageComponentNone)) { return false; }
    if (fetch) {
      OfxPropertySetHandle image = nullptr;
      if (images->clipGetImage(clip, time, nullptr, &image) != kOfxStatFailed || image) { return false; }
    }
  }
  return true;
}

bool stringIs(OfxPropertySetHandle props, const char* name, const char* expected) {
  char* value = nullptr;
  return properties->propGetString(props, name, 0, &value) == kOfxStatOK &&
      value && std::strcmp(value, expected) == 0;
}

bool generatorInstance(OfxImageEffectHandle effect) {
  OfxPropertySetHandle props = nullptr;
  OfxImageClipHandle source = nullptr;
  double size[2]{}, extent[2]{}, offset[2]{}, par = 0.0;
  return images->getPropertySet(effect, &props) == kOfxStatOK && props &&
      stringIs(props, kOfxImageEffectPropContext, kOfxImageEffectContextGenerator) &&
      properties->propGetDoubleN(props, kOfxImageEffectPropProjectSize, 2, size) == kOfxStatOK &&
      properties->propGetDoubleN(props, kOfxImageEffectPropProjectExtent, 2, extent) == kOfxStatOK &&
      properties->propGetDoubleN(props, kOfxImageEffectPropProjectOffset, 2, offset) == kOfxStatOK &&
      properties->propGetDouble(props, kOfxImageEffectPropProjectPixelAspectRatio, 0, &par) == kOfxStatOK &&
      ((size[0] == 6.0 && size[1] == 2.0) ||
       (spatialDefaultsMode() && size[0] == 10.0 && size[1] == 4.0)) &&
      extent[0] == size[0] && extent[1] == size[1] &&
      offset[0] == 0.0 && offset[1] == 0.0 && par == 2.0 &&
      (declaresInputs() ? disconnectedInputs(effect, false, 0.0) :
        images->clipGetHandle(effect, kOfxImageEffectSimpleSourceClipName,
                              &source, nullptr) != kOfxStatOK && !source);
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
  if (declaresInputs()) {
    for (const char* name : {kOfxImageEffectSimpleSourceClipName, "Aux"}) {
      OfxPropertySetHandle clip = nullptr;
      const bool required = (modeIs("required_source") && std::strcmp(name, "Source") == 0) ||
          (modeIs("required_aux") && std::strcmp(name, "Aux") == 0);
      if (images->clipDefine(effect, name, &clip) != kOfxStatOK || !clip ||
          properties->propSetString(clip, kOfxImageEffectPropSupportedComponents, 0,
                                    kOfxImageComponentRGBA) != kOfxStatOK ||
          properties->propSetInt(clip, kOfxImageClipPropOptional, 0, required ? 0 : 1) != kOfxStatOK ||
          properties->propSetInt(clip, kOfxImageEffectPropTemporalClipAccess, 0, 0) != kOfxStatOK) { return kOfxStatFailed; }
    }
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
  if (!valid) { return kOfxStatFailed; }
  if (spatialDefaultsMode()) {
    const char* names[] = {"position", "size", "canonical"};
    const double defaults[][2] = {{0.5,0.25},{1.0,1.0},{4.0,5.0}};
    for (int index=0; index<3; ++index) {
      OfxPropertySetHandle p = nullptr;
      const double minimum[2] = {index==0 ? 1.0 : 0.0, 0.0};
      const double maximum[2] = {100.0,100.0};
      if (parameters->paramDefine(params,kOfxParamTypeDouble2D,names[index],&p) != kOfxStatOK ||
          properties->propSetString(p,kOfxPropLabel,0,names[index]) != kOfxStatOK ||
          properties->propSetString(p,kOfxParamPropDoubleType,0,index==0 ? kOfxParamDoubleTypeXYAbsolute : kOfxParamDoubleTypeXY) != kOfxStatOK ||
          properties->propSetString(p,kOfxParamPropDefaultCoordinateSystem,0,index==2 ? kOfxParamCoordinatesCanonical : kOfxParamCoordinatesNormalised) != kOfxStatOK ||
          properties->propSetDoubleN(p,kOfxParamPropDefault,2,defaults[index]) != kOfxStatOK ||
          properties->propSetDoubleN(p,kOfxParamPropMin,2,minimum) != kOfxStatOK ||
          properties->propSetDoubleN(p,kOfxParamPropDisplayMin,2,minimum) != kOfxStatOK ||
          properties->propSetDoubleN(p,kOfxParamPropMax,2,maximum) != kOfxStatOK ||
          properties->propSetDoubleN(p,kOfxParamPropDisplayMax,2,maximum) != kOfxStatOK) { return kOfxStatFailed; }
    }
  }
  return kOfxStatOK;
}

OfxStatus clipPreferences(OfxPropertySetHandle out) {
  if (!out) { return kOfxStatFailed; }
  return properties->propSetString(out, kOfxImageEffectPropPreMultiplication, 0,
                                   kOfxImageUnPreMultiplied) == kOfxStatOK &&
      properties->propSetInt(out, kOfxImageEffectFrameVarying, 0, 1) == kOfxStatOK
      ? kOfxStatOK : kOfxStatFailed;
}

bool clipParDefaultsMode() {
  const char* mode = std::getenv("MONDRIAN_OPENFX_GENERATOR_MODE");
  return mode && std::strcmp(mode, "clip_par_defaults") == 0;
}

bool clipRateDefaultsMode() {
  const char* mode = std::getenv("MONDRIAN_OPENFX_GENERATOR_MODE");
  return mode && std::strcmp(mode, "clip_rate_defaults") == 0;
}

bool generatorRateFacts(OfxImageEffectHandle effect, double& rate) {
  OfxPropertySetHandle effectProps = nullptr, outputProps = nullptr;
  OfxImageClipHandle output = nullptr;
  int setable = -1, continuous = -1;
  if (!generatorInstance(effect) ||
      properties->propGetInt(host->host, kOfxImageEffectPropSetableFrameRate, 0, &setable) != kOfxStatOK || setable != 0 ||
      images->getPropertySet(effect, &effectProps) != kOfxStatOK ||
      properties->propGetDouble(effectProps, kOfxImageEffectPropFrameRate, 0, &rate) != kOfxStatOK || !std::isfinite(rate) || rate <= 0.0 ||
      images->clipGetHandle(effect, kOfxImageEffectOutputClipName, &output, &outputProps) != kOfxStatOK ||
      properties->propGetInt(outputProps, kOfxImageClipPropContinuousSamples, 0, &continuous) != kOfxStatOK || continuous != 0) { return false; }
  for (const char* name : {kOfxImageEffectPropFrameRate, kOfxImageEffectPropUnmappedFrameRate}) {
    double value = 0.0;
    if (properties->propGetDouble(outputProps, name, 0, &value) != kOfxStatOK || value != rate) { return false; }
  }
  return true;
}

bool clipDefaultsReceipt(const char* point, bool valid) {
  const char* directory = std::getenv("MONDRIAN_OPENFX_DESCRIBE_ACTION_RECEIPT_DIRECTORY");
  if (!directory || !*directory) { return false; }
#ifdef _WIN32
  const auto pid = _getpid();
#else
  const auto pid = getpid();
#endif
  const auto path = std::filesystem::u8path(directory) / (std::string(clipRateDefaultsMode() ? "generator-rate-" : "generator-par-") + std::to_string(pid) + ".txt");
  std::ofstream file(path, std::ios::binary | std::ios::app);
  file << point << " valid=" << valid << '\n';
  file.flush();
  return file.good();
}

OfxStatus clipDefaults(OfxImageEffectHandle effect, OfxPropertySetHandle out) {
  OfxImageClipHandle output = nullptr;
  OfxPropertySetHandle props = nullptr;
  double actual = 0.0, initial = 0.0;
  const bool valid = generatorInstance(effect) && out &&
      images->clipGetHandle(effect, kOfxImageEffectOutputClipName, &output, &props) == kOfxStatOK &&
      properties->propGetDouble(props, kOfxImagePropPixelAspectRatio, 0, &actual) == kOfxStatOK &&
      std::isfinite(actual) && actual > 0.0 &&
      properties->propGetDouble(out, "OfxImageClipPropPAR_Output", 0, &initial) == kOfxStatOK && initial == actual;
  bool rateValid = true;
  if (clipRateDefaultsMode()) {
    double rate = 0.0, initialRate = 0.0;
    int dimension = 0;
    rateValid = generatorRateFacts(effect, rate) &&
        properties->propGetDimension(out, kOfxImageEffectPropFrameRate, &dimension) == kOfxStatOK && dimension == 1 &&
        properties->propGetDouble(out, kOfxImageEffectPropFrameRate, 0, &initialRate) == kOfxStatOK && initialRate == rate;
  }
  return clipDefaultsReceipt("preferences", valid && rateValid) && valid && rateValid ? clipPreferences(out) : kOfxStatFailed;
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
      (!spatialDefaultsMode() && (projectSize[0] != static_cast<double>(window[2]) * projectPar ||
                                  projectSize[1] != static_cast<double>(window[3]))) ||
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
  if (mode && *mode && !(clipParDefaultsMode() || clipRateDefaultsMode() || declaresInputs() || spatialDefaultsMode())) { return kOfxStatErrValue; }
  if (declaresInputs() && !disconnectedInputs(effect, true, time)) { return kOfxStatFailed; }
  double spatial[3][2]{};
  if (spatialDefaultsMode() && !spatialValues(effect, time, spatial)) { return kOfxStatFailed; }
  if (spatialDefaultsMode() && time == 6.0) {
    // A canonical instance default remains a literal property. Changing that
    // property does not change the current value until the next omitted binding.
    OfxParamHandle parameter = nullptr;
    OfxPropertySetHandle props = nullptr;
    const double changed[2] = {8.0, 9.0};
    if (parameters->paramGetHandle(params, "canonical", &parameter, &props) != kOfxStatOK ||
        properties->propSetDoubleN(props, kOfxParamPropDefault, 2, changed) != kOfxStatOK) { return kOfxStatFailed; }
  }

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
      (!(clipParDefaultsMode() || clipRateDefaultsMode()) && imagePar != projectPar) ||
      !stringIs(image, kOfxImageEffectPropPixelDepth, kOfxBitDepthFloat) ||
      !stringIs(image, kOfxImageEffectPropComponents, kOfxImageComponentRGBA) ||
      !stringIs(image, kOfxImageEffectPropPreMultiplication, kOfxImageUnPreMultiplied) ||
      !stringIs(image, kOfxImagePropField, kOfxImageFieldNone)) { return kOfxStatFailed; }
  if (clipParDefaultsMode() || clipRateDefaultsMode()) {
    OfxPropertySetHandle outputProps = nullptr;
    double clipPar = 0.0;
    const bool valid = images->clipGetPropertySet(output, &outputProps) == kOfxStatOK &&
        properties->propGetDouble(outputProps, kOfxImagePropPixelAspectRatio, 0, &clipPar) == kOfxStatOK && clipPar == imagePar &&
        (time == 0.0 || time == 6.0 || time == 3.0) && imagePar == (time == 6.0 ? 1.2 : 1.0);
    bool rateValid = true;
    if (clipRateDefaultsMode()) {
      double rate = 0.0;
      rateValid = generatorRateFacts(effect, rate) && rate == (time == 6.0 ? 30000.0 / 1001.0 : 24.0);
    }
    if (!clipDefaultsReceipt("image", valid && rateValid) || !valid || !rateValid) { return kOfxStatFailed; }
  }
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
      const OfxRGBAColourF pixel = spatialDefaultsMode() ? OfxRGBAColourF{
          static_cast<float>(spatial[1][0]), static_cast<float>(spatial[1][1]),
          static_cast<float>(spatial[0][0]+spatial[0][1]/16.0+spatial[2][0]/128.0+spatial[2][1]/1024.0), 0.75f} : OfxRGBAColourF{
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
  if (modeIs("required_source") || modeIs("required_aux") || spatialDefaultsMode()) {
    const char* path = std::getenv("MONDRIAN_OPENFX_GENERATOR_ACTION_LOG");
    if (!path || !*path) { return kOfxStatErrValue; }
    std::ofstream file(std::filesystem::u8path(path), std::ios::binary | std::ios::app);
    file << action << '\n';
    file.flush();
    if (!file.good()) { return kOfxStatFailed; }
  }
  if (std::strcmp(action, kOfxActionLoad) == 0) { return load(); }
  if (!properties || !images || !parameters) { return kOfxStatErrMissingHostFeature; }
  const auto effect = static_cast<OfxImageEffectHandle>(const_cast<void*>(handle));
  if (std::strcmp(action, kOfxActionDescribe) == 0) { return describe(effect); }
  if (std::strcmp(action, kOfxImageEffectActionDescribeInContext) == 0) {
    return describeInContext(effect, in);
  }
  if (std::strcmp(action, kOfxActionCreateInstance) == 0) {
    return generatorInstance(effect) && (!spatialDefaultsMode() || spatialInitialDefaults(effect)) ? kOfxStatOK : kOfxStatFailed;
  }
  if (std::strcmp(action, kOfxImageEffectActionGetClipPreferences) == 0) {
    return clipParDefaultsMode() || clipRateDefaultsMode() ? clipDefaults(effect, out) : clipPreferences(out);
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
