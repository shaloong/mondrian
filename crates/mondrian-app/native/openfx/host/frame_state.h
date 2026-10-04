// Per-session frame facts. Native instances never share mutable raster state.
#ifndef MONDRIAN_OPENFX_FRAME_STATE_H
#define MONDRIAN_OPENFX_FRAME_STATE_H
#include "render.h"
#include "ofxPixels.h"
#include <cstddef>
#include <atomic>
#include <map>
#include <string>
namespace MondrianOpenFx {
  struct ParameterSource {
    int mode;
    MondrianOpenFxQuery query;
    const void* context;
    uint64_t revision;
  };
  struct FrameState {
    MondrianOpenFxFrameInfo info{};
    std::map<std::string, ParameterSource> parameterSources;
    bool applyingParameters = false;
    bool constantParameter(const std::string& name) const {
      return parameterSources.count(name) == 0;
    }
    const ParameterSource* curveParameter(const std::string& name) const {
      const auto found = parameterSources.find(name);
      return found != parameterSources.end() && found->second.mode == 2 ? &found->second : nullptr;
    }
    bool permitsParameterWrite(const std::string& name) const {
      return applyingParameters || !curveParameter(name);
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
    size_t pixelCount = 0;
    bool imagesAvailable = false;
    bool outputAvailable = false;
    mutable std::atomic<size_t> imageHandles{0};
    bool invalidImageLifetime = false;
  };
  struct ParameterAssignment {
    FrameState& state;
    explicit ParameterAssignment(FrameState& frame) : state(frame) { state.applyingParameters = true; }
    ~ParameterAssignment() { state.applyingParameters = false; }
  };
  // Every exit, including exceptions, retires caller-owned frame addresses.
  struct FrameBorrow {
    FrameState& state;
    explicit FrameBorrow(FrameState& frame) : state(frame) {}
    ~FrameBorrow() {
      state.input = nullptr;
      state.output = nullptr;
      state.pixelCount = 0;
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
