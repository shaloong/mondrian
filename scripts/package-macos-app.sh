#!/usr/bin/env bash
set -euo pipefail

# Stage and verify the same private runtime locally and in the release workflow.
# The caller retains release provenance, licenses and FFmpeg source alongside it.
if [[ $# -lt 3 || $(uname -s) != Darwin ]]; then
  echo "Usage: bash scripts/package-macos-app.sh BINARY NEW_APP_DIR VERSION [LIBRARY_DIR ...]" >&2
  exit 2
fi
binary=$1
app_dir=$2
version=$3
shift 3
if [[ ! -f "$binary" || -e "$app_dir" || -L "$app_dir" ||
      ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "An existing binary, a new app directory and a numeric bundle version are required" >&2
  exit 2
fi
search_args=()
for directory in "$@"; do
  if [[ ! -d "$directory" ]]; then
    echo "Missing private library search directory: $directory" >&2
    exit 2
  fi
  search_args+=(-s "${directory%/}/")
done
executable_dir="$app_dir/Contents/MacOS"
frameworks_dir="$app_dir/Contents/Frameworks"
mkdir -p "$executable_dir" "$frameworks_dir"
cp "$binary" "$executable_dir/mondrian"
cp -L "$(command -v ffmpeg)" "$executable_dir/ffmpeg"
cp -L "$(command -v ffprobe)" "$executable_dir/ffprobe"
cat > "$app_dir/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleExecutable</key><string>mondrian</string>
  <key>CFBundleIdentifier</key><string>org.mondrian-studio.Mondrian</string>
  <key>CFBundleName</key><string>Mondrian</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
plutil -lint "$app_dir/Contents/Info.plist"

# -od recreates the destination: all three executables must share one invocation.
# An unresolved dependency must abort, never wait for interactive input in CI.
printf 'quit\n' | dylibbundler -od -b \
  -x "$executable_dir/mondrian" -x "$executable_dir/ffmpeg" \
  -x "$executable_dir/ffprobe" -d "$frameworks_dir" \
  -p '@executable_path/../Frameworks/' ${search_args[@]+"${search_args[@]}"}

# Check every rewritten edge. Build-host resolution cannot prove private closure.
while IFS= read -r image; do
  dependencies=$(otool -L "$image")
  while IFS= read -r dependency; do
    case "$dependency" in
      /usr/lib/*|/System/Library/*) ;;
      @executable_path/../Frameworks/*)
        relative="${dependency#@executable_path/../Frameworks/}"
        if [[ ! -f "$frameworks_dir/$relative" ]]; then
          echo "Missing private dylib: $dependency from $image" >&2
          exit 1
        fi
        ;;
      *)
        echo "Non-system dependency remains: $dependency from $image" >&2
        exit 1
        ;;
    esac
  done < <(printf '%s\n' "$dependencies" | tail -n +2 | awk '{print $1}')
  # Every edge is now absolute or explicitly relative to the executable. No
  # @rpath lookup remains. Bundler rewrites can collapse distinct build-host
  # RPATHs into duplicates, which dyld rejects before main on current macOS.
  # Remove the unused search paths instead of retaining ambient loader state.
  rpaths_changed=false
  while true; do
    load_commands=$(otool -l "$image")
    rpath=$(printf '%s\n' "$load_commands" | awk '
      $1 == "cmd" { in_rpath = ($2 == "LC_RPATH") }
      in_rpath && $1 == "path" && !found {
        sub(/^[[:space:]]*path /, "")
        sub(/ \(offset [0-9]+\)$/, "")
        print
        found = 1
      }')
    [[ -n "$rpath" ]] || break
    install_name_tool -delete_rpath "$rpath" "$image"
    rpaths_changed=true
  done
  if [[ "$rpaths_changed" == true ]]; then
    codesign --force --preserve-metadata=entitlements,requirements,flags,runtime \
      --sign - "$image"
  fi
  codesign --verify --strict "$image"
done < <(printf '%s\n' "$executable_dir/mondrian" \
  "$executable_dir/ffmpeg" "$executable_dir/ffprobe"; \
  find "$frameworks_dir" -type f -print)
codesign --force --preserve-metadata=entitlements,requirements,flags,runtime \
  --sign - "$app_dir"
codesign --verify --deep --strict "$app_dir"

# A clean environment removes developer PATH and DYLD overrides. Verification
# resolves the packaged sibling tools and exercises their production contract.
env -i PATH=/usr/bin:/bin HOME="$HOME" "$executable_dir/mondrian" --verify-runtime
