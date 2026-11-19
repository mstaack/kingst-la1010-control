# la1010

A small command-line tool (and Rust library) for the **Kingst LA1010** USB logic analyzer, so you
can use it without the vendor GUI.

Plug the analyzer in, clip it onto a bus, and `la1010` will tell you what's happening on the wires:
measure clock frequencies and duty cycles, record a capture you can open later in PulseView, or
decode **UART, I²C, SPI and USB low/full speed** traffic straight into your terminal.

It is built for two kinds of users at once:

- **People at a bench** get readable, timestamped text output.
- **Scripts and AI agents** get one JSON object per line (`--json`), stable exit codes, and runs
  that always stop on their own, so nothing hangs waiting for a signal that never comes.

Supported hardware: LA1010 revisions a0, a1 and a2 (EEPROM magic 0x04, 0x07, 0x0a); tested on a2.
Runs on macOS and Linux.

## Quick start

```sh
la1010 firmware fetch                              # one-time: download the vendor firmware
la1010 info                                        # is the analyzer there?
la1010 measure -c 0-3                              # what signals are on CH0-CH3?
la1010 capture -d 2s --decode uart:rx=0,baud=115200   # read 2 s of serial output
```

## Install

**Prebuilt binary** for macOS (Apple Silicon or Intel) and Linux x86_64. No Rust toolchain needed:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/mstaack/kingst-la1010-control/releases/latest/download/la1010-installer.sh | sh
```

This puts `la1010` into `~/.cargo/bin` and adds that to your `PATH`; open a new terminal afterwards.
Binaries and checksums are also on the [releases page](https://github.com/mstaack/kingst-la1010-control/releases).

**With Cargo** (needs [Rust](https://rustup.rs); on Linux also `libssl-dev`), from
[crates.io](https://crates.io/crates/la1010):

```sh
cargo install la1010
```

To use it as a library in your own Rust project: `cargo add la1010` ([API docs](https://docs.rs/la1010)).

The vendor firmware is not included. After installing, run `la1010 firmware fetch` once (see
[Firmware](#firmware)).

### Linux: USB permissions

By default only root may talk to a new USB device. This udev rule lets your normal user access the
analyzer:

```sh
echo 'SUBSYSTEM=="usb", ATTR{idVendor}=="77a1", ATTR{idProduct}=="01a2", MODE="0660", TAG+="uaccess"' | sudo tee /etc/udev/rules.d/99-la1010.rules
sudo udevadm control --reload && sudo udevadm trigger   # then unplug and replug the analyzer
```

The same rule ships as `contrib/99-la1010.rules`. macOS needs nothing.

## Firmware

The LA1010 has no firmware of its own in flash. Every time it is plugged in, the host has to load
FX2 microcontroller firmware into its RAM and then an FPGA bitstream; `la1010` does this
automatically on first use after plug-in.

Those files are © Qingdao Kingst Electronics Co., Ltd. They are **not part of this project and not
covered by its MIT license**, so `la1010` never ships them. Instead it fetches them from Kingst when
you ask:

```sh
la1010 firmware fetch                               # download KingstVIS for Linux, install 4 files
la1010 firmware extract KingstVIS_v3.6.6.tar.gz     # offline: from a tarball, directory or binary
```

- **`firmware fetch`** downloads the KingstVIS Linux package from qdkingst.com, pulls out the four
  files the LA1010 needs, and installs them. The Kingst server can be slow (tens of KB/s), so this
  may take a few minutes. On a terminal it shows progress on stderr, and it gives up if the
  connection stalls for 60 s.
- **`firmware extract PATH`** does the same from something you already have: the KingstVIS tarball,
  an unpacked KingstVIS directory, or the KingstVIS binary itself. Use this if `fetch` keeps failing
  (download the tarball with a browser) or on a machine without internet.

Both accept `--out DIR` to choose where the files go. Otherwise they are installed into, and later
looked up in, the first of: `--firmware-dir`, `$LA1010_FIRMWARE_DIR`, `$XDG_DATA_HOME/la1010`,
`~/.local/share/la1010`. Each file is checked against the sizes and CRC32 values from KingstVIS
v3.6.6; files from other versions are installed with a warning.

The download uses the system TLS library (OpenSSL on Linux, Security.framework on macOS), because
the Kingst server only offers legacy TLS 1.2 cipher suites.

## Commands

| Command | What it does |
|---|---|
| `info` | Finds the analyzer and reports model, revision and firmware state. |
| `firmware` | Downloads or extracts the vendor firmware (see above). |
| `measure` | Reports frequency, duty cycle and pulse widths per channel. |
| `capture` | Records a fixed stretch of time, optionally triggered, decoded and saved to a file. |
| `monitor` | Decodes live traffic and stops when a condition is met. |
| `decode` | Decodes a capture you saved earlier, no hardware needed. |
| `decoders` | Lists the protocol decoders and all their options. |

`la1010 <command> --help` shows every option.

### `info`: is it connected?

```sh
la1010 info
```

The first thing to run after plugging in. It finds the analyzer on USB, loads the firmware into it
if needed, and prints what it found. Exit code 2 means the device is not connected or the firmware
is missing.

### `measure`: what is on the wires?

```sh
la1010 measure -c 0-3
```

Samples the selected channels for one second (change with `-d 500ms`) and prints one line per
channel: frequency, duty cycle, shortest and longest pulse, edge count, or "static high/low" if
nothing moved. Handy to check that a clock is running, find which pin is active, or guess a UART
baud rate before decoding.

### `capture`: record a window of time

```sh
la1010 capture -d 2s --decode uart:rx=0,baud=115200
la1010 capture --decode i2c:scl=1,sda=0 --trigger 0:fall -o boot.sr
```

Records a fixed amount of signal, either as time (`-d 2s`, default 1 s) or as a sample count
(`-n 1000000`). While it records, it can:

- **wait for a trigger**: `--trigger 0:rise` starts on a rising edge on CH0; conditions can be
  combined, e.g. `--trigger 3:fall,1:high`. `--pre 10` keeps 10 % of the capture from before the
  trigger, and `--timeout 10s` sets how long to wait before giving up (exit 3).
- **decode** one or more protocols with `--decode` (repeatable).
- **save** the raw samples with `-o`: `.sr` opens in PulseView or sigrok-cli, `.vcd` in GTKWave and
  most waveform viewers, `.csv` in anything.

The trigger is done in software: the LA1010 streams everything and `la1010` watches for the
condition. Timestamps in the output are relative to the trigger point.

### `monitor`: watch live traffic

```sh
la1010 monitor --decode usb:dp=2,dm=3 --until GET_DESCRIPTOR --json
```

Decodes continuously and prints events as they arrive, like `tail -f` for a bus. It stops at the
first of:

- `--timeout` (default 10 s; `0` runs until Ctrl-C),
- `--count N` decoded events,
- `--until TEXT`, the first output line that contains TEXT.

If `--count` or `--until` was given but not reached before the timeout, it exits with 3, so a
script can tell "saw it" from "didn't see it".

### `decode`: work on a saved file

```sh
la1010 decode boot.sr --decode i2c:scl=1,sda=0
```

Runs decoders over a `.sr` or `.vcd` file instead of live hardware. Useful to try different decoder
settings on the same capture, or to decode files recorded with PulseView or by someone else.
`--count` and `--until` work here too.

### `decoders`: what can it decode?

```sh
la1010 decoders           # all decoders
la1010 decoders spi       # one decoder in detail
la1010 decoders --json    # machine-readable, every option with its default
```

### Common options

| Option | Meaning |
|---|---|
| `-c 0-3,8` | Channels to sample. Default: the ones your decoders and trigger use, otherwise all 16. |
| `-r 24M` | Sample rate: 800 MHz divided by a whole number, at most 100 MHz. Default: picked to suit the decoders, otherwise 10 MHz. |
| `-t 1.4` | Input threshold in volts (−4.0 to 4.0). The default 1.4 V suits 1.8–5 V logic. |
| `--json` | One JSON object per line on stdout, errors as JSON on stderr. |
| `--firmware-dir DIR` | Where to look for the firmware. |

## Decoders

| Name | What it decodes | Options |
|---|---|---|
| `uart` | Serial ports, RS-232/TTL serial consoles | `rx`, `tx`, `baud` (number or `auto`), `bits`, `parity` n/e/o, `stop`, `invert` |
| `i2c` | I²C / SMBus: addresses, read/write, ACK/NACK, data | `scl`, `sda` (required) |
| `spi` | SPI in all four modes | `clk` (required), `mosi`, `miso`, `cs`, `mode` 0-3, `cs_active`, `bits`, `order` |
| `usb` | USB 1.x low speed (1.5 Mbit/s) and full speed (12 Mbit/s) | `dp`, `dm` (required), `speed` auto/fs/ls, `show_sof`, `naks`, `packets` |

A decoder spec looks like `name:opt=val,opt=val,flag`, for example `spi:clk=0,mosi=1,cs=3,mode=3`.
Channel options take the channel number the wire is connected to.

- **UART** with `baud=auto` (the default) estimates the baud rate from the shortest pulse it sees.
- **USB** groups packets into transactions, checks CRC5/CRC16 and bit stuffing, and names standard
  SETUP requests such as `GET_DESCRIPTOR`. NAKed transactions (a host polling an idle device) are
  only counted unless you pass `naks`, so the output is not flooded.

## Output

As text, one line per event with a timestamp:

```
12.345678ms    uart    rx 0x41 'A'
```

With `--json`, one JSON object per line (NDJSON). Every object has `t` (seconds; 0 = trigger),
`sample`, `proto`, `kind`, the decoder's own fields, and `text` (the same as the text output):

```json
{"t":0.012345678,"sample":123456,"proto":"uart","kind":"byte","ch":"rx","value":65,"text":"rx 0x41 'A'"}
```

Protocol errors are events too, with `"kind":"error"` and an `error` code (`framing`, `parity`,
`crc5`, `crc16`, `bitstuff`, …), so a broken frame shows up in the stream where it happened.
Diagnostics go to stderr; with `--json`, a fatal error is a single line `{"error":"…","code":N}`.

**Exit codes**

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Usage error: fix the arguments |
| 2 | Device, firmware or I/O problem |
| 3 | Trigger, `--until` or `--count` not reached in time |

## Using it with AI agents

`skill/la1010/SKILL.md` is a ready-made skill that tells an agent how to use this CLI safely
(always `--json`, bounded runs, bandwidth limits). For Claude Code, copy the `skill/la1010`
directory into `~/.claude/skills/`.

## Limits

- **Streaming only.** The LA1010 has no sample memory, so every sample goes over USB as it is taken
  and there is no hardware trigger.
- **Bandwidth.** Captures are lossless up to about 320 Mbit/s of rate × channels, for example 3
  channels at 100 MHz, 8 at 40 MHz or 16 at 20 MHz. Above that the device stops streaming and the
  capture fails with exit 2; `la1010` warns before starting. Measurements are in
  `docs/hardware-validation.md`.
- **USB decoding** needs at least 48 MHz for full speed (the default is 100 MHz on 2 channels).
  USB high speed (480 Mbit/s) is out of reach.

## Development

```sh
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test
cargo test --test hw -- --ignored --test-threads=1   # with the analyzer attached
cargo test --test net -- --ignored                   # downloads ~29 MB from qdkingst.com
```

Design notes: `docs/superpowers/specs/2026-11-18-la1010-cli-design.md`.

## License

MIT, see `LICENSE`. The Kingst vendor firmware is not included and not covered (see
[Firmware](#firmware)).
