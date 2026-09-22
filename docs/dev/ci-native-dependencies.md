# CI native dependencies

Linux title integration tests require the authored default family `Noto Sans CJK SC`.
The CI Test (Linux) and App UI jobs install `fonts-noto-cjk` and `fontconfig` from
Ubuntu's package repositories. `verify-linux-title-font.sh` checks the exact family
in the installed catalog (not a fallback match), reports the package version, and
requires the package copyright file. Missing fonts fail before the tests.

Noto Sans CJK is licensed under SIL Open Font License 1.1; see the upstream
[license](https://github.com/notofonts/noto-cjk/blob/main/Sans/LICENSE).
The distribution package retains copyright and license notices under
`/usr/share/doc/fonts-noto-cjk/copyright`. This setup installs an unmodified font
only on the CI host. No font binaries are copied into this repository or release
packages, and no Microsoft or Apple system font is redistributed. Future font
bundling must separately retain the exact font's copyright and OFL notices.
The production title policy and missing-font error remain unchanged (ADR 0007).
These tests exercise title semantics; they are not cross-machine pixel goldens.

macOS CI and Release select Homebrew `ffmpeg@8`, including its keg-only CLI and
pkg-config directory, and verify libavcodec major 62 before building. Unversioned
Homebrew FFmpeg can advance to an incompatible major while Rust bindings remain
on 8.x. This fixes the ABI major, not the complete native package identity;
release runtime/license admission still applies. The existing Homebrew GPL build
is not asserted to satisfy a different product distribution license profile.

Media uses `AV_PROFILE_*`, present in the minimum supported FFmpeg 6.1 headers,
for ProRes/DNxHR probe mapping. Removed `FF_PROFILE_*` compatibility aliases are
not part of the supported build contract. The profile mapping regression covers
all six ProRes variants and all five DNxHR profiles.
