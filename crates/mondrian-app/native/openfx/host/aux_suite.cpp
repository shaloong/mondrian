// Copyright Mondrian contributors.
// SPDX-License-Identifier: BSD-3-Clause
#include "aux_suite.h"
#include "render.h"
#include <atomic>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <limits>
#include <stdexcept>

namespace MondrianOpenFx {
namespace {
// SelectedPluginBinary already admits at most 1024 exported entries. Each may
// own a root descriptor, plus the selected Filter context and sole instance.
// Do not accidentally restrict a legal pack to the transient notice slot count.
constexpr std::size_t maxHandles = 1024 + 2;
std::atomic<std::uint64_t> nextOwner{1};
std::uint64_t ownerIdentity() {
  auto current = nextOwner.load(std::memory_order_relaxed);
  while (current != std::numeric_limits<std::uint64_t>::max()) {
    if (nextOwner.compare_exchange_weak(current, current + 1, std::memory_order_relaxed)) { return current; }
  }
  throw std::overflow_error("native feedback owner identity exhausted");
}
bool same(const char* input, const char* literal) noexcept {
  if (!input) { return false; }
  const auto length = std::strlen(literal);
  return std::strncmp(input, literal, length) == 0 && input[length] == '\0';
}
bool utf8(const char* input, std::size_t length) noexcept {
  std::size_t index = 0;
  while (index < length) {
    const auto first = static_cast<unsigned char>(input[index++]);
    if (first < 0x80) { continue; }
    unsigned remaining, minimum, value;
    if (first >= 0xc2 && first <= 0xdf) { remaining = 1; minimum = 0x80; value = first & 0x1f; }
    else if (first >= 0xe0 && first <= 0xef) { remaining = 2; minimum = 0x800; value = first & 0x0f; }
    else if (first >= 0xf0 && first <= 0xf4) { remaining = 3; minimum = 0x10000; value = first & 0x07; }
    else { return false; }
    if (remaining > length - index) { return false; }
    while (remaining--) {
      const auto next = static_cast<unsigned char>(input[index++]);
      if ((next & 0xc0) != 0x80) { return false; }
      value = (value << 6) | (next & 0x3f);
    }
    if (value < minimum || value > 0x10ffff || (value >= 0xd800 && value <= 0xdfff)) { return false; }
  }
  return true;
}
int kind(const char* type) noexcept {
  if (same(type, kOfxMessageMessage)) { return MONDRIAN_OPENFX_NOTICE_INFORMATION; }
  if (same(type, kOfxMessageWarning)) { return MONDRIAN_OPENFX_NOTICE_WARNING; }
  if (same(type, kOfxMessageError)) { return MONDRIAN_OPENFX_NOTICE_ERROR; }
  if (same(type, kOfxMessageLog)) { return MONDRIAN_OPENFX_NOTICE_LOG; }
  if (same(type, kOfxMessageFatal)) { return MONDRIAN_OPENFX_NOTICE_FATAL; }
  return MONDRIAN_OPENFX_NOTICE_NONE;
}
bool identity(MondrianOpenFxNotice& notice, const char* id) noexcept {
  if (!id) { return true; }
  std::size_t length = 0;
  while (length <= MONDRIAN_OPENFX_NOTICE_ID_BYTES && id[length]) { ++length; }
  if (length > MONDRIAN_OPENFX_NOTICE_ID_BYTES || !utf8(id, length)) { return false; }
  std::memcpy(notice.id, id, length);
  notice.id_length = length;
  return true;
}
bool formatted(MondrianOpenFxNotice& notice, int type, const char* id, const char* format, va_list args) {
  if (!format || type == MONDRIAN_OPENFX_NOTICE_NONE || !identity(notice, id)) { return false; }
  va_list copied;
  va_copy(copied, args);
  const auto length = std::vsnprintf(notice.text, sizeof(notice.text), format, copied);
  va_end(copied);
  if (length < 0 || static_cast<std::size_t>(length) > MONDRIAN_OPENFX_NOTICE_TEXT_BYTES ||
      std::memchr(notice.text, '\0', static_cast<std::size_t>(length)) ||
      !utf8(notice.text, static_cast<std::size_t>(length))) { return false; }
  notice.kind = type;
  notice.text_length = static_cast<std::size_t>(length);
  return true;
}
void copyNotice(MondrianOpenFxNotice& target, const MondrianOpenFxNotice& source) noexcept {
  target.kind = source.kind;
  target.text_length = source.text_length;
  target.id_length = source.id_length;
  std::memcpy(target.text, source.text, source.text_length + 1);
  std::memcpy(target.id, source.id, source.id_length + 1);
}
template<class Function> OfxStatus invoke(Function&& function) noexcept {
  try {
    auto* suite = activeAuxSuite();
    return suite ? function(*suite) : kOfxStatErrBadHandle;
  } catch (...) { return kOfxStatFailed; }
}
OfxStatus message(void* handle, const char* type, const char* id, const char* format, ...) noexcept {
  va_list args;
  va_start(args, format);
  const auto status = invoke([&](AuxSuite& suite) { return suite.message(handle, type, id, format, args); });
  va_end(args);
  return status;
}
OfxStatus persistent(void* handle, const char* type, const char* id, const char* format, ...) noexcept {
  va_list args;
  va_start(args, format);
  const auto status = invoke([&](AuxSuite& suite) { return suite.persistent(handle, type, id, format, args); });
  va_end(args);
  return status;
}
OfxStatus clear(void* h) noexcept { return invoke([&](AuxSuite& s) { return s.clear(h); }); }
OfxStatus start1(void* h, const char* text) noexcept { return invoke([&](AuxSuite& s) { return s.progressStart(h, text, nullptr); }); }
OfxStatus start2(void* h, const char* text, const char* id) noexcept { return invoke([&](AuxSuite& s) { return s.progressStart(h, text, id); }); }
OfxStatus update(void* h, double v) noexcept { return invoke([&](AuxSuite& s) { return s.progressUpdate(h, v); }); }
OfxStatus end(void* h) noexcept { return invoke([&](AuxSuite& s) { return s.progressEnd(h); }); }
OfxStatus time(void* h, double* v) noexcept { return invoke([&](AuxSuite& s) { return s.timelineTime(h, v); }); }
OfxStatus bounds(void* h, double* f, double* l) noexcept { return invoke([&](AuxSuite& s) { return s.timelineBounds(h, f, l); }); }
OfxStatus go(void* h, double v) noexcept { return invoke([&](AuxSuite& s) { return s.timelineGoto(h, v); }); }
const OfxMessageSuiteV2 messages{message, persistent, clear};
const OfxProgressSuiteV1 progress1{start1, update, end};
const OfxProgressSuiteV2 progress2{start2, update, end};
const OfxTimeLineSuiteV1 timeline{time, go, bounds};
}

AuxSuite::AuxSuite() { _feedback.owner = ownerIdentity(); }
std::size_t AuxSuite::residentBytes() noexcept {
  // Guard storage is outside sizeof(AuxSuite), in SDK allocation wrappers.
  // Reserve the bounded full capacity before native loading, not just current use.
  return sizeof(AuxSuite) + maxHandles * sizeof(AuxRegistrationGuard);
}
AuxRegistrationGuard* AuxSuite::find(void* handle, bool onlyInstance) const noexcept {
  if (!handle) { return nullptr; }
  for (auto* node = _head; node; node = node->_next) {
    if (node->_handle == handle && (!onlyInstance || node->_isInstance)) { return node; }
  }
  return nullptr;
}
void AuxSuite::bind(AuxRegistrationGuard& node, void* handle) {
  std::lock_guard<std::mutex> lock(_gate);
  if (!handle || node._handle || find(handle, false) || _handles == maxHandles || (node._isInstance && _instance)) {
    throw std::runtime_error("native auxiliary handle admission failed");
  }
  node._handle = handle;
  node._next = _head;
  _head = &node;
  ++_handles;
  if (node._isInstance) { _instance = &node; }
}
void AuxSuite::unbind(AuxRegistrationGuard& node) noexcept {
  // Failure here cannot leave a registered pointer to dead borrowed storage.
  // A noexcept mutex exception terminates the supervised child.
  std::lock_guard<std::mutex> lock(_gate);
  auto** current = &_head;
  while (*current && *current != &node) { current = &(*current)->_next; }
  if (*current) {
    *current = node._next;
    --_handles;
    if (_instance == &node) { _instance = nullptr; }
  }
  node._handle = nullptr;
  node._next = nullptr;
}
const void* AuxSuite::fetchSuite(const char* name, int version) noexcept {
  if (same(name, kOfxMessageSuite) && (version == 1 || version == 2)) { return &messages; }
  if (same(name, kOfxProgressSuite)) {
    if (version == 1) { return &progress1; }
    if (version == 2) { return &progress2; }
  }
  if (same(name, kOfxTimeLineSuite) && version == 1) { return &timeline; }
  return nullptr;
}
OfxStatus AuxSuite::message(void* handle, const char* type, const char* id, const char* format, va_list args) {
  std::unique_lock<std::mutex> lock(_gate);
  if (handle && !find(handle, false)) { return kOfxStatErrBadHandle; }
  if (same(type, kOfxMessageQuestion)) {
    // NULL and registered descriptor handles remain legal Message handles.
    // The current active GUI invocation, not that address, proves interactivity.
    if (!_instance || !_instance->_interactive || !_control || _control->gui_open != 1 || _control->question_work != 1 || !_control->question ||
        _closingControl || _questionActive || _feedback.invocation == 0) { return kOfxStatFailed; }
    MondrianOpenFxLiveQuestion question{};
    if (!formatted(question.text, MONDRIAN_OPENFX_NOTICE_INFORMATION, id, format, args)) { return kOfxStatFailed; }
    question.owner = _feedback.owner;
    question.invocation = _feedback.invocation;
    const auto control = *_control;
    _questionActive = true;
    // Never hold the auxiliary gate while awaiting a human. Abort, Progress
    // and Clear must keep functioning. bindControl(nullptr) owns the matching
    // in-flight barrier so the copied borrowed context cannot outlive its call.
    lock.unlock();
    int reply;
    try { reply = control.question(control.context, &question); }
    catch (...) { finishQuestion(); throw; }
    finishQuestion();
    return reply == kOfxStatReplyYes || reply == kOfxStatReplyNo ? reply : kOfxStatFailed;
  }
  if (_feedback.notice_count == MONDRIAN_OPENFX_NOTICE_SLOTS) { return kOfxStatFailed; }
  MondrianOpenFxNotice notice{};
  if (!formatted(notice, kind(type), id, format, args)) { return kOfxStatFailed; }
  _feedback.notices[_feedback.notice_count++] = notice;
  if (notice.kind == MONDRIAN_OPENFX_NOTICE_FATAL) { _fatal = true; }
  return kOfxStatOK;
}
OfxStatus AuxSuite::persistent(void* handle, const char* type, const char* id, const char* format, va_list args) {
  std::lock_guard<std::mutex> lock(_gate);
  if (!find(handle, true)) { return kOfxStatErrBadHandle; }
  const auto severity = kind(type);
  if (severity < MONDRIAN_OPENFX_NOTICE_INFORMATION || severity > MONDRIAN_OPENFX_NOTICE_ERROR ||
      _feedback.persistent_revision == std::numeric_limits<std::uint64_t>::max()) { return kOfxStatFailed; }
  MondrianOpenFxNotice notice{};
  if (!formatted(notice, severity, id, format, args)) { return kOfxStatFailed; }
  _feedback.persistent = notice;
  ++_feedback.persistent_revision;
  return kOfxStatOK;
}
OfxStatus AuxSuite::clear(void* handle) {
  std::lock_guard<std::mutex> lock(_gate);
  if (!find(handle, true)) { return kOfxStatErrBadHandle; }
  if (_feedback.persistent_revision == std::numeric_limits<std::uint64_t>::max()) { return kOfxStatFailed; }
  _feedback.persistent = {};
  ++_feedback.persistent_revision;
  return kOfxStatOK;
}
void AuxSuite::bindControl(const MondrianOpenFxInvocationControl* control) {
  std::unique_lock<std::mutex> lock(_gate);
  if (control) {
    if (_control || _questionActive || _closingControl) {
      throw std::logic_error("native invocation control already owns an active borrow");
    }
    _control = control;
    return;
  }
  _closingControl = true;
  // ControlBorrow destruction is noexcept. A synchronization failure cannot
  // return while foreign code retains the borrowed Rust context: terminate the
  // supervised child rather than publish completion with an invalid lifetime.
  _questionCompleted.wait(lock, [this] { return !_questionActive; });
  _control = nullptr;
  _closingControl = false;
}
void AuxSuite::finishQuestion() noexcept {
  std::lock_guard<std::mutex> lock(_gate);
  _questionActive = false;
  _questionCompleted.notify_all();
}
bool AuxSuite::hasInteractiveControl() {
  std::lock_guard<std::mutex> lock(_gate);
  return _control && _control->gui_open == 1 && _control->question
      && !_closingControl && !canceledLocked();
}

bool AuxSuite::hasUserEditedControl() {
  std::lock_guard<std::mutex> lock(_gate);
  return _control && _control->gui_open == 1 && _control->interactive_render == 1
      && !_closingControl;
}
OfxStatus AuxSuite::bindInteractive(void* handle, bool interactive) {
  std::lock_guard<std::mutex> lock(_gate);
  auto* node = find(handle, true);
  if (!node) { return kOfxStatErrBadHandle; }
  node->_interactive = interactive;
  return kOfxStatOK;
}
bool AuxSuite::canceledLocked() const noexcept {
  return _control && _control->abort && _control->abort(_control->context) != 0;
}
int AuxSuite::abort(void* handle) {
  std::lock_guard<std::mutex> lock(_gate);
  return find(handle, true) && canceledLocked() ? 1 : 0;
}
OfxStatus AuxSuite::publishProgressLocked() {
  if (!_control || !_control->progress) { return kOfxStatOK; }
  MondrianOpenFxLiveProgress value{};
  value.owner = _feedback.owner;
  value.invocation = _feedback.invocation;
  value.generation = _feedback.progress_generation;
  value.active = _feedback.progress_active;
  value.fraction = _feedback.progress_fraction;
  copyNotice(value.label, _feedback.progress);
  return _control->progress(_control->context, &value) == 0 ? kOfxStatOK : kOfxStatFailed;
}
OfxStatus AuxSuite::progressStart(void* handle, const char* text, const char* id) {
  std::lock_guard<std::mutex> lock(_gate);
  if (!find(handle, true)) { return kOfxStatErrBadHandle; }
  if (!text || _feedback.progress_active || _feedback.progress_generation == std::numeric_limits<std::uint64_t>::max()) { return kOfxStatFailed; }
  MondrianOpenFxNotice notice{};
  if (!identity(notice, id)) { return kOfxStatFailed; }
  std::size_t length = 0;
  while (length <= MONDRIAN_OPENFX_NOTICE_TEXT_BYTES && text[length]) { ++length; }
  if (length > MONDRIAN_OPENFX_NOTICE_TEXT_BYTES || !utf8(text, length)) { return kOfxStatFailed; }
  std::memcpy(notice.text, text, length);
  notice.text_length = length;
  notice.kind = MONDRIAN_OPENFX_NOTICE_INFORMATION;
  _feedback.progress = notice;
  _feedback.progress_fraction = 0.0;
  _feedback.progress_active = 1;
  ++_feedback.progress_generation;
  return publishProgressLocked();
}
OfxStatus AuxSuite::progressUpdate(void* handle, double fraction) {
  std::lock_guard<std::mutex> lock(_gate);
  if (!find(handle, true) || !_feedback.progress_active) { return kOfxStatErrBadHandle; }
  if (!std::isfinite(fraction) || fraction < 0.0 || fraction > 1.0) { return kOfxStatErrValue; }
  _feedback.progress_fraction = fraction;
  const auto published = publishProgressLocked();
  if (published != kOfxStatOK) { return published; }
  return canceledLocked() ? kOfxStatReplyNo : kOfxStatOK;
}
OfxStatus AuxSuite::progressEnd(void* handle) {
  std::lock_guard<std::mutex> lock(_gate);
  if (!find(handle, true) || !_feedback.progress_active) { return kOfxStatErrBadHandle; }
  _feedback.progress_active = 0;
  return publishProgressLocked();
}
OfxStatus AuxSuite::timelineTime(void* handle, double* output) {
  std::lock_guard<std::mutex> lock(_gate);
  const auto* node = find(handle, true);
  if (!node) { return kOfxStatErrBadHandle; }
  if (!output) { return kOfxStatErrValue; }
  *output = node->_time;
  return kOfxStatOK;
}
OfxStatus AuxSuite::timelineBounds(void* handle, double* first, double* last) {
  std::lock_guard<std::mutex> lock(_gate);
  const auto* node = find(handle, true);
  if (!node) { return kOfxStatErrBadHandle; }
  if (!first || !last) { return kOfxStatErrValue; }
  *first = node->_first;
  *last = node->_last;
  return kOfxStatOK;
}
OfxStatus AuxSuite::timelineGoto(void* handle, double target) {
  std::lock_guard<std::mutex> lock(_gate);
  if (!find(handle, true)) { return kOfxStatErrBadHandle; }
  return std::isfinite(target) ? kOfxStatFailed : kOfxStatErrValue;
}
OfxStatus AuxSuite::bindTimelineLocked(void* handle, double time, double first, double last) {
  auto* node = find(handle, true);
  if (!node) { return kOfxStatErrBadHandle; }
  if (!std::isfinite(time) || !std::isfinite(first) || !std::isfinite(last) || first > time || time > last) { return kOfxStatErrValue; }
  node->_time = time;
  node->_first = first;
  node->_last = last;
  return kOfxStatOK;
}
OfxStatus AuxSuite::bindTimeline(void* handle, double time, double first, double last) {
  std::lock_guard<std::mutex> lock(_gate);
  return bindTimelineLocked(handle, time, first, last);
}
OfxStatus AuxSuite::beginInvocation(void* handle, double time, double first, double last) {
  std::lock_guard<std::mutex> lock(_gate);
  if (_feedback.invocation == std::numeric_limits<std::uint64_t>::max()) { return kOfxStatErrValue; }
  const auto status = bindTimelineLocked(handle, time, first, last);
  if (status == kOfxStatOK) { ++_feedback.invocation; }
  return status;
}
bool AuxSuite::blocked(void* handle) {
  std::lock_guard<std::mutex> lock(_gate);
  return find(handle, true) && _feedback.persistent.kind == MONDRIAN_OPENFX_NOTICE_ERROR;
}
bool AuxSuite::fatal() { std::lock_guard<std::mutex> lock(_gate); return _fatal; }
void AuxSuite::report(MondrianOpenFxFeedback& output) {
  std::lock_guard<std::mutex> lock(_gate);
  output.owner = _feedback.owner;
  output.invocation = _feedback.invocation;
  output.persistent_revision = _feedback.persistent_revision;
  copyNotice(output.persistent, _feedback.persistent);
  output.progress_generation = _feedback.progress_generation;
  output.progress_active = _feedback.progress_active;
  output.progress_fraction = _feedback.progress_fraction;
  copyNotice(output.progress, _feedback.progress);
  output.notice_count = _feedback.notice_count;
  for (std::size_t index = 0; index < _feedback.notice_count; ++index) {
    copyNotice(output.notices[index], _feedback.notices[index]);
  }
  _feedback.notice_count = 0;
}
}
