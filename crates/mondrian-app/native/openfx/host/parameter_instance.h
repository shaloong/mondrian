// Copyright OpenFX and contributors to the OpenFX project.
// SPDX-License-Identifier: BSD-3-Clause
#ifndef HOST_DEMO_PARAM_INSTANCE_H
#define HOST_DEMO_PARAM_INSTANCE_H

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
    OFX::Host::Param::Descriptor& _descriptor;
  public:
    ChoiceParameter(FilterEffectInstance* effect,  const std::string& name, OFX::Host::Param::Descriptor& descriptor);
    OfxStatus get(int&);
    OfxStatus get(OfxTime time, int&);
    OfxStatus set(int);
    OfxStatus set(OfxTime time, int);
  };

  class RgbaParameter : public OFX::Host::Param::RGBAInstance {
  protected:
    FilterEffectInstance*   _effect;
    OFX::Host::Param::Descriptor& _descriptor;
  public:
    RgbaParameter(FilterEffectInstance* effect, const std::string& name, OFX::Host::Param::Descriptor& descriptor);
    OfxStatus get(double&,double&,double&,double&);
    OfxStatus get(OfxTime time, double&,double&,double&,double&);
    OfxStatus set(double,double,double,double);
    OfxStatus set(OfxTime time, double,double,double,double);
  };


  class RgbParameter : public OFX::Host::Param::RGBInstance {
  protected:
    FilterEffectInstance*   _effect;
    OFX::Host::Param::Descriptor& _descriptor;
  public:
    RgbParameter(FilterEffectInstance* effect,  const std::string& name, OFX::Host::Param::Descriptor& descriptor);
    OfxStatus get(double&,double&,double&);
    OfxStatus get(OfxTime time, double&,double&,double&);
    OfxStatus set(double,double,double);
    OfxStatus set(OfxTime time, double,double,double);
  };

  class Double2DParameter : public OFX::Host::Param::Double2DInstance {
  protected:
    FilterEffectInstance*   _effect;
    OFX::Host::Param::Descriptor& _descriptor;
  public:
    Double2DParameter(FilterEffectInstance* effect, const std::string& name, OFX::Host::Param::Descriptor& descriptor);
    OfxStatus get(double&,double&);
    OfxStatus get(OfxTime time,double&,double&);
    OfxStatus set(double,double);
    OfxStatus set(OfxTime time,double,double);
  };

  class Integer2DParameter : public OFX::Host::Param::Integer2DInstance {
  protected:
    FilterEffectInstance*   _effect;
    OFX::Host::Param::Descriptor& _descriptor;
  public:
    Integer2DParameter(FilterEffectInstance* effect,  const std::string& name, OFX::Host::Param::Descriptor& descriptor);
    OfxStatus get(int&,int&);
    OfxStatus get(OfxTime time,int&,int&);
    OfxStatus set(int,int);
    OfxStatus set(OfxTime time,int,int);
  };


}

#endif // HOST_DEMO_PARAM_INSTANCE_H
