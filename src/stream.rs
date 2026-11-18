//! Turn the LA1010 bulk stream into level-change edges.
//!
//! Stream format (verified on hardware): u16 little-endian words, one per enabled
//! channel in ascending channel order, repeating. Each word holds 16 consecutive
//! samples of one channel; bit 0 is the earliest.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edge {
    pub sample: u64,
    pub ch: u8,
    pub level: bool,
}

pub struct Deinterleaver {
    channels: Vec<u8>,
    words: Vec<u16>,
    carry: Option<u8>,
    block: u64,
    levels: u16,
}

impl Deinterleaver {
    pub fn new(mask: u16) -> Self {
        assert!(mask != 0, "at least one channel");
        let channels: Vec<u8> = (0..16).filter(|c| mask >> c & 1 == 1).collect();
        Deinterleaver {
            words: Vec::with_capacity(channels.len()),
            channels,
            carry: None,
            block: 0,
            levels: 0,
        }
    }

    /// Number of complete samples decoded so far.
    pub fn samples(&self) -> u64 {
        self.block * 16
    }

    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<Edge>) {
        let mut i = 0;
        if let Some(lo) = self.carry.take() {
            let Some(&hi) = bytes.first() else {
                self.carry = Some(lo);
                return;
            };
            self.word(u16::from_le_bytes([lo, hi]), out);
            i = 1;
        }
        while i + 1 < bytes.len() {
            self.word(u16::from_le_bytes([bytes[i], bytes[i + 1]]), out);
            i += 2;
        }
        if i < bytes.len() {
            self.carry = Some(bytes[i]);
        }
    }

    fn word(&mut self, w: u16, out: &mut Vec<Edge>) {
        self.words.push(w);
        if self.words.len() == self.channels.len() {
            self.emit_block(out);
            self.words.clear();
        }
    }

    fn emit_block(&mut self, out: &mut Vec<Edge>) {
        let base = self.block * 16;
        let start = out.len();
        for (k, &ch) in self.channels.iter().enumerate() {
            let w = self.words[k];
            let prev = (self.levels >> ch) & 1;
            let mut diff = w ^ ((w << 1) | prev);
            if self.block == 0 {
                diff |= 1;
            }
            while diff != 0 {
                let bit = diff.trailing_zeros();
                out.push(Edge {
                    sample: base + u64::from(bit),
                    ch,
                    level: (w >> bit) & 1 == 1,
                });
                diff &= diff - 1;
            }
            self.levels = (self.levels & !(1 << ch)) | ((w >> 15) << ch);
        }
        if self.channels.len() > 1 {
            out[start..].sort_unstable_by_key(|e| (e.sample, e.ch));
        }
        self.block += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(mask: u16, bytes: &[u8]) -> Vec<Edge> {
        let mut d = Deinterleaver::new(mask);
        let mut out = Vec::new();
        d.push(bytes, &mut out);
        out
    }

    fn e(sample: u64, ch: u8, level: bool) -> Edge {
        Edge { sample, ch, level }
    }

    #[test]
    fn single_channel_word() {
        // bits 4..7 high: LSB = earliest sample
        assert_eq!(
            run(0b1, &0x00F0u16.to_le_bytes()),
            vec![e(0, 0, false), e(4, 0, true), e(8, 0, false)]
        );
    }

    #[test]
    fn two_channels_round_robin() {
        let mut b = Vec::new();
        b.extend(0x0001u16.to_le_bytes()); // ch0 word
        b.extend(0xFFFFu16.to_le_bytes()); // ch1 word
        assert_eq!(
            run(0b11, &b),
            vec![e(0, 0, true), e(0, 1, true), e(1, 0, false)]
        );
    }

    #[test]
    fn sparse_mask_uses_channel_numbers() {
        let mut b = Vec::new();
        b.extend(0x0000u16.to_le_bytes()); // ch3
        b.extend(0x8000u16.to_le_bytes()); // ch9
        assert_eq!(
            run(1 << 3 | 1 << 9, &b),
            vec![e(0, 3, false), e(0, 9, false), e(15, 9, true)]
        );
    }

    #[test]
    fn level_carries_across_blocks() {
        let mut b = Vec::new();
        b.extend(0x8000u16.to_le_bytes());
        b.extend(0x0000u16.to_le_bytes());
        assert_eq!(
            run(1, &b),
            vec![e(0, 0, false), e(15, 0, true), e(16, 0, false)]
        );
    }

    #[test]
    fn split_pushes_equal_single_push() {
        let bytes: Vec<u8> = (0..64u8).map(|i| i.wrapping_mul(37)).collect();
        let whole = run(0b101, &bytes);
        let mut d = Deinterleaver::new(0b101);
        let mut parts = Vec::new();
        for chunk in bytes.chunks(3) {
            d.push(chunk, &mut parts);
        }
        assert_eq!(parts, whole);
        assert_eq!(d.samples(), 64 / 2 / 2 * 16);
    }

    #[test]
    fn real_square_fixture() {
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/sq_1m_ch0.bin"
        ))
        .unwrap();
        let edges = run(1, &bytes);
        assert!(edges.len() > 100);
        // Skip the initial edge and the first partial half period.
        for w in edges[1..].windows(2).skip(1) {
            let d = w[1].sample - w[0].sample;
            assert!(
                (499..=501).contains(&d),
                "half period {d} at {}",
                w[0].sample
            );
        }
    }
}
