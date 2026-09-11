#!/usr/bin/env bash
# Run the unit tests.
#
# The crate is a wasm cdylib, but the protocol and config modules are pure Rust
# with no host calls, and the API crate's generated bindings compile for native
# targets too. That means the tests run on the host - including the byte-for-byte
# comparison against the vector the reference Java encoder produced.
#
# This script re-creates the MSVC environment for the same reason build.sh does.
set -euo pipefail

MSVC_BASE="/c/Program Files/Microsoft Visual Studio/2022/Community/VC/Tools/MSVC"
SDK_BASE="/c/Program Files (x86)/Windows Kits/10"

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

export PATH="$HOME/.cargo/bin:$MSVC_BASE/$msvc_ver/bin/Hostx64/x64:$SDK_BASE/bin/$sdk_ver/x64:$PATH"
export LIB="C:\\Program Files\\Microsoft Visual Studio\\2022\\Community\\VC\\Tools\\MSVC\\${msvc_ver}\\lib\\x64;C:\\Program Files (x86)\\Windows Kits\\10\\Lib\\${sdk_ver}\\ucrt\\x64;C:\\Program Files (x86)\\Windows Kits\\10\\Lib\\${sdk_ver}\\um\\x64"
export INCLUDE="C:\\Program Files\\Microsoft Visual Studio\\2022\\Community\\VC\\Tools\\MSVC\\${msvc_ver}\\include;C:\\Program Files (x86)\\Windows Kits\\10\\Include\\${sdk_ver}\\ucrt;C:\\Program Files (x86)\\Windows Kits\\10\\Include\\${sdk_ver}\\um;C:\\Program Files (x86)\\Windows Kits\\10\\Include\\${sdk_ver}\\shared;C:\\Program Files (x86)\\Windows Kits\\10\\Include\\${sdk_ver}\\winrt"

cd "$(dirname "$0")"
cargo test --target x86_64-pc-windows-msvc "$@"
