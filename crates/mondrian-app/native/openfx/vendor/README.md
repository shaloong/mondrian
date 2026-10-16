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

- `host/src/ofxhHost.cpp` marks Host identity and the private Host pointer
  read-only to plugins; host-internal construction still assigns their values.

The product filter host is maintained separately in `../host`.

- `host/src/ofxhParam.cpp` registers the OFX scalar Integer/Double default
  interpolation property, including the normative Linear default.

- Numeric interpolation metadata is read-only on parameter instances.
  `host/src/ofxhPropertySuite.cpp` enforces existing read-only flags for scalar,
  array and reset operations at the plugin suite boundary.
  Its typed property constructor also forwards the declared `pluginReadOnly`
  flag to the base property; host-internal property writes remain available.
