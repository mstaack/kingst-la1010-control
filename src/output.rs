//! Decoded events and their text / NDJSON rendering.

use std::io::Write;

use serde_json::{Map, Value, json};

use crate::error::{Error, Result};
use crate::units::format_time;

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub sample: u64,
    pub proto: &'static str,
    pub kind: &'static str,
    pub text: String,
    pub fields: Map<String, Value>,
}

impl Event {
    pub fn new(
        sample: u64,
        proto: &'static str,
        kind: &'static str,
        text: impl Into<String>,
    ) -> Self {
        Event {
            sample,
            proto,
            kind,
            text: text.into(),
            fields: Map::new(),
        }
    }

    pub fn with(mut self, key: &str, v: impl Into<Value>) -> Self {
        self.fields.insert(key.to_string(), v.into());
        self
    }
}

pub struct Printer<W: Write> {
    out: W,
    json: bool,
    rate: f64,
    origin: u64,
}

impl<W: Write> Printer<W> {
    pub fn new(out: W, json: bool, rate: f64) -> Self {
        Printer {
            out,
            json,
            rate,
            origin: 0,
        }
    }

    /// Sample that maps to t = 0 (the trigger position).
    pub fn set_origin(&mut self, origin: u64) {
        self.origin = origin;
    }

    pub fn render(&self, e: &Event) -> String {
        let t = (e.sample as f64 - self.origin as f64) / self.rate;
        if !self.json {
            return format!("{:<14} {:<7} {}", format_time(t), e.proto, e.text);
        }
        let mut m = Map::new();
        m.insert("t".into(), json!((t * 1e9).round() / 1e9));
        m.insert("sample".into(), json!(e.sample));
        m.insert("proto".into(), json!(e.proto));
        m.insert("kind".into(), json!(e.kind));
        for (k, v) in &e.fields {
            m.insert(k.clone(), v.clone());
        }
        m.insert("text".into(), json!(e.text));
        Value::Object(m).to_string()
    }

    pub fn print(&mut self, e: &Event) -> Result<String> {
        let line = self.render(e);
        writeln!(self.out, "{line}")?;
        Ok(line)
    }

    pub fn flush(&mut self) -> Result<()> {
        Ok(self.out.flush()?)
    }

    pub fn into_inner(self) -> W {
        self.out
    }
}

pub fn error_json(e: &Error) -> String {
    json!({"error": e.to_string(), "code": e.exit_code()}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;

    fn ev() -> Event {
        Event::new(1_234_567, "uart", "byte", "rx 0x41 'A'")
            .with("ch", "rx")
            .with("value", 65)
    }

    #[test]
    fn text_line() {
        let p = Printer::new(Vec::new(), false, 100e6);
        assert_eq!(p.render(&ev()), "12.345670ms    uart    rx 0x41 'A'");
    }

    #[test]
    fn json_line_key_order() {
        let p = Printer::new(Vec::new(), true, 100e6);
        assert_eq!(
            p.render(&ev()),
            r#"{"t":0.01234567,"sample":1234567,"proto":"uart","kind":"byte","ch":"rx","value":65,"text":"rx 0x41 'A'"}"#
        );
    }

    #[test]
    fn origin_shifts_time() {
        let mut p = Printer::new(Vec::new(), true, 1e6);
        p.set_origin(1_234_567);
        assert!(p.render(&ev()).starts_with(r#"{"t":0.0,"#));
    }

    #[test]
    fn print_writes_line() {
        let mut p = Printer::new(Vec::new(), false, 1e6);
        p.print(&ev()).unwrap();
        assert!(
            String::from_utf8(p.into_inner())
                .unwrap()
                .ends_with("'A'\n")
        );
    }

    #[test]
    fn error_json_shape() {
        assert_eq!(
            error_json(&Error::Usage("bad".into())),
            r#"{"error":"bad","code":1}"#
        );
    }
}
