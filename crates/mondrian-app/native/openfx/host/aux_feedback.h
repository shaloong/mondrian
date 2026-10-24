// Copyright Mondrian contributors.
// SPDX-License-Identifier: BSD-3-Clause
#ifndef MONDRIAN_OPENFX_AUX_FEEDBACK_H
#define MONDRIAN_OPENFX_AUX_FEEDBACK_H
#include <stddef.h>
#include <stdint.h>

// Explicit host resource policy. These are not limits promised by OpenFX.
enum {
  MONDRIAN_OPENFX_NOTICE_SLOTS = 32,
  MONDRIAN_OPENFX_NOTICE_TEXT_BYTES = 4096,
  MONDRIAN_OPENFX_NOTICE_ID_BYTES = 256
};
enum MondrianOpenFxNoticeKind {
  MONDRIAN_OPENFX_NOTICE_NONE = 0,
  MONDRIAN_OPENFX_NOTICE_INFORMATION = 1,
  MONDRIAN_OPENFX_NOTICE_WARNING = 2,
  MONDRIAN_OPENFX_NOTICE_ERROR = 3,
  MONDRIAN_OPENFX_NOTICE_LOG = 4,
  MONDRIAN_OPENFX_NOTICE_FATAL = 5
};
struct MondrianOpenFxNotice {
  int kind;
  size_t text_length;
  size_t id_length;
  char text[MONDRIAN_OPENFX_NOTICE_TEXT_BYTES + 1];
  char id[MONDRIAN_OPENFX_NOTICE_ID_BYTES + 1];
};
struct MondrianOpenFxFeedback {
  uint64_t owner;
  uint64_t invocation;
  uint64_t persistent_revision;
  struct MondrianOpenFxNotice persistent;
  uint64_t progress_generation;
  int progress_active;
  double progress_fraction;
  struct MondrianOpenFxNotice progress;
  size_t notice_count;
  struct MondrianOpenFxNotice notices[MONDRIAN_OPENFX_NOTICE_SLOTS];
};
enum MondrianOpenFxRenderCompletion {
  MONDRIAN_OPENFX_RENDER_UNSET = 0,
  MONDRIAN_OPENFX_RENDER_FRAME = 1,
  MONDRIAN_OPENFX_RENDER_USER_BLOCKED = 2,
  MONDRIAN_OPENFX_RENDER_MEMORY_REFUSED = 3
};
struct MondrianOpenFxRenderReport {
  int completion;
  int output_alpha;
  size_t required_transient_bytes;
  struct MondrianOpenFxFeedback feedback;
};
#endif
