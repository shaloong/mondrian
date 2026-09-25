// Mondrian OpenFX filter admission ABI. Native code is called only in a supervised child.
#ifndef MONDRIAN_OPENFX_RENDER_H
#define MONDRIAN_OPENFX_RENDER_H

#ifdef __cplusplus
extern "C" {
#endif

struct MondrianOpenFxParameter {
  const char* name;
  int kind; // 1: double, 2: boolean
  double value;
};

struct MondrianOpenFxFrameInfo {
  int width;
  int height;
  double frame;
  double frame_rate;
  double first_frame;
  double last_frame;
  double pixel_aspect_ratio;
};

int mondrian_openfx_render(
    const char* binary,
    const char* bundle,
    const char* identifier,
    const char* input,
    const char* output,
    const struct MondrianOpenFxFrameInfo* frame_info,
    const struct MondrianOpenFxParameter* parameters,
    int parameter_count)
#ifdef __cplusplus
    noexcept
#endif
    ;

#ifdef __cplusplus
}
#endif

#endif
