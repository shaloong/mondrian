#!/bin/sh
set -eu

jobs="${MONDRIAN_CMAKE_BUILD_JOBS:-${CMAKE_BUILD_PARALLEL_LEVEL:-1}}"
case "$jobs" in
    ''|0|*[!0-9]*)
        printf '%s\n' "invalid Mondrian CMake build job limit: $jobs" >&2
        exit 2
        ;;
esac

if [ -n "${MONDRIAN_NATIVE_MAKE_PROGRAM:-}" ]; then
    native_make="$MONDRIAN_NATIVE_MAKE_PROGRAM"
elif [ -x /usr/bin/gmake ]; then
    native_make=/usr/bin/gmake
elif [ -x /usr/bin/make ]; then
    native_make=/usr/bin/make
elif command -v gmake >/dev/null 2>&1; then
    native_make="$(command -v gmake)"
elif command -v make >/dev/null 2>&1; then
    native_make="$(command -v make)"
else
    printf '%s\n' 'Mondrian could not locate a native Make program' >&2
    exit 127
fi

# CMake's bare `--parallel` becomes a bare `-j` at the Make boundary. The last
# command-line job option wins, so this applies an exact upper bound even when
# the nested ExternalProject explicitly requested native default parallelism.
exec "$native_make" "$@" "-j$jobs"
