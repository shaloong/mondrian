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
  OfxRectI pixels{9,9,9,9}; size_t count = 99;
  require(pixelRegionOfDefinition({2,0.25,5.5,1.75},2,pixels,count));
  require(sameRegion(pixels,{1,0,3,2}) && count == 4);
  require(pixelRegionOfDefinition({-2,-1,8,3},2,pixels,count));
  require(sameRegion(pixels,{-1,-1,4,3}) && count == 20);
  require(pixelRegionOfDefinition({1.25,0,1.25,2},2,pixels,count));
  require(count == 0 && sameRegion(pixels,{0,0,0,0}));
  require(pixelRegionOfDefinition({0,0,8192,1024},1,pixels,count));
  require(count * 16 == kMaximumDomainBytes);
  const auto original = pixels; const auto originalCount = count;
  for (const auto& invalid : {OfxRectD{0,0,8192,1025}, OfxRectD{0,0,8193,1},
       OfxRectD{3,0,2,1}, OfxRectD{0,3,2,1}, OfxRectD{0,0,1e100,1},
       OfxRectD{-1e100,0,1,1}, OfxRectD{0,0,std::numeric_limits<double>::infinity(),1},
       OfxRectD{std::numeric_limits<double>::quiet_NaN(),0,2,1}}) {
    require(!pixelRegionOfDefinition(invalid,1,pixels,count));
    require(sameRegion(pixels,original) && count == originalCount);
  }
  require(!pixelRegionOfDefinition({0,0,1,1},0,pixels,count));
  std::puts("image domain conversion: all cases passed");
}
