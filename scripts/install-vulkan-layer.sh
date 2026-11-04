#!/usr/bin/env bash
# Install, remove, or print environment for the opt-in XRizer Vulkan layer.
#
# The layer is never installed system-wide and requires no root privileges.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST_NAME="xrizer_upscaler_layer.json"
LIB_NAME="libxrizer_upscaler_layer.so"

usage() {
    cat <<EOF
Usage: $(basename "$0") [--user | --remove | --print-env]

  --user       install the layer under \$XDG_DATA_HOME (no root)
  --remove     remove the files installed by --user
  --print-env  print the portable environment for Steam/WiVRn

The layer is enabled only when XRIZER_UPSCALER_VK_LAYER=1 is set.
EOF
}

find_lib() {
    for candidate in \
        "${XRIZER_LAYER_LIB:-}" \
        "$ROOT/target/release/$LIB_NAME" \
        "$ROOT/target/debug/$LIB_NAME"; do
        if [ -n "$candidate" ] && [ -f "$candidate" ]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    echo "could not find $LIB_NAME; run cargo build -p xrizer-vulkan-layer first" >&2
    return 1
}

print_env() {
    local layer_dir="$ROOT/target/debug/vulkan/implicit_layer.d"
    if [ ! -f "$layer_dir/$MANIFEST_NAME" ]; then
        layer_dir="$ROOT/target/release/vulkan/implicit_layer.d"
    fi
    echo "XRIZER_UPSCALER_VK_LAYER=1"
    echo "VK_ADD_IMPLICIT_LAYER_PATH=$layer_dir"
    echo "# Alternatively install the manifest with: $0 --user"
}

install_user() {
    local lib
    lib="$(find_lib)"
    local data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
    local lib_dir="$data_home/xrizer/vulkan-layer"
    local manifest_dir="$data_home/vulkan/implicit_layer.d"

    install -Dm755 "$lib" "$lib_dir/$LIB_NAME"
    install -d "$manifest_dir"
    sed "s|\"./libxrizer_upscaler_layer.so\"|\"$lib_dir/$LIB_NAME\"|" \
        "$ROOT/vulkan-layer/manifest.json.in" > "$manifest_dir/$MANIFEST_NAME"
    echo "installed layer to $lib_dir"
    echo "installed manifest to $manifest_dir/$MANIFEST_NAME"
    echo "enable with: XRIZER_UPSCALER_VK_LAYER=1"
}

remove_user() {
    local data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
    local lib_dir="$data_home/xrizer/vulkan-layer"
    local manifest_dir="$data_home/vulkan/implicit_layer.d"
    rm -f "$manifest_dir/$MANIFEST_NAME" "$lib_dir/$LIB_NAME"
    rmdir "$lib_dir" 2>/dev/null || true
    echo "removed XRizer Vulkan layer files"
}

case "${1:-}" in
    --user) install_user ;;
    --remove) remove_user ;;
    --print-env) print_env ;;
    -h|--help|"") usage ;;
    *) usage; exit 1 ;;
esac
