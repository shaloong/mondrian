# Mondrian Standard SDR v1 resource notice

`mondrian_standard_sdr_rec709_v1.cube` is the AgX Base sRGB Formation LUT
from `EaryChow/AgX`, pinned to commit
`74852bd3ca64e6592c2f6315fda4bcb1f8821ac2`.

The vendored file is renamed but otherwise byte-for-byte identical to
`luts/AgX_Base_sRGB.cube` at that commit:

- Source Git blob: `5a02c49ef3926aec6cb4712b2421865c62908d59`
- SHA-256: `02f4d185608daa67fda01a1a48529bbc1533c8afdc826cde5c78f2eb5bb1b839`
- Authored input domain: FilmLight E-Gamut, log2 allocation from -10 to +15 stops
- LUT edge: 57
- Interpolation used by Mondrian: tetrahedral

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
