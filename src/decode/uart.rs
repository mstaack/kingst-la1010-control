//! Asynchronous serial. Each line is sampled at bit centres from the start-bit edge.

use crate::decode::{Decoder, Info, Lines, OptInfo, Spec};
use crate::error::Result;
use crate::output::Event;
use crate::stream::Edge;
use crate::units::parse_si;

const fn opt(
    name: &'static str,
    help: &'static str,
    default: Option<&'static str>,
    channel: bool,
) -> OptInfo {
    OptInfo {
        name,
        help,
        default,
        required: false,
        channel,
    }
}

pub const INFO: Info = Info {
    name: "uart",
    help: "asynchronous serial (TTL/RS-232 UART)",
    opts: &[
        opt("rx", "receive line channel", None, true),
        opt("tx", "transmit line channel", None, true),
        opt(
            "baud",
            "baud rate, or auto (estimated from the shortest pulse)",
            Some("auto"),
            false,
        ),
        opt("bits", "data bits 5-9", Some("8"), false),
        opt("parity", "n, e or o", Some("n"), false),
        opt("stop", "stop bits 1, 1.5 or 2", Some("1"), false),
        opt("invert", "inverted logic (idle low)", None, false),
    ],
};

const STANDARD: [f64; 22] = [
    300.0, 600.0, 1200.0, 2400.0, 4800.0, 9600.0, 14400.0, 19200.0, 28800.0, 38400.0, 57600.0,
    76800.0, 115200.0, 230400.0, 250000.0, 460800.0, 500000.0, 921600.0, 1e6, 1.5e6, 2e6, 3e6,
];
/// Auto-baud collects transitions until one of these: this many,
const AUTO_EDGES: usize = 256;
/// an idle gap this long (s),
const AUTO_GAP: f64 = 0.02;
/// or this long since the first transition (s).
const AUTO_SPAN: f64 = 0.2;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Parity {
    None,
    Even,
    Odd,
}

#[derive(Clone, Copy)]
struct Cfg {
    bits: u32,
    parity: Parity,
    stops: u32,
    invert: bool,
    /// Samples per bit; 0 until auto-baud has settled.
    bit: f64,
}

#[derive(Clone, Copy)]
enum St {
    Idle,
    /// Line low outside a frame; wait for idle (high) before the next start bit.
    Wait,
    Frame {
        start: f64,
        idx: u32,
        value: u16,
        parity_ok: bool,
        framing: bool,
    },
}

struct Line {
    name: &'static str,
    ch: u8,
    st: St,
    level: Option<bool>,
}

impl Line {
    fn advance(&mut self, until: f64, cfg: &Cfg, out: &mut Vec<Event>) {
        let Some(level) = self.level else { return };
        while let St::Frame { start, idx, .. } = self.st {
            if start + (f64::from(idx) + 0.5) * cfg.bit >= until {
                return;
            }
            self.sample_bit(level, cfg, out);
        }
    }

    fn sample_bit(&mut self, level: bool, cfg: &Cfg, out: &mut Vec<Event>) {
        let St::Frame {
            start,
            mut idx,
            mut value,
            mut parity_ok,
            mut framing,
        } = self.st
        else {
            return;
        };
        let n_par = u32::from(cfg.parity != Parity::None);
        if idx == 0 {
            if level {
                self.st = St::Idle; // start bit did not hold: glitch
                return;
            }
        } else if idx <= cfg.bits {
            value |= u16::from(level) << (idx - 1);
        } else if n_par == 1 && idx == cfg.bits + 1 {
            let ones = value.count_ones() + u32::from(level);
            parity_ok = match cfg.parity {
                Parity::Even => ones % 2 == 0,
                Parity::Odd => ones % 2 == 1,
                Parity::None => true,
            };
        } else if !level {
            framing = true;
        }
        idx += 1;
        if idx < 1 + cfg.bits + n_par + cfg.stops {
            self.st = St::Frame {
                start,
                idx,
                value,
                parity_ok,
                framing,
            };
            return;
        }
        out.push(self.event(start.round() as u64, value, parity_ok, framing));
        self.st = if level { St::Idle } else { St::Wait };
    }

    fn transition(&mut self, level: bool, sample: u64) {
        let first = self.level.is_none();
        self.level = Some(level);
        match self.st {
            St::Idle if first && !level => self.st = St::Wait,
            St::Idle if !level => {
                self.st = St::Frame {
                    start: sample as f64,
                    idx: 0,
                    value: 0,
                    parity_ok: true,
                    framing: false,
                }
            }
            St::Wait if level => self.st = St::Idle,
            _ => {}
        }
    }

    fn event(&self, sample: u64, value: u16, parity_ok: bool, framing: bool) -> Event {
        if framing && value == 0 {
            return Event::new(sample, "uart", "break", format!("{} break", self.name))
                .with("ch", self.name);
        }
        let text = if (0x20..0x7f).contains(&value) {
            format!("{} 0x{value:02x} '{}'", self.name, value as u8 as char)
        } else {
            format!("{} 0x{value:02x}", self.name)
        };
        let ev = if framing || !parity_ok {
            let err = if framing { "framing" } else { "parity" };
            Event::new(sample, "uart", "error", format!("{text} {err} error")).with("error", err)
        } else {
            Event::new(sample, "uart", "byte", text)
        };
        ev.with("ch", self.name).with("value", value)
    }
}

/// Edges of the UART lines buffered while the baud rate is unknown.
#[derive(Default)]
struct Auto {
    buf: Vec<Edge>,
    seen: u16,
    transitions: usize,
    first: Option<u64>,
    last: Option<u64>,
}

impl Auto {
    fn push(&mut self, e: Edge) {
        let bit = 1u16 << e.ch;
        if self.seen & bit != 0 {
            self.transitions += 1;
            self.first.get_or_insert(e.sample);
            self.last = Some(e.sample);
        }
        self.seen |= bit;
        self.buf.push(e);
    }

    fn expired(&self, t: u64, gap: u64, span: u64) -> bool {
        self.last.is_some_and(|l| t >= l + gap) || self.first.is_some_and(|f| t >= f + span)
    }

    /// Samples per bit: mean of the narrowest cluster of at least `need` pulse widths.
    fn estimate(&self, need: usize) -> Option<f64> {
        let mut prev: [Option<u64>; 16] = [None; 16];
        let mut seen = 0u16;
        let mut widths = Vec::new();
        for e in &self.buf {
            let c = usize::from(e.ch);
            if seen & (1 << c) != 0 {
                if let Some(p) = prev[c]
                    && e.sample - p >= 2
                {
                    widths.push(e.sample - p);
                }
                prev[c] = Some(e.sample);
            }
            seen |= 1 << c;
        }
        widths.sort_unstable();
        widths.iter().find_map(|&w| {
            let tol = (w / 10).max(1);
            let lo = widths.partition_point(|&x| x < w.saturating_sub(tol));
            let hi = widths.partition_point(|&x| x <= w + tol);
            let cluster = &widths[lo..hi];
            (cluster.len() >= need)
                .then(|| cluster.iter().sum::<u64>() as f64 / cluster.len() as f64)
        })
    }
}

pub struct Uart {
    lines: Vec<Line>,
    state: Lines,
    cfg: Cfg,
    rate: f64,
    auto: Option<Auto>,
}

impl Uart {
    pub fn new(spec: &Spec, rate: f64) -> Result<Self> {
        let mut lines = Vec::new();
        for name in ["rx", "tx"] {
            if let Some(ch) = spec.ch(name)? {
                lines.push(Line {
                    name,
                    ch,
                    st: St::Idle,
                    level: None,
                });
            }
        }
        if lines.is_empty() {
            return Err(spec.usage("need rx=CH and/or tx=CH"));
        }
        let bits: u32 = spec.num("bits", 8)?;
        if !(5..=9).contains(&bits) {
            return Err(spec.usage("bits must be 5-9"));
        }
        let parity = match spec.get("parity").unwrap_or("n") {
            "n" | "none" => Parity::None,
            "e" | "even" => Parity::Even,
            "o" | "odd" => Parity::Odd,
            o => return Err(spec.usage(format!("parity={o}: use n, e or o"))),
        };
        let stops = match spec.get("stop").unwrap_or("1") {
            "1" | "1.5" => 1,
            "2" => 2,
            o => return Err(spec.usage(format!("stop={o}: use 1, 1.5 or 2"))),
        };
        let (bit, auto) = match spec.get("baud").unwrap_or("auto") {
            "auto" => (0.0, Some(Auto::default())),
            b => {
                let baud = parse_si(b).ok_or_else(|| spec.usage(format!("invalid baud={b}")))?;
                if rate < 4.0 * baud {
                    return Err(spec.usage(format!(
                        "sample rate {rate} Hz is too low for {baud} baud (need at least 4x)"
                    )));
                }
                (rate / baud, None)
            }
        };
        let cfg = Cfg {
            bits,
            parity,
            stops,
            invert: spec.flag("invert"),
            bit,
        };
        Ok(Uart {
            lines,
            state: Lines::default(),
            cfg,
            rate,
            auto,
        })
    }

    fn decode(&mut self, edges: &[Edge], out: &mut Vec<Event>) {
        if self.cfg.bit <= 0.0 {
            return;
        }
        let cfg = self.cfg;
        let lines = &mut self.lines;
        self.state.steps(edges, |st| {
            for l in lines.iter_mut() {
                if st.first(l.ch) || st.rose(l.ch) || st.fell(l.ch) {
                    l.advance(st.sample as f64, &cfg, out);
                    l.transition(st.high(l.ch) ^ cfg.invert, st.sample);
                }
            }
        });
    }

    fn is_line(&self, ch: u8) -> bool {
        self.lines.iter().any(|l| l.ch == ch)
    }

    fn auto_limits(&self) -> (u64, u64) {
        (
            (AUTO_GAP * self.rate) as u64,
            (AUTO_SPAN * self.rate) as u64,
        )
    }

    /// Estimate the baud rate from the buffered edges, then decode them. Unless `force`,
    /// wait for more edges when no clear cluster of pulse widths exists yet.
    fn settle(&mut self, force: bool, out: &mut Vec<Event>) -> bool {
        let Some(a) = &mut self.auto else { return true };
        let width = a
            .estimate(3)
            .or_else(|| if force { a.estimate(1) } else { None });
        let Some(width) = width else {
            if force {
                self.auto = None;
                out.push(Event::new(
                    0,
                    "uart",
                    "info",
                    "baud auto-detection failed: not enough edges",
                ));
            } else {
                (a.first, a.last) = (None, None);
            }
            return false;
        };
        let buf = std::mem::take(&mut a.buf);
        self.auto = None;
        let raw = self.rate / width;
        let (baud, note) = match STANDARD.iter().find(|b| (raw / *b - 1.0).abs() < 0.04) {
            Some(&b) => (b, ""),
            None => (raw.round(), ", non-standard"),
        };
        self.cfg.bit = self.rate / baud;
        out.push(
            Event::new(0, "uart", "info", format!("baud {baud} (auto{note})")).with("baud", baud),
        );
        self.decode(&buf, out);
        true
    }
}

impl Decoder for Uart {
    fn feed(&mut self, edges: &[Edge], out: &mut Vec<Event>) {
        let mut rest = edges;
        if self.auto.is_some() {
            rest = &[];
            let (gap, span) = self.auto_limits();
            for (i, e) in edges.iter().enumerate() {
                if !self.is_line(e.ch) {
                    continue;
                }
                let a = self.auto.as_ref().expect("auto");
                if a.expired(e.sample, gap, span) && self.settle(false, out) {
                    rest = &edges[i..];
                    break;
                }
                let a = self.auto.as_mut().expect("auto");
                a.push(*e);
                if a.transitions >= AUTO_EDGES && self.settle(true, out) {
                    rest = &edges[i + 1..];
                    break;
                }
            }
        }
        self.decode(rest, out);
    }

    fn advance(&mut self, now: u64, out: &mut Vec<Event>) {
        let (gap, span) = self.auto_limits();
        if let Some(a) = &self.auto
            && !(a.expired(now, gap, span) && self.settle(false, out))
        {
            return;
        }
        let cfg = self.cfg;
        if cfg.bit > 0.0 {
            for l in &mut self.lines {
                l.advance(now as f64, &cfg, out);
            }
        }
    }

    fn finish(&mut self, end: u64, out: &mut Vec<Event>) {
        if self.auto.is_some() {
            self.settle(true, out);
        }
        let cfg = self.cfg;
        if cfg.bit > 0.0 {
            for l in &mut self.lines {
                l.advance(end as f64, &cfg, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::decode::{Spec, build};
    use crate::output::Event;
    use crate::stream::Edge;
    use crate::testgen::{Wave, uart_bits, uart_frame};

    const RATE: f64 = 10e6;

    fn line(bytes: &[u16], baud: f64, bits: u8, parity: Option<bool>, invert: bool) -> Vec<Edge> {
        let mut w = Wave::new();
        w.set(0, 0, !invert);
        let bit = RATE / baud;
        let mut t = 1000.0;
        for &b in bytes {
            let mut f = uart_bits(b, bits, parity, 1);
            if invert {
                f.iter_mut().for_each(|x| *x = !*x);
            }
            t = uart_frame(&mut w, 0, t, bit, &f) + 3.0 * bit;
        }
        w.edges()
    }

    fn decode(spec: &str, edges: &[Edge]) -> Vec<Event> {
        let mut d = build(&Spec::parse(spec).unwrap(), RATE).unwrap();
        let mut out = Vec::new();
        d.feed(edges, &mut out);
        d.finish(edges.last().map_or(0, |e| e.sample) + 100_000, &mut out);
        out
    }

    fn values(ev: &[Event]) -> Vec<(String, String)> {
        ev.iter()
            .map(|e| (e.kind.to_string(), e.text.clone()))
            .collect()
    }

    #[test]
    fn decodes_ascii() {
        let ev = decode(
            "uart:rx=0,baud=115200",
            &line(&[0x48, 0x69], 115200.0, 8, None, false),
        );
        assert_eq!(
            values(&ev),
            vec![
                ("byte".into(), "rx 0x48 'H'".into()),
                ("byte".into(), "rx 0x69 'i'".into())
            ]
        );
        assert_eq!(ev[0].fields["value"], 0x48);
        assert_eq!(ev[0].fields["ch"], "rx");
        assert!(ev[0].sample >= 1000 && ev[0].sample <= 1001);
    }

    #[test]
    fn parity_checked() {
        let edges = line(&[0x41], 9600.0, 8, Some(true), false);
        assert_eq!(
            decode("uart:rx=0,baud=9600,parity=e", &edges)[0].kind,
            "byte"
        );
        let bad = decode("uart:rx=0,baud=9600,parity=o", &edges);
        assert_eq!(bad[0].kind, "error");
        assert_eq!(bad[0].fields["error"], "parity");
    }

    #[test]
    fn framing_error_and_break() {
        let bit = RATE / 9600.0;
        let mut w = Wave::new();
        w.set(0, 0, true);
        let mut f = uart_bits(0x41, 8, None, 1);
        *f.last_mut().unwrap() = false;
        let end = uart_frame(&mut w, 0, 1000.0, bit, &f);
        w.set((end + bit) as u64, 0, true);
        // break: 20 bit times low
        w.set((end + 10.0 * bit) as u64, 0, false);
        w.set((end + 30.0 * bit) as u64, 0, true);
        let ev = decode("uart:rx=0,baud=9600", &w.edges());
        assert_eq!(ev[0].fields["error"], "framing");
        assert_eq!(ev[1].kind, "break");
        assert_eq!(ev.len(), 2);
    }

    #[test]
    fn inverted_line() {
        let ev = decode(
            "uart:rx=0,baud=115200,invert",
            &line(&[0x5a], 115200.0, 8, None, true),
        );
        assert_eq!(ev[0].fields["value"], 0x5a);
    }

    #[test]
    fn auto_baud() {
        let ev = decode("uart:rx=0", &line(&[0x55; 40], 115200.0, 8, None, false));
        assert_eq!(ev[0].kind, "info");
        assert_eq!(ev[0].fields["baud"], 115200.0);
        assert_eq!(ev.iter().filter(|e| e.kind == "byte").count(), 40);
    }

    #[test]
    fn batch_split_is_invariant() {
        let edges = line(b"hello".map(u16::from).as_slice(), 57600.0, 8, None, false);
        let whole = decode("uart:rx=0,baud=57600", &edges);
        let mut d = build(&Spec::parse("uart:rx=0,baud=57600").unwrap(), RATE).unwrap();
        let mut out = Vec::new();
        for e in &edges {
            d.feed(std::slice::from_ref(e), &mut out);
        }
        d.finish(edges.last().unwrap().sample + 100_000, &mut out);
        assert_eq!(out, whole);
        assert_eq!(whole.len(), 5);
    }

    fn ok_line() -> Vec<Edge> {
        line(
            b"OK\r\n".map(u16::from).as_slice(),
            115200.0,
            8,
            None,
            false,
        )
    }

    #[test]
    fn bytes_emitted_without_waiting_for_next_edge() {
        let edges = ok_line();
        let mut d = build(&Spec::parse("uart:rx=0,baud=115200").unwrap(), RATE).unwrap();
        let mut out = Vec::new();
        d.feed(&edges, &mut out);
        d.advance(edges.last().unwrap().sample + 200, &mut out);
        assert_eq!(
            out.iter().filter(|e| e.kind == "byte").count(),
            4,
            "{out:?}"
        );
    }

    #[test]
    fn auto_baud_settles_after_idle_gap() {
        let edges = ok_line();
        let mut d = build(&Spec::parse("uart:rx=0").unwrap(), RATE).unwrap();
        let mut out = Vec::new();
        d.feed(&edges, &mut out);
        d.advance(
            edges.last().unwrap().sample + (0.03 * RATE) as u64,
            &mut out,
        );
        assert_eq!(out[0].fields["baud"], 115200.0, "{out:?}");
        assert_eq!(out.iter().filter(|e| e.kind == "byte").count(), 4);
    }

    #[test]
    fn auto_baud_ignores_single_glitch() {
        let mut edges = line(&[0x55; 40], 115200.0, 8, None, false);
        edges.splice(
            1..1,
            [
                Edge {
                    sample: 500,
                    ch: 0,
                    level: false,
                },
                Edge {
                    sample: 502,
                    ch: 0,
                    level: true,
                },
            ],
        );
        let ev = decode("uart:rx=0", &edges);
        assert_eq!(ev[0].fields["baud"], 115200.0, "{:?}", ev[0]);
        assert_eq!(ev.iter().filter(|e| e.kind == "byte").count(), 40);
    }

    #[test]
    fn auto_baud_is_batch_invariant_and_flags_nonstandard() {
        let mut bytes = vec![0xF0u16; 140];
        bytes.extend(b"Hello".map(u16::from));
        let edges = line(&bytes, 115200.0, 8, None, false);
        let whole = decode("uart:rx=0", &edges);
        let mut d = build(&Spec::parse("uart:rx=0").unwrap(), RATE).unwrap();
        let mut out = Vec::new();
        for e in &edges {
            d.feed(std::slice::from_ref(e), &mut out);
        }
        d.finish(edges.last().unwrap().sample + 100_000, &mut out);
        assert_eq!(out, whole);
        assert!(whole[0].text.contains("non-standard"), "{:?}", whole[0]);
    }

    #[test]
    fn auto_buffer_ignores_other_channels() {
        let mut d = build(&Spec::parse("uart:rx=0").unwrap(), RATE).unwrap();
        let mut out = Vec::new();
        let other: Vec<Edge> = (0..300)
            .map(|i| Edge {
                sample: i * 10,
                ch: 1,
                level: i % 2 == 0,
            })
            .collect();
        d.feed(&other, &mut out);
        d.finish(4000, &mut out);
        assert!(out.iter().all(|e| e.kind == "info"), "{out:?}");
    }

    #[test]
    fn rate_too_low_is_usage_error() {
        let err = build(&Spec::parse("uart:rx=0,baud=5M").unwrap(), RATE)
            .err()
            .unwrap();
        assert_eq!(err.exit_code(), 1);
    }

    #[test]
    fn needs_a_line() {
        assert!(build(&Spec::parse("uart:baud=9600").unwrap(), RATE).is_err());
    }
}
