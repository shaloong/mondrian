// Copyright OpenFX and contributors to the OpenFX project.
// SPDX-License-Identifier: BSD-3-Clause


#include <iostream>
#include <cmath>
#include <set>
#include <memory>
#include <cstring>
#include <cstdint>
#include <algorithm>
#include <array>
#include <limits>
#include <type_traits>
#include <stdexcept>
#include <mutex>

#include "render.h"

// ofx
#include "ofxCore.h"
#include "ofxImageEffect.h"
#include "ofxGPURender.h"
#include "ofxPixels.h"

// ofx host
#include "ofxhBinary.h"
#include "ofxhPropertySuite.h"
#include "ofxhClip.h"
#include "ofxhParam.h"
#include "ofxhMemory.h"
#include "ofxhImageEffect.h"
#include "ofxhPluginAPICache.h"
#include "ofxhPluginCache.h"
#include "ofxhHost.h"
#include "ofxhImageEffectAPI.h"

// Mondrian's filter host
#include "host_descriptor.h"
#include "effect_instance.h"
#include "image_clip.h"
#include "parameter_instance.h"
#include "image_domain.h"

// Keep the module loaded between the optional early host callback and the
// HostSupport enumeration. Each selected worker owns exactly one native host.
class SelectedBinaryBootstrap {
  OFX::Binary _binary;
public:
  explicit SelectedBinaryBootstrap(const char* path) : _binary(path) { _binary.ref(); }
  ~SelectedBinaryBootstrap() { _binary.unref(); }
  SelectedBinaryBootstrap(const SelectedBinaryBootstrap&) = delete;
  SelectedBinaryBootstrap& operator=(const SelectedBinaryBootstrap&) = delete;

  int initialize(MondrianOpenFx::Host& host) {
    if (!_binary.isLoaded() || _binary.isInvalid()) { return 3; }
    const auto callback = reinterpret_cast<OfxStatus (*)(const OfxHost*)>(
        _binary.findSymbol("OfxSetHost"));
    if (!callback) { return 0; }
    const OfxStatus status = callback(host.getHandle());
    if (status == kOfxStatFailed) { return 7; }
    return status == kOfxStatOK ? 0 : 3;
  }
};

// The product owns selected-binary admission; the generic vendor scanner is not
// involved. Keep original export indices for PluginHandle's later OfxGetPlugin.
class SelectedPluginBinary final : public OFX::Host::PluginBinary {
  static bool boundedIdentifier(const char* value, size_t maximum) {
    if (!value) { return false; }
    for (size_t index = 0; index < maximum; ++index) {
      const auto byte = static_cast<unsigned char>(value[index]);
      if (byte == 0) { return index != 0; }
      if (byte < 0x21 || byte > 0x7e) { return false; }
    }
    return false;
  }
public:
  SelectedPluginBinary(const char* path, const char* bundle)
      : OFX::Host::PluginBinary(path, bundle, 0, 0) {}

  int initialize(OFX::Host::PluginCache& cache) {
    if (isInvalid()) { return 3; }
    _binary.ref();
    if (!isLoaded() || isInvalid()) { return 3; }
    _fileModificationTime = _binary.getTime();
    _fileSize = _binary.getSize();
    _binaryChanged = false;
    const auto count = reinterpret_cast<int (*)(void)>(
        _binary.findSymbol("OfxGetNumberOfPlugins"));
    const auto getPlugin = reinterpret_cast<OfxPlugin* (*)(int)>(
        _binary.findSymbol("OfxGetPlugin"));
    if (!count || !getPlugin) { return 3; }
    const int plugins = count();
    if (plugins < 0 || plugins > 1024) { return 3; }
    for (int index = 0; index < plugins; ++index) {
      OfxPlugin* header = getPlugin(index);
      if (!header) { return 3; }
      // This worker admits only ImageEffect API v1. Compare the exact known
      // literal including NUL; never parse or bound unrelated API metadata.
      if (header->apiVersion != 1 || !header->pluginApi ||
          std::strncmp(header->pluginApi, kOfxImageEffectPluginApi,
                       sizeof(kOfxImageEffectPluginApi)) != 0) { continue; }
      auto* api = cache.findApiHandler(kOfxImageEffectPluginApi, 1);
      if (!api) { return 3; }
      if (!boundedIdentifier(header->pluginIdentifier, 256) ||
          !header->setHost || !header->mainEntry) { return 3; }
      auto plugin = std::unique_ptr<OFX::Host::Plugin>(api->newPlugin(this, index, header));
      if (!plugin) { return 3; }
      _plugins.push_back(plugin.get());
      plugin.release();
    }
    return 0;
  }
};

// HostSupport routes host-only callbacks through a global pointer. Owners are
// acquired once for an owner lifetime; no render callback takes this lease.
// A second owner is refused immediately, including same-thread reentry. Waiting
// for a long-lived owner here could deadlock a caller preparing two sessions.
class ImageEffectHostBusy final : public std::runtime_error {
public:
  ImageEffectHostBusy() : std::runtime_error("OpenFX worker already owns a native host binding") {}
};

class ImageEffectHostLease {
  static std::recursive_mutex& ownerMutex() {
    static std::recursive_mutex mutex;
    return mutex;
  }
  std::unique_lock<std::recursive_mutex> _ownership;
  OFX::Host::ImageEffect::Host* _ownedHost;
public:
  explicit ImageEffectHostLease(OFX::Host::ImageEffect::Host& host)
      : _ownership(ownerMutex(), std::try_to_lock), _ownedHost(&host) {
    if (!_ownership.owns_lock() || OFX::Host::ImageEffect::gImageEffectHost) {
      throw ImageEffectHostBusy();
    }
  }
  ~ImageEffectHostLease() {
    if (OFX::Host::ImageEffect::gImageEffectHost == _ownedHost) {
      OFX::Host::ImageEffect::gImageEffectHost = nullptr;
    }
  }
  ImageEffectHostLease(const ImageEffectHostLease&) = delete;
  ImageEffectHostLease& operator=(const ImageEffectHostLease&) = delete;
};

class OwnedImageEffectCache {
  ImageEffectHostLease _lease;
  OFX::Host::ImageEffect::PluginCache _cache;
public:
  explicit OwnedImageEffectCache(OFX::Host::ImageEffect::Host& host)
      : _lease(host), _cache(host) {}
  OFX::Host::ImageEffect::PluginCache& get() { return _cache; }
};

struct MondrianOpenFxDiscoveryOwner {
  MondrianOpenFx::Host host;
  OwnedImageEffectCache imageCache;
  MondrianOpenFxDiscoveryOwner() : imageCache(host) {}
};

static bool admitsCpu(const OFX::Host::ImageEffect::Base& effect) {
  return effect.getProps().getStringProperty(kOfxImageEffectPropCPURenderSupported) == "true";
}

extern "C" int mondrian_openfx_discovery_host_create(void** owner, const void** handle) noexcept {
  if (!owner || !handle) { return 2; }
  *owner = nullptr;
  *handle = nullptr;
  try {
    auto discovery = std::make_unique<MondrianOpenFxDiscoveryOwner>();
    *handle = discovery->host.getHandle();
    *owner = discovery.release();
    return 0;
  } catch (const ImageEffectHostBusy&) { return MONDRIAN_OPENFX_HOST_BUSY; }
  catch (...) { return 6; }
}

extern "C" void mondrian_openfx_discovery_host_destroy(void* owner) noexcept {
  delete static_cast<MondrianOpenFxDiscoveryOwner*>(owner);
}

static int stringMode(const OFX::Host::Property::Set& properties) {
  const auto& mode = properties.getStringProperty(kOfxParamPropStringMode);
  if (mode == kOfxParamStringIsSingleLine) { return 1; }
  if (mode == kOfxParamStringIsMultiLine) { return 2; }
  if (mode == kOfxParamStringIsFilePath) { return 3; }
  if (mode == kOfxParamStringIsDirectoryPath) { return 4; }
  if (mode == kOfxParamStringIsLabel) { return 5; }
  if (mode == kOfxParamStringIsRichTextFormat) { return 6; }
  return 0;
}

static int doubleInterpretation(const OFX::Host::Param::Descriptor& parameter) {
  const auto& type = parameter.getType();
  if (type != kOfxParamTypeDouble && type != kOfxParamTypeDouble2D && type != kOfxParamTypeDouble3D) { return -1; }
  const auto& meaning = parameter.getDoubleType();
  if (meaning == kOfxParamDoubleTypePlain) { return 0; }
  if (meaning == kOfxParamDoubleTypeScale) { return 1; }
  if (type == kOfxParamTypeDouble && meaning == kOfxParamDoubleTypeAngle) { return 2; }
  if (type == kOfxParamTypeDouble2D && meaning == kOfxParamDoubleTypeXY) { return 3; }
  if (type == kOfxParamTypeDouble2D && meaning == kOfxParamDoubleTypeXYAbsolute) { return 4; }
  return -2;
}

static int spatialDefaultCoordinates(const OFX::Host::Param::Descriptor& parameter) {
  if (doubleInterpretation(parameter) < 3) { return -1; }
  const auto& coordinates = parameter.getProperties().getStringProperty(kOfxParamPropDefaultCoordinateSystem);
  if (coordinates == kOfxParamCoordinatesCanonical) { return 0; }
  if (coordinates == kOfxParamCoordinatesNormalised) { return 1; }
  return -2;
}

static bool admitsParameterDefaults(const OFX::Host::ImageEffect::Descriptor& descriptor) {
  size_t bytes = 0;
  if (descriptor.getParamList().size() > static_cast<size_t>(MondrianOpenFx::Host::kMaxParameters)) { return false; }
  for (const auto* parameter : descriptor.getParamList()) {
    if (doubleInterpretation(*parameter) == -2 || spatialDefaultCoordinates(*parameter) == -2) { return false; }
    if (parameter->getType() != kOfxParamTypeString && parameter->getType() != kOfxParamTypeStrChoice) { continue; }
    const auto& props = parameter->getProperties();
    if (parameter->getType() == kOfxParamTypeString) {
      const int mode = stringMode(props);
      if (mode == 0) { return false; }
      if (mode == 3 || mode == 4) {
        const int exists = props.getIntProperty(kOfxParamPropStringFilePathExists);
        if (exists != 0 && exists != 1) { return false; }
      }
    }
    const auto& value = props.getStringProperty(kOfxParamPropDefault);
    if (value.size() > MONDRIAN_OPENFX_MAX_STRING_BYTES || value.find('\0') != std::string::npos ||
        value.size() > MONDRIAN_OPENFX_MAX_PARAMETER_STRING_BYTES - bytes) { return false; }
    bytes += value.size();
  }
  return true;
}

static OFX::Host::ImageEffect::ImageEffectPlugin* selectFilter(
    OFX::Host::ImageEffect::PluginCache& cache,
    OFX::Host::PluginBinary& binary,
    const char* identifier, int& selectionStatus)
{
  selectionStatus = 3;
  for (int index = 0; index < binary.getNPlugins(); ++index) {
    OFX::Host::Plugin& candidate = binary.getPlugin(index);
    if (candidate.getIdentifier() != identifier) { continue; }
    auto* selected = dynamic_cast<OFX::Host::ImageEffect::ImageEffectPlugin*>(&candidate);
    if (!selected) { return nullptr; }
    if (!selected->getPluginHandle()) { selectionStatus = 7; return nullptr; }
    std::string reason;
    if (!cache.pluginSupported(&candidate, reason)) {
      std::cerr << reason << std::endl;
      return nullptr;
    }
    cache.confirmPlugin(&candidate);
    return cache.getPluginById(candidate.getIdentifier());
  }
  return nullptr;
}

static bool accepted(OfxStatus status)
{
  return status == kOfxStatOK || status == kOfxStatReplyDefault;
}

static bool admitsIndependentFrames(const OFX::Host::ImageEffect::Base& effect,
                                    const OFX::Host::ImageEffect::ClipBase* source,
                                    const OFX::Host::ImageEffect::ClipBase* output,
                                    bool generator)
{
  const int sequential = effect.getProps().getIntProperty(kOfxImageEffectInstancePropSequentialRender);
  return !effect.temporalAccess() && (sequential == 0 || sequential == 2) &&
         output && !output->temporalAccess() &&
          (generator ? (!source || source->isOptional()) : source && !source->temporalAccess());
}

static bool admitsIndependentFrames(const OFX::Host::ImageEffect::Descriptor& effect, bool generator = false)
{
  // Admission runs after DescribeInContext. A Generator may declare optional
  // inputs; only Filter Source and Output acquire image bindings in this host.
  const auto& clips = effect.getClips();
  const auto source = clips.find(kOfxImageEffectSimpleSourceClipName);
  const auto output = clips.find(kOfxImageEffectOutputClipName);
  if (!admitsIndependentFrames(effect,
      source == clips.end() ? nullptr : source->second,
      output == clips.end() ? nullptr : output->second, generator)) { return false; }
  for (const auto& entry : clips) {
    if (entry.first != kOfxImageEffectOutputClipName &&
        (generator || entry.first != kOfxImageEffectSimpleSourceClipName) &&
        !entry.second->isOptional()) { return false; }
  }
  return true;
}

static bool admitsIndependentFrames(const OFX::Host::ImageEffect::Instance& effect)
{
  return admitsIndependentFrames(effect,
      effect.getClip(kOfxImageEffectSimpleSourceClipName),
      effect.getClip(kOfxImageEffectOutputClipName), effect.getContext() == kOfxImageEffectContextGenerator);
}

static int loadFrame(MondrianOpenFx::FrameState& state, const void* input,
                     size_t frameBytes, const MondrianOpenFxFrameInfo& info, bool generator = false)
{
  if ((generator ? input != nullptr : input == nullptr) || info.source_alpha < 0 || info.source_alpha > 2 || info.width <= 0 || info.height <= 0 ||
      info.width > 8192 || info.height > 8192 ||
      info.project_width == 0 || info.project_height == 0 ||
      !std::isfinite(info.project_pixel_aspect_ratio) || info.project_pixel_aspect_ratio <= 0.0 ||
      !std::isfinite(static_cast<double>(info.project_width) * info.project_pixel_aspect_ratio) ||
      !std::isfinite(info.frame) || !std::isfinite(info.frame_rate) ||
      !std::isfinite(info.first_frame) || !std::isfinite(info.last_frame) ||
      !std::isfinite(info.pixel_aspect_ratio) || info.frame_rate <= 0.0 ||
      info.pixel_aspect_ratio <= 0.0 ||
      !std::isfinite(info.width * info.pixel_aspect_ratio) || info.first_frame > info.frame ||
      info.frame > info.last_frame) { return 2; }
  const size_t pixels = size_t(info.width) * size_t(info.height);
  if (frameBytes != pixels * sizeof(OfxRGBAColourF) ||
      frameBytes > 128u * 1024u * 1024u) { return 2; }
  if (reinterpret_cast<uintptr_t>(input) % alignof(OfxRGBAColourF) != 0) { return 2; }
  const auto* data = static_cast<const OfxRGBAColourF*>(input);
  for (size_t index = 0; !generator && index < pixels; ++index) {
    const auto& pixel = data[index];
    if (!std::isfinite(pixel.r) || !std::isfinite(pixel.g) ||
        !std::isfinite(pixel.b) || !std::isfinite(pixel.a) || (info.source_alpha == 2 && pixel.a != 1.0f)) { return 2; }
  }
  state.input = data;
  state.pixelCount = pixels;
  state.info = info;
  state.outputRodReady = false;
  state.outputPixelCount = 0;
  return 0;
}

struct NumericLayout { int kind; int dimensions; bool integer; };
static NumericLayout numericLayout(const std::string& type) {
  if (type == kOfxParamTypeDouble) { return {1, 1, false}; }
  if (type == kOfxParamTypeInteger) { return {3, 1, true}; }
  if (type == kOfxParamTypeDouble2D) { return {7, 2, false}; }
  if (type == kOfxParamTypeDouble3D) { return {8, 3, false}; }
  if (type == kOfxParamTypeRGB) { return {9, 3, false}; }
  if (type == kOfxParamTypeRGBA) { return {10, 4, false}; }
  if (type == kOfxParamTypeInteger2D) { return {11, 2, true}; }
  if (type == kOfxParamTypeInteger3D) { return {12, 3, true}; }
  return {0, 0, false};
}

template<class Base, class T, size_t N>
static OfxStatus readTuple(OFX::Host::Param::Instance* parameter, std::array<double, 4>& output) {
  auto* tuple = dynamic_cast<Base*>(parameter);
  if (!tuple) { return kOfxStatErrValue; }
  std::array<T, N> value{};
  OfxStatus status;
  if constexpr (N == 2) { status = tuple->get(value[0], value[1]); }
  else if constexpr (N == 3) { status = tuple->get(value[0], value[1], value[2]); }
  else { status = tuple->get(value[0], value[1], value[2], value[3]); }
  if (status != kOfxStatOK) { return status; }
  for (size_t index = 0; index < N; ++index) { output[index] = value[index]; }
  return kOfxStatOK;
}

template<class Base, class T, size_t N>
static OfxStatus writeTuple(OFX::Host::Param::Instance* parameter, const std::array<double, 4>& values) {
  auto* tuple = dynamic_cast<Base*>(parameter);
  if (!tuple) { return kOfxStatErrValue; }
  std::array<T, N> value{};
  for (size_t index = 0; index < N; ++index) {
    if (!std::isfinite(values[index])) { return kOfxStatErrValue; }
    if constexpr (std::is_same_v<T, int>) {
      if (std::trunc(values[index]) != values[index] || values[index] < std::numeric_limits<int>::min() ||
          values[index] > std::numeric_limits<int>::max()) { return kOfxStatErrValue; }
    }
    value[index] = static_cast<T>(values[index]);
  }
  if constexpr (N == 2) { return tuple->set(value[0], value[1]); }
  else if constexpr (N == 3) { return tuple->set(value[0], value[1], value[2]); }
  else { return tuple->set(value[0], value[1], value[2], value[3]); }
}

static OfxStatus readCompound(OFX::Host::Param::Instance* parameter, int kind, std::array<double, 4>& values) {
  switch (kind) {
    case 7: return readTuple<OFX::Host::Param::Double2DInstance, double, 2>(parameter, values);
    case 8: return readTuple<OFX::Host::Param::Double3DInstance, double, 3>(parameter, values);
    case 9: return readTuple<OFX::Host::Param::RGBInstance, double, 3>(parameter, values);
    case 10: return readTuple<OFX::Host::Param::RGBAInstance, double, 4>(parameter, values);
    case 11: return readTuple<OFX::Host::Param::Integer2DInstance, int, 2>(parameter, values);
    case 12: return readTuple<OFX::Host::Param::Integer3DInstance, int, 3>(parameter, values);
    default: return kOfxStatErrValue;
  }
}

static OfxStatus writeCompound(OFX::Host::Param::Instance* parameter, int kind, const std::array<double, 4>& values) {
  switch (kind) {
    case 7: return writeTuple<OFX::Host::Param::Double2DInstance, double, 2>(parameter, values);
    case 8: return writeTuple<OFX::Host::Param::Double3DInstance, double, 3>(parameter, values);
    case 9: return writeTuple<OFX::Host::Param::RGBInstance, double, 3>(parameter, values);
    case 10: return writeTuple<OFX::Host::Param::RGBAInstance, double, 4>(parameter, values);
    case 11: return writeTuple<OFX::Host::Param::Integer2DInstance, int, 2>(parameter, values);
    case 12: return writeTuple<OFX::Host::Param::Integer3DInstance, int, 3>(parameter, values);
    default: return kOfxStatErrValue;
  }
}

struct ParameterUpdate {
  OFX::Host::Param::Instance* parameter;
  int kind;
  std::array<double, 4> value;
  bool changed;
  std::string stringValue{};
};

// Declared in lifetime order: the instance dies before its plugin binary,
// API handler, cache, host and frame facts. No global cache keeps raw handlers.
struct MondrianOpenFxSession {
  MondrianOpenFx::FrameState frame;
  MondrianOpenFx::Host host;
  OFX::Host::PluginCache cache;
  OwnedImageEffectCache imageCache;
  std::unique_ptr<SelectedBinaryBootstrap> bootstrap;
  std::unique_ptr<SelectedPluginBinary> binary;
  std::unique_ptr<OFX::Host::ImageEffect::Instance> instance;
  bool failed = false;
  bool generator = false;

  MondrianOpenFxSession() : imageCache(host) {
    cache.setCacheVersion("mondrian-openfx-v1");
    imageCache.get().registerInCache(cache);
  }

  int applyParameters(const MondrianOpenFxParameter* parameters, int count,
                      bool notify, const char* reason, bool clipsChanged) {
    if (count < 0 || count > MondrianOpenFx::Host::kMaxParameters || (count != 0 && !parameters)) { return 2; }
    std::map<std::string, const MondrianOpenFxParameter*> authored;
    for (int index = 0; index < count; ++index) {
      const auto& value = parameters[index];
      if (!value.name || (value.query_mode < 0 || value.query_mode > 2) ||
          (value.query_mode == 2 && (!value.query || !value.query_context || value.query_revision == 0)) ||
          (value.query_mode != 2 && (value.query || value.query_context || value.query_revision != 0)) || !std::all_of(value.value, value.value + 4, [](double channel) { return std::isfinite(channel); }) ||
          !authored.emplace(value.name, &value).second ||
          !instance->getParam(value.name)) { return 4; }
    }
    std::map<std::string, MondrianOpenFx::ParameterSource> sources;
    std::vector<ParameterUpdate> updates;
    size_t stringBytes = 0;
    bool changed = clipsChanged;
    for (const auto& entry : instance->getParams()) {
      auto* parameter = entry.second;
      const auto& type = parameter->getType();
      if (type == kOfxParamTypeGroup || type == kOfxParamTypePage ||
          type == kOfxParamTypePushButton) {
        if (authored.count(entry.first)) { return 4; }
        continue;
      }
      const auto found = authored.find(entry.first);
      const auto* value = found == authored.end() ? nullptr : found->second;
      const auto& properties = parameter->getProperties();
      if (value && value->query_mode == 2 && !properties.getIntProperty(kOfxParamPropAnimates)) { return 4; }
      if (auto* scalar = dynamic_cast<OFX::Host::Param::DoubleInstance*>(parameter)) {
        const double target = value ? value->value[0] :
            properties.getDoubleProperty(kOfxParamPropDefault);
        double current = 0.0;
        if ((value && value->kind != 1) || !std::isfinite(target) ||
            target < properties.getDoubleProperty(kOfxParamPropMin) ||
            target > properties.getDoubleProperty(kOfxParamPropMax) ||
            scalar->get(current) != kOfxStatOK) { return 4; }
        updates.push_back({parameter, 1, {target}, current != target});
      } else if (auto* scalar = dynamic_cast<OFX::Host::Param::BooleanInstance*>(parameter)) {
        const double target = value ? value->value[0] :
            properties.getIntProperty(kOfxParamPropDefault);
        bool current = false;
        if ((value && value->kind != 2) || (target != 0.0 && target != 1.0) ||
            scalar->get(current) != kOfxStatOK) { return 4; }
        updates.push_back({parameter, 2, {target}, current != (target == 1.0)});
      } else if (auto* scalar = dynamic_cast<OFX::Host::Param::IntegerInstance*>(parameter)) {
        const double target = value ? value->value[0] : properties.getIntProperty(kOfxParamPropDefault);
        int current = 0;
        if ((value && value->kind != 3) || !std::isfinite(target) || std::trunc(target) != target ||
            target < properties.getIntProperty(kOfxParamPropMin) || target > properties.getIntProperty(kOfxParamPropMax) ||
            scalar->get(current) != kOfxStatOK) { return 4; }
        updates.push_back({parameter, 3, {target}, current != target});
      } else if (auto* choice = dynamic_cast<OFX::Host::Param::ChoiceInstance*>(parameter)) {
        const double target = value ? value->value[0] : properties.getIntProperty(kOfxParamPropDefault);
        const int options = properties.getDimension(kOfxParamPropChoiceOption);
        int current = 0;
        if ((value && value->kind != 4) || !std::isfinite(target) || std::trunc(target) != target ||
            target < 0 || target >= options || choice->get(current) != kOfxStatOK) { return 4; }
        if (value && value->query_mode == 2 &&
            !frame.admitsChoiceDomain(options, value->query, value->query_context)) { return 7; }
        updates.push_back({parameter, 4, {target}, current != target});
      } else if (numericLayout(type).kind >= 7) {
        const auto layout = numericLayout(type);
        std::array<double, 4> target{}, current{};
        std::array<double, 2> defaults{};
        const bool projectDefault = !value && layout.kind == 7 &&
            properties.getStringProperty(kOfxParamPropDefaultCoordinateSystem) == kOfxParamCoordinatesNormalised &&
            (properties.getStringProperty(kOfxParamPropDoubleType) == kOfxParamDoubleTypeXY ||
             properties.getStringProperty(kOfxParamPropDoubleType) == kOfxParamDoubleTypeXYAbsolute);
        if (projectDefault) {
          const auto* tuple = dynamic_cast<MondrianOpenFx::NumericTupleParameter<OFX::Host::Param::Double2DInstance, double, 2>*>(parameter);
          if (!tuple) { return 4; }
          defaults = tuple->defaults();
        }
        if ((value && value->kind != layout.kind) || readCompound(parameter, layout.kind, current) != kOfxStatOK) { return 4; }
        for (int index = 0; index < layout.dimensions; ++index) {
          const double minimum = layout.integer ? properties.getIntProperty(kOfxParamPropMin, index)
                                                : properties.getDoubleProperty(kOfxParamPropMin, index);
          const double maximum = layout.integer ? properties.getIntProperty(kOfxParamPropMax, index)
                                                : properties.getDoubleProperty(kOfxParamPropMax, index);
          target[index] = value ? value->value[index] : projectDefault ? defaults[index] : layout.integer
              ? properties.getIntProperty(kOfxParamPropDefault, index)
              : properties.getDoubleProperty(kOfxParamPropDefault, index);
          if (!std::isfinite(target[index]) || target[index] < minimum || target[index] > maximum ||
              (layout.integer && std::trunc(target[index]) != target[index])) { return 4; }
        }
        updates.push_back({parameter, layout.kind, target, current != target});
      } else if (type == kOfxParamTypeStrChoice) {
        auto* choice = dynamic_cast<OFX::Host::Param::StringInstance*>(parameter);
        if (!choice || (value && (value->kind != 5 || !value->string_value || value->string_length > 256))) { return 4; }
        std::string target = value ? std::string(value->string_value, value->string_length)
                                  : properties.getStringProperty(kOfxParamPropDefault);
        const int options = properties.getDimension(kOfxParamPropChoiceEnum);
        if (options < 1 || options != properties.getDimension(kOfxParamPropChoiceOption)) { return 4; }
        if (!value && target.empty()) { target = properties.getStringProperty(kOfxParamPropChoiceEnum, 0); }
        bool found = false;
        for (int index = 0; index < options; ++index) {
          found = found || target == properties.getStringProperty(kOfxParamPropChoiceEnum, index);
        }
        std::string current;
        if (!found || target.size() > 256 || choice->get(current) != kOfxStatOK) { return 4; }
        updates.push_back({parameter, 5, {0.0}, current != target, std::move(target)});
      } else if (type == kOfxParamTypeString) {
        auto* text = dynamic_cast<OFX::Host::Param::StringInstance*>(parameter);
        if (!text || (value && (value->kind != 6 || !value->string_value ||
            value->string_length > MONDRIAN_OPENFX_MAX_STRING_BYTES))) { return 4; }
        std::string target = value ? std::string(value->string_value, value->string_length)
                                  : properties.getStringProperty(kOfxParamPropDefault);
        std::string current;
        if (target.size() > MONDRIAN_OPENFX_MAX_STRING_BYTES || target.find('\0') != std::string::npos ||
            text->get(current) != kOfxStatOK) { return 4; }
        updates.push_back({parameter, 6, {0.0}, current != target, std::move(target)});
      } else { return 7; }
      if (updates.back().kind == 5 || updates.back().kind == 6) {
        const size_t bytes = updates.back().stringValue.size();
        if (bytes > MONDRIAN_OPENFX_MAX_PARAMETER_STRING_BYTES - stringBytes) { return 4; }
        stringBytes += bytes;
      }
      const int mode = value ? value->query_mode : 0;
      const uint64_t revision = value ? value->query_revision : 0;
      if (mode != 0) { sources.emplace(entry.first, MondrianOpenFx::ParameterSource{
          mode, value->query, value->query_context, revision}); }
      const auto old = frame.parameterSources.find(entry.first);
      const int oldMode = old == frame.parameterSources.end() ? 0 : old->second.mode;
      const uint64_t oldRevision = old == frame.parameterSources.end() ? 0 : old->second.revision;
      updates.back().changed = updates.back().changed || oldMode != mode || oldRevision != revision;
      changed = changed || updates.back().changed;
    }
    // Validation above has neither assigned values nor invoked plugin actions.
    // Once native state can change, retain failure until a complete render
    // succeeds; callback failures cannot safely reuse a partially changed instance.
    if (notify) { failed = true; }
    const bool bracket = notify && changed;
    OfxStatus status = kOfxStatOK;
    // No SDK callback can observe a partially assigned author snapshot.
    {
      MondrianOpenFx::ParameterAssignment assigning(frame);
      for (const auto& update : updates) {
        if (update.kind == 1) {
          status = static_cast<OFX::Host::Param::DoubleInstance*>(update.parameter)->set(update.value[0]);
        } else if (update.kind == 3) {
          status = static_cast<OFX::Host::Param::IntegerInstance*>(update.parameter)->set(static_cast<int>(update.value[0]));
        } else if (update.kind == 4) {
          status = static_cast<OFX::Host::Param::ChoiceInstance*>(update.parameter)->set(static_cast<int>(update.value[0]));
        } else if (update.kind == 5 || update.kind == 6) {
          status = static_cast<OFX::Host::Param::StringInstance*>(update.parameter)->set(update.stringValue.c_str());
        } else if (update.kind >= 7) {
          status = writeCompound(update.parameter, update.kind, update.value);
        } else {
          status = static_cast<OFX::Host::Param::BooleanInstance*>(update.parameter)->set(update.value[0] == 1.0);
        }
        if (status != kOfxStatOK) { break; }
      }
    }
    if (status != kOfxStatOK) { return 4; }
    frame.parameterSources = std::move(sources);
    // Instance animation state follows the same temporal source authority as
    // numeric queries, including a current-frame source without a full curve.
    for (const auto& update : updates) {
      update.parameter->getProperties().setIntProperty(kOfxParamPropIsAnimating,
          frame.constantParameter(update.parameter->getName()) ? 0 : 1);
    }
    if (bracket && !accepted(instance->beginInstanceChangedAction(reason))) { return 4; }
    const OfxPointD scale{1.0, 1.0};
    if (bracket && status == kOfxStatOK) {
      for (const auto& update : updates) {
        if (!update.changed) { continue; }
        {
          MondrianOpenFx::ImageAccess images(frame, std::strcmp(reason, kOfxChangeUserEdited) == 0);
          status = instance->paramInstanceChangedAction(
              update.parameter->getName(), reason, frame.info.frame, scale);
        }
        if (!accepted(status) || frame.invalidImageLifetime) { break; }
      }
      if (accepted(status) && !frame.invalidImageLifetime && clipsChanged) {
        for (const char* name : {"Source", "Output"}) {
          if (generator && std::strcmp(name, "Source") == 0) { continue; }
          {
            MondrianOpenFx::ImageAccess images(frame, std::strcmp(reason, kOfxChangeUserEdited) == 0);
            status = instance->clipInstanceChangedAction(name, reason, frame.info.frame, scale);
          }
          if (!accepted(status) || frame.invalidImageLifetime) { break; }
        }
      }
    }
    OfxStatus endStatus = kOfxStatOK;
    if (bracket) {
      MondrianOpenFx::ImageAccess images(frame, !frame.invalidImageLifetime && std::strcmp(reason, kOfxChangeUserEdited) == 0);
      endStatus = instance->endInstanceChangedAction(reason);
    }
    return accepted(status) && accepted(endStatus) && !frame.invalidImageLifetime ? 0 : 4;
  }

  int initialize(const char* path, const char* bundle, const char* identifier,
                 const MondrianOpenFx::FrameState& initial, int selectedContext,
                 const MondrianOpenFxParameter* parameters, int count) {
    generator = selectedContext == 1;
    const char* contextName = generator ? kOfxImageEffectContextGenerator : kOfxImageEffectContextFilter;
    MondrianOpenFx::FrameBorrow borrowed(frame);
    // The C admission boundary validated these borrowed frame facts before
    // acquiring a host owner. Do not rescan the complete raster during setup.
    frame.input = initial.input;
    frame.pixelCount = initial.pixelCount;
    frame.info = initial.info;
    frame.imageNamespace = initial.imageNamespace;
    if (!frame.beginImageBinding()) { return 2; }
    bootstrap = std::make_unique<SelectedBinaryBootstrap>(path);
    const int bootstrapped = bootstrap->initialize(host);
    if (bootstrapped != 0) { return bootstrapped; }
    binary = std::make_unique<SelectedPluginBinary>(path, bundle);
    const int enumerated = binary->initialize(cache);
    if (enumerated != 0) { return enumerated; }
    int selectionStatus;
    auto* plugin = selectFilter(imageCache.get(), *binary, identifier, selectionStatus);
    if (!plugin) { return selectionStatus; }
    auto* descriptor = plugin->getContext(contextName);
    if (!descriptor || !admitsCpu(*descriptor) || !admitsIndependentFrames(*descriptor, generator) || !admitsParameterDefaults(*descriptor)) { return 7; }
    instance.reset(plugin->createInstance(contextName, &frame));
    if (!instance) { return 4; }
    const int applied = applyParameters(parameters, count, false, kOfxChangeUserEdited, false);
    if (applied != 0) { return applied; }
    if (!accepted(instance->createInstanceAction())) { return 4; }
    if (frame.invalidChoiceDomain.load(std::memory_order_relaxed)) { failed = true; return 7; }
    return admitsCpu(*instance) && admitsIndependentFrames(*instance) ? 0 : 7;
  }

  template<class Probe>
  int render(const void* input, void* output, size_t bytes,
             const MondrianOpenFxFrameInfo& info,
             const MondrianOpenFxParameter* parameters, int count, MondrianOpenFxRenderReport& report, Probe& probe) {
    auto phase = probe.start();
    if (failed) { return 2; }
    // Project geometry is an immutable owner binding, independent of image
    // PAR and raster extent. Refuse a changed binding before loadFrame or any
    // native callback; the original instance and output remain usable.
    if (info.project_width != frame.info.project_width ||
        info.project_height != frame.info.project_height ||
        info.project_pixel_aspect_ratio != frame.info.project_pixel_aspect_ratio) { return 2; }
    // A prepared session has a fixed image-storage contract. Violating it is
    // a transport fault, unlike an author snapshot rejected before assignment.
    if (!output || info.width != frame.info.width || info.height != frame.info.height || info.source_alpha != frame.info.source_alpha ||
        reinterpret_cast<uintptr_t>(output) % alignof(OfxRGBAColourF) != 0 || output == input) {
      failed = true;
      return 2;
    }
    const auto previous = frame.info;
    MondrianOpenFx::FrameBorrow borrowed(frame);
    const int loaded = loadFrame(frame, input, bytes, info, generator);
    if (loaded != 0) { failed = true; return loaded; }
    if (!frame.beginImageBinding()) { failed = true; return 2; }
    frame.output = static_cast<OfxRGBAColourF*>(output);
    auto* outputClip = dynamic_cast<MondrianOpenFx::FilterClipInstance*>(instance->getClip("Output"));
    if (!outputClip) { failed = true; return 5; }
    probe.finish(MONDRIAN_OPENFX_SESSION_ADMISSION, phase);
    phase = probe.start();
    if (host.auxiliary().beginInvocation(instance->getHandle(), info.frame, info.first_frame, info.last_frame) != kOfxStatOK) { failed = true; return 6; }
    auto* filter = dynamic_cast<MondrianOpenFx::FilterEffectInstance*>(instance.get());
    if (!filter) { failed = true; return 5; }
    const OfxStatus guiStatus = filter->setInterfaceOpen(host.auxiliary().hasInteractiveControl());
    // Failed means plugin-private GUI setup/cleanup failed, not that Render
    // failed. The real host generic editor remains authoritative. Only a
    // fatal or invalid GUI action reply crosses the native failure boundary.
    if (!accepted(guiStatus) && guiStatus != kOfxStatFailed) {
      failed = true;
      return 5;
    }
    probe.finish(MONDRIAN_OPENFX_INVOCATION_SETUP, phase);
    phase = probe.start();
    const bool clipsChanged = previous.frame_rate != info.frame_rate ||
        previous.first_frame != info.first_frame || previous.last_frame != info.last_frame ||
        previous.pixel_aspect_ratio != info.pixel_aspect_ratio;
    const int applied = applyParameters(parameters, count, true,
        previous.frame != info.frame ? kOfxChangeTime : kOfxChangeUserEdited, clipsChanged);
    if (applied != 0) {
      // A rejected snapshot did not reach the commit boundary. Restore the
      // last accepted timing facts so the next request observes the same state.
      if (!failed) { frame.info = previous; }
      return applied;
    }
    probe.finish(MONDRIAN_OPENFX_PARAMETER_ACTIONS, phase);
    if (host.auxiliary().fatal()) { failed = true; return 6; }
    if (host.auxiliary().blocked(instance->getHandle())) {
      failed = false; // Authored-change lifecycle completed; Error is recoverable.
      report.completion = MONDRIAN_OPENFX_RENDER_USER_BLOCKED;
      return 0;
    }
    phase = probe.start();
    if (!instance->getClipPreferences()) { return 4; }
    if (host.auxiliary().fatal()) { failed = true; return 6; }
    if (host.auxiliary().blocked(instance->getHandle())) {
      failed = false;
      report.completion = MONDRIAN_OPENFX_RENDER_USER_BLOCKED;
      return 0;
    }
    if (!admitsCpu(*instance) || !admitsIndependentFrames(*instance)) { return 7; }
    for (const char* name : {"Source", "Output"}) {
      if (generator && std::strcmp(name, "Source") == 0) { continue; }
      auto* clip = instance->getClip(name);
      if (!clip || clip->getPixelDepth() != kOfxBitDepthFloat ||
          clip->getComponents() != kOfxImageComponentRGBA) { return 7; }
    }
    const auto& premultiplication = instance->getOutputPreMultiplication();
    int representation = MONDRIAN_OPENFX_ALPHA_INVALID;
    if (premultiplication == kOfxImageUnPreMultiplied) { representation = MONDRIAN_OPENFX_ALPHA_STRAIGHT; }
    else if (premultiplication == kOfxImagePreMultiplied) { representation = MONDRIAN_OPENFX_ALPHA_PREMULTIPLIED; }
    else if (premultiplication == kOfxImageOpaque) { representation = MONDRIAN_OPENFX_ALPHA_OPAQUE; }
    else { return 7; }
    probe.finish(MONDRIAN_OPENFX_CLIP_PREFERENCES, phase);
    phase = probe.start();
    // Clear the retained allocation and require a new fetch for this render.
    // Change/preference callbacks cannot make an old output appear completed.
    outputClip->beginFrame();
    const OfxPointD scale{1.0, 1.0};
    const OfxRectI viewport{0, 0, info.width, info.height};
    const OfxRectD viewportCanonical{0.0, 0.0, info.width * info.pixel_aspect_ratio,
                                    static_cast<double>(info.height)};
    const auto region = outputClip->getRegionOfDefinition(info.frame);
    OfxRectI window{};
    size_t domainPixels = 0;
    if (!MondrianOpenFx::pixelRegionOfDefinition(region, info.pixel_aspect_ratio, window, domainPixels)) { return 7; }
    if (!instance->supportsMultiResolution() && domainPixels != 0 && !MondrianOpenFx::sameRegion(window, viewport)) { return 7; }
    frame.outputCanonicalRod = region;
    frame.outputPixelRod = window;
    frame.outputPixelCount = domainPixels;
    frame.outputRodReady = true;
    if (frame.invalidImageLifetime) { failed = true; return 5; }
    if (host.auxiliary().fatal()) { failed = true; return 6; }
    if (host.auxiliary().blocked(instance->getHandle())) {
      failed = false; report.completion = MONDRIAN_OPENFX_RENDER_USER_BLOCKED; return 0;
    }
    // Empty support has no Render action or Output fetch. In particular, do not
    // begin a render sequence whose mandatory Render action would be omitted.
    if (domainPixels == 0) {
      std::fill(frame.output, frame.output + frame.pixelCount, OfxRGBAColourF{0, 0, 0, 0});
      failed = false;
      report.output_alpha = representation == MONDRIAN_OPENFX_ALPHA_OPAQUE ? MONDRIAN_OPENFX_ALPHA_STRAIGHT : representation;
      report.completion = MONDRIAN_OPENFX_RENDER_FRAME;
      probe.finish(MONDRIAN_OPENFX_FRAME_SETUP, phase);
      return 0;
    }
    const bool guiOpen = host.auxiliary().hasInteractiveControl();
    const bool userEdited = host.auxiliary().hasUserEditedControl();
    // Only ask for an identity whose current Source can be published without
    // fetching another clip/time or converting its fixed raster representation.
    const auto currentSourceIdentityCompatible = [&]() {
      if (generator || representation != info.source_alpha) { return false; }
      for (const char* name : {"Source", "Output"}) {
        const auto* clip = instance->getClip(name);
        if (!clip || !clip->getConnected() || clip->getPixelDepth() != kOfxBitDepthFloat ||
            clip->getComponents() != kOfxImageComponentRGBA ||
            clip->getPremult() != premultiplication || clip->getAspectRatio() != info.pixel_aspect_ratio ||
            clip->getFieldOrder() != kOfxImageFieldNone) { return false; }
        const auto rod = clip->getRegionOfDefinition(info.frame);
        if (rod.x1 != viewportCanonical.x1 || rod.y1 != viewportCanonical.y1 ||
            rod.x2 != viewportCanonical.x2 || rod.y2 != viewportCanonical.y2) { return false; }
      }
      return true;
    };
    if (currentSourceIdentityCompatible()) {
      probe.finish(MONDRIAN_OPENFX_FRAME_SETUP, phase);
      phase = probe.start();
      double identityTime = info.frame;
      std::string identityClip;
      const auto identityStatus = instance->isIdentityAction(identityTime, kOfxImageFieldNone,
                                                            window, scale, identityClip);
      // This optional query is frame setup, never a plugin Render action.
      probe.finish(MONDRIAN_OPENFX_FRAME_SETUP, phase);
      phase = probe.start();
      if (frame.invalidImageLifetime) {
        probe.finish(MONDRIAN_OPENFX_DISPOSITION, phase);
        failed = true; return 5;
      }
      if (host.auxiliary().fatal()) {
        probe.finish(MONDRIAN_OPENFX_DISPOSITION, phase);
        failed = true; return 6;
      }
      if (identityStatus != kOfxStatOK && identityStatus != kOfxStatReplyDefault) {
        probe.finish(MONDRIAN_OPENFX_DISPOSITION, phase);
        return 5;
      }
      if (identityStatus == kOfxStatOK &&
          (identityClip != "Source" || !std::isfinite(identityTime) || identityTime != info.frame ||
           !currentSourceIdentityCompatible())) {
        probe.finish(MONDRIAN_OPENFX_DISPOSITION, phase);
        return 7;
      }
      probe.finish(MONDRIAN_OPENFX_DISPOSITION, phase);
      phase = probe.start();
      if (host.auxiliary().blocked(instance->getHandle())) {
        failed = false;
        report.completion = MONDRIAN_OPENFX_RENDER_USER_BLOCKED;
        probe.finish(MONDRIAN_OPENFX_DISPOSITION, phase);
        return 0;
      }
      if (identityStatus == kOfxStatOK) {
        // loadFrame already validated all source pixels and the exact byte
        // count. Use the existing staging allocation without pretending Output
        // was fetched. memmove adds no partial-overlap assumption to the CABI.
        std::memmove(frame.output, frame.input, bytes);
        probe.finish(MONDRIAN_OPENFX_OUTPUT_VALIDATION, phase);
        phase = probe.start();
        failed = false;
        report.output_alpha = representation;
        report.completion = MONDRIAN_OPENFX_RENDER_FRAME;
        probe.finish(MONDRIAN_OPENFX_DISPOSITION, phase);
        return 0;
      }
    } else {
      probe.finish(MONDRIAN_OPENFX_FRAME_SETUP, phase);
    }
    // Hosts without tiles must provide complete RoD images, including negative
    // origins and pixels outside the publication viewport. Keep the ordinary
    // full-viewport path allocation/copy free; scratch is action-local and its
    // maximum payload is charged in the processor's active storage contract.
    std::unique_ptr<OfxRGBAColourF[]> domainStorage;
    if (!MondrianOpenFx::sameRegion(window, viewport)) {
      domainStorage.reset(new OfxRGBAColourF[domainPixels]);
      frame.output = domainStorage.get();
    }
    phase = probe.start();
    OfxStatus status = instance->beginRenderAction(info.frame, info.frame, 1.0,
                                                  guiOpen, scale, false, userEdited);
    if (!accepted(status)) { return 5; }
    probe.finish(MONDRIAN_OPENFX_BEGIN_SEQUENCE, phase);
    phase = probe.start();
    std::map<OFX::Host::ImageEffect::ClipInstance*, OfxRectD> rois;
    status = instance->getRegionOfInterestAction(info.frame, scale, region, rois);
    probe.finish(MONDRIAN_OPENFX_REGION_OF_INTEREST, phase);
    if (accepted(status)) {
      phase = probe.start();
      MondrianOpenFx::ImageAccess images(frame, true, true);
      {
        MondrianOpenFx::RenderParameterWriteBlock parameterWrites(frame);
        status = instance->renderAction(info.frame, kOfxImageFieldNone, window,
                                       scale, false, userEdited, false);
      }
      probe.finish(MONDRIAN_OPENFX_PLUGIN_RENDER, phase);
    }
    phase = probe.start();
    bool written = status == kOfxStatOK && !frame.invalidImageLifetime && outputClip->outputFetched();
    if (written) {
      for (size_t index = 0; index < frame.outputPixelCount; ++index) {
        const auto& pixel = frame.output[index];
        if (!std::isfinite(pixel.r) || !std::isfinite(pixel.g) ||
            !std::isfinite(pixel.b) || !std::isfinite(pixel.a)) { written = false; break; }
      }
    }
    probe.finish(MONDRIAN_OPENFX_OUTPUT_VALIDATION, phase);
    phase = probe.start();
    const auto endStatus = instance->endRenderAction(info.frame, info.frame, 1.0,
                                                    guiOpen, scale, false, userEdited);
    probe.finish(MONDRIAN_OPENFX_END_SEQUENCE, phase);
    phase = probe.start();
    const bool completed = status == kOfxStatOK && written && accepted(endStatus);
    if (frame.invalidImageLifetime) { failed = true; return 5; }
    if (host.auxiliary().fatal()) { failed = true; return 6; }
    // A failed native action or incomplete frame cannot become recoverable Error.
    // Preserve invalid-image and auxiliary-fatal priority before this refusal.
    if (!completed) {
      probe.finish(MONDRIAN_OPENFX_DISPOSITION, phase);
      return 5;
    }
    if (host.auxiliary().blocked(instance->getHandle())) {
      // EndSequenceRender has finished, but staged pixels are unpublished.
      report.completion = MONDRIAN_OPENFX_RENDER_USER_BLOCKED;
      failed = false;
      return 0;
    }
    if (completed) {
      if (domainStorage) {
        auto* published = static_cast<OfxRGBAColourF*>(output);
        const size_t domainWidth = size_t(int64_t(window.x2) - window.x1);
        for (int y = 0; y < info.height; ++y) {
          for (int x = 0; x < info.width; ++x) {
            auto& target = published[size_t(info.height - 1 - y) * info.width + x];
            target = {0, 0, 0, 0};
            if (x >= window.x1 && x < window.x2 && y >= window.y1 && y < window.y2) {
              target = frame.output[size_t(int64_t(window.y2) - 1 - y) * domainWidth + size_t(int64_t(x) - window.x1)];
              if (representation == MONDRIAN_OPENFX_ALPHA_OPAQUE) { target.a = 1.0f; }
            }
          }
        }
        if (representation == MONDRIAN_OPENFX_ALPHA_OPAQUE &&
            (window.x1 > 0 || window.y1 > 0 || window.x2 < info.width || window.y2 < info.height)) {
          // Opaque describes the native image's own support, not the transparent
          // area introduced when placing that image in the requested viewport.
          representation = MONDRIAN_OPENFX_ALPHA_STRAIGHT;
        }
      }
      failed = false; report.output_alpha = representation; report.completion = MONDRIAN_OPENFX_RENDER_FRAME;
    }
    probe.finish(MONDRIAN_OPENFX_DISPOSITION, phase);
    return completed ? 0 : 5;
  }
};

static int describe(MondrianOpenFx::Host& host, const char* binary, const char* bundle,
                    const char* identifier, int selectedContext,
                    char* label, size_t labelCapacity, size_t* labelLength,
                    MondrianOpenFxParameterCallback callback, void* context)
{
  OFX::Host::PluginCache cache;
  cache.setCacheVersion("mondrian-openfx-v1");
  OwnedImageEffectCache imageEffectPluginCache(host);
  imageEffectPluginCache.get().registerInCache(cache);
  SelectedBinaryBootstrap bootstrap(binary);
  const int bootstrapped = bootstrap.initialize(host);
  if (bootstrapped != 0) { return bootstrapped; }
  SelectedPluginBinary selectedBinary(binary, bundle);
  const int enumerated = selectedBinary.initialize(cache);
  if (enumerated != 0) { return enumerated; }
  int selectionStatus;
  auto* plugin = selectFilter(imageEffectPluginCache.get(), selectedBinary, identifier, selectionStatus);
  if (!plugin) { return selectionStatus; }
  const bool generator = selectedContext == 1;
  auto* descriptor = plugin->getContext(generator ? kOfxImageEffectContextGenerator : kOfxImageEffectContextFilter);
  if (!descriptor || !admitsCpu(*descriptor) || !admitsIndependentFrames(*descriptor, generator) || !admitsParameterDefaults(*descriptor)) { return 7; }
  const std::string& displayName = descriptor->getLabel();
  if (displayName.size() > labelCapacity) { return 7; }
  std::memcpy(label, displayName.data(), displayName.size());
  *labelLength = displayName.size();
  int count = 0;
  for (const auto* parameter : descriptor->getParamList()) {
    const std::string& type = parameter->getType();
    if (type == kOfxParamTypeGroup || type == kOfxParamTypePage ||
        type == kOfxParamTypePushButton) { continue; }
    if (++count > MondrianOpenFx::Host::kMaxParameters) { return 7; }
    MondrianOpenFxParameterInfo info{};
    info.string_file_path_exists = -1;
    info.spatial_default_coordinates = -1;
    std::vector<MondrianOpenFxChoiceOption> choices;
    // All borrowed description storage must outlive the callback below.
    std::string stringDefault;
    const std::string& name = parameter->getName();
    const std::string& label = parameter->getLabel();
    const std::string& hint = parameter->getHint();
    info.name = name.data();
    info.name_length = name.size();
    info.label = label.data();
    info.label_length = label.size();
    info.hint = hint.data();
    info.hint_length = hint.size();
    info.can_animate = parameter->getCanAnimate();
    info.secret = parameter->getSecret();
    info.enabled = parameter->getEnabled();
    const auto& properties = parameter->getProperties();
    const auto layout = numericLayout(type);
    if (layout.kind != 0) {
      info.kind = layout.kind;
      info.double_type = -1;
      if (layout.kind == 1 || layout.kind == 7 || layout.kind == 8) {
        info.double_type = doubleInterpretation(*parameter);
        info.spatial_default_coordinates = spatialDefaultCoordinates(*parameter);
        if (info.double_type < 0 || info.spatial_default_coordinates == -2) { return 7; }
      }
      const char* names[] = {kOfxParamPropDefault, kOfxParamPropMin, kOfxParamPropMax,
                             kOfxParamPropDisplayMin, kOfxParamPropDisplayMax};
      double* values[] = {info.default_value, info.minimum, info.maximum, info.display_minimum, info.display_maximum};
      for (int index = 0; index < layout.dimensions; ++index) {
        for (int field = 0; field < 5; ++field) {
          values[field][index] = layout.integer ? properties.getIntProperty(names[field], index)
                                               : properties.getDoubleProperty(names[field], index);
          if (!std::isfinite(values[field][index])) { return 7; }
        }
        if (info.minimum[index] > info.maximum[index] || info.display_minimum[index] > info.display_maximum[index] ||
            (info.spatial_default_coordinates != 1 &&
             (info.default_value[index] < info.minimum[index] || info.default_value[index] > info.maximum[index]))) { return 7; }
      }
    } else if (type == kOfxParamTypeBoolean) {
      info.kind = 2;
      info.double_type = -1;
      const int value = properties.getIntProperty(kOfxParamPropDefault);
      if (value != 0 && value != 1) { return 7; }
      info.default_value[0] = value;
      info.minimum[0] = 0.0;
      info.maximum[0] = 1.0;
      info.display_minimum[0] = 0.0;
      info.display_maximum[0] = 1.0;
    } else if (type == kOfxParamTypeString) {
      info.kind = 6;
      info.double_type = -1;
      info.string_mode = stringMode(properties);
      if (info.string_mode == 0) { return 7; }
      if (info.string_mode == 3 || info.string_mode == 4) {
        info.string_file_path_exists = properties.getIntProperty(kOfxParamPropStringFilePathExists);
        if (info.string_file_path_exists != 0 && info.string_file_path_exists != 1) { return 7; }
      }
      stringDefault = properties.getStringProperty(kOfxParamPropDefault);
      if (stringDefault.size() > MONDRIAN_OPENFX_MAX_STRING_BYTES ||
          stringDefault.find('\0') != std::string::npos) { return 7; }
      info.string_default = stringDefault.data();
      info.string_default_length = stringDefault.size();
    } else if (type == kOfxParamTypeChoice || type == kOfxParamTypeStrChoice) {
      const bool stringChoice = type == kOfxParamTypeStrChoice;
      info.kind = stringChoice ? 5 : 4;
      info.double_type = -1;
      if (!stringChoice) { info.default_value[0] = properties.getIntProperty(kOfxParamPropDefault); }
      if (stringChoice) { stringDefault = properties.getStringProperty(kOfxParamPropDefault); }
      const int options = properties.getDimension(kOfxParamPropChoiceOption);
      const int ranks = properties.getDimension(kOfxParamPropChoiceOrder);
      if (options < 1 || options > 4096 || (ranks != 0 && ranks != options) ||
          (!stringChoice && (info.default_value[0] < 0 || info.default_value[0] >= options)) ||
          (stringChoice && properties.getDimension(kOfxParamPropChoiceEnum) != options)) { return 7; }
      if (stringChoice && stringDefault.empty()) {
        stringDefault = properties.getStringProperty(kOfxParamPropChoiceEnum, 0);
      }
      bool defaultFound = !stringChoice;
      std::set<std::string> seenKeys;
      std::set<int> seenRanks;
      choices.reserve(options);
      for (int index = 0; index < options; ++index) {
        const auto& caption = properties.getStringProperty(kOfxParamPropChoiceOption, index);
        if (caption.empty() || caption.size() > 256) { return 7; }
        const int rank = ranks ? properties.getIntProperty(kOfxParamPropChoiceOrder, index) : 0;
        if (ranks && !seenRanks.insert(rank).second) { return 7; }
        const auto& key = properties.getStringProperty(stringChoice ? kOfxParamPropChoiceEnum : kOfxParamPropChoiceOption, index);
        if (stringChoice && (key.empty() || key.size() > 256 || !seenKeys.insert(key).second)) { return 7; }
        defaultFound = defaultFound || stringDefault == key;
        choices.push_back({caption.data(), caption.size(), ranks ? 1 : 0, rank,
                           stringChoice ? key.data() : nullptr, stringChoice ? key.size() : 0});
      }
      if (!defaultFound) { return 7; }
      if (stringChoice) {
        info.string_default = stringDefault.data();
        info.string_default_length = stringDefault.size();
      }
      info.choices = choices.data();
      info.choice_count = choices.size();
    } else {
      std::cerr << "unsupported parameter type: " << type << std::endl;
      return 7;
    }
    if (layout.kind != 0) {
      const auto& interpolation = properties.getStringProperty(kOfxParamInterpType);
      if (interpolation == kOfxParamInterpTypeConstantStep) { info.preferred_interpolation = 0; }
      else if (interpolation == kOfxParamInterpTypeLinear) { info.preferred_interpolation = 1; }
      else if (interpolation == kOfxParamInterpTypeSmooth) { info.preferred_interpolation = 2; }
      else { return 7; }
    }
    if (callback(context, &info) != 0) { return 8; }
  }
  return 0;
}

extern "C" int mondrian_openfx_describe(
    const char* binary, const char* bundle, const char* identifier, int selectedContext,
    char* label, size_t labelCapacity, size_t* labelLength,
    MondrianOpenFxParameterCallback callback, void* context,
    MondrianOpenFxDescribeNotices* notices) noexcept
{
  if (!notices) { return 2; }
  *notices = {};
  try {
    if ((selectedContext != 0 && selectedContext != 1) || !binary || !bundle || !identifier || !label || !labelLength ||
        !callback || !context) { return 2; }
    MondrianOpenFx::Host host;
    int status;
    try {
      status = describe(host, binary, bundle, identifier, selectedContext, label, labelCapacity,
                        labelLength, callback, context);
    } catch (const ImageEffectHostBusy&) { status = MONDRIAN_OPENFX_HOST_BUSY; }
    catch (const std::exception& error) {
      std::cerr << "OpenFX description failed: " << error.what() << std::endl;
      status = 6;
    } catch (...) { status = 6; }
    // describe's cache/bootstrap/module owners have retired, including Unload
    // callbacks they actually issued. Callbacks after this snapshot (Host's
    // destruction) are deliberately outside the whole-request observation.
    try { host.auxiliary().describeNotices(*notices); }
    catch (...) { *notices = {}; } // Observation failure never replaces native status.
    return status;
  } catch (const ImageEffectHostBusy&) { return MONDRIAN_OPENFX_HOST_BUSY; }
  catch (...) { return 6; }
}

extern "C" int mondrian_openfx_session_create(
    const char* binary, const char* bundle, const char* identifier, int selectedContext,
    const uint8_t* image_namespace,
    const void* input, size_t bytes, const MondrianOpenFxFrameInfo* info,
    const MondrianOpenFxParameter* parameters, int count,
    MondrianOpenFxSession** output) noexcept
{
  if (!output) { return 2; }
  *output = nullptr;
  if (!image_namespace) { return 2; }
  std::array<uint8_t, 16> namespaceCopy{};
  std::copy_n(image_namespace, namespaceCopy.size(), namespaceCopy.begin());
  if (std::all_of(namespaceCopy.begin(), namespaceCopy.end(), [](uint8_t byte) { return byte == 0; })) { return 2; }
  try {
    if ((selectedContext != 0 && selectedContext != 1) || !binary || !bundle || !identifier || !info || count < 0 || count > MondrianOpenFx::Host::kMaxParameters ||
        (count != 0 && !parameters)) { return 2; }
    MondrianOpenFx::FrameState initial;
    initial.imageNamespace = namespaceCopy;
    const int loaded = loadFrame(initial, input, bytes, *info, selectedContext == 1);
    if (loaded != 0) { return loaded; }
    auto session = std::make_unique<MondrianOpenFxSession>();
    const int status = session->initialize(binary, bundle, identifier, initial, selectedContext,
                                          parameters, count);
    if (status == 0) { *output = session.release(); }
    return status;
  } catch (const ImageEffectHostBusy&) { return MONDRIAN_OPENFX_HOST_BUSY; }
  catch (const std::exception& error) {
    std::cerr << "OpenFX session creation failed: " << error.what() << std::endl;
    return 6;
  } catch (...) { return 6; }
}

template<class Probe>
static int renderSessionEntry(
    MondrianOpenFxSession* session, const void* input, void* output, size_t bytes,
    const MondrianOpenFxFrameInfo* info, const MondrianOpenFxParameter* parameters,
    int count, MondrianOpenFxRenderReport* report,
    const MondrianOpenFxInvocationControl* control,
    MondrianOpenFxNativeDiagnostic* diagnostic, Probe& probe) noexcept
{
  if (report) { report->completion = MONDRIAN_OPENFX_RENDER_UNSET; report->output_alpha = MONDRIAN_OPENFX_ALPHA_INVALID; }
  if (!session) { return 2; }
  try {
    if (!report) { session->failed = true; return 2; }
    if (!info) { session->failed = true; return 2; }
    struct ControlBorrow final {
      MondrianOpenFx::AuxSuite& suite;
      ControlBorrow(MondrianOpenFx::AuxSuite& value,
                    const MondrianOpenFxInvocationControl* borrowed) : suite(value) {
        suite.bindControl(borrowed);
      }
      ~ControlBorrow() { suite.bindControl(nullptr); }
    } borrowed(session->host.auxiliary(), control);
    struct DiagnosticBorrow final {
      MondrianOpenFx::FrameState& frame;
      DiagnosticBorrow(MondrianOpenFx::FrameState& value, MondrianOpenFxNativeDiagnostic* diagnostic) : frame(value) {
        frame.renderDiagnostic = diagnostic;
      }
      ~DiagnosticBorrow() { frame.renderDiagnostic = nullptr; }
    } diagnosticBorrow(session->frame, diagnostic);
    int status = session->render(input, output, bytes, *info, parameters, count, *report, probe);
    // This single publication boundary also covers Identity/UserBlocked and
    // EndSequenceRender. A plugin cannot publish by ignoring a query error.
    if (session->frame.invalidChoiceDomain.load(std::memory_order_relaxed)) {
      session->failed = true;
      if (status == 0) {
        report->completion = MONDRIAN_OPENFX_RENDER_UNSET;
        report->output_alpha = MONDRIAN_OPENFX_ALPHA_INVALID;
        status = 7;
      }
    }
    const auto phase = probe.start();
    session->host.auxiliary().report(report->feedback);
    probe.finish(MONDRIAN_OPENFX_AUXILIARY_REPORT, phase);
    return status;
  } catch (const std::exception& error) {
    session->failed = true;
    std::cerr << "OpenFX session render failed: " << error.what() << std::endl;
    return 6;
  } catch (...) { session->failed = true; return 6; }
}

extern "C" int mondrian_openfx_session_render(
    MondrianOpenFxSession* session, const void* input, void* output, size_t bytes,
    const MondrianOpenFxFrameInfo* info, const MondrianOpenFxParameter* parameters,
    int count, MondrianOpenFxRenderReport* report,
    const MondrianOpenFxInvocationControl* control,
    MondrianOpenFxNativeDiagnostic* diagnostic) noexcept
{
  if (diagnostic) {
    MondrianOpenFx::TimedRenderProbe probe(*diagnostic);
    return renderSessionEntry(session, input, output, bytes, info, parameters, count, report, control, diagnostic, probe);
  }
  MondrianOpenFx::NoRenderProbe probe;
  return renderSessionEntry(session, input, output, bytes, info, parameters, count, report, control, nullptr, probe);
}

extern "C" int mondrian_openfx_session_close_gui(MondrianOpenFxSession* session,
    MondrianOpenFxFeedback* feedback, int* invoked) noexcept {
  if (!session || session->failed || !feedback || !invoked) { return 2; }
  *invoked = 0;
  try {
    auto* filter = dynamic_cast<MondrianOpenFx::FilterEffectInstance*>(session->instance.get());
    if (!filter) { session->failed = true; return 6; }
    const bool wasOpen = filter->isInterfaceOpen();
    const OfxStatus status = filter->setInterfaceOpen(false);
    if (wasOpen) {
      // This action did not begin another Render invocation. Drain its notices
      // now so neither a later frame nor destruction can relabel/lose them.
      session->host.auxiliary().report(*feedback);
      *invoked = 1;
    }
    // A failed plugin-private UI teardown is a legal action reply. The real
    // generic GUI has nevertheless closed and End has been paired once.
    if (status == kOfxStatOK || status == kOfxStatReplyDefault || status == kOfxStatFailed) { return 0; }
    session->failed = true;
    return 5;
  } catch (...) { session->failed = true; return 6; }
}

extern "C" void mondrian_openfx_session_destroy(MondrianOpenFxSession* session) noexcept
{
  delete session;
}

extern "C" size_t mondrian_openfx_aux_resident_bytes(void) noexcept {
  return MondrianOpenFx::AuxSuite::residentBytes();
}
