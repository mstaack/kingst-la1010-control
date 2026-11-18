//! Waveform generators used by tests. Not a stable API.

use crate::decode::usb::{crc5_11, crc16};
use crate::stream::Edge;

#[derive(Default)]
pub struct Wave {
    sets: Vec<(u64, u8, bool)>,
}

impl Wave {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, sample: u64, ch: u8, level: bool) {
        self.sets.push((sample, ch, level));
    }

    /// Sorted edges. The first set per channel is its initial level; later sets
    /// produce an edge only when the level changes. A later set at the same
    /// (sample, ch) overrides an earlier one.
    pub fn edges(&self) -> Vec<Edge> {
        let mut s = self.sets.clone();
        s.sort_by_key(|&(t, ch, _)| (t, ch));
        let mut last: [Option<bool>; 16] = [None; 16];
        let mut out: Vec<Edge> = Vec::new();
        for (t, ch, level) in s {
            if let Some(prev) = out.last_mut().filter(|e| e.sample == t && e.ch == ch) {
                prev.level = level;
                last[ch as usize] = Some(level);
                continue;
            }
            if last[ch as usize] != Some(level) {
                out.push(Edge {
                    sample: t,
                    ch,
                    level,
                });
                last[ch as usize] = Some(level);
            }
        }
        out
    }
}

/// One UART frame, LSB first: start, data, optional parity (Some(true) = even), stop bits.
pub fn uart_bits(value: u16, bits: u8, parity: Option<bool>, stops: u8) -> Vec<bool> {
    let mut v = vec![false];
    for i in 0..bits {
        v.push(value >> i & 1 == 1);
    }
    if let Some(even) = parity {
        let ones = (value & ((1 << bits) - 1)).count_ones();
        v.push((ones % 2 == 1) == even);
    }
    v.extend(std::iter::repeat_n(true, stops as usize));
    v
}

/// Drive `bits` on `ch` from `start`, `bit` samples per bit. Returns the end sample.
pub fn uart_frame(w: &mut Wave, ch: u8, start: f64, bit: f64, bits: &[bool]) -> f64 {
    for (i, &b) in bits.iter().enumerate() {
        w.set((start + i as f64 * bit).round() as u64, ch, b);
    }
    start + bits.len() as f64 * bit
}

/// I2C START: SDA falls while SCL is high, then SCL goes low. `h` = half clock period.
pub fn i2c_start(w: &mut Wave, scl: u8, sda: u8, t: u64, h: u64) -> u64 {
    w.set(t, sda, false);
    w.set(t + h, scl, false);
    t + h
}

/// 8 data bits MSB first plus the ACK bit (ack = SDA low on the 9th clock).
pub fn i2c_byte(w: &mut Wave, scl: u8, sda: u8, mut t: u64, h: u64, byte: u8, ack: bool) -> u64 {
    for i in 0..9 {
        let bit = if i < 8 {
            byte >> (7 - i) & 1 == 1
        } else {
            !ack
        };
        w.set(t + h / 2, sda, bit);
        w.set(t + h, scl, true);
        w.set(t + 2 * h, scl, false);
        t += 2 * h;
    }
    t
}

pub fn i2c_restart(w: &mut Wave, scl: u8, sda: u8, t: u64, h: u64) -> u64 {
    w.set(t + h / 2, sda, true);
    w.set(t + h, scl, true);
    w.set(t + 2 * h, sda, false);
    w.set(t + 3 * h, scl, false);
    t + 3 * h
}

pub fn i2c_stop(w: &mut Wave, scl: u8, sda: u8, t: u64, h: u64) -> u64 {
    w.set(t + h / 2, sda, false);
    w.set(t + h, scl, true);
    w.set(t + 2 * h, sda, true);
    t + 2 * h
}

/// One SPI word. CPHA=0: data set, then leading edge (sample), then trailing edge.
/// CPHA=1: leading edge, data set, then trailing edge (sample). Returns end time.
#[allow(clippy::too_many_arguments)]
pub fn spi_word(
    w: &mut Wave,
    clk: u8,
    mosi: u8,
    miso: u8,
    mut t: u64,
    h: u64,
    mode: u8,
    bits: u8,
    lsb: bool,
    vm: u32,
    vs: u32,
) -> u64 {
    let cpol = mode >> 1 & 1 == 1;
    let cpha = mode & 1 == 1;
    for i in 0..bits {
        let k = if lsb { i } else { bits - 1 - i };
        let (bm, bs) = (vm >> k & 1 == 1, vs >> k & 1 == 1);
        if cpha {
            w.set(t, clk, !cpol);
            w.set(t + 1, mosi, bm);
            w.set(t + 1, miso, bs);
            w.set(t + h, clk, cpol);
        } else {
            w.set(t, mosi, bm);
            w.set(t, miso, bs);
            w.set(t + h, clk, !cpol);
            w.set(t + 2 * h, clk, cpol);
        }
        t += 2 * h;
    }
    t
}

/// (D+, D-) for symbol 'J', 'K' or '0' (SE0).
pub fn usb_levels(sym: char, ls: bool) -> (bool, bool) {
    match (sym, ls) {
        ('0', _) => (false, false),
        ('J', false) | ('K', true) => (true, false),
        _ => (false, true),
    }
}

/// SYNC + bytes (PID first), LSB first, bit-stuffed, NRZI from idle J, then EOP (SE0 SE0 J).
pub fn usb_symbols(bytes: &[u8]) -> Vec<char> {
    let mut bits = vec![false, false, false, false, false, false, false, true];
    for b in bytes {
        for i in 0..8 {
            bits.push(b >> i & 1 == 1);
        }
    }
    let mut stuffed = Vec::new();
    let mut ones = 0;
    for b in bits {
        stuffed.push(b);
        if b {
            ones += 1;
            if ones == 6 {
                stuffed.push(false);
                ones = 0;
            }
        } else {
            ones = 0;
        }
    }
    let mut cur = 'J';
    let mut syms = Vec::new();
    for b in stuffed {
        if !b {
            cur = if cur == 'J' { 'K' } else { 'J' };
        }
        syms.push(cur);
    }
    syms.extend(['0', '0', 'J']);
    syms
}

pub fn usb_idle(w: &mut Wave, dp: u8, dm: u8, t: u64, ls: bool) {
    let (p, m) = usb_levels('J', ls);
    w.set(t, dp, p);
    w.set(t, dm, m);
}

/// Drive one packet from `t`; D- transitions lag D+ by `skew` samples. Returns end time.
#[allow(clippy::too_many_arguments)]
pub fn usb_packet(
    w: &mut Wave,
    dp: u8,
    dm: u8,
    t: f64,
    bit: f64,
    ls: bool,
    skew: u64,
    bytes: &[u8],
) -> f64 {
    let syms = usb_symbols(bytes);
    for (i, &s) in syms.iter().enumerate() {
        let at = (t + i as f64 * bit).round() as u64;
        let (p, m) = usb_levels(s, ls);
        w.set(at, dp, p);
        w.set(at + skew, dm, m);
    }
    t + syms.len() as f64 * bit
}

pub fn usb_se0(w: &mut Wave, dp: u8, dm: u8, t: u64, dur: u64, ls: bool) {
    w.set(t, dp, false);
    w.set(t, dm, false);
    usb_idle(w, dp, dm, t + dur, ls);
}

fn usb_pid(pid: u8) -> u8 {
    pid | (!pid & 0x0f) << 4
}

fn usb_token11(pid: u8, v: u16) -> Vec<u8> {
    let f = v | u16::from(crc5_11(v)) << 11;
    vec![usb_pid(pid), f as u8, (f >> 8) as u8]
}

pub fn usb_token(pid: u8, addr: u8, ep: u8) -> Vec<u8> {
    usb_token11(pid, u16::from(addr) | u16::from(ep) << 7)
}

pub fn usb_sof(frame: u16) -> Vec<u8> {
    usb_token11(0x5, frame & 0x7ff)
}

pub fn usb_data(pid: u8, payload: &[u8]) -> Vec<u8> {
    let mut v = vec![usb_pid(pid)];
    v.extend_from_slice(payload);
    v.extend(crc16(payload).to_le_bytes());
    v
}

pub fn usb_handshake(pid: u8) -> Vec<u8> {
    vec![usb_pid(pid)]
}
