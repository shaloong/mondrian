#!/usr/bin/env bash
set -euo pipefail

# Preserve the existing macOS entrypoint and its release-source default.
if [[ $(uname -s) != Darwin ]]; then
  echo "A macOS host is required" >&2
  exit 2
fi
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
exec bash "$script_dir/ffmpeg/build-unix-runtime.sh" "$@"
