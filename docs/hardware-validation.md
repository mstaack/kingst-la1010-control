# Hardware validation

Date: 2026-11-18. Unit: Kingst LA1010 rev a2 (EEPROM magic 0x0a/0x10, made 2024-03), macOS,
la1010 0.1.0 release build. Signal: R&S RTC1002 pattern generator P0 (1 kHz square, ~2.4 V high)
on CH0, threshold 1.4 V. Scope restored afterwards (PGEN ARB, STIM 8.68 µs, output off).

## Firmware

`la1010 firmware fetch` downloaded KingstVIS v3.6.6 from qdkingst.com; all four files `verified`
(size and CRC32 equal to the table in `src/firmware/mod.rs`). The vendor server (IIS 7.5) only
negotiates CBC / RSA-key-exchange TLS 1.2 suites, so the download uses the OS TLS stack
(native-tls); rustls fails with "connection reset".

## Bring-up and measure

- `la1010 info`: rev a2, manufactured 2024-03 (FX2 and FPGA were already loaded from an earlier
  session; see "Not tested").
- `la1010 measure -c 0-1 -r 1M -d 1s`: CH0 1000.013 Hz, duty 50.0%, pulse 0.499–0.500 ms;
  CH1 static low. The 13 ppm offset is the scope/analyzer clock difference.
- `cargo test --test hw -- --ignored`: 2/2 pass.

## Cold start

After a replug the device enumerates as `77a1:01a2` without product string (no firmware).
`la1010 --json info`: `fx2_uploaded:true`, `fpga_uploaded:true`, exit 0 in 4.05 s; it then
enumerates as "Kingst Logic Analyzer". A second `info` skips both uploads (14 ms).
`measure -c 0 -d 100ms` afterwards streams normally (CH0 static high, generator off).

## Lossless streaming (rate × channels)

`la1010 measure -c 0-(n-1) -r R -d 2s`; OK = CH0 min and max pulse within 1.5 samples of 500 µs.

| channels | 100M | 50M | 40M | 25M | 20M | 10M | 5M |
|---:|:--:|:--:|:--:|:--:|:--:|:--:|:--:|
| 1  | OK | OK | OK | OK | OK | OK | OK |
| 2  | OK | OK | OK | OK | OK | OK | OK |
| 3  | OK | OK | OK | OK | OK | OK | OK |
| 4  | stall | OK | OK | OK | OK | OK | OK |
| 8  | stall | stall | OK | OK | OK | OK | OK |
| 16 | stall | stall | stall | stall | OK | OK | OK |

Every OK cell is ≤ 320 Mbit/s and every failing cell is ≥ 400 Mbit/s. When overloaded the device
stops sending; the tool reports `stream stalled` and exits 2, never output with silent gaps.
10 s captures at 2 ch × 100M, 3 ch × 100M and 8 ch × 40M each produced exactly 20000 edges
(no gaps) in real time. `VERIFIED_BPS` = 320 Mbit/s.

## CLI behaviour

- `capture --trigger 0:rise -o sq.sr`: exit 0; zip contains `version`, `metadata`, `logic-1-1`;
  `decode sq.sr` reads it back.
- Triggered `--json` capture with UART decode: times relative to the trigger (pre-trigger
  event at t = −0.5 ms).
- `monitor … --timeout 0 | head -2`: exit 0 (broken pipe). SIGINT during monitor: exit 0.
  `--until "'U'"`: exit 0. Device works normally afterwards (halted on every path).
- `capture --trigger 0:rise --timeout 1s` without signal: exit 3, also as JSON on stderr.

## Not tested

- Channel mapping CH1–CH15 (needs rewiring P0).
- Threshold accuracy; UART/I²C/SPI against real buses; USB full speed on a real bus.
