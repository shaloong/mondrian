// Copyright Mondrian contributors.
// SPDX-License-Identifier: BSD-3-Clause
// Native action boundary, not an end-to-end temporal rendering qualification.
#include "ofxhPluginCache.h"
#include "ofxhImageEffectAPI.h"
#include "ofxhClip.h"
#include "host_descriptor.h"
#include "frame_state.h"
#include "effect_instance.h"
#include <cmath>
#include <cstdio>
#include <cstring>
#include <limits>
#include <memory>
#include <stdexcept>
#include <string>

namespace {
using namespace OFX::Host::ImageEffect;
void require(bool ok, const char* message) { if (!ok) { throw std::runtime_error(message); } }
bool same(const RangeMap& a, const RangeMap& b) {
  if (a.size() != b.size()) { return false; }
  for (const auto& entry : a) {
    const auto it = b.find(entry.first);
    if (it == b.end() || it->second.size() != entry.second.size()) { return false; }
    for (size_t i = 0; i < entry.second.size(); ++i) {
      if (it->second[i].min != entry.second[i].min || it->second[i].max != entry.second[i].max) { return false; }
    }
  }
  return true;
}
int exercise(const char* group, MondrianOpenFx::Host& host, const char* binaryPath, const char* bundlePath) {
  PluginCache images(host);
  OFX::Host::PluginCache plugins;
  images.registerInCache(plugins);
  OFX::Host::PluginBinary binary(binaryPath, bundlePath, &plugins);
  require(!binary.isInvalid() && binary.getNPlugins() == 1, "one actual fixture binary");
  auto* plugin = dynamic_cast<ImageEffectPlugin*>(&binary.getPlugin(0));
  require(plugin && plugin->getIdentifier() == "org.mondrian.fixture.frame-dependencies", "exact protocol fixture");
  MondrianOpenFx::FrameState frame;
  frame.info.width = frame.info.height = 1;
  frame.info.frame_rate = 24; frame.info.first_frame = -1000; frame.info.last_frame = 1000;
  frame.info.pixel_aspect_ratio = 1;
  // Production descriptor admission remains current-frame-only. Deliberately
  // construct the fixture here to test the actual HostSupport action method,
  // without advertising temporal support or providing a replacement Host.
  std::unique_ptr<Instance> instance(plugin->createInstance(kOfxImageEffectContextFilter, &frame));
  require(instance && instance->createInstanceAction() == kOfxStatOK, "actual native instance");
  auto* source = instance->getClip("Source");
  auto* aux = instance->getClip("Aux");
  auto* output = instance->getClip("Output");
  require(source && aux && output, "two named input clips and one output");
  const RangeMap sentinel{{output, {{101, 102}}}, {source, {{-99, -98}}}};
  unsigned cases = 0;
  auto query = [&](double time, int expected) {
    frame.info.frame = time;
    RangeMap actual = sentinel;
    const int status = instance->getFrameNeededAction(time, actual);
    std::printf("case time=%.17g status=%d expected=%d\n", time, status, expected);
    require(status == expected, "exact action/refusal status");
    if (status != kOfxStatOK && status != kOfxStatReplyDefault) {
      require(same(actual, sentinel), "failed negotiation cannot publish partial/default dependencies");
    } else {
      require(actual.size() == 2 && actual.count(source) && actual.count(aux) && !actual.count(output), "success replaces stale map and excludes Output");
    }
    ++cases;
    return actual;
  };
  auto defaults = [&](const RangeMap& actual, double time) {
    require(same(actual, RangeMap{{source, {{time, time}}}, {aux, {{time, time}}}}), "exact per-input default, no residual writes");
  };
  if (!std::strcmp(group, "nominal")) {
    const auto actual = query(.25, kOfxStatOK);
    require(same(actual, RangeMap{{source, {{-3.5, -2.5}, {8, 8}}}, {aux, {{.25, .25}}}}), "preserve negative/fractional/disjoint ranges without hull or rounding");
    require(same(query(.25, kOfxStatOK), actual), "same instance repeated action has no accumulation");
    defaults(query(1.25, kOfxStatReplyDefault), 1.25);
    auto empty = query(2.25, kOfxStatOK);
    require(empty.at(source).empty() && empty.at(aux).size() == 1, "explicit empty is not current-frame substitution");
    empty = query(15.25, kOfxStatOK);
    require(empty.at(source).empty() && empty.at(aux).empty(), "all inputs may be explicitly empty");
    defaults(query(16.25, kOfxStatOK), 16.25);
  } else if (!std::strcmp(group, "errors")) {
    query(3.25, kOfxStatFailed); query(4.25, kOfxStatErrMemory);
    query(5.25, kOfxStatErrFatal); query(17.25, 12345);
  } else if (!std::strcmp(group, "malformed")) {
    for (int test : {6, 7, 8, 9, 10}) { query(test + .25, kOfxStatFailed); }
  } else if (!std::strcmp(group, "limits")) {
    const auto actual = query(11.25, kOfxStatOK);
    require(actual.at(source).size() == 256 && actual.at(aux).empty(), "exact bounded maximum");
    for (int i = 0; i < 256; ++i) {
      require(actual.at(source)[i].min == i - 128.5 && actual.at(source)[i].max == i - 128.5, "maximum ranges retained exactly");
    }
    for (int test : {12, 13, 14, 18}) { query(test + .25, kOfxStatErrUnsupported); }
    defaults(query(19.25, kOfxStatReplyDefault), 19.25);
    defaults(query(16.25, kOfxStatOK), 16.25);
  } else if (!std::strcmp(group, "time")) {
    query(std::numeric_limits<double>::quiet_NaN(), kOfxStatErrValue);
    query(std::numeric_limits<double>::infinity(), kOfxStatErrValue);
    query(-std::numeric_limits<double>::infinity(), kOfxStatErrValue);
    // The no-temporal contract must skip the foreign action, with defaults at
    // any finite time, rather than asking a plugin that did not declare it.
    instance->getProps().createProperty({kOfxImageEffectPropTemporalClipAccess, OFX::Host::Property::eInt, 1, false, "0"});
    defaults(query(-1.5, kOfxStatReplyDefault), -1.5);
    instance->getProps().setIntProperty(kOfxImageEffectPropTemporalClipAccess, 1);
    defaults(query(16.25, kOfxStatOK), 16.25);
  } else { throw std::runtime_error("unknown group"); }
  std::printf("FrameDependencies PASS group=%s cases=%u\n", group, cases);
  return 0;
}
}
int main(int argc, char** argv) {
  try {
    require(argc == 4, "group binary bundle required");
    MondrianOpenFx::Host host;
    struct Binding {
      ~Binding() { OFX::Host::ImageEffect::gImageEffectHost = nullptr; }
    } binding;
    return exercise(argv[1], host, argv[2], argv[3]);
  } catch (const std::exception& error) {
    std::fprintf(stderr, "FrameDependencies FAILED: %s\n", error.what());
    return 1;
  }
}
