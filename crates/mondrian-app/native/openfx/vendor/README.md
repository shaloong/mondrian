# OpenFX HostSupport source

This directory contains OpenFX headers and HostSupport source from
AcademySoftwareFoundation/openfx commit
`e40728885390ec16276d11e00025de9b4282060c`, under the BSD-3-Clause
license in `LICENSE.md`. No third-party effect binary is shipped.

Mondrian changes to upstream source:

- Imported text has trailing whitespace removed. Git normalizes line endings.

- `host/src/ofxhBinary.cpp` opens selected Windows paths as UTF-8 through
  wide filesystem and loader calls.
- `host/include/ofxhPluginCache.h` and `host/src/ofxhPluginCache.cpp` remove
  the unused Expat XML cache reader. The selected-binary render path does
  not use the XML plugin cache, and OCIO already bundles Expat.

The product filter host is maintained separately in `../host`.
