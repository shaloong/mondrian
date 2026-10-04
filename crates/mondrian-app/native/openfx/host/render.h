// Mondrian OpenFX filter admission ABI. Native code is called only in a supervised child.
#ifndef MONDRIAN_OPENFX_RENDER_H
#define MONDRIAN_OPENFX_RENDER_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

enum {
  MONDRIAN_OPENFX_MAX_STRING_BYTES = 64 * 1024,
  MONDRIAN_OPENFX_MAX_PARAMETER_STRING_BYTES = 1024 * 1024
};

struct MondrianOpenFxParameter {
  const char* name;
  int query_mode; // 0: constant binding, 1: current-frame sample without a curve
  int kind; // 1: double, 2: boolean, 3: integer, 4: choice, 5: string choice, 6: string; 7: Double2D, 8: Double3D, 9: RGB, 10: RGBA, 11: Integer2D, 12: Integer3D
  double value[4];
  const char* string_value;
  size_t string_length;
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

struct MondrianOpenFxChoiceOption {
  const char* label;
  size_t label_length;
  int rank_present;
  int display_rank;
  const char* key;
  size_t key_length;
};

struct MondrianOpenFxParameterInfo {
  const char* name;
  size_t name_length;
  const char* label;
  size_t label_length;
  const char* hint;
  size_t hint_length;
  int kind; // 1: double, 2: boolean, 3: integer, 4: choice, 5: string choice, 6: string; 7: Double2D, 8: Double3D, 9: RGB, 10: RGBA, 11: Integer2D, 12: Integer3D
  int preferred_interpolation; // 0: hold, 1: linear, 2: smooth
  int double_type; // 0: plain, 1: scale, -1: not a Double
  double default_value[4];
  double minimum[4];
  double maximum[4];
  double display_minimum[4];
  double display_maximum[4];
  int can_animate;
  int secret;
  int enabled;
  const struct MondrianOpenFxChoiceOption* choices;
  size_t choice_count;
  const char* string_default;
  size_t string_default_length;
  int string_mode; // 0: other kind; 1: single, 2: multi, 3: file, 4: directory, 5: label, 6: RTF
  int string_file_path_exists; // 0/1 for file/directory; -1 otherwise
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
