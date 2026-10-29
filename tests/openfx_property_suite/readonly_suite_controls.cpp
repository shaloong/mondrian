// Owned controls over the repository SDK Set and actual PropertySuite V1.
// This executable is neither a plugin nor a production host test hook.
#include "ofxCore.h"
#include "ofxProperty.h"
#include "ofxImageEffect.h"
#include "ofxParam.h"
#include "ofxhPropertySuite.h"

#include <cstdint>
#include <cstring>
#include <exception>
#include <iostream>
#include <string>

namespace P = OFX::Host::Property;

struct Checks {
  unsigned total = 0;
  unsigned failed = 0;
  void expect(bool value, const std::string& label) {
    ++total;
    if (!value) { ++failed; std::cout << "FAIL " << label << '\n'; }
  }
  void status(OfxStatus actual, OfxStatus expected, const std::string& label) {
    expect(actual == expected, label);
    if (actual != expected) {
      std::cout << "STATUS " << label << " actual=" << actual
                << " expected=" << expected << '\n';
    }
  }
};

static std::uint64_t bits(double value) {
  std::uint64_t result = 0;
  static_assert(sizeof(value) == sizeof(result), "double ABI required");
  std::memcpy(&result, &value, sizeof(result));
  return result;
}
static int pointer_tokens[4] = {11, 22, 33, 44};

struct IntOps {
  using Value = int;
  using Alias = P::Int;
  static bool same(Value a, Value b) { return a == b; }
  static Value initial(int) { return 7; }
  static Value host(int index) { return index ? 42 : 41; }
  static Value changed(int index) { return index ? 92 : 91; }
  static OfxStatus scalar(const OfxPropertySuiteV1& s, OfxPropertySetHandle h,
                         const char* n, Value v) { return s.propSetInt(h, n, 0, v); }
  static OfxStatus array(const OfxPropertySuiteV1& s, OfxPropertySetHandle h,
                        const char* n, const Value* v) { return s.propSetIntN(h, n, 2, v); }
  static OfxStatus read(const OfxPropertySuiteV1& s, OfxPropertySetHandle h,
                       const char* n, Value* v) { return s.propGetIntN(h, n, 2, v); }
  static void direct(P::Set& set, const char* n, const Value* v) {
    set.setIntPropertyN(n, v, 2);
  }
};
struct DoubleOps {
  using Value = double;
  using Alias = P::Double;
  static bool same(Value a, Value b) { return bits(a) == bits(b); }
  static Value initial(int) { return 1.25; }
  static Value host(int index) { return index ? 1.5 : -0.0; }
  static Value changed(int index) { return index ? 8.5 : 7.5; }
  static OfxStatus scalar(const OfxPropertySuiteV1& s, OfxPropertySetHandle h,
                         const char* n, Value v) { return s.propSetDouble(h, n, 0, v); }
  static OfxStatus array(const OfxPropertySuiteV1& s, OfxPropertySetHandle h,
                        const char* n, const Value* v) { return s.propSetDoubleN(h, n, 2, v); }
  static OfxStatus read(const OfxPropertySuiteV1& s, OfxPropertySetHandle h,
                       const char* n, Value* v) { return s.propGetDoubleN(h, n, 2, v); }
  static void direct(P::Set& set, const char* n, const Value* v) {
    set.setDoublePropertyN(n, v, 2);
  }
};
struct StringOps {
  using Value = const char*;
  using Alias = P::String;
  static bool same(Value a, Value b) { return a && b && std::strcmp(a, b) == 0; }
  static Value initial(int) { return "base"; }
  static Value host(int index) { return index ? "host.b" : "host.a"; }
  static Value changed(int index) { return index ? "plugin.b" : "plugin.a"; }
  static OfxStatus scalar(const OfxPropertySuiteV1& s, OfxPropertySetHandle h,
                         const char* n, Value v) { return s.propSetString(h, n, 0, v); }
  static OfxStatus array(const OfxPropertySuiteV1& s, OfxPropertySetHandle h,
                        const char* n, const Value* v) { return s.propSetStringN(h, n, 2, v); }
  static OfxStatus read(const OfxPropertySuiteV1& s, OfxPropertySetHandle h,
                       const char* n, Value* v) {
    char* result[2] = {nullptr, nullptr};
    const auto status = s.propGetStringN(h, n, 2, result);
    v[0] = result[0]; v[1] = result[1];
    return status;
  }
  static void direct(P::Set& set, const char* n, const Value* v) {
    set.setStringProperty(n, v[0], 0);
    set.setStringProperty(n, v[1], 1);
  }
};
struct PointerOps {
  using Value = void*;
  using Alias = P::Pointer;
  static bool same(Value a, Value b) { return a == b; }
  static Value initial(int) { return nullptr; }
  static Value host(int index) { return &pointer_tokens[index]; }
  static Value changed(int index) { return &pointer_tokens[index + 2]; }
  static OfxStatus scalar(const OfxPropertySuiteV1& s, OfxPropertySetHandle h,
                         const char* n, Value v) { return s.propSetPointer(h, n, 0, v); }
  static OfxStatus array(const OfxPropertySuiteV1& s, OfxPropertySetHandle h,
                        const char* n, const Value* v) { return s.propSetPointerN(h, n, 2, v); }
  static OfxStatus read(const OfxPropertySuiteV1& s, OfxPropertySetHandle h,
                       const char* n, Value* v) { return s.propGetPointerN(h, n, 2, v); }
  static void direct(P::Set& set, const char* n, const Value* v) {
    set.setPointerProperty(n, v[0], 0);
    set.setPointerProperty(n, v[1], 1);
  }
};

static const P::PropSpec specs[] = {
  {"ro.int", P::eInt, 2, true, "7"}, {"rw.int", P::eInt, 2, false, "7"},
  {"ro.double", P::eDouble, 2, true, "1.25"}, {"rw.double", P::eDouble, 2, false, "1.25"},
  {"ro.string", P::eString, 2, true, "base"}, {"rw.string", P::eString, 2, false, "base"},
  {"ro.pointer", P::ePointer, 2, true, nullptr}, {"rw.pointer", P::ePointer, 2, false, nullptr},
  P::propSpecEnd
};

template<class Ops> static void values(Checks& c, const OfxPropertySuiteV1& s,
                                     const P::Set& set, const char* name,
                                     typename Ops::Value first, typename Ops::Value second,
                                     const std::string& label) {
  typename Ops::Value actual[2] = {};
  c.status(Ops::read(s, set.getHandle(), name, actual), kOfxStatOK, label + ".getN");
  c.expect(Ops::same(actual[0], first) && Ops::same(actual[1], second), label + ".literal");
  int dimension = -1;
  c.status(s.propGetDimension(set.getHandle(), name, &dimension), kOfxStatOK, label + ".dimension-status");
  c.expect(dimension == 2, label + ".dimension-two");
}

template<class Ops> static void typed(Checks& c, const OfxPropertySuiteV1& s,
                                    const char* ro, const char* rw) {
  P::Set set(specs);
  c.expect(set.fetchProperty(ro)->getPluginReadOnly(), std::string(ro) + ".declared-readonly");
  c.expect(!set.fetchProperty(rw)->getPluginReadOnly(), std::string(rw) + ".declared-writable");
  const typename Ops::Value host[2] = {Ops::host(0), Ops::host(1)};
  const typename Ops::Value replacement[2] = {Ops::changed(0), Ops::changed(1)};
  Ops::direct(set, ro, host);
  values<Ops>(c, s, set, ro, host[0], host[1], std::string(ro) + ".host-direct");
  c.status(Ops::scalar(s, set.getHandle(), ro, replacement[0]), kOfxStatErrValue, std::string(ro) + ".scalar-refusal");
  values<Ops>(c, s, set, ro, host[0], host[1], std::string(ro) + ".after-scalar");
  c.status(Ops::array(s, set.getHandle(), ro, replacement), kOfxStatErrValue, std::string(ro) + ".array-refusal");
  values<Ops>(c, s, set, ro, host[0], host[1], std::string(ro) + ".after-array");
  c.status(s.propReset(set.getHandle(), ro), kOfxStatErrValue, std::string(ro) + ".reset-refusal");
  values<Ops>(c, s, set, ro, host[0], host[1], std::string(ro) + ".after-reset");

  Ops::direct(set, rw, host);
  c.status(Ops::scalar(s, set.getHandle(), rw, replacement[0]), kOfxStatOK, std::string(rw) + ".scalar-accept");
  values<Ops>(c, s, set, rw, replacement[0], host[1], std::string(rw) + ".after-scalar");
  c.status(Ops::array(s, set.getHandle(), rw, replacement), kOfxStatOK, std::string(rw) + ".array-accept");
  values<Ops>(c, s, set, rw, replacement[0], replacement[1], std::string(rw) + ".after-array");
  c.status(s.propReset(set.getHandle(), rw), kOfxStatOK, std::string(rw) + ".reset-accept");
  values<Ops>(c, s, set, rw, Ops::initial(0), Ops::initial(1), std::string(rw) + ".default-restored");

  P::Set copied(set);
  c.expect(copied.fetchProperty(ro)->getPluginReadOnly(), std::string(ro) + ".set-copy-flag");
  c.expect(!copied.fetchProperty(rw)->getPluginReadOnly(), std::string(rw) + ".set-copy-flag");
  c.status(Ops::scalar(s, copied.getHandle(), ro, replacement[0]), kOfxStatErrValue, std::string(ro) + ".copy-refusal");
  values<Ops>(c, s, copied, ro, host[0], host[1], std::string(ro) + ".copy-unchanged");
  c.status(Ops::array(s, copied.getHandle(), rw, replacement), kOfxStatOK, std::string(rw) + ".copy-writable");
  values<Ops>(c, s, set, rw, Ops::initial(0), Ops::initial(1), std::string(rw) + ".original-independent");

  P::Set deep;
  deep.addProperty(set.fetchProperty(ro)->deepCopy());
  deep.addProperty(set.fetchProperty(rw)->deepCopy());
  c.expect(deep.fetchProperty(ro)->getPluginReadOnly(), std::string(ro) + ".deep-flag");
  c.expect(!deep.fetchProperty(rw)->getPluginReadOnly(), std::string(rw) + ".deep-flag");
  c.status(Ops::array(s, deep.getHandle(), ro, replacement), kOfxStatErrValue, std::string(ro) + ".deep-array-refusal");
  c.status(s.propReset(deep.getHandle(), ro), kOfxStatErrValue, std::string(ro) + ".deep-reset-refusal");
  values<Ops>(c, s, deep, ro, host[0], host[1], std::string(ro) + ".deep-unchanged");

  P::Set alias_copy;
  const auto* source_alias = dynamic_cast<typename Ops::Alias*>(set.fetchProperty(ro));
  c.expect(source_alias != nullptr, std::string(ro) + ".actual-typed-alias");
  if (source_alias) {
    alias_copy.addProperty(new typename Ops::Alias(*source_alias));
    c.expect(alias_copy.fetchProperty(ro)->getPluginReadOnly(), std::string(ro) + ".alias-copy-flag");
    c.status(s.propReset(alias_copy.getHandle(), ro), kOfxStatErrValue, std::string(ro) + ".alias-copy-reset-refusal");
    values<Ops>(c, s, alias_copy, ro, host[0], host[1], std::string(ro) + ".alias-copy-unchanged");
  }

  P::Set direct;
  direct.addProperty(new typename Ops::Alias(ro, 2, true, Ops::initial(0)));
  direct.addProperty(new typename Ops::Alias(rw, 2, false, Ops::initial(0)));
  c.expect(direct.fetchProperty(ro)->getPluginReadOnly(), std::string(ro) + ".direct-ctor-flag");
  c.expect(!direct.fetchProperty(rw)->getPluginReadOnly(), std::string(rw) + ".direct-ctor-flag");
  c.status(Ops::scalar(s, direct.getHandle(), ro, replacement[0]), kOfxStatErrValue, std::string(ro) + ".direct-ctor-refusal");
  values<Ops>(c, s, direct, ro, Ops::initial(0), Ops::initial(1), std::string(ro) + ".direct-default");

  P::Set child;
  child.setChainedSet(&set);
  values<Ops>(c, s, child, ro, host[0], host[1], std::string(ro) + ".chain-read");
  values<Ops>(c, s, child, rw, Ops::initial(0), Ops::initial(1), std::string(rw) + ".chain-read");
  for (const char* name : {ro, rw}) {
    c.status(Ops::scalar(s, child.getHandle(), name, replacement[0]), kOfxStatErrUnknown, std::string(name) + ".chain-scalar-local-only");
    c.status(Ops::array(s, child.getHandle(), name, replacement), kOfxStatErrUnknown, std::string(name) + ".chain-array-local-only");
    c.status(s.propReset(child.getHandle(), name), kOfxStatErrUnknown, std::string(name) + ".chain-reset-local-only");
  }
  values<Ops>(c, s, set, ro, host[0], host[1], std::string(ro) + ".parent-unchanged");
  values<Ops>(c, s, set, rw, Ops::initial(0), Ops::initial(1), std::string(rw) + ".parent-unchanged");
  c.expect(child.getProperties().empty(), std::string(ro) + ".chain-no-local-creation");

  const P::PropSpec variable_specs[] = {
    {"variable.ro", set.fetchProperty(ro)->getType(), 0, true, nullptr},
    {"variable.rw", set.fetchProperty(rw)->getType(), 0, false, nullptr}, P::propSpecEnd
  };
  P::Set variable(variable_specs);
  Ops::direct(variable, "variable.ro", host);
  c.status(Ops::scalar(s, variable.getHandle(), "variable.ro", replacement[0]), kOfxStatErrValue, std::string(ro) + ".variable-scalar-refusal");
  c.status(Ops::array(s, variable.getHandle(), "variable.ro", replacement), kOfxStatErrValue, std::string(ro) + ".variable-array-refusal");
  c.status(s.propReset(variable.getHandle(), "variable.ro"), kOfxStatErrValue, std::string(ro) + ".variable-reset-refusal");
  values<Ops>(c, s, variable, "variable.ro", host[0], host[1], std::string(ro) + ".variable-unchanged");
  c.status(Ops::array(s, variable.getHandle(), "variable.rw", replacement), kOfxStatOK, std::string(rw) + ".variable-array-accept");
  values<Ops>(c, s, variable, "variable.rw", replacement[0], replacement[1], std::string(rw) + ".variable-written");
  c.status(s.propReset(variable.getHandle(), "variable.rw"), kOfxStatOK, std::string(rw) + ".variable-reset-accept");
  int empty_dimension = -1;
  c.status(s.propGetDimension(variable.getHandle(), "variable.rw", &empty_dimension), kOfxStatOK, std::string(rw) + ".variable-empty-status");
  c.expect(empty_dimension == 0, std::string(rw) + ".variable-empty-after-reset");
}

static void representative_metadata(Checks& c, const OfxPropertySuiteV1& s) {
  const P::PropSpec descriptor_specs[] = {
    {kOfxPropType, P::eString, 1, true, kOfxTypeImageEffect},
    {kOfxPropLabel, P::eString, 1, false, "Owned label"}, P::propSpecEnd
  };
  P::Set descriptor(descriptor_specs);
  c.status(s.propSetString(descriptor.getHandle(), kOfxPropType, 0, "replacement"), kOfxStatErrValue, "descriptor.type-readonly");
  c.expect(descriptor.getStringProperty(kOfxPropType) == kOfxTypeImageEffect, "descriptor.type-literal");
  c.status(s.propSetString(descriptor.getHandle(), kOfxPropLabel, 0, "Writable label"), kOfxStatOK, "descriptor.label-writable");
  c.expect(descriptor.getStringProperty(kOfxPropLabel) == "Writable label", "descriptor.label-literal");

  const P::PropSpec instance_specs[] = {
    {kOfxImageEffectPropContext, P::eString, 1, true, ""}, P::propSpecEnd
  };
  P::Set instance(instance_specs);
  instance.setStringProperty(kOfxImageEffectPropContext, kOfxImageEffectContextFilter);
  c.status(s.propReset(instance.getHandle(), kOfxImageEffectPropContext), kOfxStatErrValue, "instance.context-readonly-reset");
  c.expect(instance.getStringProperty(kOfxImageEffectPropContext) == kOfxImageEffectContextFilter, "instance.host-context-kept");
  const P::PropSpec time_specs[] = {{kOfxPropTime, P::eDouble, 1, true, "0"}, P::propSpecEnd};
  P::Set in_args(time_specs);
  in_args.setDoubleProperty(kOfxPropTime, 12.5);
  const double other_time[] = {99.0};
  c.status(s.propSetDoubleN(in_args.getHandle(), kOfxPropTime, 1, other_time), kOfxStatErrValue, "action.time-readonly");
  c.expect(bits(in_args.getDoubleProperty(kOfxPropTime)) == bits(12.5), "action.host-time-kept");
  const P::PropSpec param_specs[] = {
    {kOfxPropType, P::eString, 1, true, kOfxTypeParameter},
    {kOfxParamPropDefault, P::eDouble, 1, false, "0"}, P::propSpecEnd
  };
  P::Set parameter(param_specs);
  c.status(s.propReset(parameter.getHandle(), kOfxPropType), kOfxStatErrValue, "parameter.type-readonly");
  c.status(s.propSetDouble(parameter.getHandle(), kOfxParamPropDefault, 0, 0.75), kOfxStatOK, "parameter.default-writable");
  c.expect(bits(parameter.getDoubleProperty(kOfxParamPropDefault)) == bits(0.75), "parameter.default-literal");
  const P::PropSpec clip_specs[] = {{kOfxImageClipPropConnected, P::eInt, 1, true, "0"}, P::propSpecEnd};
  P::Set clip(clip_specs);
  clip.setIntProperty(kOfxImageClipPropConnected, 1);
  c.status(s.propSetInt(clip.getHandle(), kOfxImageClipPropConnected, 0, 0), kOfxStatErrValue, "clip.connected-readonly");
  c.expect(clip.getIntProperty(kOfxImageClipPropConnected) == 1, "clip.host-connected-kept");
}

static void pixel_pointer(Checks& c, const OfxPropertySuiteV1& s) {
  const P::PropSpec image_specs[] = {
    {kOfxImagePropData, P::ePointer, 1, true, nullptr},
    {kOfxImagePropBounds, P::eInt, 4, true, "0"},
    {kOfxImagePropRowBytes, P::eInt, 1, true, "0"}, P::propSpecEnd
  };
  P::Set image(image_specs);
  float pixels[4] = {0.25f, 0.5f, 0.75f, 1.0f};
  float replacement[4] = {9.0f, 9.0f, 9.0f, 9.0f};
  image.setPointerProperty(kOfxImagePropData, pixels);
  const int bounds[] = {0, 0, 1, 1};
  image.setIntPropertyN(kOfxImagePropBounds, bounds, 4);
  image.setIntProperty(kOfxImagePropRowBytes, 16);
  void* returned = nullptr;
  c.status(s.propGetPointer(image.getHandle(), kOfxImagePropData, 0, &returned), kOfxStatOK, "image.data-get");
  c.expect(returned == pixels, "image.host-pointer-identity");
  c.status(s.propSetPointer(image.getHandle(), kOfxImagePropData, 0, replacement), kOfxStatErrValue, "image.metadata-scalar-refusal");
  void* changed[] = {replacement};
  c.status(s.propSetPointerN(image.getHandle(), kOfxImagePropData, 1, changed), kOfxStatErrValue, "image.metadata-array-refusal");
  c.status(s.propReset(image.getHandle(), kOfxImagePropData), kOfxStatErrValue, "image.metadata-reset-refusal");
  c.expect(image.getPointerProperty(kOfxImagePropData) == pixels, "image.pointer-unchanged");
  if (returned == pixels) {
    static_cast<float*>(returned)[0] = 0.125f;
    static_cast<float*>(returned)[3] = 0.875f;
  }
  c.expect(pixels[0] == 0.125f && pixels[1] == 0.5f && pixels[2] == 0.75f && pixels[3] == 0.875f, "image.legal-pixel-write");
  c.expect(replacement[0] == 9.0f && replacement[3] == 9.0f, "image.unselected-buffer-unchanged");
  c.expect(image.getIntProperty(kOfxImagePropRowBytes) == 16 && image.getIntProperty(kOfxImagePropBounds, 2) == 1, "image.metadata-kept");
}

int main() {
  Checks c;
  constexpr unsigned expected_checks = 456;
  bool normal_completed = false;
  const auto* suite = static_cast<const OfxPropertySuiteV1*>(P::GetSuite(1));
  c.expect(suite != nullptr, "actual-suite-v1");
  c.expect(P::GetSuite(0) == nullptr && P::GetSuite(2) == nullptr, "unsupported-suite-versions");
  if (!suite) { return 1; }
  try {
    typed<IntOps>(c, *suite, "ro.int", "rw.int");
    typed<DoubleOps>(c, *suite, "ro.double", "rw.double");
    typed<StringOps>(c, *suite, "ro.string", "rw.string");
    typed<PointerOps>(c, *suite, "ro.pointer", "rw.pointer");
    representative_metadata(c, *suite);
    pixel_pointer(c, *suite);
    normal_completed = true;
  } catch (const P::Exception& error) {
    c.expect(false, "unexpected-sdk-exception");
    std::cout << "SDK_EXCEPTION status=" << error.getStatus() << '\n';
  } catch (const std::exception& error) {
    c.expect(false, "unexpected-std-exception");
    std::cout << "EXCEPTION " << error.what() << '\n';
  } catch (...) {
    c.expect(false, "unexpected-exception");
  }
  const bool exact_total = c.total == expected_checks;
  std::cout << "OFX_READONLY_SUITE_CONTROLS total=" << c.total
            << " failed=" << c.failed
            << " normal_completed=" << (normal_completed ? 1 : 0)
            << " expected_total=" << expected_checks
            << " exact_total=" << (exact_total ? 1 : 0) << '\n';
  return normal_completed && exact_total && c.failed == 0 ? 0 : 1;
}
