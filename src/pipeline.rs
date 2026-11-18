//! Window -> decoders / measure / file writer -> printer, with --until / --count.

use std::io::Write;

use crate::decode::Decoder;
use crate::error::Result;
use crate::export::Writer;
use crate::measure::Measure;
use crate::output::{Event, Printer};
use crate::stream::Edge;
use crate::window::Window;

pub struct Outcome {
    pub triggered: bool,
    pub hit: bool,
    pub decoded: u64,
}

pub struct Pipeline<W: Write> {
    window: Window,
    decoders: Vec<Box<dyn Decoder>>,
    measure: Option<Measure>,
    writer: Option<Box<dyn Writer>>,
    printer: Printer<W>,
    until: Option<String>,
    count: Option<u64>,
    decoded: u64,
    hit: bool,
    edges: Vec<Edge>,
    events: Vec<Event>,
}

impl<W: Write> Pipeline<W> {
    pub fn new(window: Window, decoders: Vec<Box<dyn Decoder>>, printer: Printer<W>) -> Self {
        Pipeline {
            window,
            decoders,
            measure: None,
            writer: None,
            printer,
            until: None,
            count: None,
            decoded: 0,
            hit: false,
            edges: Vec::new(),
            events: Vec::new(),
        }
    }

    pub fn measure(mut self, m: Measure) -> Self {
        self.measure = Some(m);
        self
    }

    pub fn writer(mut self, w: Box<dyn Writer>) -> Self {
        self.writer = Some(w);
        self
    }

    pub fn stop_on(mut self, until: Option<String>, count: Option<u64>) -> Self {
        self.until = until;
        self.count = count;
        self
    }

    pub fn triggered(&self) -> bool {
        self.window.triggered()
    }

    /// Feed absolute edges; `now` = samples processed so far. Returns false to stop.
    pub fn push(&mut self, edges: &[Edge], now: u64) -> Result<bool> {
        self.edges.clear();
        let done = self.window.push(edges, now, &mut self.edges);
        if let Some(t) = self.window.trigger_sample() {
            self.printer.set_origin(t);
        }
        if !self.edges.is_empty() {
            for d in &mut self.decoders {
                d.feed(&self.edges, &mut self.events);
            }
            if let Some(m) = &mut self.measure {
                m.feed(&self.edges);
            }
            if let Some(w) = &mut self.writer {
                w.edges(&self.edges)?;
            }
        }
        self.emit()?;
        Ok(!done && !self.hit)
    }

    fn emit(&mut self) -> Result<()> {
        self.events.sort_by_key(|e| e.sample);
        for e in self.events.drain(..) {
            if self.hit {
                break;
            }
            let line = self.printer.print(&e)?;
            if !matches!(e.kind, "info" | "error") {
                self.decoded += 1;
            }
            let until = self.until.as_deref().is_some_and(|u| line.contains(u));
            if until || self.count.is_some_and(|c| self.decoded >= c) {
                self.hit = true;
            }
        }
        self.printer.flush()
    }

    pub fn finish(mut self, now: u64) -> Result<Outcome> {
        let end = self.window.output_len(now);
        if self.window.triggered() && !self.hit {
            for d in &mut self.decoders {
                d.finish(end, &mut self.events);
            }
        }
        if let Some(m) = &self.measure {
            self.events.extend(Measure::events(&m.finish()));
        }
        self.emit()?;
        if let Some(w) = self.writer.take() {
            w.finish(end)?;
        }
        Ok(Outcome {
            triggered: self.window.triggered(),
            hit: self.hit,
            decoded: self.decoded,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::{Spec, build};
    use crate::testgen::{Wave, uart_bits, uart_frame};

    const RATE: f64 = 10e6;

    fn hi() -> (Vec<Edge>, u64) {
        let mut w = Wave::new();
        w.set(0, 0, true);
        let bit = RATE / 115200.0;
        let mut t = 1000.0;
        for b in b"Hi" {
            t = uart_frame(&mut w, 0, t, bit, &uart_bits(u16::from(*b), 8, None, 1)) + 2.0 * bit;
        }
        (w.edges(), t as u64 + 1000)
    }

    fn pipe(buf: &mut Vec<u8>) -> Pipeline<&mut Vec<u8>> {
        let d = build(&Spec::parse("uart:rx=0,baud=115200").unwrap(), RATE).unwrap();
        Pipeline::new(
            Window::new(1, None, 0, u64::MAX),
            vec![d],
            Printer::new(buf, false, RATE),
        )
    }

    #[test]
    fn decodes_to_printer() {
        let (edges, end) = hi();
        let mut buf = Vec::new();
        let mut p = pipe(&mut buf);
        assert!(p.push(&edges, end).unwrap());
        let o = p.finish(end).unwrap();
        assert_eq!(o.decoded, 2);
        assert!(!o.hit);
        assert_eq!(String::from_utf8(buf).unwrap().lines().count(), 2);
    }

    #[test]
    fn until_stops_after_match() {
        let (edges, end) = hi();
        let mut buf = Vec::new();
        let mut p = pipe(&mut buf).stop_on(Some("'H'".into()), None);
        assert!(!p.push(&edges, end).unwrap());
        assert!(p.finish(end).unwrap().hit);
        assert_eq!(String::from_utf8(buf).unwrap().lines().count(), 1);
    }

    #[test]
    fn count_stops_after_n() {
        let (edges, end) = hi();
        let mut buf = Vec::new();
        let mut p = pipe(&mut buf).stop_on(None, Some(1));
        assert!(!p.push(&edges, end).unwrap());
        assert!(p.finish(end).unwrap().hit);
    }

    #[test]
    fn measure_reported_at_finish() {
        let (edges, end) = hi();
        let mut buf = Vec::new();
        let mut p = Pipeline::new(
            Window::new(1, None, 0, u64::MAX),
            vec![],
            Printer::new(&mut buf, false, RATE),
        )
        .measure(Measure::new(1, RATE));
        p.push(&edges, end).unwrap();
        p.finish(end).unwrap();
        assert!(String::from_utf8(buf).unwrap().contains("ch0 "));
    }
}
