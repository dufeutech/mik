# hello-typescript

A WASI HTTP handler written in TypeScript.

## Prerequisites

```bash
npm install -g @bytecodealliance/jco @bytecodealliance/componentize-js
```

## Build

```bash
npm install
npm run build
```

This will:
1. Bundle TypeScript to JavaScript with esbuild
2. Componentize to WASM with jco

## Run

```bash
mik run hello-typescript.wasm
```

## Test

```bash
curl http://localhost:3000/run/hello-typescript/
# {"message":"Hello from TypeScript!","service":"hello-typescript","lang":"typescript"}
```

## Output Size

- ~12 MB (includes SpiderMonkey JS runtime)

## See Also

- [Building Components Guide](https://dufeutech.github.io/mik/guides/building-components/)
