# Mondrian Standard HDR v1 resource notice

`mondrian_standard_hdr_1000_p3_v1.cube` is the AgX Rec.2100 HLG
1000-nit, P3-D65-limited Formation LUT from `EaryChow/AgX`, pinned to
commit `74852bd3ca64e6592c2f6315fda4bcb1f8821ac2`.

The vendored file is renamed but otherwise byte-for-byte identical to
`luts/AgX_Rec2100-HLG_p3_lim.cube` at that commit after the repository's
declared CRLF checkout normalization:

- Source Git blob: `a4a561478a5ae40d4cc94eaf8c31d07bf51046e3`
- SHA-256: `4422eb9a8d3ecc16836d241287b171758e2f2db202934d9ff51865543dd360c0`
- Authored input domain: FilmLight E-Gamut, log2 allocation from -10 to +15 stops
- Authored output: Rec.2100 HLG, 1000-nit peak, 100-nit reference white,
  P3-D65 gamut limit
- LUT edge: 57
- Interpolation used by Mondrian: tetrahedral

Mondrian decodes the formation output to OCIO's display-reference XYZ and
then applies exactly one target display encoding. This lets HLG and PQ output
share the same rendering transform without embedding either display transfer
function into Mondrian-native code.

The OCIO/AgX resources are distributed under the following BSD 3-Clause
license notice, reproduced from Blender's `ocio-license.txt` distribution:

Copyright (c) 2003-2010 Sony Pictures Imageworks Inc., et al.
All Rights Reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

* Redistributions of source code must retain the above copyright notice,
  this list of conditions and the following disclaimer.
* Redistributions in binary form must reproduce the above copyright notice,
  this list of conditions and the following disclaimer in the documentation
  and/or other materials provided with the distribution.
* Neither the name of Sony Pictures Imageworks nor the names of its
  contributors may be used to endorse or promote products derived from this
  software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER OR CONTRIBUTORS BE
LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR
CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS
INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN
CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE
POSSIBILITY OF SUCH DAMAGE.
