// Copyright Mondrian contributors.
// SPDX-License-Identifier: BSD-3-Clause
// Exercise actual HostSupport image handles backed by invocation-owned values.
// This is not a temporal renderer or a replacement Host.
#include "ofxhPluginCache.h"
#include "ofxhImageEffectAPI.h"
#include "ofxhClip.h"
#include "host_descriptor.h"
#include "frame_state.h"
#include "effect_instance.h"
#include <array>
#include <climits>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <limits>
#include <memory>
#include <stdexcept>
#include <string>
#include <thread>
#include <vector>

namespace {
using MondrianOpenFx::SourceFrameView;
void require(bool ok, const char* message) { if (!ok) { throw std::runtime_error(message); } }
struct Observation { OfxPropertySetHandle handle; const void* data; std::string id; };
struct StopProbe { unsigned calls = 0, stopAt; };
int abortBinding(const void* context) {
  auto& probe = *static_cast<StopProbe*>(const_cast<void*>(context));
  return ++probe.calls >= probe.stopAt;
}
Observation fetch(const OfxImageEffectSuiteV1& image, const OfxPropertySuiteV1& property,
                  OfxImageClipHandle clip, const SourceFrameView& expected, OfxPointD expectedScale = {1, 1}) {
  OfxPropertySetHandle handle = nullptr;
  require(image.clipGetImage(clip, expected.time, nullptr, &handle) == kOfxStatOK && handle, "actual image handle");
  void* pixels = nullptr;
  int row = 0, bounds[4]{}, rod[4]{};
  char* id = nullptr;
  double par = 0, scale[2]{};
  require(property.propGetPointer(handle, kOfxImagePropData, 0, &pixels) == kOfxStatOK &&
      property.propGetInt(handle, kOfxImagePropRowBytes, 0, &row) == kOfxStatOK &&
      property.propGetIntN(handle, kOfxImagePropBounds, 4, bounds) == kOfxStatOK &&
      property.propGetIntN(handle, kOfxImagePropRegionOfDefinition, 4, rod) == kOfxStatOK &&
      property.propGetString(handle, kOfxImagePropUniqueIdentifier, 0, &id) == kOfxStatOK && id &&
      property.propGetDouble(handle, kOfxImagePropPixelAspectRatio, 0, &par) == kOfxStatOK &&
      property.propGetDoubleN(handle, kOfxImageEffectPropRenderScale, 2, scale) == kOfxStatOK &&
      scale[0] == expectedScale.x && scale[1] == expectedScale.y,
      "complete actual image metadata");
  const int domain[]{expected.x, expected.y, expected.x + expected.width, expected.y + expected.height};
  require(!std::memcmp(bounds, domain, sizeof(domain)) && !std::memcmp(rod, domain, sizeof(domain)) &&
      row == -expected.width * int(sizeof(OfxRGBAColourF)) && par == expected.pixelAspectRatio,
      "per-time bounds/stride/PAR");
  for (int y = 0; y < expected.height; ++y) {
    auto* actual = static_cast<const unsigned char*>(pixels) + ptrdiff_t(y) * row;
    const auto* wanted = expected.pixels + size_t(expected.height - 1 - y) * expected.width;
    require(!std::memcmp(actual, wanted, size_t(expected.width) * sizeof(OfxRGBAColourF)), "complete independent RGBA rows");
  }
  OfxRectD canonical{};
  require(image.clipGetRegionOfDefinition(clip, expected.time, &canonical) == kOfxStatOK &&
      canonical.x1 == expected.x * par / expectedScale.x && canonical.x2 == (expected.x + expected.width) * par / expectedScale.x &&
      canonical.y1 == expected.y / expectedScale.y && canonical.y2 == (expected.y + expected.height) / expectedScale.y, "exact canonical RoD at sample time");
  return {handle, pixels, id};
}
int exercise(MondrianOpenFx::Host& host, const char* binaryPath, const char* bundlePath) {
  OFX::Host::ImageEffect::PluginCache images(host);
  OFX::Host::PluginCache plugins;
  images.registerInCache(plugins);
  OFX::Host::PluginBinary binary(binaryPath, bundlePath, &plugins);
  require(!binary.isInvalid() && binary.getNPlugins() == 1, "one actual Basic plugin");
  auto* plugin = dynamic_cast<OFX::Host::ImageEffect::ImageEffectPlugin*>(&binary.getPlugin(0));
  require(plugin && plugin->getIdentifier() == "uk.co.thefoundry.BasicGainPlugin", "pinned official Basic class");
  std::array<OfxRGBAColourF, 6> current{}, past{}, future{};
  for (size_t i = 0; i < 6; ++i) {
    current[i] = {float(i), -.25f, 2.5f, .5f};
    past[i] = {float(i + 8), -2.0f, .125f, .75f};
    future[i] = {-float(i + 8), .5f, 8.0f, .25f};
  }
  const auto savedCurrent = current, savedPast = past, savedFuture = future;
  MondrianOpenFx::FrameState frame;
  frame.info.render_scale[0] = frame.info.render_scale[1] = 1;
  frame.info.width = 3; frame.info.height = 2; frame.info.frame = .25;
  frame.info.first_frame = -1000; frame.info.last_frame = 1000;
  frame.info.frame_rate = 24; frame.info.pixel_aspect_ratio = 2;
  frame.info.source_alpha = 0;
  frame.imageNamespace[0] = 1;
  // This host-side image-wrapper control does not turn Basic into a temporal
  // algorithm. Declare the synthetic instance's access before constructing it.
  auto* descriptor = plugin->getContext(kOfxImageEffectContextFilter);
  require(descriptor, "actual context descriptor");
  descriptor->getProps().setIntProperty(kOfxImageEffectPropTemporalClipAccess, 1);
  descriptor->getClips().at("Source")->getProps().setIntProperty(kOfxImageEffectPropTemporalClipAccess, 1);
  std::unique_ptr<OFX::Host::ImageEffect::Instance> instance(plugin->createInstance(kOfxImageEffectContextFilter, &frame));
  require(instance && instance->createInstanceAction() == kOfxStatOK, "actual SDK instance");
  const auto& image = *static_cast<const OfxImageEffectSuiteV1*>(host.fetchSuite(kOfxImageEffectSuite, 1));
  const auto& property = *static_cast<const OfxPropertySuiteV1*>(host.fetchSuite(kOfxPropertySuite, 1));
  OfxImageClipHandle source = nullptr;
  require(image.clipGetHandle(instance->getHandle(), "Source", &source, nullptr) == kOfxStatOK && source, "original Source clip");
  std::array<SourceFrameView, 2> values{{
      {-1.5, past.data(), sizeof(past), -2, 3, 2, 3, 0, 2},
      {2.25, future.data(), sizeof(future), 4, -3, 3, 2, 0, 2}}};
  SourceFrameView now{.25, current.data(), sizeof(current), 0, 0, 3, 2, 0, 2};
  auto rebound = values;
  rebound[0].pixels = future.data();
  const std::vector<OfxRGBAColourF> large(8192, OfxRGBAColourF{.25f, -.5f, 2, .75f});
  const SourceFrameView largeView{-1.75, large.data(), large.size() * sizeof(OfxRGBAColourF), 0, 0, 4096, 2, 0, 2};
  std::vector<SourceFrameView> maximum;
  for (int i = 0; i < 256; ++i) { maximum.push_back(SourceFrameView{i - 128.5, current.data(), sizeof(OfxRGBAColourF), 0, 0, 1, 1, 0, 2}); }
  std::string firstId;
  {
    MondrianOpenFx::FrameBorrow borrow(frame);
    frame.input = current.data(); frame.pixelCount = 6;
    require(frame.beginImageBinding() && frame.bindSourceFrames(values.data(), values.size()), "complete immutable set admitted");
    MondrianOpenFx::ImageAccess access(frame, true);
    auto a = fetch(image, property, source, values[0]);
    auto b = fetch(image, property, source, values[0]);
    auto c = fetch(image, property, source, values[1]);
    auto d = fetch(image, property, source, now);
    require(a.handle != b.handle && a.data == b.data && a.id == b.id &&
        a.id != c.id && a.id != d.id && frame.imageHandles == 4, "independent handles and per-value identities");
    firstId = a.id;
    require(!frame.bindSourceFrames(nullptr, 0), "cannot replace a set while images are leased");
    for (const auto& value : {d, c, b, a}) { require(image.clipReleaseImage(value.handle) == kOfxStatOK, "release original handle"); }
    require(frame.imageHandles == 0, "all independent handles released");
    OfxPropertySetHandle missing = reinterpret_cast<OfxPropertySetHandle>(&frame);
    require(image.clipGetImage(source, -1.49, nullptr, &missing) == kOfxStatFailed && !missing, "missing exact time is not rounded/substituted");
    OfxRectD missingRod{};
    require(image.clipGetRegionOfDefinition(source, -1.49, &missingRod) == kOfxStatFailed,
        "missing time has no fabricated current-frame RoD");
    std::atomic<unsigned> failed{0};
    std::vector<std::thread> readers;
    for (int i = 0; i < 8; ++i) {
      readers.emplace_back([&] {
        try {
          auto one = fetch(image, property, source, values[0]);
          auto two = fetch(image, property, source, values[1]);
          if (one.id != firstId || one.handle == two.handle ||
              image.clipReleaseImage(two.handle) != kOfxStatOK ||
              image.clipReleaseImage(one.handle) != kOfxStatOK) { ++failed; }
        } catch (...) { ++failed; }
      });
    }
    for (auto& reader : readers) { reader.join(); }
    require(failed == 0 && frame.imageHandles == 0, "concurrent read-only images retain independent handles");
    require(frame.info.frame == now.time && frame.info.width == 3 && frame.info.height == 2 &&
        frame.info.pixel_aspect_ratio == 2 && frame.info.frame_rate == 24,
        "historical image access cannot replace the output-time/grid authority");
    auto refuse = [&](SourceFrameView invalid) {
      require(!frame.bindSourceFrames(&invalid, 1) && frame.sourceFrames == values.data() && frame.sourceFrameCount == 2,
          "invalid snapshot cannot partially replace the accepted set");
    };
    auto invalid = values[0]; invalid.time = std::numeric_limits<double>::quiet_NaN(); refuse(invalid);
    invalid = values[0]; invalid.time = now.time; refuse(invalid);
    invalid = values[0]; invalid.time = -1001; refuse(invalid);
    invalid = values[0]; invalid.pixels = nullptr; refuse(invalid);
    invalid = values[0]; invalid.bytes -= 16; refuse(invalid);
    invalid = values[0]; invalid.width = 0; refuse(invalid);
    invalid = values[0]; invalid.x = INT_MAX; refuse(invalid);
    invalid = values[0]; invalid.alpha = 1; refuse(invalid);
    invalid = values[0]; invalid.pixelAspectRatio = 1; refuse(invalid);
    invalid = values[0]; invalid.pixelAspectRatio = std::numeric_limits<double>::infinity(); refuse(invalid);
    auto nonfinite = past; nonfinite[3].g = std::numeric_limits<float>::quiet_NaN();
    invalid = values[0]; invalid.pixels = nonfinite.data(); refuse(invalid);
    std::array<SourceFrameView, 2> duplicate{values[0], values[0]};
    require(!frame.bindSourceFrames(duplicate.data(), 2) && frame.sourceFrames == values.data(), "duplicate exact times refused atomically");
    require(!frame.bindSourceFrames(nullptr, 1) && !frame.bindSourceFrames(values.data(), 257), "invalid list shape bounded before reading");
    auto recovered = fetch(image, property, source, values[0]);
    require(recovered.id == firstId && image.clipReleaseImage(recovered.handle) == kOfxStatOK, "accepted value survives all refusals");
    for (unsigned stopAt : {3u, 4u}) {
      StopProbe stop{0, stopAt};
      MondrianOpenFxInvocationControl control{};
      control.context = &stop; control.abort = abortBinding;
      const auto revision = frame.sourceBindingRevision;
      require(!frame.bindSourceFrames(&largeView, 1, &control) && stop.calls == stopAt &&
          frame.sourceFrames == values.data() && frame.sourceFrameCount == 2 && frame.sourceBindingRevision == revision,
          "original-owner stop during scan or before publication preserves the accepted binding");
      auto afterStop = fetch(image, property, source, values[0]);
      require(afterStop.id == firstId && image.clipReleaseImage(afterStop.handle) == kOfxStatOK,
          "no partial publication or stale borrow after canceled admission");
    }
    require(frame.bindSourceFrames(rebound.data(), rebound.size()), "new immutable snapshot after handles release");
    auto changed = fetch(image, property, source, rebound[0]);
    require(changed.id != firstId && image.clipReleaseImage(changed.handle) == kOfxStatOK,
        "same time and array slot cannot reuse an old snapshot identity");
    require(frame.bindSourceFrames(maximum.data(), maximum.size()), "exact additional-frame count limit");
    auto atLimit = fetch(image, property, source, maximum.back());
    require(image.clipReleaseImage(atLimit.handle) == kOfxStatOK, "last admitted value works");
  }
  require(!frame.sourceFrames && !frame.sourceFrameCount && !frame.input && frame.imageHandles == 0 && !frame.invalidImageLifetime,
      "original invocation retires every borrowed address and image capability");
  {
    MondrianOpenFx::FrameBorrow borrow(frame);
    frame.input = current.data(); frame.pixelCount = 6;
    require(frame.beginImageBinding() && frame.bindSourceFrames(values.data(), values.size()), "same owner binds next invocation");
    MondrianOpenFx::ImageAccess access(frame, true);
    auto again = fetch(image, property, source, values[0]);
    require(again.id != firstId && image.clipReleaseImage(again.handle) == kOfxStatOK, "reused backing cannot retain stale image identity");
  }
  // Density changes retain this exact instance and all temporal pixel origins.
  // Only metadata and canonical geometry change; row bytes and pixels do not.
  for (const auto scale : {OfxPointD{.5,.5}, OfxPointD{.75,.5}, OfxPointD{1,1}}) {
    frame.info.render_scale[0] = scale.x; frame.info.render_scale[1] = scale.y;
    MondrianOpenFx::FrameBorrow borrow(frame);
    frame.input = current.data(); frame.pixelCount = 6;
    require(frame.beginImageBinding() && frame.bindSourceFrames(values.data(),values.size()), "sampled immutable temporal set");
    MondrianOpenFx::ImageAccess access(frame,true);
    for (const auto& value : {now,values[0],values[1]}) {
      const auto observed = fetch(image,property,source,value,scale);
      require(image.clipReleaseImage(observed.handle) == kOfxStatOK, "sampled image release");
    }
    require(frame.imageHandles == 0 && !frame.invalidImageLifetime, "sampled leases settle on their original owner");
  }
  require(!std::memcmp(current.data(), savedCurrent.data(), sizeof(current)) &&
      !std::memcmp(past.data(), savedPast.data(), sizeof(past)) && !std::memcmp(future.data(), savedFuture.data(), sizeof(future)),
      "borrowed upstream pixels never mutated or owned by image wrappers");
  std::puts("SourceFrameBindings PASS: exact views, domains, identities, refusals, concurrent handles and invocation retirement");
  return 0;
}
}
int main(int argc, char** argv) {
  try {
    require(argc == 3, "official Basic binary and bundle required");
    MondrianOpenFx::Host host;
    struct Binding { ~Binding() { OFX::Host::ImageEffect::gImageEffectHost = nullptr; } } binding;
    return exercise(host, argv[1], argv[2]);
  } catch (const std::exception& error) {
    std::fprintf(stderr, "SourceFrameBindings FAILED: %s\n", error.what());
    return 1;
  }
}
