#!/bin/sh
set -eu

# Install cargo-udeps with fallback from binary to cargo install
CARGO_UDEPS_VERSION="${CARGO_UDEPS_VERSION:-0.1.60}"
CARGO_BIN_DIR="${CARGO_HOME:-$HOME/.cargo}/bin"

# Ensure cargo bin directory is in PATH and exists
export PATH="$CARGO_BIN_DIR:$PATH"
mkdir -p "$CARGO_BIN_DIR"

# Check if already installed and working
if command -v cargo-udeps >/dev/null 2>&1; then
    if cargo udeps --version >/dev/null 2>&1; then
        echo "cargo-udeps is already installed and working"
        exit 0
    else
        echo "cargo-udeps exists but not working, reinstalling..."
        rm -f "$CARGO_BIN_DIR/cargo-udeps"
    fi
fi

echo "Installing cargo-udeps v$CARGO_UDEPS_VERSION"

# Try binary first, fallback to cargo install if it fails
try_binary_install() {
    echo "Attempting binary installation..."
    TEMP_DIR=$(mktemp -d)
    cd "$TEMP_DIR"
    
    ARCH="x86_64-unknown-linux-musl"
    URL="https://github.com/est31/cargo-udeps/releases/download/v$CARGO_UDEPS_VERSION/cargo-udeps-v$CARGO_UDEPS_VERSION-$ARCH.tar.gz"
    
    if wget -q -O cargo-udeps.tar.gz "$URL" && tar -xzf cargo-udeps.tar.gz; then
        chmod +x "cargo-udeps-v$CARGO_UDEPS_VERSION-$ARCH/cargo-udeps"
        mv "cargo-udeps-v$CARGO_UDEPS_VERSION-$ARCH/cargo-udeps" "$CARGO_BIN_DIR/"
        
        cd - >/dev/null
        rm -rf "$TEMP_DIR"
        
        # Test if it works
        echo "Testing binary installation..."
        echo "cargo-udeps binary: $(which cargo-udeps)"
        echo "cargo --list | grep udeps:"
        cargo --list | grep udeps || echo "No 'udeps' found in cargo commands"
        
        if cargo udeps --version >/dev/null 2>&1; then
            echo "✓ Binary installation successful"
            return 0
        else
            echo "✗ Binary doesn't work as cargo subcommand"
            echo "Standalone cargo-udeps test:"
            if cargo-udeps --version >/dev/null 2>&1; then
                echo "cargo-udeps standalone works, but cargo udeps doesn't"
            else
                echo "cargo-udeps standalone also fails"
            fi
            rm -f "$CARGO_BIN_DIR/cargo-udeps"
            return 1
        fi
    else
        cd - >/dev/null
        rm -rf "$TEMP_DIR"
        return 1
    fi
}

# Try binary installation, fallback to cargo install
if ! try_binary_install; then
    echo "Binary installation failed, using cargo install..."
    # cargo-udeps requires nightly
    rustup toolchain install nightly
    cargo +nightly install --force --version "=$CARGO_UDEPS_VERSION" cargo-udeps
fi

echo "✓ cargo-udeps installation complete"
cargo udeps --version
