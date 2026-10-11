# Official CLAP 1.2.10 ABI oracle

Build `latest-sdk-abi.cpp` independently with the actual native C++ compiler and
`-I vendor/clap-sys/official/include`. Record full compiler path/version/hash,
argv, transcript, unchanged official source receipt and executable/output hashes.
For GCC/Clang the source expects C++17 (`-std=c++17`); for MSVC use `/std:c++17`.
Execute that exact binary and preserve its JSON stdout as a new receipt.

Set `MONDRIAN_CLAP_LATEST_ABI_REPORT` to that report and run the ignored root
`clap_latest_full_abi_matches_native_headers` test with one test thread.
The source has 228 exact C++ callback signature assertions. Separate Rust field
assignments bind that same immutable inventory to the actual canonical package.
The runtime report compares 137 named structs, every declared field (including
all five promoted anonymous window members), every generated named C alias,
295 numeric/string constants and the three header-local version/color helpers.
Strings are emitted as hex; unsigned 64-bit values retain unsigned JSON values.

Native C++ enum identity is preserved in expected signatures. Alias extent and
scalar class are checked separately against actual Rust. Integer C enum carriers
may have platform/compiler signedness distinct from the parser; this does not
permit changing a fixed signed/unsigned scalar alias or a pointer/float/record.
Every defined enum constant is compared with its native value.

Windows generation does not qualify macOS/Linux native execution. Each platform
must compile this oracle and execute its matching Rust test separately, including
the actual `c_ulong`/pointer widths. A layout/signature PASS is not a plugin
lifecycle, host extension, native editor, vendor or realtime deadline PASS.
