---
name: la1010
description: Use when capturing, measuring or decoding digital signals (UART, I2C, SPI, USB low/full speed) with the Kingst LA1010 logic analyzer via the la1010 CLI.
---

# Kingst LA1010 logic analyzer (`la1010` CLI)

16 inputs, streaming only, max 100 MSa/s. Input only: this tool never drives outputs.

## First use
- `la1010 firmware fetch` once (downloads KingstVIS from qdkingst.com, installs 4 files).
- `la1010 info --json` → model, revision, firmware state. Exit 2 = not connected or firmware missing.

## Always
- Use `--json` and parse NDJSON: keys `t` (s, 0 = trigger), `sample`, `proto`, `kind`, fields, `text`.
- Every run is bounded: capture/measure default 1 s; monitor default `--timeout 10s`.
- Exit: 0 ok, 1 usage (fix args), 2 device/firmware/IO, 3 trigger/`--until`/`--count` not reached.
- Threshold `-t` default 1.4 V (fine for 1.8–5 V logic); use `-t 0.9` for 1.8 V-only buses.
- Keep rate × channels ≤ 320 Mbit/s (e.g. 3 ch @ 100M, 8 ch @ 40M, 16 ch @ 20M); above that the
  capture fails with "stream stalled" (exit 2). The CLI warns before starting.

## Recipes
- What is alive? `la1010 measure -c 0-15 --json` → per channel freq/duty/min/max pulse or static level.
- Unknown UART: `la1010 capture -c 0 -d 2s --decode uart:rx=0 --json` (baud auto; first event
  `kind=info` reports it). Then pin it: `uart:rx=0,baud=115200`.
- I2C from the first START: `la1010 capture --decode i2c:scl=1,sda=0 --trigger 0:fall -d 500ms --json`.
- SPI with CS: `la1010 capture --decode spi:clk=0,mosi=1,miso=2,cs=3,mode=0 -d 200ms --json`.
- USB enumeration: `la1010 monitor --decode usb:dp=2,dm=3 --until GET_DESCRIPTOR --timeout 20s --json`,
  then plug the device in. NAKs are counted, not printed (`naks` to show; `packets` for raw packets).
- Save for PulseView: add `-o capture.sr`; re-decode later: `la1010 decode capture.sr --decode …`.
- `la1010 decoders --json` lists every option.

## Limits (measured 2026-11-18, rev a2)
| channels | max lossless rate |
|---:|---|
| 1–3 | 100 MHz |
| 4 | 50 MHz |
| 8 | 40 MHz |
| 16 | 20 MHz |
- USB full speed needs ≥ 48 MHz (default 100M, 2 channels). No USB high speed.
- No hardware trigger (software trigger in the host); no sample memory.
