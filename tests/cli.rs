//! Binary tests that need no hardware.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use la1010::export;
use la1010::testgen::{Wave, uart_bits, uart_frame};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_la1010"))
}

fn uart_file(dir: &Path, name: &str) -> PathBuf {
    let rate = 10e6;
    let mut w = Wave::new();
    w.set(0, 0, true);
    let bit = rate / 115200.0;
    let mut t = 1000.0;
    for b in b"Hi" {
        t = uart_frame(&mut w, 0, t, bit, &uart_bits(u16::from(*b), 8, None, 1)) + 2.0 * bit;
    }
    let p = dir.join(name);
    let mut wr = export::create(&p, 1, rate).unwrap();
    wr.edges(&w.edges()).unwrap();
    wr.finish(t as u64 + 1000).unwrap();
    p
}

#[test]
fn decode_vcd_and_sr_text() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["u.vcd", "u.sr"] {
        let f = uart_file(dir.path(), name);
        let out = bin()
            .args([
                "decode",
                f.to_str().unwrap(),
                "--decode",
                "uart:rx=0,baud=115200",
            ])
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let s = String::from_utf8(out.stdout).unwrap();
        assert!(
            s.contains("rx 0x48 'H'") && s.contains("rx 0x69 'i'"),
            "{name}: {s}"
        );
    }
}

#[test]
fn decode_json_lines() {
    let dir = tempfile::tempdir().unwrap();
    let f = uart_file(dir.path(), "u.vcd");
    let out = bin()
        .args([
            "--json",
            "decode",
            f.to_str().unwrap(),
            "--decode",
            "uart:rx=0,baud=115200",
        ])
        .output()
        .unwrap();
    let lines: Vec<serde_json::Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["kind"], "byte");
    assert_eq!(lines[0]["value"], 72);
    assert_eq!(lines[0]["proto"], "uart");
}

#[test]
fn until_hit_and_miss() {
    let dir = tempfile::tempdir().unwrap();
    let f = uart_file(dir.path(), "u.vcd");
    let f = f.to_str().unwrap();
    let hit = bin()
        .args([
            "decode",
            f,
            "--decode",
            "uart:rx=0,baud=115200",
            "--until",
            "'H'",
        ])
        .output()
        .unwrap();
    assert_eq!(hit.status.code(), Some(0));
    assert_eq!(String::from_utf8(hit.stdout).unwrap().lines().count(), 1);
    let miss = bin()
        .args([
            "decode",
            f,
            "--decode",
            "uart:rx=0,baud=115200",
            "--until",
            "zzz",
        ])
        .output()
        .unwrap();
    assert_eq!(miss.status.code(), Some(3));
}

#[test]
fn usage_errors_exit_1() {
    let out = bin()
        .args(["decode", "x.vcd", "--decode", "nope:a=1"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown decoder"));
    assert_eq!(
        bin()
            .args(["capture", "--bogus"])
            .output()
            .unwrap()
            .status
            .code(),
        Some(1)
    );
    assert_eq!(
        bin()
            .args(["capture", "-r", "200M"])
            .output()
            .unwrap()
            .status
            .code(),
        Some(1)
    );
}

#[test]
fn json_error_on_stderr() {
    let out = bin()
        .args([
            "--json",
            "decode",
            "/nonexistent/x.vcd",
            "--decode",
            "uart:rx=0",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let v: serde_json::Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(v["code"], 2);
}

#[test]
fn decoders_listing() {
    let out = bin().args(["decoders", "--json"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let names: Vec<_> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["uart", "i2c", "spi", "usb"]);
}

#[test]
fn help_and_version_exit_0() {
    let h = bin().arg("--help").output().unwrap();
    assert_eq!(h.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&h.stdout).contains("Exit codes"));
    assert_eq!(
        bin().arg("--version").output().unwrap().status.code(),
        Some(0)
    );
}

#[test]
fn firmware_extract_rejects_junk() {
    let dir = tempfile::tempdir().unwrap();
    let junk = dir.path().join("junk.bin");
    std::fs::write(&junk, b"not kingstvis").unwrap();
    let out = bin()
        .args([
            "firmware",
            "extract",
            junk.to_str().unwrap(),
            "--out",
            dir.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn closed_stdout_exits_0() {
    let dir = tempfile::tempdir().unwrap();
    let f = uart_file(dir.path(), "u.vcd");
    let mut child = bin()
        .args([
            "decode",
            f.to_str().unwrap(),
            "--decode",
            "uart:rx=0,baud=115200",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    assert_eq!(child.wait().unwrap().code(), Some(0));
}
