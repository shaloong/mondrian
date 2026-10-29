#!/usr/bin/env bash
set -euo pipefail

if [[ $(uname -s) != Linux ]]; then
  echo "A Linux host is required" >&2
  exit 2
fi
# The official signed-tag mirror also works where ffmpeg.org is unreachable.
export MONDRIAN_FFMPEG_SOURCE_KIND=${MONDRIAN_FFMPEG_SOURCE_KIND:-github}
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
exec bash "$script_dir/ffmpeg/build-unix-runtime.sh" "$@"
