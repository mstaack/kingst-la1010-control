//! Waveform generators used by tests. Not a stable API.

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
