# hello-go

WASI HTTP component in Go using TinyGo.

## Prerequisites

```bash
# Install TinyGo from https://tinygo.org/getting-started/install/
go mod download
```

## Build

```bash
# Get WIT from SDK
WIT_DIR=$(go list -m -f '{{.Dir}}' github.com/rajatjindal/wasi-go-sdk)/wit

# Build
mkdir -p dist
tinygo build -target=wasip2 --wit-package "$WIT_DIR" --wit-world sdk -o dist/hello-go.wasm .
```

## Run

```bash
mik run dist/hello-go.wasm
curl http://localhost:3000/run/hello-go/
# {"message":"Hello from Go!","lang":"go","path":"/"}
```

## Size

Go components are ~1.3MB (stripped ~304KB).
