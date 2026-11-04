#!/usr/bin/env bash
# Builds the Rust library for the phone and generates its Kotlin bindings.
# Usage: build-rust.sh <jni libs dir> <kotlin output dir>
set -euo pipefail

ndk_version=30.0.16248370
android_api=33
target=aarch64-linux-android

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
sdk="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-$HOME/Android/Sdk}}"
bin="$sdk/ndk/$ndk_version/toolchains/llvm/prebuilt/linux-x86_64/bin"

if [[ ! -x "$bin/llvm-ar" ]]; then
    echo "NDK $ndk_version not found in $sdk" >&2
    exit 1
fi

export CC_aarch64_linux_android="$bin/aarch64-linux-android$android_api-clang"
export AR_aarch64_linux_android="$bin/llvm-ar"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$CC_aarch64_linux_android"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS="-C link-arg=-Wl,-z,max-page-size=16384"

cd "$root"
cargo build --release --target "$target" -p lis-ffi --lib

mkdir -p "$1/arm64-v8a"
cp "target/$target/release/liblis_ffi.so" "$1/arm64-v8a/"

# From a host build, as the phone's library has its symbols stripped
cargo build -p lis-ffi --lib
cargo run -q -p lis-ffi --bin uniffi-bindgen -- generate --no-format \
    --library target/debug/liblis_ffi.so --language kotlin --out-dir "$2"
