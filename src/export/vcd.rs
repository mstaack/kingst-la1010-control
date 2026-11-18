//! Value Change Dump, one 1-bit wire per channel named D<n>.

use std::collections::HashMap;
use std::io::{Read, Write};

use crate::error::Result;
use crate::export::{Recording, Writer, invalid};
use crate::stream::Edge;

pub struct VcdWriter<W: Write> {
    out: W,
    ps: u64,
    last: Option<u64>,
}

fn id(ch: u8) -> char {
    (b'A' + ch) as char
}

impl<W: Write> VcdWriter<W> {
    pub fn new(mut out: W, mask: u16, rate: f64) -> Result<Self> {
        writeln!(out, "$version la1010 {} $end", env!("CARGO_PKG_VERSION"))?;
        writeln!(out, "$timescale 1 ps $end")?;
        writeln!(out, "$comment samplerate {rate} $end")?;
        writeln!(out, "$scope module la1010 $end")?;
        for ch in (0..16u8).filter(|c| mask >> c & 1 == 1) {
            writeln!(out, "$var wire 1 {} D{ch} $end", id(ch))?;
        }
        writeln!(out, "$upscope $end\n$enddefinitions $end")?;
        Ok(VcdWriter {
            out,
            ps: (1e12 / rate).round() as u64,
            last: None,
        })
    }
}

impl<W: Write> Writer for VcdWriter<W> {
    fn edges(&mut self, edges: &[Edge]) -> Result<()> {
        for e in edges {
            if self.last != Some(e.sample) {
                writeln!(self.out, "#{}", e.sample * self.ps)?;
                self.last = Some(e.sample);
            }
            writeln!(self.out, "{}{}", u8::from(e.level), id(e.ch))?;
        }
        Ok(())
    }

    fn finish(mut self: Box<Self>, end: u64) -> Result<()> {
        if self.last != Some(end) {
            writeln!(self.out, "#{}", end * self.ps)?;
        }
        self.out.flush()?;
        Ok(())
    }
}

/// Timescale like "1ps" or "10 ns" -> time units per second.
fn units_per_second(s: &str) -> Result<f64> {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let n: f64 = s[..split]
        .parse()
        .ok()
        .filter(|n| *n > 0.0)
        .ok_or_else(|| invalid(format!("VCD: bad timescale '{s}'")))?;
    let unit = match s[split..].trim() {
        "s" => 1.0,
        "ms" => 1e3,
        "us" => 1e6,
        "ns" => 1e9,
        "ps" => 1e12,
        "fs" => 1e15,
        u => return Err(invalid(format!("VCD: bad timescale unit '{u}'"))),
    };
    Ok(unit / n)
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

pub fn read(mut r: impl Read) -> Result<Recording> {
    let mut text = String::new();
    r.read_to_string(&mut text)?;
    let mut tok = text.split_whitespace();
    let mut ups: Option<f64> = None;
    // Written by la1010; without it the rate is inferred from the timestamps.
    let mut hint: Option<f64> = None;
    let mut ids: HashMap<String, u8> = HashMap::new();
    let mut used = 0u16;
    let mut t: u64 = 0;
    let mut changes: Vec<(u64, u8, bool)> = Vec::new();
    while let Some(w) = tok.next() {
        match w {
            "$timescale" => {
                let s: String = tok.by_ref().take_while(|x| *x != "$end").collect();
                ups = Some(units_per_second(&s)?);
            }
            "$comment" => {
                let v: Vec<&str> = tok.by_ref().take_while(|x| *x != "$end").collect();
                if let ["samplerate", r] = v[..] {
                    hint = r.parse().ok().filter(|r: &f64| r.is_finite() && *r > 0.0);
                }
            }
            "$var" => {
                let v: Vec<&str> = tok.by_ref().take_while(|x| *x != "$end").collect();
                if v.len() >= 4 && v[1] == "1" {
                    let named = v[3]
                        .strip_prefix('D')
                        .and_then(|n| n.parse::<u8>().ok())
                        .filter(|c| *c < 16 && used >> c & 1 == 0);
                    if let Some(ch) = named.or_else(|| (0..16).find(|c| used >> c & 1 == 0)) {
                        used |= 1 << ch;
                        ids.insert(v[2].to_string(), ch);
                    }
                }
            }
            "$dumpvars" | "$dumpall" | "$dumpon" | "$dumpoff" | "$end" => {}
            w if w.starts_with('$') => {
                for x in tok.by_ref() {
                    if x == "$end" {
                        break;
                    }
                }
            }
            w if w.starts_with('#') => {
                t = w[1..]
                    .parse()
                    .map_err(|_| invalid(format!("VCD: bad timestamp '{w}'")))?;
            }
            w if w.starts_with(['b', 'B', 'r', 'R']) => {
                tok.next();
            }
            w => {
                let mut c = w.chars();
                let v = c.next();
                if let Some(&ch) = ids.get(c.as_str()) {
                    changes.push((t, ch, v == Some('1')));
                }
            }
        }
    }
    let ups = ups.ok_or_else(|| invalid("VCD: missing $timescale"))?;
    let end_t = changes.iter().map(|c| c.0).max().unwrap_or(0).max(t);
    // Time units per sample.
    let g = match hint {
        Some(rate) if ups >= rate => (ups / rate).round() as u64,
        _ => changes
            .iter()
            .map(|c| c.0)
            .chain([end_t])
            .fold(0, gcd)
            .max(1),
    };
    changes.sort_by_key(|c| (c.0, c.1));
    let mut last: [Option<bool>; 16] = [None; 16];
    let mut edges = Vec::new();
    for (t, ch, level) in changes {
        if last[ch as usize] != Some(level) {
            edges.push(Edge {
                sample: t / g,
                ch,
                level,
            });
            last[ch as usize] = Some(level);
        }
    }
    Ok(Recording {
        rate: hint.filter(|r| ups >= *r).unwrap_or(ups / g as f64),
        mask: used,
        edges,
        end: end_t / g,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::Writer;

    fn e(sample: u64, ch: u8, level: bool) -> Edge {
        Edge { sample, ch, level }
    }

    #[test]
    fn round_trip() {
        let edges = vec![
            e(0, 0, false),
            e(0, 3, true),
            e(3, 0, true),
            e(7, 3, false),
            e(7, 0, false),
        ];
        let mut buf = Vec::new();
        {
            let mut w: Box<dyn Writer + '_> =
                Box::new(VcdWriter::new(&mut buf, 0b1001, 10e6).unwrap());
            w.edges(&edges).unwrap();
            w.finish(10).unwrap();
        }
        let text = String::from_utf8(buf.clone()).unwrap();
        assert!(text.contains("$timescale 1 ps $end"));
        assert!(text.contains("$var wire 1 D D3 $end"));
        assert!(text.contains("#300000\n1A"));
        let rec = read(&buf[..]).unwrap();
        assert_eq!(rec.rate, 10e6);
        assert_eq!(rec.mask, 0b1001);
        assert_eq!(rec.end, 10);
        let mut want = edges.clone();
        want.sort_by_key(|e| (e.sample, e.ch));
        assert_eq!(rec.edges, want);
    }

    #[test]
    fn foreign_vcd_with_vectors_and_names() {
        let src = "$date today $end\n$timescale 10 ns $end\n$scope module top $end\n\
                   $var wire 1 a clk $end\n$var wire 8 b bus $end\n$var wire 1 c data $end\n\
                   $upscope $end\n$enddefinitions $end\n$dumpvars\n0a\nb00000000 b\n1c\n$end\n\
                   #2\n1a\n#4\n0a\n0c\n";
        let rec = read(src.as_bytes()).unwrap();
        assert_eq!(rec.mask, 0b11);
        assert_eq!(rec.rate, 50e6); // 10 ns units, gcd 2
        assert_eq!(
            rec.edges,
            vec![
                e(0, 0, false),
                e(0, 1, true),
                e(1, 0, true),
                e(2, 0, false),
                e(2, 1, false)
            ]
        );
    }

    #[test]
    fn static_line_keeps_sample_rate() {
        let mut buf = Vec::new();
        {
            let mut w: Box<dyn Writer + '_> = Box::new(VcdWriter::new(&mut buf, 1, 100e6).unwrap());
            w.edges(&[e(0, 0, true)]).unwrap();
            w.finish(1_000_000).unwrap();
        }
        let rec = read(&buf[..]).unwrap();
        assert_eq!(rec.rate, 100e6);
        assert_eq!(rec.end, 1_000_000);
    }

    #[test]
    fn zero_timescale_is_error() {
        assert!(read("$timescale 0 ps $end\n$var wire 1 A D0 $end\n#0\n1A\n".as_bytes()).is_err());
    }

    #[test]
    fn missing_timescale_is_error() {
        assert_eq!(
            read("$var wire 1 A D0 $end\n#0\n1A\n".as_bytes())
                .unwrap_err()
                .exit_code(),
            2
        );
    }
}
