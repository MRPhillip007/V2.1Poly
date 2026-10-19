//! The offset of this machine's clock (spec §7.4): from an NTP server (SNTP, RFC 4330), else from a
//! server's time and the moments it was asked and answered (Binance's `/api/v3/time`).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Result};
use tokio::net::UdpSocket;
use tokio::time::timeout;

/// Seconds from 1900 (NTP's era) to 1970.
const NTP_TO_UNIX: u64 = 2_208_988_800;
/// Requests per measurement; the one with the shortest round trip counts.
const TRIES: usize = 5;
/// How long one request waits for its answer.
const WAIT: Duration = Duration::from_millis(500);

/// The offset of this machine's clock from a server's: the server's time minus the middle of the
/// request, ms (rounded down).
pub fn clock_offset_ms(server_ms: i64, sent_ms: i64, received_ms: i64) -> i64 {
    server_ms - (sent_ms + received_ms).div_euclid(2)
}

fn now_ntp() -> (u64, [u8; 8]) {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = since.as_secs() + NTP_TO_UNIX;
    let frac = (u64::from(since.subsec_nanos()) << 32) / 1_000_000_000;
    let mut stamp = [0u8; 8];
    stamp[..4].copy_from_slice(&(secs as u32).to_be_bytes());
    stamp[4..].copy_from_slice(&(frac as u32).to_be_bytes());
    (unix_ms(&stamp), stamp)
}

/// An NTP timestamp as unix ms (this era: 1968–2036).
fn unix_ms(stamp: &[u8]) -> u64 {
    let secs = u64::from(u32::from_be_bytes(stamp[..4].try_into().expect("4 bytes")));
    let frac = u64::from(u32::from_be_bytes(stamp[4..8].try_into().expect("4 bytes")));
    (secs - NTP_TO_UNIX) * 1000 + ((frac * 1000) >> 32)
}

/// One SNTP exchange: (offset, round trip), ms.
async fn exchange(sock: &UdpSocket) -> Result<(i64, i64)> {
    let mut req = [0u8; 48];
    req[0] = 0x23; // leap 0, version 4, mode 3 (client)
    let (t1, stamp) = now_ntp();
    req[40..48].copy_from_slice(&stamp);
    sock.send(&req).await?;
    let mut ans = [0u8; 48];
    let n = timeout(WAIT, sock.recv(&mut ans)).await.map_err(|_| anyhow!("no answer in {WAIT:?}"))??;
    let (t4, _) = now_ntp();
    if n < 48 || ans[0] & 0x07 != 4 || ans[24..32] != stamp {
        bail!("not an answer to this request");
    }
    let (t2, t3) = (unix_ms(&ans[32..40]) as i64, unix_ms(&ans[40..48]) as i64);
    let (t1, t4) = (t1 as i64, t4 as i64);
    Ok((((t2 - t1) + (t3 - t4)).div_euclid(2), (t4 - t1) - (t3 - t2)))
}

/// The offset of this machine's clock from the NTP server `server` (`host:port`) and the round trip of
/// the measurement, ms: of five requests, the one with the shortest round trip.
pub async fn sntp_offset_ms(server: &str) -> Result<(i64, i64)> {
    let sock = UdpSocket::bind("0.0.0.0:0").await?;
    sock.connect(server).await?;
    let mut best: Option<(i64, i64)> = None;
    let mut why = String::new();
    for _ in 0..TRIES {
        match exchange(&sock).await {
            Ok((offset, rtt)) if best.is_none_or(|(_, r)| rtt < r) => best = Some((offset, rtt)),
            Ok(_) => {}
            Err(e) => why = e.to_string(),
        }
    }
    best.ok_or_else(|| anyhow!("ntp {server}: {why}"))
}
