#!/usr/bin/env bash
# Build the confluxmap companion plugin for Pumpkin (wasm32-wasip2).
#
# Why this script exists: under Git Bash, GNU coreutils' `link` shadows MSVC's
# `link.exe` on PATH, so rustc's link step dies with
# "link: extra operand '...rcgu.o'". Re-creating the MSVC x64 environment first
# puts the correct linker and the Windows SDK import libraries back in front.
#
# Usage: ./build.sh [extra cargo args]
set -euo pipefail

MSVC_BASE="/c/Program Files/Microsoft Visual Studio/2022/Community/VC/Tools/MSVC"
SDK_BASE="/c/Program Files (x86)/Windows Kits/10"

# Newest installed toolset / SDK wins; the literals are only a fallback for the
# machine this project was developed on.
if [ -d "$MSVC_BASE" ]; then
    msvc_ver="$(ls "$MSVC_BASE" | sort -V | tail -1)"
else
    msvc_ver="14.44.35207"
fi
if [ -d "$SDK_BASE/Include" ]; then
    sdk_ver="$(ls "$SDK_BASE/Include" | sort -V | tail -1)"
else
    sdk_ver="10.0.26100.0"
fi

MSVC_WIN="C:\\Program Files\\Microsoft Visual Studio\\2022\\Community\\VC\\Tools\\MSVC\\${msvc_ver}"
SDK_WIN="C:\\Program Files (x86)\\Windows Kits\\10"

export PATH="$HOME/.cargo/bin:$MSVC_BASE/$msvc_ver/bin/Hostx64/x64:$SDK_BASE/bin/$sdk_ver/x64:$PATH"
export LIB="${MSVC_WIN}\\lib\\x64;${SDK_WIN}\\Lib\\${sdk_ver}\\ucrt\\x64;${SDK_WIN}\\Lib\\${sdk_ver}\\um\\x64"
export INCLUDE="${MSVC_WIN}\\include;${SDK_WIN}\\Include\\${sdk_ver}\\ucrt;${SDK_WIN}\\Include\\${sdk_ver}\\um;${SDK_WIN}\\Include\\${sdk_ver}\\shared;${SDK_WIN}\\Include\\${sdk_ver}\\winrt"

cd "$(dirname "$0")"
cargo build --release "$@"

echo
echo "=== artifact ==="
ls -la target/wasm32-wasip2/release/*.wasm
