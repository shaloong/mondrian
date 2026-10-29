// Copyright Mondrian contributors.
// SPDX-License-Identifier: BSD-3-Clause
#include "image_domain.h"
#include <cstdio>
#include <cstdlib>
#include <limits>
#include <initializer_list>
namespace {
void require(bool condition) { if (!condition) { std::abort(); } }
}
int main() {
  using namespace MondrianOpenFx;
  const OfxPointD unit{1,1};
  OfxRectI pixels{9,9,9,9}; size_t count = 99;
  require(pixelRegionOfDefinition({2,0.25,5.5,1.75},2,unit,pixels,count));
  require(sameRegion(pixels,{1,0,3,2}) && count == 4);
  require(pixelRegionOfDefinition({-2,-1,8,3},2,unit,pixels,count));
  require(sameRegion(pixels,{-1,-1,4,3}) && count == 20);
  require(pixelRegionOfDefinition({1.25,0,1.25,2},2,unit,pixels,count));
  require(count == 0 && sameRegion(pixels,{0,0,0,0}));
  require(pixelRegionOfDefinition({0,0,8192,1024},1,unit,pixels,count));
  require(count * 16 == kMaximumDomainBytes);
  const auto original = pixels; const auto originalCount = count;
  for (const auto& invalid : {OfxRectD{0,0,8192,1025}, OfxRectD{0,0,8193,1},
       OfxRectD{3,0,2,1}, OfxRectD{0,3,2,1}, OfxRectD{0,0,1e100,1},
       OfxRectD{-1e100,0,1,1}, OfxRectD{0,0,std::numeric_limits<double>::infinity(),1},
       OfxRectD{std::numeric_limits<double>::quiet_NaN(),0,2,1}}) {
    require(!pixelRegionOfDefinition(invalid,1,unit,pixels,count));
    require(sameRegion(pixels,original) && count == originalCount);
  }
  require(!pixelRegionOfDefinition({0,0,1,1},0,unit,pixels,count));
  OfxRectD canonical{};
  require(canonicalRegionOfDefinition({-3,-2,5,4},2,{.5,.25},canonical));
  require(canonical.x1 == -12 && canonical.y1 == -8 && canonical.x2 == 20 && canonical.y2 == 16);
  require(pixelRegionOfDefinition(canonical,2,{.5,.25},pixels,count));
  require(sameRegion(pixels,{-3,-2,5,4}) && count == 48);
  require(pixelRegionOfDefinition({-4,-1,10,5},2,{.75,.5},pixels,count));
  require(sameRegion(pixels,{-2,-1,4,3}) && count == 24);
  // Admission is based on actual backing pixels, independent of canonical size.
  require(pixelRegionOfDefinition({0,0,16384,2048},1,{.5,.5},pixels,count));
  require(count * 16 == kMaximumDomainBytes);
  for (const auto scale : {OfxPointD{0,1}, OfxPointD{1,-1},
       OfxPointD{std::numeric_limits<double>::infinity(),1},
       OfxPointD{1,std::numeric_limits<double>::quiet_NaN()}}) {
    const auto before = pixels; const auto beforeCount = count;
    const auto beforeCanonical = canonical;
    require(!pixelRegionOfDefinition({0,0,1,1},1,scale,pixels,count));
    require(sameRegion(pixels,before) && count == beforeCount);
    require(!canonicalRegionOfDefinition({0,0,1,1},1,scale,canonical));
    require(canonical.x1 == beforeCanonical.x1 && canonical.y2 == beforeCanonical.y2);
  }
  require(!canonicalRegionOfDefinition({0,0,2,2},std::numeric_limits<double>::max(),unit,canonical));
  std::puts("image domain conversion: all cases passed");
}
