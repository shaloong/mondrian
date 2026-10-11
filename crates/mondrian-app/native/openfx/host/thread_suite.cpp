// Copyright Mondrian contributors.
// SPDX-License-Identifier: BSD-3-Clause
#include "thread_suite.h"
#include <atomic>
#include <array>
#include <condition_variable>
#include <cstdint>
#include <exception>
#include <limits>
#include <mutex>
#include <thread>
#ifdef _WIN32
#include <windows.h>
#include <process.h>
#else
#include <pthread.h>
#include <unistd.h>
#endif

namespace MondrianOpenFx {
namespace {
constexpr std::size_t CALLBACK_STACK_BYTES = 8 * 1024 * 1024;
thread_local const void* callbackOwner = nullptr;

std::size_t pageBytes() noexcept {
#ifdef _WIN32
  SYSTEM_INFO info{};
  GetSystemInfo(&info);
  return info.dwPageSize;
#else
  const long page = sysconf(_SC_PAGESIZE);
  return page > 0 ? static_cast<std::size_t>(page) : 0;
#endif
}

// Fixed owner memory bounds all suite-owned mutex storage. Blind handles carry
// a non-reusable generation and slot; they are never dereferenced as pointers.
constexpr unsigned MUTEX_SLOT_BITS = 10;
constexpr std::size_t MUTEX_SLOTS = std::size_t{1} << MUTEX_SLOT_BITS;
constexpr std::uintptr_t MUTEX_SLOT_MASK = MUTEX_SLOTS - 1;
std::atomic<std::uintptr_t> nextMutexGeneration{1};
std::uintptr_t allocateMutexGeneration() noexcept {
  auto value = nextMutexGeneration.load(std::memory_order_relaxed);
  while (value <= (std::numeric_limits<std::uintptr_t>::max() >> MUTEX_SLOT_BITS)) {
    if (nextMutexGeneration.compare_exchange_weak(value, value + 1, std::memory_order_relaxed)) {
      return value;
    }
  }
  return 0;
}

struct MutexState {
  std::mutex gate;
  std::condition_variable changed;
  bool alive = false;
  std::uintptr_t token = 0;
  std::thread::id owner;
  std::uint64_t depth = 0;
  std::int64_t count = 0;
  bool initialize(std::uintptr_t identity, int initial) {
    std::lock_guard<std::mutex> lock(gate);
    if (alive) { return false; }
    alive = true;
    token = identity;
    count = initial;
    owner = {};
    depth = 0;
    // Non-positive initial counts are accepted without inventing semaphore
    // credits. Actual successful lock ownership always excludes other threads.
    if (initial > 0) {
      owner = std::this_thread::get_id();
      depth = static_cast<unsigned>(initial);
    }
    return true;
  }
  OfxStatus acquire(std::uintptr_t identity, bool wait) {
    std::unique_lock<std::mutex> lock(gate);
    const auto caller = std::this_thread::get_id();
    if (wait) { changed.wait(lock, [&] { return !alive || token != identity || depth == 0 || owner == caller; }); }
    if (!alive || token != identity) { return kOfxStatErrBadHandle; }
    if (depth != 0 && owner != caller) { return kOfxStatFailed; }
    if (count == std::numeric_limits<std::int64_t>::max() ||
        depth == std::numeric_limits<std::uint64_t>::max()) { return kOfxStatErrValue; }
    owner = caller;
    ++depth;
    ++count;
    return kOfxStatOK;
  }
  OfxStatus release(std::uintptr_t identity) {
    std::lock_guard<std::mutex> lock(gate);
    if (!alive || token != identity) { return kOfxStatErrBadHandle; }
    if (depth == 0 || owner != std::this_thread::get_id()) { return kOfxStatErrValue; }
    --depth;
    --count;
    if (depth == 0) { owner = {}; changed.notify_all(); }
    return kOfxStatOK;
  }
  OfxStatus retire(std::uintptr_t identity) {
    std::lock_guard<std::mutex> lock(gate);
    if (!alive || token != identity) { return kOfxStatErrBadHandle; }
    alive = false;
    changed.notify_all();
    return kOfxStatOK;
  }
  void shutdown() {
    std::lock_guard<std::mutex> lock(gate);
    alive = false;
    changed.notify_all();
  }
};
}

struct ThreadSuite::State {
  std::mutex gate;
  std::condition_variable ready, completed;
  bool started = false, stopped = false, jobReady = false, jobBusy = false, jobCompleted = false;
  OfxThreadFunctionV1* function = nullptr;
  void* argument = nullptr;
  OfxStatus result = kOfxStatFailed;
#ifdef _WIN32
  HANDLE thread = nullptr;
  static unsigned __stdcall entry(void* owner) noexcept {
    static_cast<State*>(owner)->work();
    return 0;
  }
#else
  pthread_t thread{};
  static void* entry(void* owner) noexcept {
    static_cast<State*>(owner)->work();
    return nullptr;
  }
#endif
  std::mutex creationGate;
  std::array<MutexState, MUTEX_SLOTS> mutexes;
  std::size_t nextSlot = 0;
  MutexState* find(OfxMutexHandle handle) noexcept {
    const auto token = reinterpret_cast<std::uintptr_t>(handle);
    if ((token >> MUTEX_SLOT_BITS) == 0) { return nullptr; }
    return &mutexes[token & MUTEX_SLOT_MASK];
  }
  bool start() noexcept {
    const auto page = pageBytes();
    if (page == 0 || CALLBACK_STACK_BYTES % page != 0) { return false; }
#ifdef _WIN32
    thread = reinterpret_cast<HANDLE>(_beginthreadex(nullptr,
        static_cast<unsigned>(CALLBACK_STACK_BYTES), entry, this,
        STACK_SIZE_PARAM_IS_A_RESERVATION, nullptr));
    return thread != nullptr;
#else
    pthread_attr_t attributes;
    if (pthread_attr_init(&attributes) != 0) { return false; }
    const int stackStatus = pthread_attr_setstacksize(&attributes, CALLBACK_STACK_BYTES);
    const int guardStatus = pthread_attr_setguardsize(&attributes, page);
    const int launch = stackStatus == 0 && guardStatus == 0
        ? pthread_create(&thread, &attributes, entry, this) : -1;
    pthread_attr_destroy(&attributes);
    return launch == 0;
#endif
  }
  void work() noexcept {
    try {
      std::unique_lock<std::mutex> lock(gate);
      for (;;) {
        ready.wait(lock, [&] { return stopped || jobReady; });
        if (stopped) { return; }
        auto* run = function;
        void* data = argument;
        jobReady = false;
        lock.unlock();
        callbackOwner = this;
        OfxStatus status = kOfxStatOK;
        try { run(0, 1, data); } catch (...) { status = kOfxStatFailed; }
        callbackOwner = nullptr;
        lock.lock();
        result = status;
        function = nullptr;
        argument = nullptr;
        // Only the accepted caller may release this reservation after consuming
        // its own result. A second caller cannot replace that result meanwhile.
        jobCompleted = true;
        completed.notify_all();
      }
    } catch (...) {
      // The supervised process owns recovery; never publish a false completion
      // or leave an application process waiting on a dead native dispatcher.
      std::terminate();
    }
  }
  ~State() {
    for (auto& mutex : mutexes) { mutex.shutdown(); }
    if (!started) { return; }
    {
      std::lock_guard<std::mutex> lock(gate);
      stopped = true;
      ready.notify_one();
    }
#ifdef _WIN32
    if (WaitForSingleObject(thread, INFINITE) != WAIT_OBJECT_0) { std::terminate(); }
    CloseHandle(thread);
#else
    if (pthread_join(thread, nullptr) != 0) { std::terminate(); }
#endif
  }
};

ThreadSuite::ThreadSuite() : _state(std::make_unique<State>()) {}
ThreadSuite::~ThreadSuite() = default;
std::size_t ThreadSuite::residentBytes() noexcept {
  const auto page = pageBytes();
  if (page == 0 || CALLBACK_STACK_BYTES % page != 0) { return 0; }
  return CALLBACK_STACK_BYTES + page + sizeof(State) + sizeof(ThreadSuite);
}
OfxStatus ThreadSuite::run(OfxThreadFunctionV1* function, unsigned requested, void* argument) noexcept {
  if (callbackOwner == _state.get()) { return kOfxStatErrExists; }
  if (!function || requested == 0) { return kOfxStatFailed; }
  bool published = false;
  try {
    std::unique_lock<std::mutex> lock(_state->gate);
    if (_state->stopped || _state->jobBusy) { return kOfxStatFailed; }
    if (!_state->started) {
      if (!_state->start()) { return kOfxStatFailed; }
      _state->started = true;
    }
    _state->function = function;
    _state->argument = argument;
    _state->jobReady = true;
    _state->jobBusy = true;
    _state->jobCompleted = false;
    published = true;
    _state->ready.notify_one();
    _state->completed.wait(lock, [&] { return _state->jobCompleted; });
    const auto result = _state->result;
    _state->jobCompleted = false;
    _state->jobBusy = false;
    return result;
  } catch (...) {
    // Accepted work owns the caller's borrowed frame/parameter lifetime until
    // completion. A dispatcher synchronization failure cannot safely return
    // while that callback may still execute; the supervised process is reaped.
    if (published) { std::terminate(); }
    return kOfxStatFailed;
  }
}
OfxStatus ThreadSuite::numCPUs(unsigned* output) const noexcept {
  if (!output) { return kOfxStatFailed; }
  *output = 1;
  return kOfxStatOK;
}
OfxStatus ThreadSuite::index(unsigned* output) const noexcept {
  if (!output) { return kOfxStatFailed; }
  *output = 0;
  return kOfxStatOK;
}
int ThreadSuite::isSpawned() const noexcept { return callbackOwner == _state.get() ? 1 : 0; }
OfxStatus ThreadSuite::createMutex(OfxMutexHandle* output, int count) noexcept {
  if (!output) { return kOfxStatFailed; }
  *output = nullptr;
  try {
    const auto generation = allocateMutexGeneration();
    if (generation == 0) { return kOfxStatErrMemory; }
    std::lock_guard<std::mutex> lock(_state->creationGate);
    for (std::size_t offset = 0; offset < MUTEX_SLOTS; ++offset) {
      const auto slot = (_state->nextSlot + offset) & MUTEX_SLOT_MASK;
      const auto token = (generation << MUTEX_SLOT_BITS) | slot;
      if (_state->mutexes[slot].initialize(token, count)) {
        _state->nextSlot = (slot + 1) & MUTEX_SLOT_MASK;
        *output = reinterpret_cast<OfxMutexHandle>(token);
        return kOfxStatOK;
      }
    }
    return kOfxStatErrMemory;
  } catch (...) { return kOfxStatErrMemory; }
}
OfxStatus ThreadSuite::destroyMutex(OfxMutexHandle handle) noexcept {
  try {
    auto* value = _state->find(handle);
    return value ? value->retire(reinterpret_cast<std::uintptr_t>(handle)) : kOfxStatErrBadHandle;
  } catch (...) { return kOfxStatFailed; }
}
OfxStatus ThreadSuite::lockMutex(OfxMutexHandle handle) noexcept {
  try { auto* value = _state->find(handle); return value ? value->acquire(reinterpret_cast<std::uintptr_t>(handle), true) : kOfxStatErrBadHandle; }
  catch (...) { return kOfxStatFailed; }
}
OfxStatus ThreadSuite::unlockMutex(OfxMutexHandle handle) noexcept {
  try { auto* value = _state->find(handle); return value ? value->release(reinterpret_cast<std::uintptr_t>(handle)) : kOfxStatErrBadHandle; }
  catch (...) { return kOfxStatFailed; }
}
OfxStatus ThreadSuite::tryMutex(OfxMutexHandle handle) noexcept {
  try { auto* value = _state->find(handle); return value ? value->acquire(reinterpret_cast<std::uintptr_t>(handle), false) : kOfxStatErrBadHandle; }
  catch (...) { return kOfxStatFailed; }
}
}

extern "C" std::size_t mondrian_openfx_thread_resident_bytes() noexcept {
  return MondrianOpenFx::ThreadSuite::residentBytes();
}
