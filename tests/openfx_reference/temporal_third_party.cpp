// Copyright Mondrian contributors.
// SPDX-License-Identifier: BSD-3-Clause
// Supervised, actual product C ABI with the published OpenFX-Misc TimeBlur 1.0.
// This finite native control does not qualify Timeline/Preview/Export or RT.
#include "render.h"
#include "ofxCore.h"
#include "ofxPixels.h"
#include <array>
#include <atomic>
#include <cstdio>
#include <cstring>
#include <memory>
#include <stdexcept>

namespace {
void require(bool value, const char* message) {
  if (!value) { throw std::runtime_error(message); }
}
using Pixels = std::array<OfxRGBAColourF, 6>;
Pixels signal(double time, float seed) {
  Pixels result{};
  for (size_t i = 0; i < result.size(); ++i) {
    const float base = float(i) / 16;
    result[i] = {base + float(time) + seed, -base - 2 * float(time) - seed,
                 4 * base + float(time * time) + seed, 1};
  }
  return result;
}
struct Sources {
  const size_t count;
  std::array<double, 8> times{};
  std::array<Pixels, 8> pixels;
  mutable std::atomic<unsigned> fetched{0};
  Sources(double time, float seed, size_t divisions) : count(divisions) {
    for (size_t i = 0; i < count; ++i) {
      times[i] = time - .5 + double(i) / divisions;
      pixels[i] = signal(times[i], seed);
    }
  }
  static int fetch(const void* context, double time, MondrianOpenFxSourceFrame* output,
                   size_t* index) noexcept {
    const auto& self = *static_cast<const Sources*>(context);
    for (size_t i = 0; i < self.count; ++i) {
      if (time != self.times[i]) { continue; }
      *output = {time, self.pixels[i].data(), sizeof(Pixels), 0, 0, 3, 2, 0, 1};
      *index = i + 1;
      self.fetched.fetch_or(1u << i);
      return kOfxStatOK;
    }
    return kOfxStatFailed;
  }
};
struct Description {
  unsigned division = 0, shutter = 0, offset = 0;
  static int observe(void* context, const MondrianOpenFxParameterInfo* parameter) {
    auto& self = *static_cast<Description*>(context);
    if (std::strcmp(parameter->name, "division") == 0 && parameter->kind == 3) { ++self.division; }
    if (std::strcmp(parameter->name, "shutter") == 0 && parameter->kind == 1) { ++self.shutter; }
    if (std::strcmp(parameter->name, "shutterOffset") == 0 && parameter->kind == 4) { ++self.offset; }
    return 0;
  }
};
struct Session {
  MondrianOpenFxSession* value = nullptr;
  ~Session() { if (value) { mondrian_openfx_session_destroy(value); } }
};
}

int main(int argc, char** argv) {
  try {
    require(argc == 3, "published Misc binary and bundle required");
    const char* identifier = "net.sf.openfx.TimeBlur";
    Description description;
    auto notices = std::make_unique<MondrianOpenFxDescribeNotices>();
    char label[256]{};
    size_t length = 0;
    require(mondrian_openfx_describe(argv[1], argv[2], identifier, 1, 0, 0,
        label, sizeof(label), &length, Description::observe, &description, notices.get()) == 0,
        "admit actual published TimeBlur descriptor");
    require(std::strcmp(label, "TimeBlurOFX") == 0 && description.division == 1 &&
        description.shutter == 1 && description.offset == 1, "exact external parameter kinds");
    std::array<MondrianOpenFxParameter, 3> parameters{};
    parameters[0].name = "division"; parameters[0].kind = 3; parameters[0].value[0] = 4;
    parameters[1].name = "shutter"; parameters[1].kind = 1; parameters[1].value[0] = 1;
    parameters[2].name = "shutterOffset"; parameters[2].kind = 4; parameters[2].value[0] = 0;
    MondrianOpenFxFrameInfo info{};
    info.width = info.project_width = 3;
    info.height = info.project_height = 2;
    info.pixel_aspect_ratio = info.project_pixel_aspect_ratio = 1;
    info.render_scale[0] = info.render_scale[1] = 1;
    info.frame = 2; info.frame_rate = 24; info.first_frame = 0; info.last_frame = 10;
    Pixels current = signal(2, 0), output{};
    const std::array<uint8_t, 16> identity{9, 8, 7, 6};
    Session session;
    require(mondrian_openfx_session_create(argv[1], argv[2], identifier, 1, 0, 0,
        identity.data(), current.data(), sizeof(current), &info, parameters.data(),
        int(parameters.size()), &session.value) == 0 && session.value, "create actual temporal owner");
    const auto owner = session.value;
    // Unit centered shutter excludes its end. Divisions8 also exercises the
    // plugin's multi-pass ImageMemory accumulator. Independent moment literals.
    const std::array<double, 6> times{2, 3, 2, 2, 0, 10};
    const std::array<size_t, 6> divisions{4, 8, 4, 1, 4, 4};
    const std::array<unsigned, 6> sourceMasks{0b1011, 0b11101111, 0b1011, 1, 0b1000, 0b0011};
    // Outside [0,10], Source has empty RoD and no image. Its contribution is
    // transparent black, not a clamped endpoint or a renormalized average.
    const std::array<float, 6> coverage{1, 1, 1, 1, .5f, .75f};
    const std::array<float, 6> mean{1.875f, 2.9375f, 1.875f, 1.5f, .0625f, 7.3125f};
    const std::array<float, 6> squareMean{3.59375f, 8.7109375f, 3.59375f, 2.25f, .015625f, 71.328125f};
    for (size_t pass = 0; pass < times.size(); ++pass) {
      info.frame = times[pass];
      const float seed = float(pass) / 4;
      parameters[0].value[0] = double(divisions[pass]);
      current = signal(info.frame, seed);
      const auto original = current;
      Sources sources(info.frame, seed, divisions[pass]);
      const MondrianOpenFxSourceProvider provider{&sources, Sources::fetch};
      auto report = std::make_unique<MondrianOpenFxRenderReport>();
      const int status = mondrian_openfx_session_render_resolved(session.value,
          current.data(), output.data(), sizeof(output), &info, parameters.data(),
          int(parameters.size()), report.get(), nullptr, nullptr, nullptr, 0, &provider);
      if (status != 0) { std::fprintf(stderr, "TimeBlur pass %zu native status %d\n", pass, status); }
      require(status == 0 && report->completion == MONDRIAN_OPENFX_RENDER_FRAME,
          "actual TimeBlur processing succeeds");
      require(session.value == owner && std::memcmp(current.data(), original.data(), sizeof(current)) == 0,
          "same native owner and immutable current input");
      require(sources.fetched.load() == sourceMasks[pass], "exact fractional historical/future sources");
      for (size_t i = 0; i < output.size(); ++i) {
        const float base = float(i) / 16;
        const float weight = coverage[pass];
        const OfxRGBAColourF expected{weight * base + mean[pass] + weight * seed,
                                     -weight * base - 2 * mean[pass] - weight * seed,
                                     4 * weight * base + squareMean[pass] + weight * seed, weight};
        require(std::memcmp(&output[i], &expected, sizeof(expected)) == 0, "independent full Float32 RGBA oracle");
      }
    }
    std::puts("TemporalThirdParty PASS: TimeBlur1.0 fractional sources/full RGBA/same owner; original4 passes plus first/last-frame transparent absence, changed inputs, accumulator and temporal Identity");
    return 0;
  } catch (const std::exception& error) {
    std::fprintf(stderr, "TemporalThirdParty FAILED: %s\n", error.what());
    return 1;
  }
}
