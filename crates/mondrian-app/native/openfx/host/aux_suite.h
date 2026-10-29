// Copyright Mondrian contributors.
// SPDX-License-Identifier: BSD-3-Clause
#ifndef MONDRIAN_OPENFX_AUX_SUITE_H
#define MONDRIAN_OPENFX_AUX_SUITE_H
#include "ofxCore.h"
#include "ofxMessage.h"
#include "ofxProgress.h"
#include "ofxTimeLine.h"
#include "aux_feedback.h"
struct MondrianOpenFxInvocationControl;
struct MondrianOpenFxDescribeNotices;
#include <cstdarg>
#include <cstdint>
#include <mutex>
#include <condition_variable>

namespace MondrianOpenFx {
class AuxRegistrationGuard;
class AuxSuite final {
  friend class AuxRegistrationGuard;
  std::mutex _gate;
  std::condition_variable _questionCompleted;
  bool _questionActive = false;
  bool _closingControl = false;
  void finishQuestion() noexcept;
  AuxRegistrationGuard* _head = nullptr;
  std::size_t _handles = 0;
  AuxRegistrationGuard* _instance = nullptr;
  MondrianOpenFxFeedback _feedback{};
  bool _fatal = false;
  bool _noticeIncomplete = false;
  const MondrianOpenFxInvocationControl* _control = nullptr;
  bool canceledLocked() const noexcept;
  OfxStatus publishProgressLocked();
  AuxRegistrationGuard* find(void*, bool instanceOnly) const noexcept;
  void bind(AuxRegistrationGuard&, void*);
  OfxStatus bindTimelineLocked(void*, double, double, double);
  void unbind(AuxRegistrationGuard&) noexcept;
public:
  AuxSuite();
  // Only the dispatch owner binds/unbinds a scoped borrowed control. The gate
  // also covers legal foreign-thread callbacks before the borrow is cleared.
  void bindControl(const MondrianOpenFxInvocationControl*);
  int abort(void*);
  bool hasInteractiveControl();
  bool hasUserEditedControl();
  OfxStatus bindInteractive(void*, bool);
  AuxSuite(const AuxSuite&) = delete;
  AuxSuite& operator=(const AuxSuite&) = delete;
  static std::size_t residentBytes() noexcept;
  const void* fetchSuite(const char*, int) noexcept;
  OfxStatus message(void*, const char*, const char*, const char*, va_list);
  OfxStatus persistent(void*, const char*, const char*, const char*, va_list);
  OfxStatus clear(void*);
  OfxStatus progressStart(void*, const char*, const char*);
  OfxStatus progressUpdate(void*, double);
  OfxStatus progressEnd(void*);
  OfxStatus timelineTime(void*, double*);
  OfxStatus timelineBounds(void*, double*, double*);
  OfxStatus timelineGoto(void*, double);
  // Initial construction binds copied timeline facts, without issuing Render.
  OfxStatus bindTimeline(void*, double time, double first, double last);
  // Called only by the sole session request owner, before SDK actions.
  OfxStatus beginInvocation(void*, double time, double first, double last);
  bool blocked(void*);
  bool fatal();
  // Snapshot and consume transient facts only after an actual matching report.
  void report(MondrianOpenFxFeedback&);
  // After metadata-owned SDK/module cleanup, before Host destruction.
  void describeNotices(MondrianOpenFxDescribeNotices&);
};

// This is the FIRST base; the SDK Descriptor/Instance is the SECOND base.
// SDK base destruction/actions finish before this guard unregisters.
class AuxRegistrationGuard {
  friend class AuxSuite;
  AuxSuite& _suite;
  AuxRegistrationGuard* _next = nullptr;
  void* _handle = nullptr;
  bool _isInstance;
  bool _interactive = false;
  double _time = 0.0, _first = 0.0, _last = 0.0;
protected:
  AuxRegistrationGuard(AuxSuite& suite, bool instance) : _suite(suite), _isInstance(instance) {}
  ~AuxRegistrationGuard() { _suite.unbind(*this); }
  void bindAuxiliary(void* handle) { _suite.bind(*this, handle); }
  AuxSuite& auxiliary() const noexcept { return _suite; }
public:
  AuxRegistrationGuard(const AuxRegistrationGuard&) = delete;
  AuxRegistrationGuard& operator=(const AuxRegistrationGuard&) = delete;
};

// Existing exclusive SDK host lease is the callback lifetime authority.
AuxSuite* activeAuxSuite() noexcept;
}
#endif
