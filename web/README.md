# ferrisboy-web

The WebAssembly browser frontend for the ferrisboy Game Boy emulator. Open a
page, drop in a `.gb` ROM, and play — with picture, sound, and keyboard input,
zero install.

## Build

Requires the `wasm32-unknown-unknown` target and
[`wasm-pack`](https://rustwasm.github.io/wasm-pack/) (`cargo install wasm-pack`).

From the workspace root:

```sh
wasm-pack build web --target web --out-dir pkg --release
```

This produces `web/pkg/ferrisboy_web.js` (the JS glue) and
`web/pkg/ferrisboy_web_bg.wasm` (the compiled core).

## Run

ES modules and WebAssembly will **not** load over `file://` — you must serve the
`web/` directory over HTTP. The simplest option:

```sh
cd web
python3 -m http.server 8080
```

Then open <http://localhost:8080/> and drag in (or pick) a `.gb` ROM.

## Controls

| Key                         | Button   |
| --------------------------- | -------- |
| Arrow keys                  | D-pad    |
| `Z`                         | A        |
| `X`                         | B        |
| `Enter`                     | Start    |
| `Shift`                     | Select   |

Sound starts on your first key press or click (browsers block audio until a
user gesture).

## Saves

Battery-backed cartridge RAM is persisted automatically to `localStorage`,
keyed by the ROM's internal title (`ferrisboy.save.<TITLE>`). It is written
about once a second while dirty and on tab close, and restored automatically the
next time you load the same ROM in the same browser.
