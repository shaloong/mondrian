# Official OpenFX Basic Gain qualification fixture

This project builds the official Basic Gain example from OpenFX commit
`e40728885390ec16276d11e00025de9b4282060c` as an external `.ofx.bundle`.
The checkout and built binary stay outside the product and Git tree. The
example's upstream BSD-3-Clause license and source remain with its checkout.
The fixture has no Conan dependency; it compiles only `Examples/Basic/basic.cpp`
and uses the official headers and `Info.plist`.

```sh
git clone https://github.com/AcademySoftwareFoundation/openfx.git /path/to/openfx
git -C /path/to/openfx checkout e40728885390ec16276d11e00025de9b4282060c
cmake -S tests/openfx_reference -B /path/to/build \
  -DOPENFX_SOURCE_DIR=/path/to/openfx
cmake --build /path/to/build --config Release
```

On Windows, point `MONDRIAN_OPENFX_REFERENCE_BINARY` to
`/path/to/build/Basic.ofx.bundle/Contents/Win64/Basic.ofx`; on macOS use
`Contents/MacOS/Basic.ofx`; on Linux use
`Contents/Linux-x86-64/Basic.ofx`. Then run:

```sh
cargo test -p mondrian-app --test openfx_host \
  official_basic_filter_renders_authored_float32_pixels_in_child -- --ignored --nocapture
```

The test verifies pinned discovery, parameter description, native Float32
rendering, authored per-component gains on a later frame with a non-square
pixel aspect ratio, preservation of negative/HDR/alpha values, and equality
between direct host and compiled effect-graph output.
The result qualifies this example and this host subset only; other vendors,
contexts, pixel formats, and controls require separate evidence.
