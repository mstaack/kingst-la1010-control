//! CSV: one row per sample where any channel changes, plus a final row at the end.

use std::io::Write;

use crate::error::Result;
use crate::export::Writer;
use crate::stream::Edge;

pub struct CsvWriter<W: Write> {
    out: W,
    channels: Vec<u8>,
    rate: f64,
    levels: u16,
    pending: Option<u64>,
}

impl<W: Write> CsvWriter<W> {
    pub fn new(mut out: W, mask: u16, rate: f64) -> Result<Self> {
        let channels: Vec<u8> = (0..16).filter(|c| mask >> c & 1 == 1).collect();
        let head: String = channels.iter().map(|c| format!(",D{c}")).collect();
        writeln!(out, "time_s{head}")?;
        Ok(CsvWriter {
            out,
            channels,
            rate,
            levels: 0,
            pending: None,
        })
    }

    fn row(&mut self, sample: u64) -> Result<()> {
        let vals: String = self
            .channels
            .iter()
            .map(|c| format!(",{}", self.levels >> c & 1))
            .collect();
        writeln!(self.out, "{:.9}{vals}", sample as f64 / self.rate)?;
        Ok(())
    }
}

impl<W: Write> Writer for CsvWriter<W> {
    fn edges(&mut self, edges: &[Edge]) -> Result<()> {
        for e in edges {
            if let Some(p) = self.pending.filter(|p| *p != e.sample) {
                self.row(p)?;
            }
            self.pending = Some(e.sample);
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
        if let Some(p) = self.pending {
            self.row(p)?;
            if p != end {
                self.row(end)?;
            }
        }
        self.out.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::Writer;

    #[test]
    fn rows_per_change() {
        let mut buf = Vec::new();
        {
            let mut w: Box<dyn Writer + '_> =
                Box::new(CsvWriter::new(&mut buf, 0b11, 1e6).unwrap());
            w.edges(&[
                Edge {
                    sample: 0,
                    ch: 0,
                    level: false,
                },
                Edge {
                    sample: 0,
                    ch: 1,
                    level: true,
                },
                Edge {
                    sample: 5,
                    ch: 0,
                    level: true,
                },
            ])
            .unwrap();
            w.finish(10).unwrap();
        }
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "time_s,D0,D1\n0.000000000,0,1\n0.000005000,1,1\n0.000010000,1,1\n"
        );
    }
}
