//! USB 1.x low/full speed from D+/D-: line states -> NRZI bits -> packets -> transactions.

use crate::decode::{Decoder, Info, Lines, OptInfo, Spec};
use crate::error::Result;
use crate::output::Event;
use crate::stream::Edge;
use crate::units::format_time;

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
    name: "usb",
    help: "USB 1.x low/full speed (not high speed); needs >= 48 MHz for full speed",
    opts: &[
        opt("dp", "D+ channel", None, true, true),
        opt("dm", "D- channel", None, true, true),
        opt("speed", "auto, fs or ls", Some("auto"), false, false),
        opt("show_sof", "print SOF packets", None, false, false),
        opt(
            "naks",
            "print NAKed transactions (default: count only)",
            None,
            false,
            false,
        ),
        opt("packets", "also print every packet", None, false, false),
    ],
};

pub fn crc5_bits(bits: impl IntoIterator<Item = bool>) -> u8 {
    let mut crc: u8 = 0x1f;
    for b in bits {
        let fb = (crc & 1) ^ u8::from(b);
        crc >>= 1;
        if fb != 0 {
            crc ^= 0x14;
        }
    }
    !crc & 0x1f
}

/// CRC5 over the 11-bit token field (address | endpoint << 7, or frame number).
pub fn crc5_11(v: u16) -> u8 {
    crc5_bits((0..11).map(|i| v >> i & 1 == 1))
}

pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xffff;
    for &b in data {
        crc ^= u16::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xA001
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

pub fn pid_name(pid: u8) -> &'static str {
    match pid {
        0x1 => "OUT",
        0x9 => "IN",
        0x5 => "SOF",
        0xD => "SETUP",
        0x3 => "DATA0",
        0xB => "DATA1",
        0x7 => "DATA2",
        0xF => "MDATA",
        0x2 => "ACK",
        0xA => "NAK",
        0xE => "STALL",
        0x6 => "NYET",
        0xC => "PRE",
        0x8 => "SPLIT",
        0x4 => "PING",
        _ => "RESERVED",
    }
}

/// Name a standard SETUP request (8-byte setup packet). None for class/vendor requests.
pub fn describe_setup(d: &[u8]) -> Option<String> {
    if d.len() != 8 || d[0] & 0x60 != 0 {
        return None;
    }
    let value = u16::from_le_bytes([d[2], d[3]]);
    let len = u16::from_le_bytes([d[6], d[7]]);
    Some(match d[1] {
        0 => "GET_STATUS".into(),
        1 => "CLEAR_FEATURE".into(),
        3 => "SET_FEATURE".into(),
        5 => format!("SET_ADDRESS {value}"),
        6 => {
            let ty = match d[3] {
                1 => "Device",
                2 => "Configuration",
                3 => return Some(format!("GET_DESCRIPTOR String idx={} len={len}", d[2])),
                4 => "Interface",
                5 => "Endpoint",
                6 => "Device_Qualifier",
                7 => "Other_Speed_Configuration",
                15 => "BOS",
                0x21 => "HID",
                0x22 => "Report",
                _ => "Unknown",
            };
            format!("GET_DESCRIPTOR {ty} len={len}")
        }
        7 => "SET_DESCRIPTOR".into(),
        8 => "GET_CONFIGURATION".into(),
        9 => format!("SET_CONFIGURATION {value}"),
        10 => "GET_INTERFACE".into(),
        11 => format!("SET_INTERFACE alt={value}"),
        12 => "SYNCH_FRAME".into(),
        r => format!("request 0x{r:02x}"),
    })
}

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_spaced(d: &[u8]) -> String {
    d.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn err(sample: u64, code: &'static str, text: impl Into<String>) -> Event {
    Event::new(sample, "usb", "error", text).with("error", code)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Sym {
    J,
    K,
    Se0,
    Se1,
}

struct Raw {
    start: u64,
    bits: Vec<bool>,
    ones: u32,
    stuff_err: bool,
}

impl Raw {
    fn push(&mut self, b: bool) {
        if self.ones == 6 {
            self.ones = 0;
            if !b {
                return; // stuffed bit
            }
            self.stuff_err = true;
        }
        self.bits.push(b);
        self.ones = if b { self.ones + 1 } else { 0 };
    }

    /// A run of `n` bit times of one J/K state: transition (0) then n-1 ones.
    fn run(&mut self, n: u64) {
        self.push(false);
        for _ in 1..n {
            self.push(true);
        }
    }
}

enum Packet {
    Token { pid: u8, addr: u8, ep: u8 },
    Sof { frame: u16 },
    Data { pid: u8, data: Vec<u8> },
    Handshake { pid: u8 },
    Other { pid: u8 },
}

struct Txn {
    start: u64,
    token: Option<(u8, u8, u8)>,
    data: Option<(u8, Vec<u8>)>,
}

pub struct Usb {
    dp: u8,
    dm: u8,
    rate: f64,
    ls: Option<bool>,
    bit: f64,
    lines: Lines,
    sym: Option<Sym>,
    since: u64,
    raw: Option<Raw>,
    suspended: bool,
    show_sof: bool,
    show_naks: bool,
    packets: bool,
    naks: u64,
    txn: Option<Txn>,
}

impl Usb {
    pub fn new(spec: &Spec, rate: f64) -> Result<Self> {
        let dp = spec.ch("dp")?.ok_or_else(|| spec.usage("missing dp="))?;
        let dm = spec.ch("dm")?.ok_or_else(|| spec.usage("missing dm="))?;
        if dp == dm {
            return Err(spec.usage("dp and dm must be different channels"));
        }
        let ls = match spec.get("speed").unwrap_or("auto") {
            "auto" => None,
            "fs" | "full" => Some(false),
            "ls" | "low" => Some(true),
            o => return Err(spec.usage(format!("speed={o}: use auto, fs or ls"))),
        };
        let min = if ls == Some(false) { 48e6 } else { 6e6 };
        if rate < min {
            return Err(spec.usage(format!(
                "sample rate {rate} Hz is too low (need at least {min} Hz; use 100M for full speed)"
            )));
        }
        let mut u = Usb {
            dp,
            dm,
            rate,
            ls: None,
            bit: 0.0,
            lines: Lines::default(),
            sym: None,
            since: 0,
            raw: None,
            suspended: false,
            show_sof: spec.flag("show_sof"),
            show_naks: spec.flag("naks"),
            packets: spec.flag("packets"),
            naks: 0,
            txn: None,
        };
        if let Some(l) = ls {
            u.set_speed(l);
        }
        Ok(u)
    }

    fn set_speed(&mut self, ls: bool) {
        self.ls = Some(ls);
        self.bit = self.rate / if ls { 1.5e6 } else { 12e6 };
    }

    fn sym(&self, dp: bool, dm: bool) -> Sym {
        let ls = self.ls == Some(true);
        match (dp, dm) {
            (false, false) => Sym::Se0,
            (true, true) => Sym::Se1,
            (true, false) => {
                if ls {
                    Sym::K
                } else {
                    Sym::J
                }
            }
            (false, true) => {
                if ls {
                    Sym::J
                } else {
                    Sym::K
                }
            }
        }
    }

    fn step(&mut self, sample: u64, dp: bool, dm: bool, out: &mut Vec<Event>) {
        if self.ls.is_none() {
            let ls = match (dp, dm) {
                (true, false) => false,
                (false, true) => true,
                _ => return,
            };
            self.set_speed(ls);
            let name = if ls { "low" } else { "full" };
            out.push(
                Event::new(sample, "usb", "info", format!("{name} speed (auto)"))
                    .with("speed", name),
            );
            if !ls && self.rate < 48e6 {
                out.push(err(
                    sample,
                    "rate",
                    "sample rate too low for full speed (need >= 48 MHz)",
                ));
            }
        }
        let new = self.sym(dp, dm);
        match self.sym {
            Some(old) if old == new => {}
            Some(old) => {
                let dur = sample - self.since;
                if (dur as f64) < 0.5 * self.bit {
                    self.sym = Some(new); // glitch: its time goes to the next run
                    return;
                }
                self.run(old, self.since, dur, out);
                self.since = sample;
                self.sym = Some(new);
            }
            None => {
                self.since = sample;
                self.sym = Some(new);
            }
        }
    }

    fn run(&mut self, sym: Sym, start: u64, dur: u64, out: &mut Vec<Event>) {
        let n = ((dur as f64 / self.bit).round() as u64).max(1);
        let secs = dur as f64 / self.rate;
        if self.raw.is_none() {
            match sym {
                Sym::J if secs >= 3e-3 && !self.suspended => {
                    self.suspended = true;
                    self.flush_txn(out);
                    let at = start + (3e-3 * self.rate) as u64;
                    out.push(Event::new(at, "usb", "suspend", "suspend"));
                }
                Sym::Se0 if secs >= 2.5e-6 => {
                    self.flush_txn(out);
                    self.suspended = false;
                    out.push(
                        Event::new(
                            start,
                            "usb",
                            "reset",
                            format!("bus reset {}", format_time(secs)),
                        )
                        .with("duration_s", secs),
                    );
                }
                Sym::K => {
                    if self.suspended {
                        self.suspended = false;
                        out.push(Event::new(start, "usb", "resume", "resume"));
                        if n > 8 {
                            return;
                        }
                    }
                    let mut raw = Raw {
                        start,
                        bits: Vec::new(),
                        ones: 0,
                        stuff_err: false,
                    };
                    raw.run(n);
                    self.raw = Some(raw);
                }
                _ => {}
            }
            return;
        }
        match sym {
            Sym::J | Sym::K => self.raw.as_mut().expect("in packet").run(n),
            Sym::Se0 => {
                let raw = self.raw.take().expect("in packet");
                self.packet(raw, out);
            }
            Sym::Se1 => {
                let raw = self.raw.take().expect("in packet");
                out.push(err(raw.start, "se1", "SE1 line state inside packet"));
            }
        }
    }

    fn packet(&mut self, raw: Raw, out: &mut Vec<Event>) {
        let start = raw.start;
        if raw.stuff_err {
            out.push(err(start, "bitstuff", "bit-stuffing violation"));
            return;
        }
        let bytes: Vec<u8> = raw
            .bits
            .chunks_exact(8)
            .map(|c| c.iter().rev().fold(0u8, |a, &b| a << 1 | u8::from(b)))
            .collect();
        if bytes.len() < 2 || bytes[0] != 0x80 {
            out.push(err(start, "sync", "bad SYNC"));
            return;
        }
        let p = bytes[1];
        if p & 0x0f != !(p >> 4) & 0x0f {
            out.push(err(start, "pid", format!("bad PID byte 0x{p:02x}")));
            return;
        }
        let pid = p & 0x0f;
        let body = &bytes[2..];
        let short = || {
            err(
                start,
                "short",
                format!("{} packet too short", pid_name(pid)),
            )
        };
        let pkt = match pid {
            0x1 | 0x9 | 0xD | 0x4 | 0x5 => {
                if body.len() < 2 {
                    out.push(short());
                    return;
                }
                let v = u16::from_le_bytes([body[0], body[1]]);
                if crc5_11(v & 0x7ff) != (v >> 11) as u8 {
                    out.push(err(
                        start,
                        "crc5",
                        format!("{} CRC5 mismatch", pid_name(pid)),
                    ));
                    return;
                }
                if pid == 0x5 {
                    Packet::Sof { frame: v & 0x7ff }
                } else {
                    Packet::Token {
                        pid,
                        addr: (v & 0x7f) as u8,
                        ep: (v >> 7 & 0xf) as u8,
                    }
                }
            }
            0x3 | 0xB | 0x7 | 0xF => {
                if body.len() < 2 {
                    out.push(short());
                    return;
                }
                let (data, crc) = body.split_at(body.len() - 2);
                if crc16(data) != u16::from_le_bytes([crc[0], crc[1]]) {
                    out.push(
                        err(start, "crc16", format!("{} CRC16 mismatch", pid_name(pid)))
                            .with("data", hex(data)),
                    );
                    return;
                }
                Packet::Data {
                    pid,
                    data: data.to_vec(),
                }
            }
            0x2 | 0xA | 0xE | 0x6 => Packet::Handshake { pid },
            _ => Packet::Other { pid },
        };
        if self.packets {
            out.push(packet_event(start, &pkt));
        }
        self.transact(start, pkt, out);
    }

    fn transact(&mut self, start: u64, p: Packet, out: &mut Vec<Event>) {
        match p {
            Packet::Token { pid, addr, ep } => {
                self.flush_txn(out);
                self.txn = Some(Txn {
                    start,
                    token: Some((pid, addr, ep)),
                    data: None,
                });
            }
            Packet::Sof { frame } => {
                self.flush_txn(out);
                if self.show_sof {
                    out.push(
                        Event::new(start, "usb", "sof", format!("SOF {frame}"))
                            .with("frame", frame),
                    );
                }
            }
            Packet::Data { pid, data } => {
                if let Some(t) = self.txn.as_mut().filter(|t| t.data.is_none()) {
                    t.data = Some((pid, data));
                } else {
                    self.flush_txn(out);
                    self.txn = Some(Txn {
                        start,
                        token: None,
                        data: Some((pid, data)),
                    });
                }
            }
            Packet::Handshake { pid } => {
                let t = self.txn.take().unwrap_or(Txn {
                    start,
                    token: None,
                    data: None,
                });
                self.emit_txn(t, Some(pid), out);
            }
            Packet::Other { pid } => {
                self.flush_txn(out);
                if !self.packets {
                    out.push(packet_event(start, &Packet::Other { pid }));
                }
            }
        }
    }

    fn flush_txn(&mut self, out: &mut Vec<Event>) {
        if let Some(t) = self.txn.take() {
            self.emit_txn(t, None, out);
        }
    }

    fn emit_txn(&mut self, t: Txn, hs: Option<u8>, out: &mut Vec<Event>) {
        if hs == Some(0xA) && !self.show_naks {
            self.naks += 1;
            return;
        }
        let mut parts = Vec::new();
        let mut ev = Event::new(t.start, "usb", "transaction", "");
        if let Some((pid, addr, ep)) = t.token {
            parts.push(format!("{} addr={addr} ep={ep}", pid_name(pid)));
            ev = ev
                .with("pid", pid_name(pid))
                .with("addr", addr)
                .with("ep", ep);
        }
        if let Some((dpid, data)) = &t.data {
            parts.push(format!("{} [{}]", pid_name(*dpid), hex_spaced(data)));
            ev = ev.with("data_pid", pid_name(*dpid)).with("data", hex(data));
            if t.token.is_some_and(|tk| tk.0 == 0xD)
                && let Some(r) = describe_setup(data)
            {
                parts.push(r.clone());
                ev = ev.with("request", r);
            }
        }
        let hs_name = hs.map(pid_name);
        parts.push(hs_name.unwrap_or("no handshake").to_string());
        ev = ev.with("handshake", hs_name);
        ev.text = parts.join(" ");
        out.push(ev);
    }
}

fn packet_event(start: u64, p: &Packet) -> Event {
    match p {
        Packet::Token { pid, addr, ep } => Event::new(
            start,
            "usb",
            "packet",
            format!("{} addr={addr} ep={ep}", pid_name(*pid)),
        )
        .with("pid", pid_name(*pid))
        .with("addr", *addr)
        .with("ep", *ep),
        Packet::Sof { frame } => Event::new(start, "usb", "packet", format!("SOF {frame}"))
            .with("pid", "SOF")
            .with("frame", *frame),
        Packet::Data { pid, data } => Event::new(
            start,
            "usb",
            "packet",
            format!("{} [{}]", pid_name(*pid), hex_spaced(data)),
        )
        .with("pid", pid_name(*pid))
        .with("data", hex(data)),
        Packet::Handshake { pid } | Packet::Other { pid } => {
            Event::new(start, "usb", "packet", pid_name(*pid)).with("pid", pid_name(*pid))
        }
    }
}

impl Decoder for Usb {
    fn feed(&mut self, edges: &[Edge], out: &mut Vec<Event>) {
        let mut lines = std::mem::take(&mut self.lines);
        let (dp, dm) = (self.dp, self.dm);
        lines.steps(edges, |st| {
            let known = |c| st.was_known(c) || st.touched(c);
            if (st.touched(dp) || st.touched(dm)) && known(dp) && known(dm) {
                self.step(st.sample, st.high(dp), st.high(dm), out);
            }
        });
        self.lines = lines;
    }

    fn finish(&mut self, end: u64, out: &mut Vec<Event>) {
        if self.raw.is_none() && self.sym == Some(Sym::J) {
            self.run(Sym::J, self.since, end.saturating_sub(self.since), out);
        }
        if let Some(raw) = self.raw.take() {
            out.push(err(
                raw.start,
                "truncated",
                "packet cut off at end of capture",
            ));
        }
        self.flush_txn(out);
        if self.naks > 0 {
            out.push(
                Event::new(
                    end,
                    "usb",
                    "info",
                    format!(
                        "{} NAKed transactions not shown (add naks to show)",
                        self.naks
                    ),
                )
                .with("naks", self.naks),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::{Spec, build};
    use crate::testgen::*;

    const RATE: f64 = 100e6;
    const FS: f64 = RATE / 12e6;
    const SETUP: u8 = 0xD;
    const IN: u8 = 0x9;
    const OUT: u8 = 0x1;
    const DATA0: u8 = 0x3;
    const ACK: u8 = 0x2;
    const NAK: u8 = 0xA;

    struct Bus {
        w: Wave,
        t: f64,
        bit: f64,
        ls: bool,
        skew: u64,
    }

    impl Bus {
        fn new(ls: bool, bit: f64) -> Self {
            let mut w = Wave::new();
            usb_idle(&mut w, 0, 1, 0, ls);
            Bus {
                w,
                t: 1000.0,
                bit,
                ls,
                skew: 0,
            }
        }
        fn send(&mut self, bytes: &[u8]) -> &mut Self {
            self.t = usb_packet(
                &mut self.w,
                0,
                1,
                self.t,
                self.bit,
                self.ls,
                self.skew,
                bytes,
            ) + 4.0 * self.bit;
            self
        }
        fn decode(&self, spec: &str, rate: f64) -> Vec<Event> {
            let mut d = build(&Spec::parse(spec).unwrap(), rate).unwrap();
            let mut out = Vec::new();
            d.feed(&self.w.edges(), &mut out);
            d.finish(self.t as u64 + 1000, &mut out);
            out
        }
    }

    fn kinds<'a>(ev: &'a [Event], kind: &str) -> Vec<&'a Event> {
        ev.iter().filter(|e| e.kind == kind).collect()
    }

    #[test]
    fn crc_vectors() {
        assert_eq!(crc16(b"123456789"), 0xB4C8); // CRC-16/USB check value
        let bits = b"123456789"
            .iter()
            .flat_map(|b| (0..8).map(move |i| b >> i & 1 == 1));
        assert_eq!(crc5_bits(bits), 0x19); // CRC-5/USB check value
        assert_eq!(usb_token(SETUP, 0, 0), vec![0x2D, 0x00, 0x10]);
    }

    #[test]
    fn setup_transaction() {
        let mut b = Bus::new(false, FS);
        b.send(&usb_token(SETUP, 0, 0))
            .send(&usb_data(DATA0, &[0x80, 6, 0, 1, 0, 0, 0x40, 0]))
            .send(&usb_handshake(ACK));
        let ev = b.decode("usb:dp=0,dm=1", RATE);
        assert_eq!(ev[0].fields["speed"], "full");
        let tx = kinds(&ev, "transaction");
        assert_eq!(tx.len(), 1);
        assert_eq!(
            tx[0].text,
            "SETUP addr=0 ep=0 DATA0 [80 06 00 01 00 00 40 00] GET_DESCRIPTOR Device len=64 ACK"
        );
        assert_eq!(tx[0].fields["data"], "8006000100004000");
        assert_eq!(tx[0].fields["handshake"], "ACK");
        assert!(kinds(&ev, "error").is_empty());
    }

    #[test]
    fn bit_stuffing_payload() {
        let mut b = Bus::new(false, FS);
        b.send(&usb_token(OUT, 3, 2))
            .send(&usb_data(DATA0, &[0xff; 4]))
            .send(&usb_handshake(ACK));
        let tx = b.decode("usb:dp=0,dm=1", RATE);
        assert_eq!(kinds(&tx, "transaction")[0].fields["data"], "ffffffff");
    }

    #[test]
    fn crc16_error_reported() {
        let mut b = Bus::new(false, FS);
        let mut data = usb_data(DATA0, &[1, 2, 3]);
        *data.last_mut().unwrap() ^= 0x01;
        b.send(&usb_token(OUT, 1, 1))
            .send(&data)
            .send(&usb_handshake(ACK));
        let ev = b.decode("usb:dp=0,dm=1", RATE);
        assert_eq!(kinds(&ev, "error")[0].fields["error"], "crc16");
    }

    #[test]
    fn sof_hidden_unless_requested() {
        let mut b = Bus::new(false, FS);
        b.send(&usb_sof(1234));
        assert!(kinds(&b.decode("usb:dp=0,dm=1", RATE), "sof").is_empty());
        let ev = b.decode("usb:dp=0,dm=1,show_sof", RATE);
        assert_eq!(kinds(&ev, "sof")[0].fields["frame"], 1234);
    }

    #[test]
    fn naks_counted_not_printed() {
        let mut b = Bus::new(false, FS);
        for _ in 0..3 {
            b.send(&usb_token(IN, 1, 1)).send(&usb_handshake(NAK));
        }
        let ev = b.decode("usb:dp=0,dm=1", RATE);
        assert!(kinds(&ev, "transaction").is_empty());
        assert_eq!(ev.last().unwrap().fields["naks"], 3);
        assert_eq!(
            kinds(&b.decode("usb:dp=0,dm=1,naks", RATE), "transaction").len(),
            3
        );
    }

    #[test]
    fn low_speed_auto_detect() {
        let rate = 12e6;
        let mut b = Bus::new(true, rate / 1.5e6);
        b.send(&usb_token(OUT, 5, 1))
            .send(&usb_data(DATA0, &[1, 2]))
            .send(&usb_handshake(ACK));
        let ev = b.decode("usb:dp=0,dm=1", rate);
        assert_eq!(ev[0].fields["speed"], "low");
        assert_eq!(
            kinds(&ev, "transaction")[0].text,
            "OUT addr=5 ep=1 DATA0 [01 02] ACK"
        );
    }

    #[test]
    fn bus_reset() {
        let mut w = Wave::new();
        usb_idle(&mut w, 0, 1, 0, false);
        usb_se0(&mut w, 0, 1, 1000, (10e-3 * RATE) as u64, false);
        let mut d = build(&Spec::parse("usb:dp=0,dm=1").unwrap(), RATE).unwrap();
        let mut out = Vec::new();
        d.feed(&w.edges(), &mut out);
        d.finish(2_000_000, &mut out);
        assert_eq!(kinds(&out, "reset").len(), 1);
    }

    #[test]
    fn tolerates_dp_dm_skew() {
        let mut b = Bus::new(false, FS);
        b.skew = 1;
        b.send(&usb_token(SETUP, 7, 0))
            .send(&usb_data(DATA0, &[0, 5, 9, 0, 0, 0, 0, 0]))
            .send(&usb_handshake(ACK));
        let ev = b.decode("usb:dp=0,dm=1", RATE);
        assert!(kinds(&ev, "error").is_empty(), "{ev:?}");
        assert_eq!(
            kinds(&ev, "transaction")[0].fields["request"],
            "SET_ADDRESS 9"
        );
    }

    #[test]
    fn batch_split_is_invariant() {
        let mut b = Bus::new(false, FS);
        b.send(&usb_token(OUT, 3, 2))
            .send(&usb_data(DATA0, &[0x55, 0xff, 0x00]))
            .send(&usb_handshake(ACK));
        let whole = b.decode("usb:dp=0,dm=1", RATE);
        let mut d = build(&Spec::parse("usb:dp=0,dm=1").unwrap(), RATE).unwrap();
        let mut out = Vec::new();
        for e in b.w.edges() {
            d.feed(&[e], &mut out);
        }
        d.finish(b.t as u64 + 1000, &mut out);
        assert_eq!(out, whole);
    }

    #[test]
    fn full_speed_needs_48mhz() {
        assert!(build(&Spec::parse("usb:dp=0,dm=1,speed=fs").unwrap(), 24e6).is_err());
        assert!(Spec::parse("usb:dp=0").is_err());
    }
}
