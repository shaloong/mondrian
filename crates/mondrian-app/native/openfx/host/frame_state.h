// Per-session frame facts. Native instances never share mutable raster state.
#ifndef MONDRIAN_OPENFX_FRAME_STATE_H
#define MONDRIAN_OPENFX_FRAME_STATE_H
#include "render.h"
#include "ofxCore.h"
#include "ofxPixels.h"
#include <cstddef>
#include <atomic>
#include <map>
#include <string>
#include <limits>
#include <array>
#include <algorithm>
#include <cstdint>
#include <cstring>
namespace MondrianOpenFx {
  // The OFX Float32 ABI uses IEEE binary32. Classify its representation without
  // arithmetic or aliasing: every exponent-255 lane is nonfinite, including
  // both signs and all quiet/signaling NaN payloads. Other bits are untouched.
  // In particular MSVC's std::isfinite(float) otherwise calls CRT _fdtest for
  // every channel in both complete-raster admission scans.
  inline bool finiteFloatRgba(const OfxRGBAColourF& pixel) noexcept {
    static_assert(std::numeric_limits<float>::is_iec559 && sizeof(float) == sizeof(std::uint32_t),
                  "OFX Float32 requires IEEE binary32");
    std::uint32_t bits[4];
    static_assert(sizeof(bits) == sizeof(pixel), "OFX RGBA has four packed float lanes");
    std::memcpy(bits, &pixel, sizeof(bits));
    constexpr std::uint32_t exponent = 0x7f800000u;
    return (bits[0] & exponent) != exponent && (bits[1] & exponent) != exponent &&
           (bits[2] & exponent) != exponent && (bits[3] & exponent) != exponent;
  }
  // Private canonical ordinal-domain query, not animation key count (3).
  constexpr int kChoiceOrdinalDomainSizeQuery = 6;
  struct ParameterSource {
    int mode;
    MondrianOpenFxQuery query;
    const void* context;
    uint64_t revision;
  };
  struct FrameState {
    MondrianOpenFxFrameInfo info{};
    // Borrowed only for an explicitly measured action. Legal callback threads
    // join before CABI return; only Output first-fetch writes the nested clear.
    MondrianOpenFxNativeDiagnostic* renderDiagnostic = nullptr;
    // Parent-issued lifetime namespace survives no reprepare/reopen. Plugins
    // may persist UID-indexed analysis beyond one child, so process counters
    // alone cannot identify image content across fresh owners.
    std::array<uint8_t, 16> imageNamespace{};
    uint64_t imageGeneration = 0;
    std::string sourceIdentifier;
    std::string outputIdentifier;
    bool beginImageBinding() {
      if (std::all_of(imageNamespace.begin(), imageNamespace.end(), [](uint8_t byte) { return byte == 0; }) || imageGeneration == std::numeric_limits<uint64_t>::max()) { return false; }
      ++imageGeneration;
      constexpr char hex[] = "0123456789abcdef";
      char encoded[32];
      for (size_t index = 0; index < imageNamespace.size(); ++index) {
        encoded[index * 2] = hex[imageNamespace[index] >> 4];
        encoded[index * 2 + 1] = hex[imageNamespace[index] & 15];
      }
      const std::string prefix = "mondrian-ofx:" + std::string(encoded, sizeof(encoded)) + ":" + std::to_string(imageGeneration) + ":";
      sourceIdentifier = prefix + "Source";
      outputIdentifier = prefix + "Output";
      return true;
    }

    std::map<std::string, ParameterSource> parameterSources;
    bool applyingParameters = false;
    // Native setters cannot mutate parameters during the ordinary CPU Render
    // callback. This parameter capability is independent of image access.
    bool renderParameterWritesBlocked = false;
    // The ordinary host-owned Inspector can write through the author session;
    // plugin-origin writes require the later coherent C2 author bridge.
    bool readOnlyInteractive = false;
    bool constantParameter(const std::string& name) const {
      return parameterSources.count(name) == 0;
    }
    const ParameterSource* curveParameter(const std::string& name) const {
      const auto found = parameterSources.find(name);
      return found != parameterSources.end() && found->second.mode == 2 ? &found->second : nullptr;
    }
    bool permitsParameterWrite(const std::string& name) const {
      return applyingParameters || (!renderParameterWritesBlocked && !readOnlyInteractive && !curveParameter(name));
    }
    int queryParameter(const std::string& name, int operation, double start,
                       double end, int index, int direction, MondrianOpenFxQueryResult& result) const {
      const auto* source = curveParameter(name);
      return source ? source->query(source->context, operation, start, end, index, direction, &result) : 4;
    }
    bool permitsParameterTime(const std::string& name, double time) const {
      return constantParameter(name) || time == info.frame;
    }
    const OfxRGBAColourF* input = nullptr;
    OfxRGBAColourF* output = nullptr;
    // Output image support is independent of the caller's fixed viewport.
    // Only Render borrows output storage; no backing survives FrameBorrow.
    OfxRectD outputCanonicalRod{};
    OfxRectI outputPixelRod{};
    size_t outputPixelCount = 0;
    bool outputRodReady = false;
    std::atomic<bool> queryingOutputRod{false};
    size_t pixelCount = 0;
    bool imagesAvailable = false;
    bool outputAvailable = false;
    mutable std::atomic<size_t> imageHandles{0};
    bool invalidImageLifetime = false;
    // A transient unsupported ordinal-domain change is terminal even if the
    // plugin restores its options or ignores a failed value query.
    mutable std::atomic<bool> invalidChoiceDomain{false};
    bool admitsChoiceDomain(int count, MondrianOpenFxQuery query, const void* context) const {
      MondrianOpenFxQueryResult result{};
      if (count < 1 || !query || !context ||
          query(context, kChoiceOrdinalDomainSizeQuery, 0, 0, 0, 0, &result) != 0 ||
          result.key_count != static_cast<unsigned int>(count)) {
        invalidChoiceDomain.store(true, std::memory_order_relaxed);
        return false;
      }
      return true;
    }
    bool admitsChoiceDomain(const std::string& name, int count) const {
      const auto* source = curveParameter(name);
      return !source || admitsChoiceDomain(count, source->query, source->context);
    }
  };
  struct ParameterAssignment {
    FrameState& state;
    explicit ParameterAssignment(FrameState& frame) : state(frame) { state.applyingParameters = true; }
    ~ParameterAssignment() { state.applyingParameters = false; }
  };
  // Restore the prior capability on every exit, including native exceptions;
  // a nested scope must never unblock its enclosing Render callback.
  struct RenderParameterWriteBlock {
    FrameState& state;
    const bool previous;
    explicit RenderParameterWriteBlock(FrameState& frame) noexcept
      : state(frame), previous(frame.renderParameterWritesBlocked) {
      state.renderParameterWritesBlocked = true;
    }
    ~RenderParameterWriteBlock() noexcept { state.renderParameterWritesBlocked = previous; }
    RenderParameterWriteBlock(const RenderParameterWriteBlock&) = delete;
    RenderParameterWriteBlock& operator=(const RenderParameterWriteBlock&) = delete;
    RenderParameterWriteBlock(RenderParameterWriteBlock&&) = delete;
    RenderParameterWriteBlock& operator=(RenderParameterWriteBlock&&) = delete;
  };
  // Every exit, including exceptions, retires caller-owned frame addresses.
  struct FrameBorrow {
    FrameState& state;
    explicit FrameBorrow(FrameState& frame) : state(frame) {}
    ~FrameBorrow() {
      state.input = nullptr;
      state.output = nullptr;
      state.pixelCount = 0;
      state.outputPixelCount = 0;
      state.outputRodReady = false;
      state.imagesAvailable = false;
      state.outputAvailable = false;
    }
  };
  struct ImageAccess {
    FrameState& state;
    const bool previous;
    const bool previousOutput;
    ImageAccess(FrameState& frame, bool available, bool output = false)
      : state(frame), previous(frame.imagesAvailable), previousOutput(frame.outputAvailable) {
      state.imagesAvailable = available;
      state.outputAvailable = output;
    }
    ~ImageAccess() {
      if (state.imageHandles.load() != 0) { state.invalidImageLifetime = true; }
      state.imagesAvailable = previous;
      state.outputAvailable = previousOutput;
    }
  };
}
#endif
