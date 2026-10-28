#!/usr/bin/env bash
set -euo pipefail

# One CLI/library runtime and precision-patch recipe for Darwin and Linux.
# Dependencies are supplied by the caller; this builder never installs them.
if [[ $# != 3 || ( "$3" != 1 && "$3" != 2 ) || "$1" != /* || "$2" != /* ]]; then
  echo "Usage: bash scripts/ffmpeg/build-unix-runtime.sh ABSOLUTE_WORK_DIR ABSOLUTE_PREFIX JOBS(1|2)" >&2
  exit 2
fi

platform_args=()
platform_ldflags=
case $(uname -s) in
  Darwin)
    default_cc=clang
    default_cxx=clang++
    platform_ldflags=-Wl,-headerpad_max_install_names
    platform_args=(--enable-videotoolbox --enable-audiotoolbox)
    ;;
  Linux)
    default_cc=cc
    default_cxx=c++
    # The development prefix owns the shared libraries used by its CLI tools.
    platform_args=(--enable-rpath)
    ;;
  *)
    echo "A Darwin or Linux host is required" >&2
    exit 2
    ;;
esac

work_dir=$1
prefix=$2
jobs=$3
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
version=8.1.2
case ${MONDRIAN_FFMPEG_SOURCE_KIND:-release} in
  release)
    archive_name="ffmpeg-$version.tar.xz"
    archive_sha256=464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c
    archive_url="https://ffmpeg.org/releases/$archive_name"
    ;;
  github)
    # Official mirror of the verified, signed n8.1.2 tag. Only the source
    # archive packaging differs from ffmpeg.org's release tarball.
    archive_name="ffmpeg-$version.tar.gz"
    archive_sha256=2ae7e42343cfffb811d15cfe98b6d005f082595fcdf034d30a4ff90cfed9f9c6
    archive_url=https://github.com/FFmpeg/FFmpeg/archive/38b88335f99e76ed89ff3c93f877fdefce736c13.tar.gz
    ;;
  *)
    echo "MONDRIAN_FFMPEG_SOURCE_KIND must be release or github" >&2
    exit 2
    ;;
esac

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$@"
  else
    shasum -a 256 "$@"
  fi
}

archive="$work_dir/$archive_name"
mkdir -p "$work_dir" "$prefix"
if [[ ! -f "$archive" ]]; then
  curl --fail --show-error --location --proto '=https' --retry 3 \
    --connect-timeout 20 --max-time 180 "$archive_url" -o "$archive.part"
  mv "$archive.part" "$archive"
fi
printf '%s  %s\n' "$archive_sha256" "$archive" | sha256 -c

source_dir=$(mktemp -d "$work_dir/ffmpeg-source.XXXXXX")
tar -xf "$archive" -C "$source_dir" --strip-components=1
# GitHub archives omit VERSION. Mirror release packaging so version.sh cannot
# inherit the enclosing Mondrian checkout's Git revision instead of FFmpeg's.
[[ $(cat "$source_dir/RELEASE") == "$version" ]]
printf '%s\n' "$version" > "$source_dir/VERSION"
[[ $(sh "$source_dir/ffbuild/version.sh" "$source_dir") == "$version" ]]
patch_file="$script_dir/$version-prores12.patch"
rgba_patch_file="$script_dir/$version-rgba64-signed-chroma.patch"
patch --directory="$source_dir" --strip=1 --fuzz=0 < "$patch_file"
patch --directory="$source_dir" --strip=1 --fuzz=0 < "$rgba_patch_file"
cd "$source_dir"
./configure \
  --prefix="$prefix" --extra-version=mondrian-prores12 \
  --cc="${CC:-$default_cc}" --cxx="${CXX:-$default_cxx}" \
  --pkg-config="${PKG_CONFIG:-pkg-config}" \
  --extra-cflags="${CFLAGS:-}" \
  --extra-ldflags="${LDFLAGS:-} $platform_ldflags" \
  --enable-gpl --enable-version3 --enable-shared --disable-static --enable-pic \
  --disable-doc --disable-ffplay \
  --enable-libx264 --enable-libx265 --enable-libaom --enable-libdav1d --enable-libopenjpeg \
  --enable-libfreetype --enable-libharfbuzz --enable-libfontconfig --enable-libass \
  --enable-openssl --enable-zlib "${platform_args[@]}"
make -j "$jobs"
make install

# Retain the current corresponding source and real build inputs with the runtime.
source_package="$prefix/share/mondrian-ffmpeg-source"
mkdir -p "$source_package/scripts/ffmpeg"
cp "$archive" ffbuild/config.log LICENSE.md COPYING.GPLv3 "$source_package/"
cp "$patch_file" "$rgba_patch_file" "$script_dir/build-unix-runtime.sh" \
  "$source_package/scripts/ffmpeg/"
cp "$script_dir/../build-macos-ffmpeg.sh" "$script_dir/../build-linux-ffmpeg.sh" \
  "$source_package/scripts/"
(cd "$source_package" && sha256 \
  "$archive_name" "scripts/ffmpeg/$version-prores12.patch" \
  "scripts/ffmpeg/$version-rgba64-signed-chroma.patch" \
  scripts/ffmpeg/build-unix-runtime.sh scripts/build-macos-ffmpeg.sh scripts/build-linux-ffmpeg.sh \
  config.log LICENSE.md COPYING.GPLv3 > SHA256SUMS)
cd "$work_dir"
[[ "$source_dir" == "$work_dir"/ffmpeg-source.* && -d "$source_dir" ]]
rm -rf "$source_dir"
