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
