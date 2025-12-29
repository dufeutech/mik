# hello-js

WASI HTTP component in JavaScript using jco/ComponentizeJS.

## Prerequisites

```bash
npm install @bytecodealliance/jco @bytecodealliance/componentize-js
```

## Build

```bash
# Get WASI HTTP 0.2.0 WIT
git clone --depth 1 --branch v0.2.0 https://github.com/WebAssembly/wasi-http.git
cp -r wasi-http/wit .

# Build
mkdir -p dist
npx jco componentize app.js --wit wit --world-name proxy -o dist/hello-js.wasm
```

## Run

```bash
mik run dist/hello-js.wasm
curl http://localhost:3000/run/hello-js/
# {"message":"Hello from JavaScript!","lang":"javascript","path":"/"}
```

## Size

JavaScript components are ~12MB due to the embedded SpiderMonkey engine.
