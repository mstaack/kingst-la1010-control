# la1010

Command-line capture and protocol decoding for the **Kingst LA1010** USB logic analyzer, without
the vendor GUI. Measures signals, captures to files that PulseView/sigrok can open, and decodes
UART, I²C, SPI and USB low/full speed, as plain text or NDJSON. Built to be driven by people and
by agents: every run is bounded, output is machine-readable, exit codes are stable.

Supported: LA1010 revisions a0, a1, a2 (EEPROM magic 0x04, 0x07, 0x0a). Tested on a2.
Platforms: macOS and Linux.

## Install

**macOS (Apple Silicon or Intel) and Linux x86_64 — prebuilt binary, no Rust needed:**

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/mstaack/kingst-la1010-control/releases/latest/download/la1010-installer.sh | sh
```

This installs `la1010` into `~/.cargo/bin` and adds it to your `PATH` (open a new terminal afterwards).
Binaries and checksums are also on the [releases page](https://github.com/mstaack/kingst-la1010-control/releases).

**With Cargo** (needs [Rust](https://rustup.rs); on Linux also `libssl-dev`) — from [crates.io](https://crates.io/crates/la1010):

```sh
cargo install la1010
```

As a library: `cargo add la1010` ([docs](https://docs.rs/la1010)).

The vendor firmware is not included. After installing, run `la1010 firmware fetch` once (see below).

**Linux permissions** — let your user access the device without root:

```sh
echo 'SUBSYSTEM=="usb", ATTR{idVendor}=="77a1", ATTR{idProduct}=="01a2", MODE="0660", TAG+="uaccess"' | sudo tee /etc/udev/rules.d/99-la1010.rules
sudo udevadm control --reload && sudo udevadm trigger   # then replug the analyzer
```

The same rule is in `contrib/99-la1010.rules`. macOS needs nothing.

The firmware download uses the system TLS library (OpenSSL on Linux, Security.framework on macOS),
because the vendor server only offers legacy TLS 1.2 cipher suites.

## Firmware

The LA1010 has no firmware in flash: on every plug-in the host loads FX2 firmware into RAM and an
FPGA bitstream. These files are © Qingdao Kingst Electronics Co., Ltd. They are **not part of this
project and not covered by its MIT license**; they are downloaded from Kingst on your request:

```sh
la1010 firmware fetch                        # downloads KingstVIS for Linux, installs 4 files
la1010 firmware extract KingstVIS_v3.6.6.tar.gz   # offline: from a tarball, directory or binary
```

Files go to the first of `--firmware-dir`, `$LA1010_FIRMWARE_DIR`, `$XDG_DATA_HOME/la1010`,
`~/.local/share/la1010`, and are looked up in the same order. Each file is checked against the
sizes and CRC32 values of KingstVIS v3.6.6; other versions install with a warning.

The Kingst server can be slow (tens of KB/s), so `fetch` may take several minutes. On a terminal it
shows progress on stderr; it gives up if the connection stalls for 60 s. If it keeps failing,
download the tarball with a browser and use `firmware extract`.

## Usage

```
la1010 info                                    identify the analyzer
la1010 measure -c 0-3                          frequency / duty / pulses on CH0-3 for 1 s
la1010 capture -d 2s --decode uart:rx=0,baud=115200
la1010 capture --decode i2c:scl=1,sda=0 --trigger 0:fall -o boot.sr
la1010 monitor --decode usb:dp=2,dm=3 --until GET_DESCRIPTOR --json
la1010 decode boot.sr --decode i2c:scl=1,sda=0
la1010 decoders                                decoders and their options
```

Common options: `-c` channels (default: those the decoders/trigger use, else 0-15), `-r` sample
rate (800 MHz / n, max 100 MHz; default chosen from the decoders), `-t` threshold volts
(default 1.4), `--json`.

- `capture`: fixed length (`-d`, default 1 s, or `-n` samples); software trigger
  (`--trigger 0:rise`, `3:fall,1:high`, `--pre` percent, `--timeout` to wait); `-o` writes
  `.sr` (PulseView/sigrok-cli), `.vcd` or `.csv`.
- `monitor`: decodes live until `--timeout` (default 10 s, `0` = until Ctrl-C), `--count N` or
  `--until TEXT`.
- `decode`: decodes a saved `.sr` or `.vcd`.

### Decoders

| Name | Options |
|---|---|
| `uart` | `rx`, `tx`, `baud` (number or `auto`), `bits`, `parity` n/e/o, `stop`, `invert` |
| `i2c` | `scl`, `sda` (required) |
| `spi` | `clk` (required), `mosi`, `miso`, `cs`, `mode` 0-3, `cs_active`, `bits`, `order` |
| `usb` | `dp`, `dm` (required), `speed` auto/fs/ls, `show_sof`, `naks`, `packets` |

Spec syntax: `name:opt=val,opt=val,flag`, e.g. `spi:clk=0,mosi=1,cs=3,mode=3`.
`la1010 decoders --json` lists every option.

USB decoding groups packets into transactions, checks CRC5/CRC16 and bit stuffing, and names
standard SETUP requests. NAKed transactions (polling) are counted, not printed, unless `naks` is
given.

### Output

Text: `12.345678ms    uart    rx 0x41 'A'`. With `--json`, one JSON object per line with the
keys `t` (seconds; 0 = trigger), `sample`, `proto`, `kind`, decoder fields, `text`:

```json
{"t":0.012345678,"sample":123456,"proto":"uart","kind":"byte","ch":"rx","value":65,"text":"rx 0x41 'A'"}
```

Errors are events with `"kind":"error"` and an `error` code (`framing`, `parity`, `crc5`,
`crc16`, `bitstuff`, …). Diagnostics go to stderr; with `--json` a fatal error is one line
`{"error":"…","code":N}`.

Exit codes: 0 ok, 1 usage, 2 device/firmware/IO, 3 trigger/`--until`/`--count` not reached.

## Limits

- Streaming only: the LA1010 has no sample memory, so there is no hardware trigger.
- Lossless up to about 320 Mbit/s of rate × channels (e.g. 3 ch at 100 MHz, 8 ch at 40 MHz,
  16 ch at 20 MHz). Above that the device stops and the capture fails with exit 2.
  See `docs/hardware-validation.md`.
- USB full speed needs at least 48 MHz (default 100 MHz, 2 channels). No USB high speed.

## Development

```sh
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test
cargo test --test hw -- --ignored --test-threads=1   # with the analyzer attached
cargo test --test net -- --ignored                   # downloads from qdkingst.com
```

Design: `docs/superpowers/specs/2026-11-18-la1010-cli-design.md`.

## License

MIT, see `LICENSE`. Vendor firmware is excluded (see above).
