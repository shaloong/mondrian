// Copyright Mondrian contributors.
// SPDX-License-Identifier: BSD-3-Clause
// Supervise this executable like every native lab child. It links the actual
// product host/C ABI and loads real OFX binaries, not a replacement host.
#include "render.h"
#include "ofxPixels.h"
#include "ofxImageEffect.h"
#include <array>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <fstream>
#include <filesystem>
#include <limits>
#include <stdexcept>
#include <string>
#include <utility>

namespace {
void require(bool value, const char* message) {
  if (!value) { throw std::runtime_error(message); }
}
struct Session {
  MondrianOpenFxSession* value = nullptr;
  ~Session() { if (value) { mondrian_openfx_session_destroy(value); } }
};
struct Memory {
  bool grant = true;
  size_t last = 0;
  unsigned calls = 0;
};
struct Completion {
  int completion, output_alpha;
  size_t required_transient_bytes;
};
int acquire(const void* context, size_t bytes) {
  auto& memory = *static_cast<Memory*>(const_cast<void*>(context));
  ++memory.calls;
  memory.last = bytes;
  return memory.grant ? 1 : 0;
}
uint32_t dspMask() {
  return (1u << MONDRIAN_OPENFX_BEGIN_SEQUENCE) |
         (1u << MONDRIAN_OPENFX_PLUGIN_RENDER) |
         (1u << MONDRIAN_OPENFX_END_SEQUENCE);
}
std::array<unsigned, 3> generatorActions() {
  const char* path = std::getenv("MONDRIAN_OPENFX_GENERATOR_ACTION_LOG");
  require(path && *path, "GeneratorProbe action log is required");
  std::ifstream file(std::filesystem::u8path(path));
  require(file.good(), "read actual plugin action log");
  std::array<unsigned, 3> counts{};
  std::string action;
  while (std::getline(file, action)) {
    if (action == kOfxImageEffectActionBeginSequenceRender) { ++counts[0]; }
    if (action == kOfxImageEffectActionRender) { ++counts[1]; }
    if (action == kOfxImageEffectActionEndSequenceRender) { ++counts[2]; }
  }
  return counts;
}
}

int main(int argc, char** argv) {
  try {
    require(argc == 4, "mode, binary and bundle are required");
    const std::string mode(argv[1]);
    const bool generator = mode == "generator";
    require(generator || mode == "filter" || mode == "misc", "unknown case");
    const char* identifier = generator ? "org.mondrian.fixture.ofx-generator" :
        mode == "misc" ? "net.sf.openfx.Invert" : "uk.co.thefoundry.BasicGainPlugin";
    const uint32_t major = mode == "misc" ? 2 : 1;
    const std::array<uint8_t, 16> identity{1, 2, 3, 4};
    std::array<OfxRGBAColourF, 6> input{};
    std::array<OfxRGBAColourF, 6> output{};
    for (size_t i = 0; i < input.size(); ++i) {
      input[i] = {float(i + 1) / 16, float(i + 2) / 16, float(i + 3) / 16, float(i + 1) / 8};
    }
    MondrianOpenFxFrameInfo info{};
    info.source_alpha = 0; info.width = 3; info.height = 2;
    info.project_width = 3; info.project_height = 2;
    info.project_pixel_aspect_ratio = generator ? 2.0 : 1.0;
    info.frame = 0; info.frame_rate = 24; info.first_frame = 0; info.last_frame = 20;
    info.pixel_aspect_ratio = generator ? 2.0 : 1.0;
    MondrianOpenFxParameter scale{};
    scale.name = "scale"; scale.kind = 1; scale.value[0] = 0.5;
    MondrianOpenFxParameter alpha{};
    alpha.name = "NatronOfxParamProcessA"; alpha.kind = 2; alpha.value[0] = 0;
    const auto* parameters = generator ? nullptr : mode == "filter" ? &scale : &alpha;
    const int parameterCount = generator ? 0 : 1;
    Session session;
    require(mondrian_openfx_session_create(argv[2], argv[3], identifier, major, 0,
        generator ? 1 : 0, identity.data(), generator ? nullptr : input.data(), sizeof(input),
        &info, parameters, parameterCount,
        &session.value) == 0 && session.value, "create real native owner");
    const auto owner = session.value;
    // Invalid owner geometry is refused before native actions and does not
    // poison the accepted instance used by all subsequent successful frames.
    auto wrong = info;
    ++wrong.project_width;
    auto rejected = std::make_unique<MondrianOpenFxRenderReport>();
    MondrianOpenFxNativeDiagnostic rejectedDiagnostic{};
    for (auto& pixel : output) { pixel = {123, 123, 123, 123}; }
    require(mondrian_openfx_session_render(session.value, generator ? nullptr : input.data(),
        output.data(), sizeof(output), &wrong, parameters, parameterCount, rejected.get(),
        nullptr, &rejectedDiagnostic) == 2 && rejected->completion == MONDRIAN_OPENFX_RENDER_UNSET &&
        rejectedDiagnostic.present_mask == (1u << MONDRIAN_OPENFX_AUXILIARY_REPORT),
        "owner mismatch allows only host feedback reporting, no preflight/native phases");
    for (const auto& pixel : output) { require(pixel.r == 123 && pixel.a == 123, "owner mismatch preserves staging"); }
    if (!generator) {
      auto invalid = input;
      invalid[0].r = std::numeric_limits<float>::quiet_NaN();
      MondrianOpenFxSession* refused = nullptr;
      require(mondrian_openfx_session_create(argv[2], argv[3], identifier, major, 0, 0,
          identity.data(), invalid.data(), sizeof(invalid), &info, parameters, parameterCount,
          &refused) == 2 && !refused, "invalid pixels refuse before acquiring another native owner");
    }
    Memory memory;
    MondrianOpenFxInvocationControl control{};
    control.context = &memory; control.acquire_memory = acquire;
    auto render = [&](double time) {
      info.frame = time;
      // The actual ABI report contains a large bounded notice array. Keep its
      // owner on the heap; returning only checked scalar observations avoids
      // multiplying that storage across the native main-thread stack.
      auto report = std::make_unique<MondrianOpenFxRenderReport>();
      MondrianOpenFxNativeDiagnostic diagnostic{};
      for (auto& pixel : output) { pixel = {123, 123, 123, 123}; }
      const int status = mondrian_openfx_session_render(session.value,
          generator ? nullptr : input.data(), output.data(), sizeof(output), &info,
          parameters, parameterCount,
          report.get(), &control, &diagnostic);
      require(session.value == owner, "same owner throughout preflight and processing");
      require(status == 0 && diagnostic.counter_overflowed == 0, "native frame must succeed");
      return std::make_pair(Completion{report->completion, report->output_alpha,
                                     report->required_transient_bytes}, diagnostic);
    };
    if (!generator) {
      for (int pass = 0; pass < 4; ++pass) {
        // Same-time value changes require UserEdited notifications with legal
        // current Source image access; the existing SessionProbe checks both
        // independent image handles and releases inside that native action.
        scale.value[0] = pass % 2 ? 0.75 : 0.5;
        const auto [report, diagnostic] = render(0);
        require(report.completion == MONDRIAN_OPENFX_RENDER_FRAME && report.output_alpha == 0,
            "complete straight native frame");
        require((diagnostic.present_mask & dspMask()) == dspMask(), "real Begin/Render/End");
        for (size_t i = 0; i < input.size(); ++i) {
          const auto& a = input[i]; const auto& b = output[i];
          const float gain = float(scale.value[0]);
          require(b.r == (mode == "filter" ? a.r * gain : 1 - a.r) &&
              b.g == (mode == "filter" ? a.g * gain : 1 - a.g) &&
              b.b == (mode == "filter" ? a.b * gain : 1 - a.b) &&
              b.a == (mode == "filter" ? a.a * gain : a.a), "independent complete RGBA");
        }
      }
      require(memory.calls == 0, "full viewport needs no transient domain allocation");
    } else {
      const auto first = render(0);
      require(first.first.completion == MONDRIAN_OPENFX_RENDER_FRAME && memory.last == 64,
          "partial native domain admitted exactly");
      const auto actions = generatorActions();
      require(actions == std::array<unsigned, 3>{1, 1, 1}, "actual first render bracket");
      memory.grant = false;
      const auto denied = render(1);
      require(denied.first.completion == MONDRIAN_OPENFX_RENDER_MEMORY_REFUSED &&
          denied.first.required_transient_bytes == 320 && memory.last == 320,
          "preflight refusal from original callback");
      require((denied.second.present_mask & dspMask()) == 0,
          "no Begin/Render/End after domain refusal");
      require(generatorActions() == actions, "plugin confirms denial ran no DSP action");
      for (const auto& pixel : output) { require(pixel.r == 123 && pixel.a == 123, "denial leaves staging unchanged"); }
      memory.grant = true;
      const auto expanded = render(1);
      require(expanded.first.completion == MONDRIAN_OPENFX_RENDER_FRAME &&
          expanded.first.output_alpha == MONDRIAN_OPENFX_ALPHA_OPAQUE &&
          (expanded.second.present_mask & dspMask()) == dspMask(), "same valid owner retries admitted frame");
      require(generatorActions() == std::array<unsigned, 3>{2, 2, 2}, "one actual admitted retry bracket");
      for (int y = 0; y < 2; ++y) {
        for (int x = 0; x < 3; ++x) {
          const auto& pixel = output[size_t(1 - y) * 3 + x];
          require(pixel.r == float(x + 4) / 16 && pixel.g == float(y + 3) / 8 &&
              pixel.b == 1.0f / 16 && pixel.a == 1, "expanded domain complete RGBA");
        }
      }
      const auto empty = render(2);
      require(empty.first.completion == MONDRIAN_OPENFX_RENDER_FRAME &&
          (empty.second.present_mask & dspMask()) == 0, "empty domain has no render bracket");
      require(generatorActions() == std::array<unsigned, 3>{2, 2, 2} && memory.calls == 3,
          "plugin confirms empty output needs no DSP or further allocation");
      for (const auto& pixel : output) { require(pixel.r == 0 && pixel.g == 0 && pixel.b == 0 && pixel.a == 0, "empty domain is transparent"); }
    }
    std::printf("FramePreparation PASS mode=%s same_owner=1\n", mode.c_str());
    return 0;
  } catch (const std::exception& error) {
    std::fprintf(stderr, "FramePreparation FAILED: %s\n", error.what());
    return 1;
  }
}
