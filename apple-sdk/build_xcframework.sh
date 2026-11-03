#!/usr/bin/env bash
set -euo pipefail

export PATH="$HOME/.asdf/shims:$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-15.0}"
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-14.0}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
HEADERS_DIR="$SCRIPT_DIR/include"
OUTPUT_XCFRAMEWORK="$SCRIPT_DIR/RuffleApple.xcframework"

PROFILE="${RUFFLE_APPLE_PROFILE:-release}"
RUN_TESTS=0
TARGETS=()

for arg in "$@"; do
    case "$arg" in
        --dev)
            PROFILE="dev"
            ;;
        --release)
            PROFILE="release"
            ;;
        --test)
            RUN_TESTS=1
            ;;
        *)
            TARGETS+=("$arg")
            ;;
    esac
done

if [[ "$RUN_TESTS" -eq 1 ]]; then
    echo "Running ruffle_apple_sdk Rust unit tests..."
    cargo test --manifest-path "$REPO_ROOT/Cargo.toml" -p ruffle_apple_sdk
    exit 0
fi

CARGO_PROFILE_FLAG="--release"
TARGET_SUBDIR="release"
if [[ "$PROFILE" == "dev" ]]; then
    CARGO_PROFILE_FLAG=""
    TARGET_SUBDIR="debug"
fi

# Default targets: iOS device, iOS Simulator (Apple Silicon), and macOS (Apple Silicon)
if [[ ${#TARGETS[@]} -eq 0 ]]; then
    TARGETS=(
        "aarch64-apple-ios"
        "aarch64-apple-ios-sim"
        "aarch64-apple-darwin"
    )
fi

SYSROOT="$(rustc --print sysroot)"
RUST_VER="$(rustc --version | awk '{print $2}')"
XCFRAMEWORK_ARGS=()

install_rust_std_without_rustup() {
    local target="$1"
    local tmp_dir
    tmp_dir="$(mktemp -d)"
    local url="https://static.rust-lang.org/dist/rust-std-${RUST_VER}-${target}.tar.xz"
    echo "Downloading rust-std (${RUST_VER}) for ${target} into ${SYSROOT}..."
    if curl -fsSL "$url" -o "$tmp_dir/rust-std.tar.xz"; then
        tar -xf "$tmp_dir/rust-std.tar.xz" -C "$tmp_dir"
        "$tmp_dir/rust-std-${RUST_VER}-${target}/install.sh" --prefix="$SYSROOT" >/dev/null
        rm -rf "$tmp_dir"
        return 0
    else
        rm -rf "$tmp_dir"
        return 1
    fi
}

for TARGET in "${TARGETS[@]}"; do
    TARGET_LIB_DIR="$SYSROOT/lib/rustlib/$TARGET/lib"
    if [[ ! -d "$TARGET_LIB_DIR" ]]; then
        if command -v rustup >/dev/null 2>&1; then
            echo "Installing missing Rust target via rustup: $TARGET"
            rustup target add "$TARGET"
        elif ! install_rust_std_without_rustup "$TARGET"; then
            echo "Skipping $TARGET (could not install target stdlib for $RUST_VER)"
            continue
        fi
    fi

    echo "Building ruffle_apple_sdk ($PROFILE) for $TARGET..."
    if [[ -n "$CARGO_PROFILE_FLAG" ]]; then
        cargo build --manifest-path "$REPO_ROOT/Cargo.toml" -p ruffle_apple_sdk --target "$TARGET" "$CARGO_PROFILE_FLAG"
    else
        cargo build --manifest-path "$REPO_ROOT/Cargo.toml" -p ruffle_apple_sdk --target "$TARGET"
    fi

    LIB_PATH="$REPO_ROOT/target/$TARGET/$TARGET_SUBDIR/libruffle_apple_sdk.a"
    XCFRAMEWORK_ARGS+=("-library" "$LIB_PATH" "-headers" "$HEADERS_DIR")
done

if [[ ${#XCFRAMEWORK_ARGS[@]} -eq 0 ]]; then
    echo "Error: No targets were built."
    exit 1
fi

rm -rf "$OUTPUT_XCFRAMEWORK"
echo "Creating $OUTPUT_XCFRAMEWORK..."
xcodebuild -create-xcframework "${XCFRAMEWORK_ARGS[@]}" -output "$OUTPUT_XCFRAMEWORK"
touch "$SCRIPT_DIR/FlashPlayerKit/Package.swift"
echo "Successfully created $OUTPUT_XCFRAMEWORK"
