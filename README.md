# lume-on_this_day

Historical events from Wikipedia's On This Day.

## Preview

![Slide Preview](art/j-card.svg)

## Usage

Build the slide:

```bash
./build.sh
```

Produces `on_this_day.vzglyd` — a packaged slide ready to be placed in your VZGLYD slides directory.

## Sidecar

This slide consumes data from [`lume-on_this_day-sidecar`](https://github.com/lume-industries/lume-on_this_day-sidecar).
The sidecar fetches data, parses it, and delivers JSON payloads via the VZGLYD sidecar channel ABI.
Multiple visual slide designs can share this same sidecar.

## Requirements

- Rust stable with `wasm32-wasip1` target: `rustup target add wasm32-wasip1`

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
