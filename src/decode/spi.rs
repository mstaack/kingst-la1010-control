//! SPI: words sampled on the mode's sampling edge, grouped into CS-delimited transfers.

use crate::decode::{Decoder, Info, Lines, OptInfo, Spec};
use crate::error::Result;
use crate::output::Event;
use crate::stream::Edge;

const fn opt(
    name: &'static str,
    help: &'static str,
    default: Option<&'static str>,
    required: bool,
    channel: bool,
) -> OptInfo {
    OptInfo {
        name,
        help,
        default,
        required,
        channel,
    }
}

pub const INFO: Info = Info {
    name: "spi",
    help: "SPI (modes 0-3, optional chip select)",
    opts: &[
        opt("clk", "clock channel", None, true, true),
        opt("mosi", "controller-out channel", None, false, true),
        opt("miso", "controller-in channel", None, false, true),
        opt(
            "cs",
            "chip-select channel (omit: always selected)",
            None,
            false,
            true,
        ),
        opt(
            "mode",
            "SPI mode 0-3 (CPOL<<1 | CPHA)",
            Some("0"),
            false,
            false,
        ),
        opt("cs_active", "low or high", Some("low"), false, false),
        opt("bits", "word size 1-32", Some("8"), false, false),
        opt("order", "msb or lsb first", Some("msb"), false, false),
    ],
};

pub struct Spi {
    clk: u8,
    mosi: Option<u8>,
    miso: Option<u8>,
    cs: Option<u8>,
    cs_low: bool,
    rise: bool,
    bits: u8,
    lsb: bool,
    lines: Lines,
    active: bool,
    n: u8,
    vm: u32,
    vs: u32,
    start: u64,
    tstart: u64,
    words: Vec<(u32, u32)>,
}

impl Spi {
    pub fn new(spec: &Spec) -> Result<Self> {
        let clk = spec.ch("clk")?.ok_or_else(|| spec.usage("missing clk="))?;
        let (mosi, miso, cs) = (spec.ch("mosi")?, spec.ch("miso")?, spec.ch("cs")?);
        if mosi.is_none() && miso.is_none() {
            return Err(spec.usage("need mosi=CH and/or miso=CH"));
        }
        let mode: u8 = spec.num("mode", 0)?;
        if mode > 3 {
            return Err(spec.usage("mode must be 0-3"));
        }
        let bits: u8 = spec.num("bits", 8)?;
        if !(1..=32).contains(&bits) {
            return Err(spec.usage("bits must be 1-32"));
        }
        let cs_low = match spec.get("cs_active").unwrap_or("low") {
            "low" => true,
            "high" => false,
            o => return Err(spec.usage(format!("cs_active={o}: use low or high"))),
        };
        let lsb = match spec.get("order").unwrap_or("msb") {
            "msb" => false,
            "lsb" => true,
            o => return Err(spec.usage(format!("order={o}: use msb or lsb"))),
        };
        let (cpol, cpha) = (mode >> 1, mode & 1);
        Ok(Spi {
            clk,
            mosi,
            miso,
            cs,
            cs_low,
            rise: cpol == cpha,
            bits,
            lsb,
            lines: Lines::default(),
            active: cs.is_none(),
            n: 0,
            vm: 0,
            vs: 0,
            start: 0,
            tstart: 0,
            words: Vec::new(),
        })
    }

    fn hex(&self, v: u32) -> String {
        format!("{v:0w$x}", w = usize::from(self.bits).div_ceil(4))
    }

    fn word_event(&self) -> Event {
        let mut parts = Vec::new();
        let mut ev = Event::new(self.start, "spi", "word", "");
        if self.mosi.is_some() {
            parts.push(format!("MOSI 0x{}", self.hex(self.vm)));
            ev = ev.with("mosi", self.vm);
        }
        if self.miso.is_some() {
            parts.push(format!("MISO 0x{}", self.hex(self.vs)));
            ev = ev.with("miso", self.vs);
        }
        ev.text = parts.join(" ");
        ev
    }

    fn transfer_event(&self, out: &mut Vec<Event>) {
        if self.words.is_empty() {
            return;
        }
        let join = |sel: fn(&(u32, u32)) -> u32| {
            self.words
                .iter()
                .map(|w| self.hex(sel(w)))
                .collect::<Vec<_>>()
                .join(" ")
        };
        let mut ev = Event::new(self.tstart, "spi", "transfer", "").with("words", self.words.len());
        let mut parts = vec!["transfer".to_string()];
        if self.mosi.is_some() {
            let m = join(|w| w.0);
            parts.push(format!("mosi=[{m}]"));
            ev = ev.with("mosi", m.replace(' ', ""));
        }
        if self.miso.is_some() {
            let s = join(|w| w.1);
            parts.push(format!("miso=[{s}]"));
            ev = ev.with("miso", s.replace(' ', ""));
        }
        ev.text = parts.join(" ");
        out.push(ev);
    }
}

impl Decoder for Spi {
    fn feed(&mut self, edges: &[Edge], out: &mut Vec<Event>) {
        let mut lines = std::mem::take(&mut self.lines);
        lines.steps(edges, |st| {
            if let Some(cs) = self.cs
                && (st.first(cs) || st.rose(cs) || st.fell(cs))
            {
                let selected = st.high(cs) != self.cs_low;
                if selected && !self.active {
                    self.active = true;
                    self.n = 0;
                    self.vm = 0;
                    self.vs = 0;
                    self.words.clear();
                    self.tstart = st.sample;
                } else if !selected && self.active {
                    if self.n > 0 {
                        out.push(
                            Event::new(
                                self.start,
                                "spi",
                                "error",
                                format!("incomplete word ({} bits)", self.n),
                            )
                            .with("error", "incomplete"),
                        );
                    }
                    self.transfer_event(out);
                    self.active = false;
                }
            }
            let edge = if self.rise {
                st.rose(self.clk)
            } else {
                st.fell(self.clk)
            };
            if !self.active || !edge {
                return;
            }
            if self.n == 0 {
                self.start = st.sample;
            }
            let bm = self.mosi.is_some_and(|c| st.high(c));
            let bs = self.miso.is_some_and(|c| st.high(c));
            if self.lsb {
                self.vm |= u32::from(bm) << self.n;
                self.vs |= u32::from(bs) << self.n;
            } else {
                self.vm = self.vm << 1 | u32::from(bm);
                self.vs = self.vs << 1 | u32::from(bs);
            }
            self.n += 1;
            if self.n == self.bits {
                out.push(self.word_event());
                self.words.push((self.vm, self.vs));
                self.n = 0;
                self.vm = 0;
                self.vs = 0;
            }
        });
        self.lines = lines;
    }

    fn finish(&mut self, _end: u64, _out: &mut Vec<Event>) {}
}

#[cfg(test)]
mod tests {
    use crate::decode::{Spec, build};
    use crate::output::Event;
    use crate::testgen::{Wave, spi_word};

    // ch0 clk, ch1 mosi, ch2 miso, ch3 cs
    fn run(spec: &str, mode: u8, lsb: bool, words: &[(u32, u32)], cut: Option<u8>) -> Vec<Event> {
        let mut w = Wave::new();
        w.set(0, 0, mode >> 1 & 1 == 1);
        w.set(0, 1, false);
        w.set(0, 2, false);
        w.set(0, 3, true);
        w.set(100, 3, false);
        let mut t = 200;
        for &(m, s) in words {
            t = spi_word(&mut w, 0, 1, 2, t, 10, mode, 8, lsb, m, s) + 20;
        }
        if let Some(bits) = cut {
            t = spi_word(&mut w, 0, 1, 2, t, 10, mode, bits, lsb, 0xff, 0xff) + 20;
        }
        w.set(t, 3, true);
        let mut d = build(&Spec::parse(spec).unwrap(), 10e6).unwrap();
        let mut out = Vec::new();
        d.feed(&w.edges(), &mut out);
        d.finish(t + 100, &mut out);
        out
    }

    #[test]
    fn mode0_words_and_transfer() {
        let ev = run(
            "spi:clk=0,mosi=1,miso=2,cs=3",
            0,
            false,
            &[(0x12, 0xff), (0x34, 0x00)],
            None,
        );
        let t: Vec<_> = ev.iter().map(|e| (e.kind, e.text.as_str())).collect();
        assert_eq!(
            t,
            vec![
                ("word", "MOSI 0x12 MISO 0xff"),
                ("word", "MOSI 0x34 MISO 0x00"),
                ("transfer", "transfer mosi=[12 34] miso=[ff 00]"),
            ]
        );
        assert_eq!(ev[2].fields["words"], 2);
    }

    #[test]
    fn all_modes_and_lsb_first() {
        for mode in 0..4 {
            let spec = format!("spi:clk=0,mosi=1,cs=3,mode={mode}");
            let ev = run(&spec, mode, false, &[(0xa5, 0)], None);
            assert_eq!(ev[0].fields["mosi"], 0xa5, "mode {mode}");
            assert!(!ev[0].fields.contains_key("miso"));
        }
        let ev = run(
            "spi:clk=0,mosi=1,cs=3,order=lsb",
            0,
            true,
            &[(0x01, 0)],
            None,
        );
        assert_eq!(ev[0].fields["mosi"], 0x01);
    }

    #[test]
    fn incomplete_word_on_cs_release() {
        let ev = run("spi:clk=0,mosi=1,cs=3", 0, false, &[(0x55, 0)], Some(3));
        assert_eq!(ev[1].kind, "error");
        assert_eq!(ev[1].fields["error"], "incomplete");
    }

    #[test]
    fn needs_a_data_line() {
        assert!(build(&Spec::parse("spi:clk=0,cs=3").unwrap(), 10e6).is_err());
    }
}
