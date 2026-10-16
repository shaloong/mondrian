// Copyright Mondrian contributors.
// SPDX-License-Identifier: BSD-3-Clause
// Test-only global allocation replacement around the actual HostSupport suite.
#include "ofxhPluginCache.h"
#include "ofxhImageEffectAPI.h"
#include "host_descriptor.h"
#include "frame_state.h"
#include <cstdio>
#include <cstdlib>
#include <memory>
#include <new>
#include <stdexcept>

namespace {
struct AllocationProbe {
  bool armed = false, fail_array = false, unexpected = false;
  void* owner = nullptr;
  unsigned scalar_allocations = 0, scalar_deletions = 0, array_calls = 0, array_failures = 0;
  int live_owners = 0;
} probe;

void* allocateBytes(std::size_t bytes) {
  if (void* storage = std::malloc(bytes ? bytes : 1)) { return storage; }
  throw std::bad_alloc();
}
void releaseBytes(void* storage) noexcept {
  if (probe.armed && storage && storage == probe.owner) {
    ++probe.scalar_deletions;
    --probe.live_owners;
  }
  std::free(storage);
}
}

// Initialization, plugin Load/Describe/Create and all printing occur unarmed.
// The armed call is synchronous and has no plugin/DSP/thread callback; any
// allocation other than the one Memory::Instance and its 257-byte array fails
// the oracle rather than being silently excluded from the count.
void* operator new(std::size_t bytes) {
  void* storage = allocateBytes(bytes);
  if (probe.armed) {
    ++probe.scalar_allocations;
    if (bytes != sizeof(OFX::Host::Memory::Instance) || probe.scalar_allocations != 1) {
      probe.unexpected = true;
    } else {
      probe.owner = storage;
      ++probe.live_owners;
    }
  }
  return storage;
}
void* operator new[](std::size_t bytes) {
  if (probe.armed) {
    ++probe.array_calls;
    if (bytes != 257) { probe.unexpected = true; }
    if (probe.fail_array) {
      probe.fail_array = false;
      ++probe.array_failures;
      throw std::bad_alloc();
    }
  }
  return allocateBytes(bytes);
}
void operator delete(void* storage) noexcept { releaseBytes(storage); }
void operator delete(void* storage, std::size_t) noexcept { releaseBytes(storage); }
void operator delete[](void* storage) noexcept { releaseBytes(storage); }
void operator delete[](void* storage, std::size_t) noexcept { releaseBytes(storage); }

namespace {
bool failureCase(const OfxImageEffectSuiteV1& suite, OfxImageEffectHandle instance, const char* path) {
  // A real output variable may contain any previous value; never pass this
  // sentinel as an input handle or free it after a refused allocation.
  OfxImageMemoryHandle memory = reinterpret_cast<OfxImageMemoryHandle>(&probe);
  probe = {};
  probe.armed = true;
  probe.fail_array = true;
  OfxStatus status;
  try {
    status = suite.imageMemoryAlloc(instance, 257, &memory);
  } catch (...) {
    probe.armed = false;
    std::fprintf(stderr, "imageMemoryAlloc exception escaped for %s\n", path);
    return false;
  }
  probe.armed = false;
  const bool passed = status == kOfxStatErrMemory && !memory && !probe.unexpected &&
      probe.scalar_allocations == 1 && probe.scalar_deletions == 1 &&
      probe.array_calls == 1 && probe.array_failures == 1 && probe.live_owners == 0;
  std::printf("{\"path\":\"%s\",\"status\":%d,\"out_null\":%s,"
              "\"scalar_allocations\":%u,\"scalar_deletions\":%u,\"array_calls\":%u,"
              "\"array_failures\":%u,\"live_owners\":%d,\"unexpected\":%s,\"passed\":%s}\n",
              path, status, memory ? "false" : "true", probe.scalar_allocations,
              probe.scalar_deletions, probe.array_calls, probe.array_failures,
              probe.live_owners, probe.unexpected ? "true" : "false", passed ? "true" : "false");
  return passed;
}

int exercise(MondrianOpenFx::Host& host, const char* binaryPath, const char* bundlePath) {
  OFX::Host::ImageEffect::PluginCache images(host);
  OFX::Host::PluginCache plugins;
  images.registerInCache(plugins);
  OFX::Host::PluginBinary binary(binaryPath, bundlePath, &plugins);
  if (binary.isInvalid() || binary.getNPlugins() != 1) {
    throw std::runtime_error("expected the single pinned official Basic plugin");
  }
  auto* plugin = dynamic_cast<OFX::Host::ImageEffect::ImageEffectPlugin*>(&binary.getPlugin(0));
  if (!plugin || plugin->getIdentifier() != "uk.co.thefoundry.BasicGainPlugin") {
    throw std::runtime_error("unexpected plugin class");
  }
  MondrianOpenFx::FrameState frame;
  frame.info.width = frame.info.height = 1;
  frame.info.frame_rate = 24.0;
  frame.info.last_frame = frame.info.pixel_aspect_ratio = 1.0;
  std::unique_ptr<OFX::Host::ImageEffect::Instance> instance(
      plugin->createInstance(kOfxImageEffectContextFilter, &frame));
  if (!instance || instance->createInstanceAction() != kOfxStatOK) {
    throw std::runtime_error("actual Basic instance creation failed");
  }
  const auto* suite = static_cast<const OfxImageEffectSuiteV1*>(host.fetchSuite(kOfxImageEffectSuite, 1));
  if (!suite) { throw std::runtime_error("image suite unavailable"); }
  const bool hostPassed = failureCase(*suite, nullptr, "host");
  const bool instancePassed = failureCase(*suite, instance->getHandle(), "instance");
  return hostPassed && instancePassed ? 0 : 1;
}
}

int main(int argc, char** argv) {
  if (argc != 3) {
    std::fprintf(stderr, "usage: ImageMemoryOwnership <official Basic binary> <Basic bundle>\n");
    return 2;
  }
  try {
    MondrianOpenFx::Host host;
    int result;
    try { result = exercise(host, argv[1], argv[2]); }
    catch (...) {
      // exercise unwound its actual instance, plugin and module before the
      // existing global callback binding is retired while Host is still alive.
      OFX::Host::ImageEffect::gImageEffectHost = nullptr;
      throw;
    }
    OFX::Host::ImageEffect::gImageEffectHost = nullptr;
    return result;
  } catch (const std::exception& error) {
    std::fprintf(stderr, "ImageMemoryOwnership setup failed: %s\n", error.what());
    return 2;
  }
}
