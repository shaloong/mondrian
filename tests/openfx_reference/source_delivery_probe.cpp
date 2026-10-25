// Copyright Mondrian contributors.
// SPDX-License-Identifier: BSD-3-Clause
// Deliberately a transport calibration fixture, NOT a conforming temporal
// plugin: temporal admission remains disabled while the graph resolver is absent.
#include "ofxCore.h"
#include "ofxImageEffect.h"
#include "ofxProperty.h"
#include "ofxPixels.h"
#include <array>
#include <atomic>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <thread>

namespace {
OfxHost* host = nullptr;
const OfxPropertySuiteV1* property = nullptr;
const OfxImageEffectSuiteV1* image = nullptr;
void setHost(OfxHost* value) { host = value; }
bool log(const char* action) {
  const char* path = std::getenv("MONDRIAN_OPENFX_SOURCE_DELIVERY_LOG");
  if (!path) { return false; }
  FILE* file = std::fopen(path, "ab");
  if (!file) { return false; }
  const bool ok = std::fprintf(file, "%s\n", action) > 0;
  return std::fclose(file) == 0 && ok;
}
struct Image {
  OfxPropertySetHandle value = nullptr;
  ~Image() { if (value) { image->clipReleaseImage(value); } }
  OfxRGBAColourF* data = nullptr;
  int row = 0;
  std::array<int, 4> bounds{};
  bool fetch(OfxImageClipHandle clip, double time, std::array<int, 4> expected) {
    double par = 0;
    void* pointer = nullptr;
    const bool fetched =
      image->clipGetImage(clip, time, nullptr, &value) == kOfxStatOK && value &&
      property->propGetPointer(value, kOfxImagePropData, 0, &pointer) == kOfxStatOK && pointer;
    data = static_cast<OfxRGBAColourF*>(pointer);
    return fetched &&
      property->propGetInt(value, kOfxImagePropRowBytes, 0, &row) == kOfxStatOK &&
      property->propGetIntN(value, kOfxImagePropBounds, 4, bounds.data()) == kOfxStatOK && bounds == expected &&
      property->propGetDouble(value, kOfxImagePropPixelAspectRatio, 0, &par) == kOfxStatOK && par == 1.0 &&
      row == -(bounds[2] - bounds[0]) * int(sizeof(OfxRGBAColourF));
  }
  OfxRGBAColourF& pixel(int topDownIndex) {
    const int width = bounds[2] - bounds[0], height = bounds[3] - bounds[1];
    // Host supplies bottom-origin bounds/data and a negative row stride.
    auto* rowData = reinterpret_cast<OfxRGBAColourF*>(reinterpret_cast<char*>(data) +
        (height - 1 - topDownIndex / width) * row);
    return rowData[topDownIndex % width];
  }
  OfxRGBAColourF& at(int x, int y) {
    return reinterpret_cast<OfxRGBAColourF*>(reinterpret_cast<char*>(data) + (y - bounds[1])*row)[x - bounds[0]];
  }
};
OfxStatus entry(const char* action, const void* raw, OfxPropertySetHandle in, OfxPropertySetHandle out) {
  try {
    if (!log(action)) { return kOfxStatFailed; }
    if (!std::strcmp(action, kOfxActionLoad)) {
      property = static_cast<const OfxPropertySuiteV1*>(host->fetchSuite(host->host, kOfxPropertySuite, 1));
      image = static_cast<const OfxImageEffectSuiteV1*>(host->fetchSuite(host->host, kOfxImageEffectSuite, 1));
      return property && image ? kOfxStatOK : kOfxStatErrMissingHostFeature;
    }
    if (!std::strcmp(action, kOfxActionUnload)) { return kOfxStatOK; }
    const auto handle = static_cast<OfxImageEffectHandle>(const_cast<void*>(raw));
    OfxPropertySetHandle props = nullptr;
    if (image->getPropertySet(handle, &props) != kOfxStatOK) { return kOfxStatErrBadHandle; }
    if (!std::strcmp(action, kOfxActionDescribe)) {
      property->propSetString(props, kOfxPropLabel, 0, "Source delivery ABI calibration");
      property->propSetString(props, kOfxImageEffectPropSupportedContexts, 0, kOfxImageEffectContextFilter);
      property->propSetString(props, kOfxImageEffectPropSupportedPixelDepths, 0, kOfxBitDepthFloat);
      property->propSetInt(props, kOfxImageEffectPropTemporalClipAccess, 0, 0);
      return kOfxStatOK;
    }
    if (!std::strcmp(action, kOfxImageEffectActionDescribeInContext)) {
      for (const char* name : {"Source", "Output"}) {
        OfxPropertySetHandle clip = nullptr;
        if (image->clipDefine(handle, name, &clip) != kOfxStatOK) { return kOfxStatFailed; }
        property->propSetString(clip, kOfxImageEffectPropSupportedComponents, 0, kOfxImageComponentRGBA);
      }
      return kOfxStatOK;
    }
    if (!std::strcmp(action, kOfxActionCreateInstance) || !std::strcmp(action, kOfxActionDestroyInstance)) {
      return kOfxStatOK;
    }
    if (!std::strcmp(action, kOfxImageEffectActionGetRegionOfDefinition)) {
      const double rod[4] = {-1, -1, 4, 3};
      return property->propSetDoubleN(out, kOfxImageEffectPropRegionOfDefinition, 4, rod);
    }
    if (std::strcmp(action, kOfxImageEffectActionRender)) { return kOfxStatReplyDefault; }
    OfxImageClipHandle source = nullptr, output = nullptr;
    double time = 0;
    if (property->propGetDouble(in, kOfxPropTime, 0, &time) != kOfxStatOK ||
        image->clipGetHandle(handle, "Source", &source, nullptr) != kOfxStatOK ||
        image->clipGetHandle(handle, "Output", &output, nullptr) != kOfxStatOK) { return kOfxStatFailed; }
    Image current, past, future, result;
    if (!current.fetch(source, time, {0, 0, 3, 2}) ||
        !result.fetch(output, time, {-1, -1, 4, 3})) { return kOfxStatFailed; }
    // Exercise late Source requests with both current and Output leases alive.
    // Fixture-only concurrent readers must share the same immutable image set.
    if (std::getenv("MONDRIAN_OPENFX_SOURCE_DELIVERY_THREADS")) {
      std::atomic<bool> valid{true};
      std::array<std::thread, 8> readers;
      for (auto& reader : readers) {
        reader = std::thread([&] {
          Image a, b;
          if (!a.fetch(source, -1.5, {-2, 3, 0, 6}) ||
              !b.fetch(source, 2.25, {4, -3, 8, -2})) { valid.store(false); }
        });
      }
      for (auto& reader : readers) { reader.join(); }
      if (!valid.load()) { return kOfxStatFailed; }
    }
    if (!past.fetch(source, -1.5, {-2, 3, 0, 6}) ||
        !future.fetch(source, 2.25, {4, -3, 8, -2})) { return kOfxStatFailed; }
    if (!log("sources-held")) { return kOfxStatFailed; }
    if (std::getenv("MONDRIAN_OPENFX_SOURCE_DELIVERY_WAIT")) {
      while (!image->abort(handle)) { std::this_thread::sleep_for(std::chrono::milliseconds(1)); }
      return kOfxStatFailed;
    }
    for (int i = 0; i < 6; ++i) {
      const auto& a = current.pixel(i);
      const auto& b = past.pixel((i + 1) % 6);
      const auto& c = future.pixel((i + 2) % 4);
      result.at(i % 3, 1 - i / 3) = {a.r + 2*b.r + 3*c.r, a.g + 2*b.g + 3*c.g,
                         a.b + 2*b.b + 3*c.b, a.a + 2*b.a + 3*c.a};
    }
    return kOfxStatOK;
  } catch (...) { return kOfxStatFailed; }
}
OfxPlugin plugin{kOfxImageEffectPluginApi, 1, "org.mondrian.fixture.source-delivery", 1, 0, setHost, entry};
}
extern "C" {
OfxExport int OfxGetNumberOfPlugins() { return 1; }
OfxExport OfxPlugin* OfxGetPlugin(int index) { return index == 0 ? &plugin : nullptr; }
}
