#!/usr/bin/env bash
set -euo pipefail

# Build one shared CLI/library runtime. Callers supply development dependencies
# through PKG_CONFIG_PATH, CFLAGS and LDFLAGS; this script never installs them.
if [[ $# != 3 || ( "$3" != 1 && "$3" != 2 ) ]]; then
  echo "Usage: bash scripts/build-macos-ffmpeg.sh ABSOLUTE_WORK_DIR ABSOLUTE_PREFIX JOBS(1|2)" >&2
  exit 2
fi
if [[ $(uname -s) != Darwin || "$1" != /* || "$2" != /* ]]; then
  echo "A macOS host and absolute work/install paths are required" >&2
  exit 2
fi

work_dir=$1
prefix=$2
jobs=$3
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
version=8.1.2
archive="$work_dir/ffmpeg-$version.tar.xz"
archive_sha256=464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c
mkdir -p "$work_dir" "$prefix"
if [[ ! -f "$archive" ]]; then
  curl --fail --show-error --location --proto '=https' --retry 3 \
    "https://ffmpeg.org/releases/ffmpeg-$version.tar.xz" -o "$archive"
fi
printf '%s  %s\n' "$archive_sha256" "$archive" | shasum -a 256 --check

# Each attempt owns a fresh source directory; never mutate a caller's checkout.
source_dir=$(mktemp -d "$work_dir/ffmpeg-source.XXXXXX")
tar -xf "$archive" -C "$source_dir" --strip-components=1
patch_file="$script_dir/ffmpeg/$version-prores12.patch"
rgba_patch_file="$script_dir/ffmpeg/$version-rgba64-signed-chroma.patch"
patch --directory="$source_dir" --strip=1 --fuzz=0 < "$patch_file"
patch --directory="$source_dir" --strip=1 --fuzz=0 < "$rgba_patch_file"
cd "$source_dir"
./configure \
  --prefix="$prefix" --extra-version=mondrian-prores12 \
  --cc="${CC:-clang}" --cxx="${CXX:-clang++}" \
  --pkg-config="${PKG_CONFIG:-pkg-config}" \
  --extra-cflags="${CFLAGS:-}" \
  --extra-ldflags="${LDFLAGS:-} -Wl,-headerpad_max_install_names" \
  --enable-gpl --enable-version3 --enable-shared --disable-static --enable-pic \
  --disable-doc --disable-ffplay \
  --enable-libx264 --enable-libx265 --enable-libaom --enable-libdav1d --enable-libopenjpeg \
  --enable-libfreetype --enable-libharfbuzz --enable-libfontconfig --enable-libass \
  --enable-openssl --enable-zlib --enable-videotoolbox --enable-audiotoolbox
make -j "$jobs"
make install

# Keep the exact source, modifications and configuration with the runtime.
source_package="$prefix/share/mondrian-ffmpeg-source"
mkdir -p "$source_package"
cp "$archive" "$patch_file" "$rgba_patch_file" "$script_dir/build-macos-ffmpeg.sh" ffbuild/config.log "$source_package/"
cp LICENSE.md COPYING.GPLv3 "$source_package/"
(cd "$source_package" && shasum -a 256 \
  "ffmpeg-$version.tar.xz" "$version-prores12.patch" "$version-rgba64-signed-chroma.patch" build-macos-ffmpeg.sh \
  config.log LICENSE.md COPYING.GPLv3 > SHA256SUMS)
cd "$work_dir"
rm -rf "$source_dir"
