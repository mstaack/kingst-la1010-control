//! I²C: START/STOP from SDA edges while SCL is high, bits sampled on SCL rising edges.

use crate::decode::{Decoder, Info, Lines, OptInfo, Spec};
use crate::error::Result;
use crate::output::Event;
use crate::stream::Edge;

pub const INFO: Info = Info {
    name: "i2c",
    help: "I2C / TWI / SMBus (7-bit addressing)",
    opts: &[
        OptInfo {
            name: "scl",
            help: "clock channel",
            default: None,
            required: true,
            channel: true,
        },
        OptInfo {
            name: "sda",
            help: "data channel",
            default: None,
            required: true,
            channel: true,
        },
    ],
};

pub struct I2c {
    scl: u8,
    sda: u8,
    lines: Lines,
    active: bool,
    first: bool,
    nbits: u8,
    byte: u8,
    byte_start: u64,
}

impl I2c {
    pub fn new(spec: &Spec) -> Result<Self> {
        let req = |k: &str| -> Result<u8> {
            spec.ch(k)?
                .ok_or_else(|| spec.usage(format!("missing {k}=")))
        };
        Ok(I2c {
            scl: req("scl")?,
            sda: req("sda")?,
            lines: Lines::default(),
            active: false,
            first: false,
            nbits: 0,
            byte: 0,
            byte_start: 0,
        })
    }
}

fn ack_text(ack: bool) -> &'static str {
    if ack { "ACK" } else { "NACK" }
}

impl Decoder for I2c {
    fn feed(&mut self, edges: &[Edge], out: &mut Vec<Event>) {
        let I2c {
            scl,
            sda,
            lines,
            active,
            first,
            nbits,
            byte,
            byte_start,
        } = self;
        let (scl, sda) = (*scl, *sda);
        lines.steps(edges, |st| {
            if st.was_known(scl) && st.was_high(scl) && st.high(scl) {
                if st.fell(sda) {
                    let text = if *active { "Sr" } else { "S" };
                    out.push(Event::new(st.sample, "i2c", "start", text));
                    *active = true;
                    *first = true;
                    *nbits = 0;
                    *byte = 0;
                    return;
                }
                if st.rose(sda) && *active {
                    out.push(Event::new(st.sample, "i2c", "stop", "P"));
                    *active = false;
                    return;
                }
            }
            if !*active || !st.rose(scl) {
                return;
            }
            let bit = st.high(sda);
            if *nbits == 0 {
                *byte_start = st.sample;
            }
            if *nbits < 8 {
                *byte = *byte << 1 | u8::from(bit);
                *nbits += 1;
                return;
            }
            let ack = !bit;
            let ev = if *first {
                let (addr, rw) = (*byte >> 1, if *byte & 1 == 1 { "R" } else { "W" });
                Event::new(
                    *byte_start,
                    "i2c",
                    "address",
                    format!("{rw} 0x{addr:02x} {}", ack_text(ack)),
                )
                .with("addr", addr)
                .with("rw", rw)
            } else {
                Event::new(
                    *byte_start,
                    "i2c",
                    "data",
                    format!("data 0x{:02x} {}", *byte, ack_text(ack)),
                )
                .with("value", *byte)
            };
            out.push(ev.with("ack", ack));
            *nbits = 0;
            *byte = 0;
            *first = false;
        });
    }

    fn finish(&mut self, _end: u64, _out: &mut Vec<Event>) {}
}

#[cfg(test)]
mod tests {
    use crate::decode::{Spec, build};
    use crate::output::Event;
    use crate::stream::Edge;
    use crate::testgen::{Wave, i2c_byte, i2c_restart, i2c_start, i2c_stop};

    const H: u64 = 50; // 100 kHz at 10 MSa/s

    fn bus() -> Wave {
        let mut w = Wave::new();
        w.set(0, 1, true); // scl
        w.set(0, 0, true); // sda
        w
    }

    fn run(edges: &[Edge]) -> Vec<Event> {
        let mut d = build(&Spec::parse("i2c:scl=1,sda=0").unwrap(), 10e6).unwrap();
        let mut out = Vec::new();
        d.feed(edges, &mut out);
        d.finish(edges.last().unwrap().sample + 10, &mut out);
        out
    }

    fn texts(ev: &[Event]) -> Vec<&str> {
        ev.iter().map(|e| e.text.as_str()).collect()
    }

    #[test]
    fn write_transaction() {
        let mut w = bus();
        let mut t = i2c_start(&mut w, 1, 0, 100, H);
        t = i2c_byte(&mut w, 1, 0, t, H, 0xA0, true);
        t = i2c_byte(&mut w, 1, 0, t, H, 0x00, true);
        t = i2c_byte(&mut w, 1, 0, t, H, 0xAB, false);
        i2c_stop(&mut w, 1, 0, t, H);
        let ev = run(&w.edges());
        assert_eq!(
            texts(&ev),
            vec!["S", "W 0x50 ACK", "data 0x00 ACK", "data 0xab NACK", "P"]
        );
        assert_eq!(ev[1].fields["addr"], 0x50);
        assert_eq!(ev[1].fields["rw"], "W");
        assert_eq!(ev[3].fields["ack"], false);
    }

    #[test]
    fn repeated_start_read() {
        let mut w = bus();
        let mut t = i2c_start(&mut w, 1, 0, 100, H);
        t = i2c_byte(&mut w, 1, 0, t, H, 0xA0, true);
        t = i2c_byte(&mut w, 1, 0, t, H, 0x10, true);
        t = i2c_restart(&mut w, 1, 0, t, H);
        t = i2c_byte(&mut w, 1, 0, t, H, 0xA1, true);
        t = i2c_byte(&mut w, 1, 0, t, H, 0x42, false);
        i2c_stop(&mut w, 1, 0, t, H);
        assert_eq!(
            texts(&run(&w.edges())),
            vec![
                "S",
                "W 0x50 ACK",
                "data 0x10 ACK",
                "Sr",
                "R 0x50 ACK",
                "data 0x42 NACK",
                "P"
            ]
        );
    }

    #[test]
    fn batch_split_is_invariant() {
        let mut w = bus();
        let t = i2c_start(&mut w, 1, 0, 100, H);
        let t = i2c_byte(&mut w, 1, 0, t, H, 0x3C, true);
        i2c_stop(&mut w, 1, 0, t, H);
        let edges = w.edges();
        let whole = run(&edges);
        let mut d = build(&Spec::parse("i2c:scl=1,sda=0").unwrap(), 10e6).unwrap();
        let mut out = Vec::new();
        for e in &edges {
            d.feed(std::slice::from_ref(e), &mut out);
        }
        d.finish(edges.last().unwrap().sample + 10, &mut out);
        assert_eq!(out, whole);
    }

    #[test]
    fn requires_both_lines() {
        assert!(Spec::parse("i2c:scl=1").is_err());
    }
}
