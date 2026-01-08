#!/bin/sh
set -eu

# Install cargo-make with fallback from binary to cargo install
CARGO_MAKE_VERSION="${CARGO_MAKE_VERSION:-0.37.24}"
CARGO_BIN_DIR="${CARGO_HOME:-$HOME/.cargo}/bin"

# Ensure cargo bin directory is in PATH and exists
export PATH="$CARGO_BIN_DIR:$PATH"
mkdir -p "$CARGO_BIN_DIR"

# Check if already installed and working
if command -v cargo-make >/dev/null 2>&1; then
    if cargo make --version >/dev/null 2>&1; then
        echo "cargo-make is already installed and working"
        exit 0
    else
        echo "cargo-make exists but not working, reinstalling..."
        rm -f "$CARGO_BIN_DIR/cargo-make"
    fi
fi

echo "Installing cargo-make v$CARGO_MAKE_VERSION"

# Try binary first, fallback to cargo install if it fails
try_binary_install() {
    echo "Attempting binary installation..."
    TEMP_DIR=$(mktemp -d)
    cd "$TEMP_DIR"
    
    ARCH="x86_64-unknown-linux-musl"
    URL="https://github.com/sagiegurari/cargo-make/releases/download/$CARGO_MAKE_VERSION/cargo-make-v$CARGO_MAKE_VERSION-$ARCH.zip"
    
    if wget -q -O cargo-make.zip "$URL" && unzip -q cargo-make.zip; then
        chmod +x "cargo-make-v$CARGO_MAKE_VERSION-$ARCH/cargo-make"
        mv "cargo-make-v$CARGO_MAKE_VERSION-$ARCH/cargo-make" "$CARGO_BIN_DIR/"
        
        cd - >/dev/null
        rm -rf "$TEMP_DIR"
        
        # Test if it works
        echo "Testing binary installation..."
        echo "cargo-make binary: $(which cargo-make)"
        echo "cargo --list | grep make:"
        cargo --list | grep make || echo "No 'make' found in cargo commands"
        
        if cargo make --version >/dev/null 2>&1; then
            echo "✓ Binary installation successful"
            return 0
        else
            echo "✗ Binary doesn't work as cargo subcommand"
            echo "Standalone cargo-make test:"
            if cargo-make --version >/dev/null 2>&1; then
                echo "cargo-make standalone works, but cargo make doesn't"
            else
                echo "cargo-make standalone also fails"
            fi
            rm -f "$CARGO_BIN_DIR/cargo-make"
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
    cargo install --force --version "=$CARGO_MAKE_VERSION" cargo-make
fi

echo "✓ cargo-make installation complete"
cargo make --version
