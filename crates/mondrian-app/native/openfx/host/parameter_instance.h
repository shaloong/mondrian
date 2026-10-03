// Copyright OpenFX and contributors to the OpenFX project.
// SPDX-License-Identifier: BSD-3-Clause
#ifndef HOST_DEMO_PARAM_INSTANCE_H
#define HOST_DEMO_PARAM_INSTANCE_H

#include <array>

namespace MondrianOpenFx {

  class PushbuttonParameter : public OFX::Host::Param::PushbuttonInstance {
  protected:
    FilterEffectInstance*   _effect;
    OFX::Host::Param::Descriptor& _descriptor;
  public:
    PushbuttonParameter(FilterEffectInstance* effect, const std::string& name, OFX::Host::Param::Descriptor& descriptor);
  };

  class IntegerParameter : public OFX::Host::Param::IntegerInstance {
  protected:
    FilterEffectInstance*   _effect;
    OFX::Host::Param::Descriptor& _descriptor;
    int _value;
  public:
    IntegerParameter(FilterEffectInstance* effect, const std::string& name, OFX::Host::Param::Descriptor& descriptor);
    OfxStatus get(int&);
    OfxStatus get(OfxTime time, int&);
    OfxStatus set(int);
    OfxStatus set(OfxTime time, int);
  };

  class DoubleParameter : public OFX::Host::Param::DoubleInstance {
  protected:
    FilterEffectInstance*   _effect;
    OFX::Host::Param::Descriptor& _descriptor;
    double _value;
  public:
    DoubleParameter(FilterEffectInstance* effect, const std::string& name, OFX::Host::Param::Descriptor& descriptor);
    OfxStatus get(double&);
    OfxStatus get(OfxTime time, double&);
    OfxStatus set(double);
    OfxStatus set(OfxTime time, double);
    OfxStatus derive(OfxTime time, double&);
    OfxStatus integrate(OfxTime time1, OfxTime time2, double&);
  };

  class BooleanParameter : public OFX::Host::Param::BooleanInstance {
  protected:
    FilterEffectInstance*   _effect;
    OFX::Host::Param::Descriptor& _descriptor;
    bool _value;
  public:
    BooleanParameter(FilterEffectInstance* effect, const std::string& name, OFX::Host::Param::Descriptor& descriptor);
    OfxStatus get(bool&);
    OfxStatus get(OfxTime time, bool&);
    OfxStatus set(bool);
    OfxStatus set(OfxTime time, bool);
  };

  class ChoiceParameter : public OFX::Host::Param::ChoiceInstance {
  protected:
    FilterEffectInstance*   _effect;
    int _value;
  public:
    ChoiceParameter(FilterEffectInstance* effect,  const std::string& name, OFX::Host::Param::Descriptor& descriptor);
    OfxStatus get(int&);
    OfxStatus get(OfxTime time, int&);
    OfxStatus set(int);
    OfxStatus set(OfxTime time, int);
  };

  class StrChoiceParameter : public OFX::Host::Param::StringInstance {
    FilterEffectInstance* _effect;
    std::string _value;
    bool contains(const std::string& value) const;
  public:
    StrChoiceParameter(FilterEffectInstance* effect, OFX::Host::Param::Descriptor& descriptor);
    OfxStatus get(std::string&) override;
    OfxStatus get(OfxTime time, std::string&) override;
    OfxStatus set(const char*) override;
    OfxStatus set(OfxTime time, const char*) override;
  };

  class StringParameter : public OFX::Host::Param::StringInstance {
    FilterEffectInstance* _effect;
    std::string _value;
  public:
    StringParameter(FilterEffectInstance* effect, OFX::Host::Param::Descriptor& descriptor);
    OfxStatus get(std::string&) override;
    OfxStatus get(OfxTime time, std::string&) override;
    OfxStatus set(const char*) override;
    OfxStatus set(OfxTime time, const char*) override;
  };

  // Bounded execution snapshot for one atomic SDK parameter.
  template<class T, size_t N>
  class NumericTupleState {
    FilterEffectInstance* _effect;
    OFX::Host::Param::Descriptor& _descriptor;
    std::array<T, N> _values;
    T property(const char* name, size_t index) const;
    bool admits(const std::array<T, N>& values) const;
  public:
    NumericTupleState(FilterEffectInstance* effect, OFX::Host::Param::Descriptor& descriptor);
    OfxStatus read(const std::array<T*, N>& outputs) const;
    OfxStatus read(OfxTime time, const std::array<T*, N>& outputs) const;
    OfxStatus write(const std::array<T, N>& values);
    OfxStatus write(OfxTime time, const std::array<T, N>& values);
    OfxStatus derive(OfxTime time, const std::array<T*, N>& outputs) const;
    OfxStatus integrate(OfxTime start, OfxTime end, const std::array<T*, N>& outputs) const;
  };

  template<class SdkInstance, class T, size_t N> class NumericTupleParameter;

  template<class SdkInstance, class T>
  class NumericTupleParameter<SdkInstance, T, 2> : public SdkInstance {
    NumericTupleState<T, 2> _value;
  public:
    NumericTupleParameter(FilterEffectInstance* effect, OFX::Host::Param::Descriptor& descriptor)
        : SdkInstance(descriptor, effect), _value(effect, descriptor) {}
    OfxStatus get(T& x, T& y) override { return _value.read({&x, &y}); }
    OfxStatus get(OfxTime time, T& x, T& y) override { return _value.read(time, {&x, &y}); }
    OfxStatus set(T x, T y) override { return _value.write({x, y}); }
    OfxStatus set(OfxTime time, T x, T y) override { return _value.write(time, {x, y}); }
    OfxStatus derive(OfxTime time, T& x, T& y) override { return _value.derive(time, {&x, &y}); }
    OfxStatus integrate(OfxTime start, OfxTime end, T& x, T& y) override {
      return _value.integrate(start, end, {&x, &y});
    }
  };

  template<class SdkInstance, class T>
  class NumericTupleParameter<SdkInstance, T, 3> : public SdkInstance {
    NumericTupleState<T, 3> _value;
  public:
    NumericTupleParameter(FilterEffectInstance* effect, OFX::Host::Param::Descriptor& descriptor)
        : SdkInstance(descriptor, effect), _value(effect, descriptor) {}
    OfxStatus get(T& x, T& y, T& z) override { return _value.read({&x, &y, &z}); }
    OfxStatus get(OfxTime time, T& x, T& y, T& z) override { return _value.read(time, {&x, &y, &z}); }
    OfxStatus set(T x, T y, T z) override { return _value.write({x, y, z}); }
    OfxStatus set(OfxTime time, T x, T y, T z) override { return _value.write(time, {x, y, z}); }
    OfxStatus derive(OfxTime time, T& x, T& y, T& z) override { return _value.derive(time, {&x, &y, &z}); }
    OfxStatus integrate(OfxTime start, OfxTime end, T& x, T& y, T& z) override {
      return _value.integrate(start, end, {&x, &y, &z});
    }
  };

  template<class SdkInstance, class T>
  class NumericTupleParameter<SdkInstance, T, 4> : public SdkInstance {
    NumericTupleState<T, 4> _value;
  public:
    NumericTupleParameter(FilterEffectInstance* effect, OFX::Host::Param::Descriptor& descriptor)
        : SdkInstance(descriptor, effect), _value(effect, descriptor) {}
    OfxStatus get(T& x, T& y, T& z, T& w) override { return _value.read({&x, &y, &z, &w}); }
    OfxStatus get(OfxTime time, T& x, T& y, T& z, T& w) override { return _value.read(time, {&x, &y, &z, &w}); }
    OfxStatus set(T x, T y, T z, T w) override { return _value.write({x, y, z, w}); }
    OfxStatus set(OfxTime time, T x, T y, T z, T w) override { return _value.write(time, {x, y, z, w}); }
    OfxStatus derive(OfxTime time, T& x, T& y, T& z, T& w) override { return _value.derive(time, {&x, &y, &z, &w}); }
    OfxStatus integrate(OfxTime start, OfxTime end, T& x, T& y, T& z, T& w) override {
      return _value.integrate(start, end, {&x, &y, &z, &w});
    }
  };


}

#endif // HOST_DEMO_PARAM_INSTANCE_H
