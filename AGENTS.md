# la1010: agent notes

CLI + library for the Kingst LA1010 USB logic analyzer: capture, measure, export, decode
UART / I2C / SPI / USB LS/FS. Rust 2024, MIT.

## Commands
- Build/test: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`
- Hardware tests (ask the user first): `cargo test --test hw -- --ignored --test-threads=1`
- Network test (downloads ~29 MB): `cargo test --test net -- --ignored`

## Layout
- `src/stream.rs` USB words -> `Edge { sample, ch, level }` (sorted; initial levels at sample 0).
  Everything downstream works on edges.
- `src/window.rs` software trigger + capture length; `src/pipeline.rs` fan-out to decoders,
  measure, file writer, printer.
- `src/decode/*` decoders implement `Decoder { feed, finish }` and must give identical output
  for any batch split (each has a batch-invariance test). Register new ones in `decode/mod.rs`
  (`catalog`, `build`, `recommended_rate`) and add a `testgen` generator.
- `src/device.rs` protocol over the `Transport` trait (mock-tested); `src/usb.rs` nusb impl;
  `src/capture.rs` streaming loop (halts on every exit path).
- `src/firmware/*` extraction from KingstVIS; vendor blobs are never committed or embedded.
- Spec: `docs/superpowers/specs/2026-11-18-la1010-cli-design.md`;
  measurements: `docs/hardware-validation.md`.

## Rules
- Do not copy sigrok code (GPL); the protocol facts are in the spec.
- Exit codes 0/1/2/3 and NDJSON key order are a public interface.
- Never enable LA1010 PWM outputs or a scope pattern generator without the user's OK;
  restore instrument state after hardware tests.
