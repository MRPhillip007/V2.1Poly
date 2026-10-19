//! The offset of this machine's clock: from an NTP server, or from a server's time and the moments it
//! was asked and answered.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use v21_live::cold::ntp::{clock_offset_ms, sntp_offset_ms};

#[test]
fn clock_offset_is_the_server_minus_the_middle() {
    assert_eq!(clock_offset_ms(1_000, 0, 100), 950);
    assert_eq!(clock_offset_ms(0, 1_000, 1_000), -1_000);
    assert_eq!(clock_offset_ms(5_000, 4_000, 4_003), 999, "the middle is rounded down");
}

/// The NTP timestamp (seconds since 1900, fraction of 2^32) of this machine's time plus `ahead_ms`.
fn ntp_now(ahead_ms: i64) -> [u8; 8] {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap() + Duration::from_millis(ahead_ms as u64);
    let secs = since.as_secs() + 2_208_988_800;
    let frac = (u64::from(since.subsec_nanos()) << 32) / 1_000_000_000;
    let mut out = [0u8; 8];
    out[..4].copy_from_slice(&(secs as u32).to_be_bytes());
    out[4..].copy_from_slice(&(frac as u32).to_be_bytes());
    out
}

/// A local NTP server whose clock is `ahead_ms` ahead of this machine's; with `wrong_origin` it
/// answers as if to another request.
async fn ntp_server(ahead_ms: i64, wrong_origin: bool) -> String {
    let sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = sock.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let mut buf = [0u8; 48];
        while let Ok((n, from)) = sock.recv_from(&mut buf).await {
            if n < 48 {
                continue;
            }
            let mut out = [0u8; 48];
            out[0] = 0x24; // leap 0, version 4, mode 4 (server)
            out[1] = 2; // stratum
            out[24..32].copy_from_slice(&buf[40..48]); // the request's transmit time
            if wrong_origin {
                out[31] ^= 1;
            }
            out[32..40].copy_from_slice(&ntp_now(ahead_ms));
            out[40..48].copy_from_slice(&ntp_now(ahead_ms));
            let _ = sock.send_to(&out, from).await;
        }
    });
    addr
}

#[tokio::test]
async fn sntp_offset_against_a_local_server() {
    let server = ntp_server(1_042, false).await;
    let (offset, rtt) = sntp_offset_ms(&server).await.unwrap();
    assert!((offset - 1_042).abs() <= 2, "{offset}");
    assert!((0..100).contains(&rtt), "{rtt}");
}

#[tokio::test]
async fn an_answer_to_another_request_is_refused() {
    let server = ntp_server(0, true).await;
    assert!(sntp_offset_ms(&server).await.is_err());
}

#[tokio::test]
async fn a_server_that_never_answers_is_an_error() {
    // a socket that reads nothing
    let quiet = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = quiet.local_addr().unwrap().to_string();
    let started = std::time::Instant::now();
    assert!(sntp_offset_ms(&addr).await.is_err());
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
}
