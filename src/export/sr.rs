//! sigrok session files (srzip v2), readable by PulseView and sigrok-cli.

use std::fs::File;
use std::io::{Read, Write};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::error::{Error, Result};
use crate::export::{Recording, Writer, invalid};
use crate::stream::Edge;
use crate::units::parse_si;

const CHUNK: usize = 4 << 20;

fn zerr(e: zip::result::ZipError) -> Error {
    Error::Io(std::io::Error::other(e))
}

fn opts() -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(CompressionMethod::Deflated)
}

pub struct SrWriter {
    zip: ZipWriter<File>,
    levels: u16,
    pos: u64,
    chunk: Vec<u8>,
    n: u32,
}

impl SrWriter {
    pub fn new(file: File, mask: u16, rate: f64) -> Result<Self> {
        let mut zip = ZipWriter::new(file);
        zip.start_file("version", opts()).map_err(zerr)?;
        zip.write_all(b"2")?;
        let mut meta = format!(
            "[global]\nsigrok version=0.5.2\n\n[device 1]\ncapturefile=logic-1\ntotal probes=16\nsamplerate={}\ntotal analog=0\n",
            rate.round() as u64
        );
        for ch in (0..16).filter(|c| mask >> c & 1 == 1) {
            meta.push_str(&format!("probe{}=D{ch}\n", ch + 1));
        }
        meta.push_str("unitsize=2\n");
        zip.start_file("metadata", opts()).map_err(zerr)?;
        zip.write_all(meta.as_bytes())?;
        Ok(SrWriter {
            zip,
            levels: 0,
            pos: 0,
            chunk: Vec::with_capacity(CHUNK),
            n: 0,
        })
    }

    fn fill(&mut self, until: u64) -> Result<()> {
        let bytes = self.levels.to_le_bytes();
        while self.pos < until {
            let room = (CHUNK - self.chunk.len()) / 2;
            let k = room.min((until - self.pos) as usize);
            for _ in 0..k {
                self.chunk.extend_from_slice(&bytes);
            }
            self.pos += k as u64;
            if self.chunk.len() == CHUNK {
                self.flush_chunk()?;
            }
        }
        Ok(())
    }

    fn flush_chunk(&mut self) -> Result<()> {
        if self.chunk.is_empty() {
            return Ok(());
        }
        self.n += 1;
        self.zip
            .start_file(format!("logic-1-{}", self.n), opts())
            .map_err(zerr)?;
        self.zip.write_all(&self.chunk)?;
        self.chunk.clear();
        Ok(())
    }
}

impl Writer for SrWriter {
    fn edges(&mut self, edges: &[Edge]) -> Result<()> {
        for e in edges {
            self.fill(e.sample)?;
            let bit = 1u16 << e.ch;
            self.levels = if e.level {
                self.levels | bit
            } else {
                self.levels & !bit
            };
        }
        Ok(())
    }

    fn finish(mut self: Box<Self>, end: u64) -> Result<()> {
        self.fill(end)?;
        self.flush_chunk()?;
        self.zip.finish().map_err(zerr)?;
        Ok(())
    }
}

/// "10000000", "10 MHz", "500 kHz".
pub fn parse_rate(v: &str) -> Result<f64> {
    let t = v.trim();
    let t = t.strip_suffix("Hz").unwrap_or(t).replace(' ', "");
    parse_si(&t).ok_or_else(|| invalid(format!("sr: bad samplerate '{v}'")))
}

pub fn read(file: File) -> Result<Recording> {
    let mut zip = ZipArchive::new(file).map_err(zerr)?;
    let mut meta = String::new();
    zip.by_name("metadata")
        .map_err(zerr)?
        .read_to_string(&mut meta)?;
    let (mut rate, mut unitsize, mut mask) = (None, 1usize, 0u16);
    for line in meta.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        match k.trim() {
            "samplerate" => rate = Some(parse_rate(v)?),
            "unitsize" => {
                unitsize = v.trim().parse().map_err(|_| invalid("sr: bad unitsize"))?;
            }
            k => {
                if let Some(n) = k.strip_prefix("probe").and_then(|n| n.parse::<u8>().ok())
                    && (1..=16).contains(&n)
                {
                    mask |= 1 << (n - 1);
                }
            }
        }
    }
    let rate = rate.ok_or_else(|| invalid("sr: no samplerate"))?;
    if !(1..=8).contains(&unitsize) {
        return Err(invalid("sr: bad unitsize"));
    }
    let mut chunks: Vec<u32> = zip
        .file_names()
        .filter_map(|n| n.ok()?.strip_prefix("logic-1-")?.parse().ok())
        .collect();
    chunks.sort_unstable();
    let (mut edges, mut levels, mut pos) = (Vec::new(), 0u16, 0u64);
    for n in chunks {
        let mut data = Vec::new();
        zip.by_name(&format!("logic-1-{n}"))
            .map_err(zerr)?
            .read_to_end(&mut data)?;
        for s in data.chunks_exact(unitsize) {
            let hi = if unitsize > 1 {
                u16::from(s[1]) << 8
            } else {
                0
            };
            let v = (u16::from(s[0]) | hi) & mask;
            let diff = if pos == 0 { mask } else { v ^ levels };
            for ch in (0..16u8).filter(|c| diff >> c & 1 == 1) {
                edges.push(Edge {
                    sample: pos,
                    ch,
                    level: v >> ch & 1 == 1,
                });
            }
            levels = v;
            pos += 1;
        }
    }
    Ok(Recording {
        rate,
        mask,
        edges,
        end: pos,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::Writer;

    fn e(sample: u64, ch: u8, level: bool) -> Edge {
        Edge { sample, ch, level }
    }

    fn round_trip(edges: &[Edge], mask: u16, end: u64) -> Recording {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.sr");
        let mut w: Box<dyn Writer> =
            Box::new(SrWriter::new(File::create(&path).unwrap(), mask, 10e6).unwrap());
        w.edges(edges).unwrap();
        w.finish(end).unwrap();
        read(File::open(&path).unwrap()).unwrap()
    }

    #[test]
    fn small_round_trip() {
        let edges = [e(0, 0, false), e(0, 2, true), e(5, 0, true), e(9, 2, false)];
        let rec = round_trip(&edges, 0b101, 12);
        assert_eq!(
            rec,
            Recording {
                rate: 10e6,
                mask: 0b101,
                edges: edges.to_vec(),
                end: 12
            }
        );
    }

    #[test]
    fn multi_chunk_round_trip() {
        let edges = [
            e(0, 1, false),
            e(2_500_000, 1, true),
            e(2_999_999, 1, false),
        ];
        let rec = round_trip(&edges, 0b10, 3_000_000);
        assert_eq!(rec.edges, edges.to_vec());
        assert_eq!(rec.end, 3_000_000);
    }

    #[test]
    fn rate_strings() {
        assert_eq!(parse_rate("10000000").unwrap(), 10e6);
        assert_eq!(parse_rate("10 MHz").unwrap(), 10e6);
        assert_eq!(parse_rate("500 kHz").unwrap(), 500e3);
        assert!(parse_rate("fast").is_err());
    }
}
