#!/bin/bash
# Install benchmark tools for WSL (Ubuntu/Debian)
# Run with: wsl bash install-bench-tools.sh

set -e

echo "=== Installing benchmark tools for mik ==="
echo ""

# Update package list
echo "[1/4] Updating package list..."
sudo apt-get update

# Install build dependencies for wrk
echo "[2/4] Installing build dependencies..."
sudo apt-get install -y build-essential libssl-dev git unzip curl

# Install wrk
echo "[3/4] Installing wrk..."
if ! command -v wrk &> /dev/null; then
    cd /tmp
    git clone https://github.com/wg/wrk.git
    cd wrk
    make -j$(nproc)
    sudo cp wrk /usr/local/bin/
    cd ..
    rm -rf wrk
    echo "wrk installed successfully"
else
    echo "wrk already installed"
fi

# Install oha (pre-built binary)
echo "[4/4] Installing oha..."
if ! command -v oha &> /dev/null; then
    cd /tmp
    # Get latest oha release for Linux x86_64
    OHA_VERSION=$(curl -s https://api.github.com/repos/hatoo/oha/releases/latest | grep '"tag_name"' | cut -d'"' -f4)
    echo "Downloading oha ${OHA_VERSION}..."
    curl -LO "https://github.com/hatoo/oha/releases/download/${OHA_VERSION}/oha-linux-amd64"
    chmod +x oha-linux-amd64
    sudo mv oha-linux-amd64 /usr/local/bin/oha
    echo "oha installed successfully"
else
    echo "oha already installed"
fi

echo ""
echo "=== Installation complete ==="
echo ""
echo "Verify installations:"
echo "  wrk --version"
echo "  oha --version"
echo ""
echo "Run benchmarks (from Windows, with mik server running):"
echo "  wsl wrk -t4 -c100 -d30s http://localhost:3000/run/hello-world/"
echo "  wsl oha -n 10000 -c 100 http://localhost:3000/run/hello-world/"
