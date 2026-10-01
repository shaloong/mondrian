// Mondrian OpenFX filter admission ABI. Native code is called only in a supervised child.
#ifndef MONDRIAN_OPENFX_RENDER_H
#define MONDRIAN_OPENFX_RENDER_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

struct MondrianOpenFxParameter {
  const char* name;
  int kind; // 1: double, 2: boolean, 3: integer
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

struct MondrianOpenFxParameterInfo {
  const char* name;
  size_t name_length;
  const char* label;
  size_t label_length;
  const char* hint;
  size_t hint_length;
  int kind; // 1: double, 2: boolean, 3: integer
  int double_type; // 0: plain, 1: scale, -1: not a Double
  double default_value;
  double minimum;
  double maximum;
  double display_minimum;
  double display_maximum;
  int can_animate;
  int secret;
  int enabled;
};

typedef int (*MondrianOpenFxParameterCallback)(
    void* context, const struct MondrianOpenFxParameterInfo* parameter);

int mondrian_openfx_describe(
    const char* binary,
    const char* bundle,
    const char* identifier,
    char* label,
    size_t label_capacity,
    size_t* label_length,
    MondrianOpenFxParameterCallback callback,
    void* context)
#ifdef __cplusplus
    noexcept
#endif
    ;

// Handles are owned by one supervised child. Calls on a handle are serialized.
struct MondrianOpenFxSession;

int mondrian_openfx_session_create(
    const char* binary, const char* bundle, const char* identifier,
    const void* input, size_t frame_bytes,
    const struct MondrianOpenFxFrameInfo* frame_info,
    const struct MondrianOpenFxParameter* parameters, int parameter_count,
    struct MondrianOpenFxSession** session)
#ifdef __cplusplus
    noexcept
#endif
    ;

// Output is caller-owned staging storage, valid only when status is zero.
// Native actions may modify staging on failure; publication belongs to Rust.
int mondrian_openfx_session_render(
    struct MondrianOpenFxSession* session, const void* input, void* output,
    size_t frame_bytes, const struct MondrianOpenFxFrameInfo* frame_info,
    const struct MondrianOpenFxParameter* parameters, int parameter_count)
#ifdef __cplusplus
    noexcept
#endif
    ;

void mondrian_openfx_session_destroy(struct MondrianOpenFxSession* session)
#ifdef __cplusplus
    noexcept
#endif
    ;

#ifdef __cplusplus
}
#endif

#endif
