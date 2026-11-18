//! Network test against the vendor server. Run with the user's OK:
//!   cargo test --test net -- --ignored

/// qdkingst.com (IIS 7.5) offers only CBC/RSA-kex TLS 1.2 suites, which rustls rejects.
#[test]
#[ignore = "downloads ~29 MB from qdkingst.com"]
fn fetch_from_vendor() {
    let blobs = la1010::firmware::fetch(
        la1010::firmware::VENDOR_URL,
        la1010::firmware::IDLE_TIMEOUT,
        |_, _| {},
    )
    .unwrap();
    for b in &blobs {
        let (_, size, crc) = la1010::firmware::KNOWN
            .iter()
            .find(|k| k.0 == b.name)
            .unwrap();
        assert_eq!(
            (b.data.len(), la1010::firmware::crc32(&b.data)),
            (*size, *crc),
            "{}",
            b.name
        );
    }
    assert_eq!(blobs.len(), 4);
}
