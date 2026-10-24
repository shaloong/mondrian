// The actual native admission classifier must preserve the standard finite
// predicate for all exponent classes, both signs and NaN payload boundaries.
#include "frame_state.h"
#include <array>
#include <cmath>
#include <cstdint>
#include <cstring>
#include <iostream>

int main() {
  std::size_t checked = 0;
  std::size_t failures = 0;
  const auto check = [&](const std::array<std::uint32_t, 4>& bits) {
    OfxRGBAColourF pixel;
    static_assert(sizeof(pixel) == sizeof(bits));
    std::memcpy(&pixel, bits.data(), sizeof(pixel));
    const bool expected = std::isfinite(pixel.r) && std::isfinite(pixel.g) &&
                          std::isfinite(pixel.b) && std::isfinite(pixel.a);
    const bool actual = MondrianOpenFx::finiteFloatRgba(pixel);
    if (actual != expected || std::memcmp(&pixel, bits.data(), sizeof(pixel)) != 0) {
      if (failures < 4) { std::cerr << "finite classification or unchanged-storage mismatch\n"; }
      ++failures;
    }
    ++checked;
  };
  // Zero/subnormal/normal/finite-maximum/Infinity and both signaling/quiet NaN
  // boundaries reach every RGBA lane, including negative values and alpha>1.
  for (unsigned lane = 0; lane < 4; ++lane) {
    for (std::uint32_t exponent = 0; exponent < 256; ++exponent) {
      for (std::uint32_t fraction : {0u, 1u, 0x3fffffu, 0x400000u, 0x7fffffu}) {
        for (std::uint32_t sign : {0u, 0x80000000u}) {
          std::array<std::uint32_t, 4> bits{0x80000000u, 0xc0200000u, 0x477fe000u, 0x3fa00000u};
          bits[lane] = sign | (exponent << 23) | fraction;
          check(bits);
        }
      }
    }
  }
  // Mixed lane classes also exercise short-circuit combinations. This fixed
  // integer generator constructs representations, never converts numeric values.
  std::uint32_t state = 0x23a749b1u;
  for (unsigned sample = 0; sample < 65536; ++sample) {
    std::array<std::uint32_t, 4> bits;
    for (auto& lane : bits) {
      state = state * 1664525u + 1013904223u;
      lane = state;
    }
    check(bits);
  }
  std::cout << "FramePixels: " << checked << " representations, " << failures << " failures\n";
  return failures == 0 ? 0 : 1;
}
