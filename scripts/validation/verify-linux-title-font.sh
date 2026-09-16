#!/usr/bin/env bash
# Check the declared family, not fontconfig's potentially substituted match.
set -euo pipefail
family='Noto Sans CJK SC'
if ! fc-list --format='%{family}\n' | tr ',' '\n' | grep -Fx "$family"; then
    echo "Missing required title font: $family (install fonts-noto-cjk)." >&2
    exit 1
fi
# Record package provenance. The distribution owns the installed copyright file.
dpkg-query -W -f='${Package} ${Version}\n' fonts-noto-cjk
test -s /usr/share/doc/fonts-noto-cjk/copyright
