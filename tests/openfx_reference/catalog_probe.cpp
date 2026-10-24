// Copyright Mondrian contributors.
// SPDX-License-Identifier: BSD-3-Clause
// Owned cold Catalog fixture. Never used as third-party compatibility evidence.
#include "ofxImageEffect.h"
#include "ofxPixels.h"
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <filesystem>
#include <thread>
#include <chrono>
#ifdef _WIN32
#include <process.h>
#else
#include <unistd.h>
#endif
namespace {
OfxHost* host = nullptr;
const OfxPropertySuiteV1* properties = nullptr;
const OfxImageEffectSuiteV1* images = nullptr;
bool mode(const char* expected) {
  auto* actual = std::getenv("MONDRIAN_OPENFX_CATALOG_PROBE_MODE");
  return actual && std::strcmp(actual, expected) == 0;
}
OfxStatus dispatch(int index, const char* action, const void* handle, OfxPropertySetHandle in, OfxPropertySetHandle out) {
  auto* path = std::getenv("MONDRIAN_OPENFX_CATALOG_PROBE_LOG");
  if (!action || !path || !*path) return kOfxStatErrValue;
  {
    std::ofstream log(std::filesystem::u8path(path), std::ios::binary | std::ios::app);
#ifdef _WIN32
    auto pid = _getpid();
#else
    auto pid = getpid();
#endif
    log << pid << '|' << index << '|' << action << '\n'; log.flush();
    if (!log.good()) return kOfxStatFailed;
  }
  if (std::strcmp(action, kOfxActionLoad) == 0) {
    if (index == 1 && mode("crash_middle")) std::_Exit(23);
    if (index == 1 && mode("block_middle")) std::this_thread::sleep_for(std::chrono::seconds(60));
    if (!host || !host->host || !host->fetchSuite) return kOfxStatErrMissingHostFeature;
    properties = static_cast<const OfxPropertySuiteV1*>(host->fetchSuite(host->host, kOfxPropertySuite, 1));
    images = static_cast<const OfxImageEffectSuiteV1*>(host->fetchSuite(host->host, kOfxImageEffectSuite, 1));
    return properties && images ? kOfxStatOK : kOfxStatErrMissingHostFeature;
  }
  if (!properties || !images) return kOfxStatErrMissingHostFeature;
  auto effect = static_cast<OfxImageEffectHandle>(const_cast<void*>(handle));
  if (std::strcmp(action, kOfxActionDescribe) == 0) {
    OfxPropertySetHandle props = nullptr;
    if (images->getPropertySet(effect, &props) != kOfxStatOK) return kOfxStatFailed;
    if (properties->propSetString(props, kOfxPropLabel, 0, "Catalog Probe") != kOfxStatOK ||
        properties->propSetString(props, kOfxImageEffectPropSupportedContexts, 0, kOfxImageEffectContextGenerator) != kOfxStatOK ||
        properties->propSetString(props, kOfxImageEffectPropSupportedPixelDepths, 0, kOfxBitDepthFloat) != kOfxStatOK ||
        properties->propSetString(props, kOfxImageEffectPluginRenderThreadSafety, 0, kOfxImageEffectRenderUnsafe) != kOfxStatOK ||
        properties->propSetInt(props, kOfxImageEffectPluginPropHostFrameThreading, 0, 0) != kOfxStatOK ||
        properties->propSetInt(props, kOfxImageEffectPropSupportsTiles, 0, 0) != kOfxStatOK ||
        properties->propSetInt(props, kOfxImageEffectPropSupportsMultiResolution, 0, 0) != kOfxStatOK ||
        properties->propSetInt(props, kOfxImageEffectPropTemporalClipAccess, 0, 0) != kOfxStatOK) return kOfxStatFailed;
    if (mode("versions")) {
      const char* labels[] = {"Catalog Probe v1.0", "Catalog Probe v2.0", "Catalog Probe v2.1", "Catalog Probe v3.0"};
      if (properties->propSetString(props, kOfxPropLabel, 0, labels[index]) != kOfxStatOK) return kOfxStatFailed;
    }
    return kOfxStatOK;
  }
  if (std::strcmp(action, kOfxImageEffectActionDescribeInContext) == 0) {
    char* context = nullptr; OfxPropertySetHandle output = nullptr;
    if (!in || properties->propGetString(in, kOfxImageEffectPropContext, 0, &context) != kOfxStatOK ||
        !context || std::strcmp(context, kOfxImageEffectContextGenerator) != 0 ||
        images->clipDefine(effect, kOfxImageEffectOutputClipName, &output) != kOfxStatOK ||
        properties->propSetString(output, kOfxImageEffectPropSupportedComponents, 0, kOfxImageComponentRGBA) != kOfxStatOK) return kOfxStatFailed;
    return kOfxStatOK;
  }
  if (std::strcmp(action, kOfxActionUnload) == 0) {
    properties = nullptr; images = nullptr; return kOfxStatOK;
  }
  if (mode("versions") && std::strcmp(action, kOfxImageEffectActionGetClipPreferences) == 0) {
    return out && properties->propSetString(out, kOfxImageEffectPropPreMultiplication, 0,
        kOfxImageUnPreMultiplied) == kOfxStatOK ? kOfxStatOK : kOfxStatFailed;
  }
  if (mode("versions") && std::strcmp(action, kOfxImageEffectActionRender) == 0) {
    OfxImageClipHandle clip = nullptr; OfxPropertySetHandle image = nullptr;
    double time = 0; void* data = nullptr; int bounds[4]{}, rowBytes = 0;
    if (!in || properties->propGetDouble(in, kOfxPropTime, 0, &time) != kOfxStatOK ||
        images->clipGetHandle(effect, kOfxImageEffectOutputClipName, &clip, nullptr) != kOfxStatOK ||
        images->clipGetImage(clip, time, nullptr, &image) != kOfxStatOK || !image) return kOfxStatFailed;
    struct Lease { OfxPropertySetHandle image; ~Lease() { images->clipReleaseImage(image); } } lease{image};
    if (properties->propGetPointer(image, kOfxImagePropData, 0, &data) != kOfxStatOK || !data ||
        properties->propGetIntN(image, kOfxImagePropBounds, 4, bounds) != kOfxStatOK ||
        properties->propGetInt(image, kOfxImagePropRowBytes, 0, &rowBytes) != kOfxStatOK) return kOfxStatFailed;
    const float version = float(index + 1);
    const OfxRGBAColourF pixel{version/16, version/32, 0.25f, 1.0f};
    for (int y=bounds[1]; y<bounds[3]; ++y) {
      auto* row = static_cast<char*>(data) + ptrdiff_t(y-bounds[1])*rowBytes;
      for (int x=bounds[0]; x<bounds[2]; ++x) std::memcpy(row+ptrdiff_t(x-bounds[0])*sizeof(pixel), &pixel, sizeof(pixel));
    }
    return kOfxStatOK;
  }
  return kOfxStatReplyDefault;
}
template<int Index> OfxStatus entry(const char* action, const void* handle, OfxPropertySetHandle in, OfxPropertySetHandle out) {
  try { return dispatch(Index, action, handle, in, out); } catch (...) { return kOfxStatErrUnknown; }
}
void setHost(OfxHost* value) { host = value; }
OfxPlugin plugins[] = {
  {kOfxImageEffectPluginApi, 1, "org.mondrian.fixture.catalog-a", 1, 0, setHost, entry<0>},
  {kOfxImageEffectPluginApi, 1, "org.mondrian.fixture.catalog-b", 1, 0, setHost, entry<1>},
  {kOfxImageEffectPluginApi, 1, "org.mondrian.fixture.catalog-c", 1, 0, setHost, entry<2>},
};
// Same identifier, including two minors of one major, in deliberately unsorted order.
OfxPlugin versions[] = {
  {kOfxImageEffectPluginApi, 1, "org.mondrian.fixture.catalog-version", 2, 1, setHost, entry<2>},
  {kOfxImageEffectPluginApi, 1, "org.mondrian.fixture.catalog-version", 3, 0, setHost, entry<3>},
  {kOfxImageEffectPluginApi, 1, "org.mondrian.fixture.catalog-version", 1, 0, setHost, entry<0>},
  {kOfxImageEffectPluginApi, 1, "org.mondrian.fixture.catalog-version", 2, 0, setHost, entry<1>},
};
}
extern "C" {
OfxExport int OfxGetNumberOfPlugins() { return mode("versions") ? 4 : 3; }
OfxExport OfxPlugin* OfxGetPlugin(int index) { return index >= 0 && index < OfxGetNumberOfPlugins() ? &(mode("versions") ? versions : plugins)[index] : nullptr; }
}
