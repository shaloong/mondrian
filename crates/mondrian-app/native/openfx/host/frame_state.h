// Per-session frame facts. Native instances never share mutable raster state.
#ifndef MONDRIAN_OPENFX_FRAME_STATE_H
#define MONDRIAN_OPENFX_FRAME_STATE_H
#include "render.h"
#include "ofxPixels.h"
#include <cstddef>
#include <atomic>
#include <set>
#include <string>
namespace MondrianOpenFx {
  struct FrameState {
    MondrianOpenFxFrameInfo info{};
    std::set<std::string> frameSampleParameters;
    bool constantParameter(const std::string& name) const {
      return frameSampleParameters.count(name) == 0;
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
