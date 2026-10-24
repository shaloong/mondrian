// Copyright Mondrian contributors.
// SPDX-License-Identifier: BSD-3-Clause
#ifndef MONDRIAN_OPENFX_IMAGE_DOMAIN_H
#define MONDRIAN_OPENFX_IMAGE_DOMAIN_H
#include "ofxCore.h"
#include <cstddef>
#include <cmath>
#include <cstdint>
#include <limits>

namespace MondrianOpenFx {
// Full RoD backing is bounded by the same 128 MiB/8192-axis contract as an
// ordinary mapped frame. The original parent bank grants the actual extra
// payload before allocation; this absolute cap is not an upfront reservation.
constexpr size_t kMaximumDomainBytes = 128u * 1024u * 1024u;
inline bool pixelRegionOfDefinition(const OfxRectD& canonical, double par,
                                    OfxRectI& pixels, size_t& count) {
  const double coordinates[] = {canonical.x1, canonical.y1, canonical.x2, canonical.y2};
  for (double value : coordinates) { if (!std::isfinite(value)) { return false; } }
  if (!std::isfinite(par) || par <= 0 || canonical.x1 > canonical.x2 || canonical.y1 > canonical.y2) { return false; }
  // Empty canonical support remains empty even if its boundary is fractional.
  if (canonical.x1 == canonical.x2 || canonical.y1 == canonical.y2) {
    pixels = {0, 0, 0, 0}; count = 0; return true;
  }
  const double enclosing[] = {std::floor(canonical.x1 / par), std::floor(canonical.y1),
                              std::ceil(canonical.x2 / par), std::ceil(canonical.y2)};
  for (double value : enclosing) {
    if (!std::isfinite(value) || value < std::numeric_limits<int>::min() || value > std::numeric_limits<int>::max()) { return false; }
  }
  const OfxRectI result{int(enclosing[0]), int(enclosing[1]), int(enclosing[2]), int(enclosing[3])};
  const auto width = int64_t(result.x2) - result.x1;
  const auto height = int64_t(result.y2) - result.y1;
  if (width <= 0 || height <= 0 || width > 8192 || height > 8192 ||
      uint64_t(width) * uint64_t(height) > kMaximumDomainBytes / (4 * sizeof(float))) { return false; }
  pixels = result; count = size_t(width) * size_t(height); return true;
}
inline bool sameRegion(const OfxRectI& a, const OfxRectI& b) {
  return a.x1 == b.x1 && a.y1 == b.y1 && a.x2 == b.x2 && a.y2 == b.y2;
}
}
#endif
