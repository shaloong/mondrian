// Fixed default-off measurement storage, not an OpenFX suite or owner authority.
#ifndef MONDRIAN_OPENFX_RENDER_DIAGNOSTIC_H
#define MONDRIAN_OPENFX_RENDER_DIAGNOSTIC_H
#include <stdint.h>
#include <stddef.h>

enum MondrianOpenFxNativePhase {
  MONDRIAN_OPENFX_SESSION_ADMISSION,
  MONDRIAN_OPENFX_INVOCATION_SETUP,
  MONDRIAN_OPENFX_PARAMETER_ACTIONS,
  MONDRIAN_OPENFX_CLIP_PREFERENCES,
  MONDRIAN_OPENFX_FRAME_SETUP,
  MONDRIAN_OPENFX_BEGIN_SEQUENCE,
  MONDRIAN_OPENFX_REGION_OF_INTEREST,
  MONDRIAN_OPENFX_PLUGIN_RENDER,
  MONDRIAN_OPENFX_OUTPUT_VALIDATION,
  MONDRIAN_OPENFX_END_SEQUENCE,
  MONDRIAN_OPENFX_DISPOSITION,
  MONDRIAN_OPENFX_AUXILIARY_REPORT,
  MONDRIAN_OPENFX_NATIVE_PHASES
};
struct MondrianOpenFxNativeDiagnostic {
  uint32_t present_mask;
  uint32_t counter_overflowed;
  uint64_t total_ns;
  uint64_t output_clear_ns; // Nested inside plugin Render; never add to its parent.
  uint64_t output_clear_bytes;
  uint64_t output_clear_calls;
  uint64_t phase_ns[MONDRIAN_OPENFX_NATIVE_PHASES];
};
#ifdef __cplusplus
#include <chrono>
static_assert(sizeof(MondrianOpenFxNativeDiagnostic) == 136);
static_assert(offsetof(MondrianOpenFxNativeDiagnostic, phase_ns) == 40);
#include <limits>
namespace MondrianOpenFx {
  using DiagnosticClock = std::chrono::steady_clock;
  inline bool diagnosticNanoseconds(DiagnosticClock::time_point start, uint64_t& result) noexcept {
    const auto elapsed = DiagnosticClock::now() - start;
    if (elapsed < DiagnosticClock::duration::zero() ||
        elapsed > std::chrono::duration_cast<DiagnosticClock::duration>(std::chrono::nanoseconds::max())) { return false; }
    result = static_cast<uint64_t>(std::chrono::duration_cast<std::chrono::nanoseconds>(elapsed).count());
    return true;
  }
  // Ordinary instantiation has no clocks, allocation or output writes.
  struct NoRenderProbe {
    struct Stamp {};
    Stamp start() const noexcept { return {}; }
    void finish(MondrianOpenFxNativePhase, Stamp) const noexcept {}
  };
  struct TimedRenderProbe {
    using Stamp = DiagnosticClock::time_point;
    MondrianOpenFxNativeDiagnostic& report;
    Stamp total;
    explicit TimedRenderProbe(MondrianOpenFxNativeDiagnostic& value) noexcept
      : report(value), total(DiagnosticClock::now()) { report = {}; }
    ~TimedRenderProbe() {
      if (!diagnosticNanoseconds(total, report.total_ns)) { report.counter_overflowed = 1; }
    }
    Stamp start() const noexcept { return DiagnosticClock::now(); }
    void finish(MondrianOpenFxNativePhase phase, Stamp start) noexcept {
      uint64_t elapsed = 0;
      if (!diagnosticNanoseconds(start, elapsed) || elapsed > std::numeric_limits<uint64_t>::max() - report.phase_ns[phase]) {
        report.counter_overflowed = 1; return;
      }
      report.phase_ns[phase] += elapsed;
      report.present_mask |= uint32_t{1} << phase;
    }
  };
}
#endif
#endif
