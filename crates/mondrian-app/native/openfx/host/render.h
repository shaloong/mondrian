// Mondrian OpenFX filter admission ABI. Native code is called only in a supervised child.
#ifndef MONDRIAN_OPENFX_RENDER_H
#define MONDRIAN_OPENFX_RENDER_H

#include <stddef.h>
#include <stdint.h>
#include "aux_feedback.h"
#include "render_diagnostic.h"

#ifdef __cplusplus
extern "C" {
#endif

enum {
  MONDRIAN_OPENFX_MAX_STRING_BYTES = 64 * 1024,
  MONDRIAN_OPENFX_MAX_PARAMETER_STRING_BYTES = 1024 * 1024
};

// A supervised worker owns one ImageEffect host callback binding. Creating
// a second owner returns immediately; it never waits for an active owner to die.
enum { MONDRIAN_OPENFX_HOST_BUSY = 9 };

// Discovery owns the same native host descriptor as selected rendering.
// The handle remains borrowed until the selected library has been unloaded.
int mondrian_openfx_discovery_host_create(void** owner, const void** handle)
#ifdef __cplusplus
    noexcept
#endif
    ;
void mondrian_openfx_discovery_host_destroy(void* owner)
#ifdef __cplusplus
    noexcept
#endif
    ;

struct MondrianOpenFxQueryResult {
  double value[4];
  const char* string_value;
  size_t string_length;
  unsigned int key_count;
  double key_time;
  int key_index;
};
// Value, derivative, integral, key count, indexed key time, directional key index.
typedef int (*MondrianOpenFxQuery)(const void*, int, double, double, int, int,
                                  struct MondrianOpenFxQueryResult*);

struct MondrianOpenFxParameter {
  const char* name;
  int query_mode; // 0: constant, 1: current-frame sample, 2: canonical curve
  int kind; // 1: double, 2: boolean, 3: integer, 4: choice, 5: string choice, 6: string; 7: Double2D, 8: Double3D, 9: RGB, 10: RGBA, 11: Integer2D, 12: Integer3D
  double value[4];
  const char* string_value;
  size_t string_length;
  MondrianOpenFxQuery query;
  const void* query_context;
  uint64_t query_revision;
};

struct MondrianOpenFxFrameInfo {
  int source_alpha; // Filter input or Generator output staging: 0 straight Float, 1 associated Float, 2 opaque
  int width;
  int height;
  uint32_t project_width;
  uint32_t project_height;
  double project_pixel_aspect_ratio;
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
  int double_type; // 0: plain, 1: scale, 2: scalar angle, 3: XY, 4: XYAbsolute, -1: not a Double
  int spatial_default_coordinates; // 0: canonical, 1: normalised, -1: not spatial
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

// Whole metadata request only: no Render owner/invocation or emitter identity.
struct MondrianOpenFxDescribeNotices {
  int complete;
  int fatal;
  size_t notice_count;
  struct MondrianOpenFxNotice notices[MONDRIAN_OPENFX_NOTICE_SLOTS];
};

int mondrian_openfx_describe(
    const char* binary,
    const char* bundle,
    const char* identifier,
    int selected_context, // 0: Filter, 1: Generator; every other value refused
    char* label,
    size_t label_capacity,
    size_t* label_length,
    MondrianOpenFxParameterCallback callback,
    void* context,
    struct MondrianOpenFxDescribeNotices* notices)
#ifdef __cplusplus
    noexcept
#endif
    ;

// Known bounded auxiliary state; this is not a native process RSS cap.
size_t mondrian_openfx_aux_resident_bytes(void)
#ifdef __cplusplus
    noexcept
#endif
    ;

// Handles are owned by one supervised child. Calls on a handle are serialized.
struct MondrianOpenFxSession;

int mondrian_openfx_session_create(
    const char* binary, const char* bundle, const char* identifier,
    int selected_context, // 0: Filter with input, 1: Output-only Generator with null input
    const uint8_t* image_namespace, // exactly 16 parent-issued bytes; copied once
    const void* input, size_t frame_bytes,
    const struct MondrianOpenFxFrameInfo* frame_info,
    const struct MondrianOpenFxParameter* parameters, int parameter_count,
    struct MondrianOpenFxSession** session)
#ifdef __cplusplus
    noexcept
#endif
    ;

// A successful render returns the representation negotiated by clip preferences.
// Pixels remain in that representation until the Rust adapter boundary.
enum MondrianOpenFxOutputAlpha {
    MONDRIAN_OPENFX_ALPHA_INVALID = -1,
    MONDRIAN_OPENFX_ALPHA_STRAIGHT = 0,
    MONDRIAN_OPENFX_ALPHA_PREMULTIPLIED = 1,
    MONDRIAN_OPENFX_ALPHA_OPAQUE = 2
};

struct MondrianOpenFxLiveProgress {
  uint64_t owner, invocation, generation;
  int active;
  double fraction;
  struct MondrianOpenFxNotice label;
};
// Borrowed only within this render call. Callback context and all legal spawned
// callback computations remain live until the action returns. No SDK pointer
// or borrowed parent FnMut crosses into this thread-safe bounded POD seam.
struct MondrianOpenFxLiveQuestion {
  uint64_t owner, invocation;
  struct MondrianOpenFxNotice text;
};
struct MondrianOpenFxInvocationControl {
  const void* context;
  int (*abort)(const void*);
  int (*progress)(const void*, const struct MondrianOpenFxLiveProgress*);
  // Exactly 0 for ordinary frame work, 1 only for proved active effect GUI work.
  int gui_open;
  int interactive_render;
  int question_work;
  int (*question)(const void*, const struct MondrianOpenFxLiveQuestion*);
};

// Output is caller-owned staging storage, valid only when status is zero.
// Native actions may modify staging on failure; publication belongs to Rust.
int mondrian_openfx_session_render(
    struct MondrianOpenFxSession* session, const void* input, void* output,
    size_t frame_bytes, const struct MondrianOpenFxFrameInfo* frame_info,
    const struct MondrianOpenFxParameter* parameters, int parameter_count,
    struct MondrianOpenFxRenderReport* report,
    const struct MondrianOpenFxInvocationControl* control,
    struct MondrianOpenFxNativeDiagnostic* diagnostic) // null: no timing clocks
#ifdef __cplusplus
    noexcept
#endif
    ;

// Logical controlled callback stack, guard and fixed suite state reservation.
// Returns zero if this platform cannot configure the declared reservation.
size_t mondrian_openfx_thread_resident_bytes(void)
#ifdef __cplusplus
    noexcept
#endif
    ;

/** Close the real generic GUI on the existing instance, without a render. */
int mondrian_openfx_session_close_gui(struct MondrianOpenFxSession* session,
    struct MondrianOpenFxFeedback* feedback, int* invoked)
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
