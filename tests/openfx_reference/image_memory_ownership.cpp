// Copyright Mondrian contributors.
// SPDX-License-Identifier: BSD-3-Clause
// Test-only global allocation replacement around the actual HostSupport suite.
#include "ofxhPluginCache.h"
#include "ofxhImageEffectAPI.h"
#include "ofxhClip.h"
#include "host_descriptor.h"
#include "frame_state.h"
#include "effect_instance.h"
#include "image_clip.h"
#include <cstdio>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <new>
#include <stdexcept>

namespace {
struct AllocationProbe {
  bool armed = false, fail_array = false, unexpected = false, fail_image_owner = false;
  void* owner = nullptr;
  unsigned scalar_allocations = 0, scalar_deletions = 0, array_calls = 0, array_failures = 0;
  unsigned scalar_failures = 0;
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
// the original oracle. The separate image-fetch window fails only the first
// real FrameImage allocation, before a wrapper or property handle exists.
void* operator new(std::size_t bytes) {
  if (probe.armed && probe.fail_image_owner) {
    probe.fail_image_owner = false;
    ++probe.scalar_failures;
    if (bytes != sizeof(MondrianOpenFx::FrameImage)) { probe.unexpected = true; }
    throw std::bad_alloc();
  }
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

bool descriptorRefusal(const OfxImageEffectSuiteV1& suite,
                       const OFX::Host::ImageEffect::Descriptor& descriptor, const char* path) {
  // This is a live Base from the actual Basic plugin, not a forged pointer.
  // Descriptor and instance handles share the Base type but not its contract.
  OfxImageMemoryHandle memory = reinterpret_cast<OfxImageMemoryHandle>(&probe);
  probe = {};
  probe.armed = true;
  OfxStatus status;
  try {
    status = suite.imageMemoryAlloc(descriptor.getHandle(), 257, &memory);
  } catch (...) {
    probe.armed = false;
    std::fprintf(stderr, "imageMemoryAlloc exception escaped for %s\n", path);
    return false;
  }
  probe.armed = false;
  const bool passed = status == kOfxStatErrBadHandle && !memory && !probe.unexpected &&
      probe.scalar_allocations == 0 && probe.scalar_deletions == 0 &&
      probe.array_calls == 0 && probe.array_failures == 0 && probe.live_owners == 0;
  std::printf("{\"path\":\"%s\",\"status\":%d,\"out_null\":%s,"
              "\"scalar_allocations\":%u,\"scalar_deletions\":%u,\"array_calls\":%u,"
              "\"array_failures\":%u,\"live_owners\":%d,\"unexpected\":%s,\"passed\":%s}\n",
              path, status, memory ? "false" : "true", probe.scalar_allocations,
              probe.scalar_deletions, probe.array_calls, probe.array_failures,
              probe.live_owners, probe.unexpected ? "true" : "false", passed ? "true" : "false");
  return passed;
}

bool memoryAllocationRecovery(const OfxImageEffectSuiteV1& suite,
                              OfxImageEffectHandle instance, const char* path) {
  OfxImageMemoryHandle memory = nullptr;
  probe = {};
  probe.armed = true;
  OfxStatus status = kOfxStatFailed, freeStatus = kOfxStatFailed;
  bool aligned = false, nestedPreserved = false, relockPreserved = false, unmatchedUnlock = false;
  try {
    status = suite.imageMemoryAlloc(instance, 257, &memory);
    if (status == kOfxStatOK && memory) {
      unsigned char expected[257];
      for (size_t index = 0; index < sizeof(expected); ++index) {
        expected[index] = static_cast<unsigned char>(index);
      }
      void *first = nullptr, *second = nullptr, *third = nullptr;
      if (suite.imageMemoryLock(memory, &first) == kOfxStatOK && first) {
        aligned = reinterpret_cast<std::uintptr_t>(first) % 16 == 0;
        std::memcpy(first, expected, sizeof(expected));
        const bool nested = suite.imageMemoryLock(memory, &second) == kOfxStatOK && second == first;
        const bool unlockedNested = suite.imageMemoryUnlock(memory) == kOfxStatOK;
        nestedPreserved = nested && unlockedNested && std::memcmp(first, expected, sizeof(expected)) == 0;
        const bool unlocked = suite.imageMemoryUnlock(memory) == kOfxStatOK;
        unmatchedUnlock = unlocked && suite.imageMemoryUnlock(memory) == kOfxStatOK;
        if (suite.imageMemoryLock(memory, &third) == kOfxStatOK && third) {
          relockPreserved = std::memcmp(third, expected, sizeof(expected)) == 0 &&
              suite.imageMemoryUnlock(memory) == kOfxStatOK;
        }
      }
      freeStatus = suite.imageMemoryFree(memory);
    }
  } catch (...) {
    probe.armed = false;
    std::fprintf(stderr, "image memory recovery exception escaped for %s\n", path);
    return false;
  }
  probe.armed = false;
  const bool passed = status == kOfxStatOK && memory && freeStatus == kOfxStatOK &&
      aligned && nestedPreserved && relockPreserved && unmatchedUnlock && !probe.unexpected &&
      probe.scalar_allocations == 1 && probe.scalar_deletions == 1 &&
      probe.array_calls == 1 && probe.array_failures == 0 && probe.live_owners == 0;
  std::printf("{\"path\":\"%s\",\"status\":%d,\"out_nonnull\":%s,\"free_status\":%d,"
              "\"aligned\":%s,\"nested_preserved\":%s,\"relock_preserved\":%s,\"unmatched_unlock\":%s,"
              "\"scalar_allocations\":%u,\"scalar_deletions\":%u,\"array_calls\":%u,"
              "\"array_failures\":%u,\"live_owners\":%d,\"unexpected\":%s,\"passed\":%s}\n",
              path, status, memory ? "true" : "false", freeStatus,
              aligned ? "true" : "false", nestedPreserved ? "true" : "false",
              relockPreserved ? "true" : "false", unmatchedUnlock ? "true" : "false",
              probe.scalar_allocations, probe.scalar_deletions, probe.array_calls,
              probe.array_failures, probe.live_owners, probe.unexpected ? "true" : "false", passed ? "true" : "false");
  return passed;
}

bool imageFetchFailureAndRecovery(const OfxImageEffectSuiteV1& suite,
                                  MondrianOpenFx::FilterClipInstance& clip,
                                  MondrianOpenFx::FrameState& frame, const char* path) {
  unsigned char sourceBefore[sizeof(OfxRGBAColourF)], outputBefore[sizeof(OfxRGBAColourF)];
  std::memcpy(sourceBefore, frame.input, sizeof(sourceBefore));
  std::memcpy(outputBefore, frame.output, sizeof(outputBefore));
  // A sentinel is an output value only; it is never submitted as a handle.
  OfxPropertySetHandle image = reinterpret_cast<OfxPropertySetHandle>(&probe);
  OfxStatus status;
  struct FailureWindow {
    FailureWindow() { probe = {}; probe.armed = probe.fail_image_owner = true; }
    ~FailureWindow() { probe.armed = false; }
  };
  try {
    const FailureWindow armed;
    status = suite.clipGetImage(clip.getHandle(), frame.info.frame, nullptr, &image);
  } catch (...) {
    std::fprintf(stderr, "clipGetImage exception escaped for %s\n", path);
    return false;
  }
  const bool outNull = image == nullptr;
  const size_t handlesAfterFailure = frame.imageHandles.load();
  const bool pixelsUnchanged = std::memcmp(sourceBefore, frame.input, sizeof(sourceBefore)) == 0 &&
      std::memcmp(outputBefore, frame.output, sizeof(outputBefore)) == 0;
  const bool refused = status == kOfxStatErrMemory && outNull && !probe.unexpected &&
      probe.scalar_failures == 1 && probe.scalar_allocations == 0 &&
      probe.array_calls == 0 && probe.live_owners == 0 && handlesAfterFailure == 0 &&
      !clip.outputFetched() && pixelsUnchanged;

  // The same actual clip and raster remain usable after the failed allocation.
  // This succeeds even on the old wrong-status implementation, independently
  // of the ErrMemory assertion above; no retry of the injected failure occurs.
  image = nullptr;
  const OfxStatus fetchStatus = suite.clipGetImage(clip.getHandle(), frame.info.frame, nullptr, &image);
  const bool fetched = fetchStatus == kOfxStatOK && image && frame.imageHandles.load() == 1;
  const OfxStatus releaseStatus = image ? suite.clipReleaseImage(image) : kOfxStatFailed;
  const OfxRGBAColourF transparent{0, 0, 0, 0};
  const bool sourceUnchanged = std::memcmp(sourceBefore, frame.input, sizeof(sourceBefore)) == 0;
  const bool outputCorrect = clip.isOutput() ?
      std::memcmp(&transparent, frame.output, sizeof(transparent)) == 0 :
      std::memcmp(outputBefore, frame.output, sizeof(outputBefore)) == 0;
  const bool recovered = fetched && releaseStatus == kOfxStatOK && frame.imageHandles.load() == 0 &&
      sourceUnchanged && outputCorrect && !frame.invalidImageLifetime;
  const bool passed = refused && recovered;
  std::printf("{\"path\":\"%s\",\"status\":%d,\"out_null\":%s,"
              "\"scalar_failures\":%u,\"scalar_allocations\":%u,\"image_handles_after_failure\":%zu,"
              "\"pixels_unchanged\":%s,\"recovered\":%s,\"passed\":%s}\n",
              path, status, outNull ? "true" : "false", probe.scalar_failures,
              probe.scalar_allocations, handlesAfterFailure, pixelsUnchanged ? "true" : "false",
              recovered ? "true" : "false", passed ? "true" : "false");
  return passed;
}

bool clipDescriptorRefusal(const OfxImageEffectSuiteV1& suite,
                           const OFX::Host::ImageEffect::ClipDescriptor& descriptor,
                           const MondrianOpenFx::FilterClipInstance& clip,
                           MondrianOpenFx::FrameState& frame, bool imageQuery, const char* path) {
  unsigned char sourceBefore[sizeof(OfxRGBAColourF)], outputBefore[sizeof(OfxRGBAColourF)];
  std::memcpy(sourceBefore, frame.input, sizeof(sourceBefore));
  std::memcpy(outputBefore, frame.output, sizeof(outputBefore));
  const size_t handlesBefore = frame.imageHandles.load();
  const bool fetchedBefore = clip.outputFetched();
  const bool invalidBefore = frame.invalidImageLifetime;
  // These are real Basic ClipBase objects of the wrong kind. Only the image
  // output starts with a sentinel; it is never submitted as an input handle.
  OfxPropertySetHandle image = reinterpret_cast<OfxPropertySetHandle>(&probe);
  OfxRectD bounds{11, 13, 17, 19};
  probe = {};
  probe.armed = true;
  OfxStatus status;
  try {
    status = imageQuery ?
        suite.clipGetImage(descriptor.getHandle(), frame.info.frame, nullptr, &image) :
        suite.clipGetRegionOfDefinition(descriptor.getHandle(), frame.info.frame, &bounds);
  } catch (...) {
    probe.armed = false;
    std::fprintf(stderr, "clip descriptor query exception escaped for %s\n", path);
    return false;
  }
  probe.armed = false;
  const bool outNull = image == nullptr;
  const bool boundsZero = bounds.x1 == 0 && bounds.y1 == 0 && bounds.x2 == 0 && bounds.y2 == 0;
  const size_t handlesAfter = frame.imageHandles.load();
  const bool pixelsUnchanged = std::memcmp(sourceBefore, frame.input, sizeof(sourceBefore)) == 0 &&
      std::memcmp(outputBefore, frame.output, sizeof(outputBefore)) == 0;
  const bool fetchedUnchanged = clip.outputFetched() == fetchedBefore;
  const bool lifetimeUnchanged = frame.invalidImageLifetime == invalidBefore;
  const bool passed = status == kOfxStatErrBadHandle && (imageQuery ? outNull : boundsZero) &&
      !probe.unexpected && probe.scalar_allocations == 0 && probe.scalar_deletions == 0 &&
      probe.array_calls == 0 && probe.array_failures == 0 && probe.scalar_failures == 0 &&
      probe.live_owners == 0 && handlesBefore == 0 && handlesAfter == 0 && pixelsUnchanged &&
      fetchedUnchanged && !invalidBefore && lifetimeUnchanged;
  std::printf("{\"path\":\"%s\",\"status\":%d,\"out_null\":%s,\"bounds_zero\":%s,"
              "\"scalar_allocations\":%u,\"scalar_deletions\":%u,\"array_calls\":%u,"
              "\"array_failures\":%u,\"scalar_failures\":%u,\"live_owners\":%d,\"unexpected\":%s,"
              "\"image_handles_before\":%zu,\"image_handles_after\":%zu,\"pixels_unchanged\":%s,"
              "\"output_fetched_unchanged\":%s,\"invalid_lifetime_unchanged\":%s,\"passed\":%s}\n",
              path, status, outNull ? "true" : "false", boundsZero ? "true" : "false",
              probe.scalar_allocations, probe.scalar_deletions, probe.array_calls,
              probe.array_failures, probe.scalar_failures, probe.live_owners,
              probe.unexpected ? "true" : "false", handlesBefore, handlesAfter,
              pixelsUnchanged ? "true" : "false", fetchedUnchanged ? "true" : "false",
              lifetimeUnchanged ? "true" : "false", passed ? "true" : "false");
  std::fflush(stdout);
  return passed;
}

bool clipInstanceRecovery(const OfxImageEffectSuiteV1& suite, OfxImageEffectHandle instance,
                          const OFX::Host::ImageEffect::ClipDescriptor& descriptor,
                          MondrianOpenFx::FilterClipInstance& clip,
                          MondrianOpenFx::FrameState& frame, const char* name, const char* path) {
  // The normal image fetch is deliberately unarmed: constructing its real
  // FrameImage and property set is expected to allocate, unlike a refusal.
  unsigned char sourceBefore[sizeof(OfxRGBAColourF)], outputBefore[sizeof(OfxRGBAColourF)];
  std::memcpy(sourceBefore, frame.input, sizeof(sourceBefore));
  std::memcpy(outputBefore, frame.output, sizeof(outputBefore));
  OfxImageClipHandle handle = nullptr;
  OfxPropertySetHandle properties = nullptr, image = nullptr;
  const auto handleStatus = suite.clipGetHandle(instance, name, &handle, &properties);
  const bool exactHandle = handleStatus == kOfxStatOK && handle == clip.getHandle() &&
      handle != descriptor.getHandle() && properties == clip.getPropHandle();
  OfxRectD bounds{11, 13, 17, 19};
  const auto rodStatus = suite.clipGetRegionOfDefinition(handle, frame.info.frame, &bounds);
  const bool exactBounds = bounds.x1 == 0 && bounds.y1 == 0 && bounds.x2 == 1 && bounds.y2 == 1;
  const auto fetchStatus = suite.clipGetImage(handle, frame.info.frame, nullptr, &image);
  const size_t handlesFetched = frame.imageHandles.load();
  const auto releaseStatus = image ? suite.clipReleaseImage(image) : kOfxStatFailed;
  const size_t handlesAfter = frame.imageHandles.load();
  const OfxRGBAColourF transparent{0, 0, 0, 0};
  const bool pixelsCorrect = std::memcmp(sourceBefore, frame.input, sizeof(sourceBefore)) == 0 &&
      (clip.isOutput() ? std::memcmp(&transparent, frame.output, sizeof(transparent)) == 0 :
                        std::memcmp(outputBefore, frame.output, sizeof(outputBefore)) == 0);
  const bool passed = !probe.armed && exactHandle && rodStatus == kOfxStatOK && exactBounds &&
      fetchStatus == kOfxStatOK && image && handlesFetched == 1 && releaseStatus == kOfxStatOK &&
      handlesAfter == 0 && pixelsCorrect && !frame.invalidImageLifetime;
  std::printf("{\"path\":\"%s\",\"handle_status\":%d,\"exact_handle\":%s,"
              "\"rod_status\":%d,\"exact_bounds\":%s,\"fetch_status\":%d,\"out_nonnull\":%s,"
              "\"image_handles_fetched\":%zu,\"release_status\":%d,\"image_handles_after\":%zu,"
              "\"pixels_correct\":%s,\"passed\":%s}\n",
              path, handleStatus, exactHandle ? "true" : "false", rodStatus,
              exactBounds ? "true" : "false", fetchStatus, image ? "true" : "false",
              handlesFetched, releaseStatus, handlesAfter, pixelsCorrect ? "true" : "false",
              passed ? "true" : "false");
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
  if (!instance->getClipPreferences()) { throw std::runtime_error("actual Basic clip preferences failed"); }
  auto* source = dynamic_cast<MondrianOpenFx::FilterClipInstance*>(instance->getClip("Source"));
  auto* output = dynamic_cast<MondrianOpenFx::FilterClipInstance*>(instance->getClip("Output"));
  if (!source || !output) { throw std::runtime_error("actual Basic Source/Output clips missing"); }
  const OfxRGBAColourF inputPixel{-0.25f, 2.5f, 0.125f, 0.5f};
  OfxRGBAColourF outputPixel{3.0f, -1.0f, 0.75f, 0.25f};
  frame.input = &inputPixel;
  frame.output = &outputPixel;
  frame.pixelCount = 1;
  const MondrianOpenFx::FrameBorrow borrowed(frame);
  const MondrianOpenFx::ImageAccess imagesAvailable(frame, true, true);
  const bool sourcePassed = imageFetchFailureAndRecovery(*suite, *source, frame, "source-image");
  const bool outputPassed = imageFetchFailureAndRecovery(*suite, *output, frame, "output-image");
  auto* context = plugin->getContext(kOfxImageEffectContextFilter);
  if (!context || context == &plugin->getDescriptor()) {
    throw std::runtime_error("actual Basic root/context descriptors are missing or aliased");
  }
  std::fflush(stdout); // Preserve the original four controls if the old invalid cast crashes.
  const bool rootRefused = descriptorRefusal(*suite, plugin->getDescriptor(), "root-descriptor");
  const bool contextRefused = descriptorRefusal(*suite, *context, "filter-descriptor");
  const bool hostRecovered = memoryAllocationRecovery(*suite, nullptr, "host-recovery");
  const bool instanceRecovered = memoryAllocationRecovery(*suite, instance->getHandle(), "instance-recovery");
  // The original eight controls and their output remain intact before any
  // wrong-kind Clip query reaches the old unchecked virtual dispatch.
  std::fflush(stdout);
  const auto& clips = context->getClips();
  const auto* sourceDescriptor = clips.at("Source");
  const auto* outputDescriptor = clips.at("Output");
  if (!sourceDescriptor || !outputDescriptor || sourceDescriptor == outputDescriptor ||
      sourceDescriptor->getHandle() == source->getHandle() ||
      outputDescriptor->getHandle() == output->getHandle()) {
    throw std::runtime_error("actual Basic Clip descriptors are missing or aliased");
  }
  const bool sourceImageRefused = clipDescriptorRefusal(*suite, *sourceDescriptor, *source, frame,
      true, "source-clip-descriptor-image");
  const bool outputImageRefused = clipDescriptorRefusal(*suite, *outputDescriptor, *output, frame,
      true, "output-clip-descriptor-image");
  const bool sourceRodRefused = clipDescriptorRefusal(*suite, *sourceDescriptor, *source, frame,
      false, "source-clip-descriptor-rod");
  const bool outputRodRefused = clipDescriptorRefusal(*suite, *outputDescriptor, *output, frame,
      false, "output-clip-descriptor-rod");
  const bool sourceClipRecovered = clipInstanceRecovery(*suite, instance->getHandle(),
      *sourceDescriptor, *source, frame, "Source", "source-clip-recovery");
  const bool outputClipRecovered = clipInstanceRecovery(*suite, instance->getHandle(),
      *outputDescriptor, *output, frame, "Output", "output-clip-recovery");
  return hostPassed && instancePassed && sourcePassed && outputPassed &&
      rootRefused && contextRefused && hostRecovered && instanceRecovered && sourceImageRefused &&
      outputImageRefused && sourceRodRefused && outputRodRefused && sourceClipRecovered &&
      outputClipRecovered ? 0 : 1;
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
