// Copyright Mondrian contributors.
// SPDX-License-Identifier: BSD-3-Clause
// Protocol-only fixture. It does not provide a temporal renderer.
#include "ofxCore.h"
#include "ofxImageEffect.h"
#include "ofxProperty.h"
#include <array>
#include <climits>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <limits>
#include <new>
#include <vector>

namespace {
OfxHost* host = nullptr;
const OfxPropertySuiteV1* property = nullptr;
const OfxImageEffectSuiteV1* image = nullptr;
struct State { unsigned queries = 0; };
void setHost(OfxHost* value) { host = value; }
bool log(const char* action) {
  const char* path = std::getenv("MONDRIAN_OPENFX_FRAME_DEPENDENCIES_ACTION_LOG");
  if (!path) { return false; }
  FILE* file = std::fopen(path, "ab");
  if (!file) { return false; }
  const bool ok = std::fprintf(file, "%s\n", action) > 0;
  return std::fclose(file) == 0 && ok;
}
OfxStatus entry(const char* action, const void* handle,
                OfxPropertySetHandle in, OfxPropertySetHandle out) {
  try {
    if (!log(action)) { return kOfxStatFailed; }
    if (!std::strcmp(action, kOfxActionLoad)) {
      property = static_cast<const OfxPropertySuiteV1*>(host->fetchSuite(host->host, kOfxPropertySuite, 1));
      image = static_cast<const OfxImageEffectSuiteV1*>(host->fetchSuite(host->host, kOfxImageEffectSuite, 1));
      return property && image ? kOfxStatOK : kOfxStatErrMissingHostFeature;
    }
    if (!std::strcmp(action, kOfxActionUnload)) { return kOfxStatOK; }
    OfxPropertySetHandle props = nullptr;
    if (image->getPropertySet(static_cast<OfxImageEffectHandle>(const_cast<void*>(handle)), &props) != kOfxStatOK) {
      return kOfxStatErrBadHandle;
    }
    if (!std::strcmp(action, kOfxActionDescribe)) {
      property->propSetString(props, kOfxPropLabel, 0, "Frame dependencies protocol probe");
      property->propSetString(props, kOfxImageEffectPropSupportedContexts, 0, kOfxImageEffectContextFilter);
      property->propSetString(props, kOfxImageEffectPropSupportedPixelDepths, 0, kOfxBitDepthFloat);
      property->propSetInt(props, kOfxImageEffectPropTemporalClipAccess, 0, 1);
      return kOfxStatOK;
    }
    if (!std::strcmp(action, kOfxImageEffectActionDescribeInContext)) {
      for (const char* name : {"Source", "Aux", "Output"}) {
        OfxPropertySetHandle clip = nullptr;
        if (image->clipDefine(static_cast<OfxImageEffectHandle>(const_cast<void*>(handle)), name, &clip) != kOfxStatOK) {
          return kOfxStatFailed;
        }
        property->propSetString(clip, kOfxImageEffectPropSupportedComponents, 0, kOfxImageComponentRGBA);
        if (!std::strcmp(name, "Aux")) { property->propSetInt(clip, kOfxImageClipPropOptional, 0, 1); }
      }
      return kOfxStatOK;
    }
    if (!std::strcmp(action, kOfxActionCreateInstance)) {
      auto* state = new State;
      if (property->propSetPointer(props, kOfxPropInstanceData, 0, state) != kOfxStatOK) {
        delete state; return kOfxStatFailed;
      }
      return kOfxStatOK;
    }
    if (!std::strcmp(action, kOfxActionDestroyInstance)) {
      void* value = nullptr;
      if (property->propGetPointer(props, kOfxPropInstanceData, 0, &value) != kOfxStatOK || !value) { return kOfxStatFailed; }
      delete static_cast<State*>(value);
      property->propSetPointer(props, kOfxPropInstanceData, 0, nullptr);
      return kOfxStatOK;
    }
    if (std::strcmp(action, kOfxImageEffectActionGetFramesNeeded)) {
      return !std::strcmp(action, kOfxImageEffectActionRender) ? kOfxStatFailed : kOfxStatReplyDefault;
    }
    void* value = nullptr;
    double time = 0;
    if (property->propGetPointer(props, kOfxPropInstanceData, 0, &value) != kOfxStatOK || !value ||
        property->propGetDouble(in, kOfxPropTime, 0, &time) != kOfxStatOK || !std::isfinite(time)) { return kOfxStatFailed; }
    ++static_cast<State*>(value)->queries;
    const char* source = "OfxImageClipPropFrameRange_Source";
    const char* aux = "OfxImageClipPropFrameRange_Aux";
    for (const char* name : {source, aux}) {
      int dimension = -1;
      double defaults[2]{};
      if (property->propGetDimension(out, name, &dimension) != kOfxStatOK || dimension != 2 ||
          property->propGetDoubleN(out, name, 2, defaults) != kOfxStatOK || defaults[0] != time || defaults[1] != time) { return kOfxStatFailed; }
    }
    int dimension = -1;
    if (property->propGetDimension(out, "OfxImageClipPropFrameRange_Output", &dimension) != kOfxStatErrUnknown ||
        property->propSetDouble(in, kOfxPropTime, 0, 123) != kOfxStatErrValue) { return kOfxStatFailed; }
    const int test = int(time - .25);
    const double disjoint[]{-3.5, -2.5, 8, 8};
    if (test == 0) { return property->propSetDoubleN(out, source, 4, disjoint); }
    if (test == 1) {
      const double ignored[]{std::numeric_limits<double>::quiet_NaN(), 3, 7};
      property->propSetDoubleN(out, source, 3, ignored);
      return kOfxStatReplyDefault;
    }
    if (test == 2 || test == 15) {
      property->propSetDoubleN(out, source, 0, nullptr);
      if (test == 15) { property->propSetDoubleN(out, aux, 0, nullptr); }
      return kOfxStatOK;
    }
    if (test >= 3 && test <= 5) {
      property->propSetDoubleN(out, source, 4, disjoint);
      return test == 3 ? kOfxStatFailed : test == 4 ? kOfxStatErrMemory : kOfxStatErrFatal;
    }
    if (test == 6 || test == 7) {
      if (test == 7) { property->propSetDoubleN(out, source, 4, disjoint); }
      return property->propSetDoubleN(out, test == 6 ? source : aux, 3, disjoint);
    }
    if (test >= 8 && test <= 10) {
      const double invalid[]{test == 8 ? std::numeric_limits<double>::quiet_NaN() :
          test == 9 ? std::numeric_limits<double>::infinity() : 5, 4};
      return property->propSetDoubleN(out, source, 2, invalid);
    }
    if (test == 11 || test == 12 || test == 14 || test == 18 || test == 19) {
      const int count = test == 12 || test == 18 || test == 19 ? 257 : 256;
      std::vector<double> ranges;
      for (int i = 0; i < count; ++i) { ranges.push_back(i - 128.5); ranges.push_back(i - 128.5); }
      const auto status = property->propSetDoubleN(out, source, int(ranges.size()), ranges.data());
      if (count == 257) {
        if (status != kOfxStatErrBadIndex || property->propGetDimension(out, source, &dimension) != kOfxStatOK || dimension != 2) { return kOfxStatFailed; }
        if (test == 18) { property->propReset(out, source); }
        return test == 19 ? kOfxStatReplyDefault : kOfxStatOK;
      }
      if (status != kOfxStatOK) { return kOfxStatFailed; }
      if (test == 11) { property->propSetDoubleN(out, aux, 0, nullptr); }
      return kOfxStatOK;
    }
    if (test == 13) {
      const auto status = property->propSetDouble(out, source, INT_MAX, 0);
      return status == kOfxStatErrBadIndex ? kOfxStatOK : kOfxStatFailed;
    }
    return test == 17 ? 12345 : kOfxStatOK;
  } catch (const std::bad_alloc&) { return kOfxStatErrMemory; }
    catch (...) { return kOfxStatFailed; }
}
OfxPlugin plugin{kOfxImageEffectPluginApi, 1, "org.mondrian.fixture.frame-dependencies", 1, 0, setHost, entry};
}

#ifdef _WIN32
#define EXPORT __declspec(dllexport)
#else
#define EXPORT __attribute__((visibility("default")))
#endif
extern "C" EXPORT int OfxGetNumberOfPlugins() { return 1; }
extern "C" EXPORT OfxPlugin* OfxGetPlugin(int index) { return index == 0 ? &plugin : nullptr; }
