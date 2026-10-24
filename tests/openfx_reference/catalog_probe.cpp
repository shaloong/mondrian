// Copyright Mondrian contributors.
// SPDX-License-Identifier: BSD-3-Clause
// Owned cold Catalog fixture. Never used as third-party compatibility evidence.
#include "ofxImageEffect.h"
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
OfxStatus dispatch(int index, const char* action, const void* handle, OfxPropertySetHandle in) {
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
  return kOfxStatReplyDefault;
}
template<int Index> OfxStatus entry(const char* action, const void* handle, OfxPropertySetHandle in, OfxPropertySetHandle) {
  try { return dispatch(Index, action, handle, in); } catch (...) { return kOfxStatErrUnknown; }
}
void setHost(OfxHost* value) { host = value; }
OfxPlugin plugins[] = {
  {kOfxImageEffectPluginApi, 1, "org.mondrian.fixture.catalog-a", 1, 0, setHost, entry<0>},
  {kOfxImageEffectPluginApi, 1, "org.mondrian.fixture.catalog-b", 1, 0, setHost, entry<1>},
  {kOfxImageEffectPluginApi, 1, "org.mondrian.fixture.catalog-c", 1, 0, setHost, entry<2>},
};
}
extern "C" {
OfxExport int OfxGetNumberOfPlugins() { return 3; }
OfxExport OfxPlugin* OfxGetPlugin(int index) { return index >= 0 && index < 3 ? &plugins[index] : nullptr; }
}
