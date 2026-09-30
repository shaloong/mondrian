// Per-session frame facts. Native instances never share mutable raster state.
#ifndef MONDRIAN_OPENFX_FRAME_STATE_H
#define MONDRIAN_OPENFX_FRAME_STATE_H
#include "render.h"
#include "ofxPixels.h"
#include <vector>
namespace MondrianOpenFx {
  struct FrameState {
    MondrianOpenFxFrameInfo info{};
    std::vector<OfxRGBAColourF> input;
  };
}
#endif
