// Copyright Mondrian contributors.
// SPDX-License-Identifier: BSD-3-Clause
#ifndef MONDRIAN_OPENFX_THREAD_SUITE_H
#define MONDRIAN_OPENFX_THREAD_SUITE_H
#include "ofxMultiThread.h"
#include <cstddef>
#include <memory>

namespace MondrianOpenFx {
// An exclusive native worker grants one active CPU to this host. Its caller
// blocks while the owned callback thread computes; no process-global pool is used.
class ThreadSuite final {
  struct State;
  std::unique_ptr<State> _state;
public:
  ThreadSuite();
  ~ThreadSuite();
  ThreadSuite(const ThreadSuite&) = delete;
  ThreadSuite& operator=(const ThreadSuite&) = delete;
  static std::size_t residentBytes() noexcept;
  OfxStatus run(OfxThreadFunctionV1*, unsigned, void*) noexcept;
  OfxStatus numCPUs(unsigned*) const noexcept;
  OfxStatus index(unsigned*) const noexcept;
  int isSpawned() const noexcept;
  OfxStatus createMutex(OfxMutexHandle*, int) noexcept;
  OfxStatus destroyMutex(OfxMutexHandle) noexcept;
  OfxStatus lockMutex(OfxMutexHandle) noexcept;
  OfxStatus unlockMutex(OfxMutexHandle) noexcept;
  OfxStatus tryMutex(OfxMutexHandle) noexcept;
};
}
#endif
