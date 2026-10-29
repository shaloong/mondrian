// Copyright Mondrian contributors.
// SPDX-License-Identifier: BSD-3-Clause
// Exercise the actual exported suite, including hooks and chained property sets.
#include "ofxCore.h"
#include "ofxProperty.h"
#include "ofxhPropertySuite.h"
#include <array>
#include <climits>
#include <cstdio>
#include <cstring>
#include <type_traits>

namespace P = OFX::Host::Property;
namespace {
int failures = 0;
void require(bool ok, const char* kind, const char* operation) {
  if (!ok) { ++failures; std::printf("FAIL %s: %s\n", kind, operation); }
}
struct Hook final : P::GetHook {
  mutable int calls = 0;
  int dimension = 2;
  const std::string text = "hook";
  int getDimension(const std::string&) const override { return dimension; }
  int getIntProperty(const std::string&, int) const override { ++calls; return 7; }
  double getDoubleProperty(const std::string&, int) const override { ++calls; return .5; }
  void* getPointerProperty(const std::string&, int) const override { ++calls; return nullptr; }
  const std::string& getStringProperty(const std::string&, int) const override { ++calls; return text; }
  void getIntPropertyN(const std::string& n, int* v, int count) const override {
    for (int i=0; i<count; ++i) v[i] = getIntProperty(n,i);
  }
  void getDoublePropertyN(const std::string& n, double* v, int count) const override {
    for (int i=0; i<count; ++i) v[i] = getDoubleProperty(n,i);
  }
  void getPointerPropertyN(const std::string& n, void** v, int count) const override {
    for (int i=0; i<count; ++i) v[i] = getPointerProperty(n,i);
  }
};

template<class T, class Get, class GetN>
void check(const OfxPropertySuiteV1& suite, OfxPropertySetHandle set,
           const char* name, Get get, GetN getN, T sentinel, T expected,
           int dimension, Hook* hook) {
  int reported = -1;
  require(suite.propGetDimension(set,name,&reported)==kOfxStatOK && reported==dimension,
          name,"actual dimension");
  for (int index : {-1,dimension}) {
    T value = sentinel;
    const int calls = hook ? hook->calls : 0;
    require(get(set,name,index,&value)==kOfxStatErrBadIndex && value==sentinel,
            name,"invalid scalar must refuse without writing");
    require(!hook || hook->calls==calls,name,"invalid scalar cannot dispatch hook");
  }
  for (int count : {-1,dimension+1}) {
    std::array<T,4> values{sentinel,sentinel,sentinel,sentinel};
    const auto before = values;
    const int calls = hook ? hook->calls : 0;
    require(getN(set,name,count,values.data())==kOfxStatErrBadIndex && values==before,
            name,"invalid count must refuse without a partial write");
    require(!hook || hook->calls==calls,name,"invalid count cannot dispatch hook");
  }
  std::array<T,4> values{sentinel,sentinel,sentinel,sentinel};
  const auto before = values;
  require(getN(set,name,0,values.data())==kOfxStatOK && values==before,name,"empty read");
  for (int count=1; count<=dimension; ++count) {
    values.fill(sentinel);
    require(getN(set,name,count,values.data())==kOfxStatOK,name,"valid prefix read");
    for (int i=0; i<4; ++i) {
      bool equal = values[i] == (i<count ? expected : sentinel);
      if constexpr (std::is_same_v<T,char*>) {
        if (i<count) equal = values[i] && !std::strcmp(values[i],expected);
      }
      require(equal,name,"exact prefix and untouched suffix");
    }
  }
}
}

int main() {
  const auto& suite = *static_cast<const OfxPropertySuiteV1*>(P::GetSuite(1));
  for (int shape=0; shape<6; ++shape) {
    const bool hooked = shape==2 || shape==3;
    const bool dynamic = shape==1 || shape==3 || shape==4;
    const int dimension = shape==4 ? 0 : 2;
    Hook hook;
    P::Set owner, chained;
    // Host construction is the only writer; getters are called across the API.
    owner.addProperty(new P::Int("int",dynamic?0:2,false,7));
    owner.addProperty(new P::Double("double",dynamic?0:2,false,.5));
    owner.addProperty(new P::String("string",dynamic?0:2,false,"hook"));
    owner.addProperty(new P::Pointer("pointer",dynamic?0:2,false,nullptr));
    if (dynamic && dimension) {
      for (int i=0; i<2; ++i) {
        owner.setIntProperty("int",7,i); owner.setDoubleProperty("double",.5,i);
        owner.setStringProperty("string","hook",i); owner.setPointerProperty("pointer",nullptr,i);
      }
    }
    if (hooked) for (auto n : {"int","double","string","pointer"}) owner.setGetHook(n,&hook);
    chained.setChainedSet(&owner);
    auto set = (shape==5 ? chained : owner).getHandle();
    std::printf("shape %d\n",shape);
    check(suite,set,"int",suite.propGetInt,suite.propGetIntN,-73,7,dimension,hooked?&hook:nullptr);
    check(suite,set,"double",suite.propGetDouble,suite.propGetDoubleN,-73.0,.5,dimension,hooked?&hook:nullptr);
    char marker[]="untouched", expected[]="hook";
    check(suite,set,"string",suite.propGetString,suite.propGetStringN,marker,expected,dimension,hooked?&hook:nullptr);
    void* pointer=&failures;
    check(suite,set,"pointer",suite.propGetPointer,suite.propGetPointerN,pointer,static_cast<void*>(nullptr),dimension,hooked?&hook:nullptr);
  }
  {
    P::Set owner;
    owner.addProperty(new P::Double("bounded", 0, false, 0));
    auto* bounded = owner.fetchProperty("bounded");
    bounded->setMaximumDimension(2);
    const double values[]{.25, .5, .75};
    const auto set = owner.getHandle();
    require(suite.propSetDoubleN(set, "bounded", 2, values) == kOfxStatOK &&
        bounded->getDimension() == 2 && !bounded->isFixedSize(), "bounded", "limit keeps a variable property variable");
    require(suite.propSetDoubleN(set, "bounded", -1, values) == kOfxStatErrBadIndex &&
        suite.propSetDouble(set, "bounded", -1, .75) == kOfxStatErrBadIndex && !bounded->writeLimitExceeded(),
        "bounded", "ordinary invalid indices do not claim a capacity refusal");
    require(suite.propSetDoubleN(set, "bounded", INT_MAX, values) == kOfxStatErrBadIndex &&
        bounded->writeLimitExceeded() && bounded->getDimension() == 2 &&
        owner.getDoubleProperty("bounded", 0) == .25 && owner.getDoubleProperty("bounded", 1) == .5,
        "bounded", "excessive array count refuses before resize/copy or partial mutation");
    P::Set copied(owner);
    auto* copy = copied.fetchProperty("bounded");
    require(copy->writeLimitExceeded() && suite.propSetDouble(copied.getHandle(), "bounded", 2, .75) == kOfxStatErrBadIndex &&
        copy->getDimension() == 2, "bounded", "cloning preserves bound and refusal");
    require(suite.propReset(set, "bounded") == kOfxStatOK && bounded->getDimension() == 0 && bounded->writeLimitExceeded(),
        "bounded", "reset does not erase the action's refused-write evidence");
    require(suite.propSetDoubleN(set, "bounded", 0, nullptr) == kOfxStatOK && bounded->getDimension() == 0,
        "bounded", "explicit zero-count write remains empty");
    require(suite.propSetDouble(set, "bounded", INT_MAX, .75) == kOfxStatErrBadIndex && bounded->getDimension() == 0,
        "bounded", "scalar index refuses before index-plus-one overflow");
  }
  std::printf("property read bounds: %d failures\n",failures);
  return failures ? 1 : 0;
}
