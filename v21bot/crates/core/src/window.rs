//! The geometry of a market window, the coins and sides, and the time helpers the backtest uses.

/// A market window lasts 15 minutes.
pub const WINDOW_S: i64 = 900;
/// The price to beat and the final price are 60-second averages (the venue's TWAP rule).
pub const TWAP_S: i64 = 60;

/// The outcome a bet is on: the Up token, or the Down token (whose ask is 1 − the Up bid).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Side {
    Up,
    Down,
}

/// A coin whose markets the strategy watches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Coin {
    Btc,
    Eth,
    Sol,
}

impl Coin {
    /// The coin as the market slugs and the strategy files spell it.
    pub fn parse(s: &str) -> Option<Coin> {
        match s {
            "btc" => Some(Coin::Btc),
            "eth" => Some(Coin::Eth),
            "sol" => Some(Coin::Sol),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Coin::Btc => "btc",
            Coin::Eth => "eth",
            Coin::Sol => "sol",
        }
    }

    /// The Binance spot pair whose 1-s closes stand in for the coin's Chainlink price.
    pub fn binance_symbol(self) -> &'static str {
        match self {
            Coin::Btc => "BTCUSDT",
            Coin::Eth => "ETHUSDT",
            Coin::Sol => "SOLUSDT",
        }
    }
}

/// Python's float floor division and remainder (`_float_div_mod` of CPython), so that an hour
/// computed here is the one `(s + t_trig) // 3600 % 24` gives there, rounding included.
fn py_divmod(vx: f64, wx: f64) -> (f64, f64) {
    let mut m = vx % wx;
    let mut div = (vx - m) / wx;
    if m != 0.0 {
        if (wx < 0.0) != (m < 0.0) {
            m += wx;
            div -= 1.0;
        }
    } else {
        m = 0.0_f64.copysign(wx);
    }
    let floordiv = if div != 0.0 {
        let f = div.floor();
        if div - f > 0.5 { f + 1.0 } else { f }
    } else {
        0.0_f64.copysign(vx / wx)
    };
    (floordiv, m)
}

/// The UTC hour of the entry moment `s + t_trig`, as `int((s + t_trig) // 3600 % 24)`.
pub fn utc_hour(s: i64, t_trig: f64) -> u32 {
    let (hours, _) = py_divmod(s as f64 + t_trig, 3600.0);
    let (_, hour) = py_divmod(hours, 24.0);
    hour as u32
}

/// The entry moment in unix ms as the backtest stores it: `s0 + (t_trig * 1000).astype('int64')`,
/// which truncates toward zero.
pub fn entry_ms(s0_ms: i64, t_trig: f64) -> i64 {
    s0_ms + (t_trig * 1000.0) as i64
}

/// Days since 1970-01-01 of a proleptic Gregorian date (H. Hinnant's days-from-civil).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

/// Unix ms of a UTC time written `YYYY-MM-DDTHH:MM:SSZ` (the strategy files) or `YYYY-MM-DDTHH:MMZ`
/// (the incident list). Anything else, or a date or time out of range, is `None`.
pub fn parse_utc_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let with_seconds = match b.len() {
        20 => true,
        17 => false,
        _ => return None,
    };
    let num = |from: usize, to: usize| -> Option<i64> {
        let part = b.get(from..to)?;
        if part.iter().all(u8::is_ascii_digit) {
            Some(part.iter().fold(0i64, |acc, d| acc * 10 + i64::from(d - b'0')))
        } else {
            None
        }
    };
    let sep = |at: usize, c: u8| b.get(at) == Some(&c);
    if !(sep(4, b'-') && sep(7, b'-') && sep(10, b'T') && sep(13, b':') && sep(b.len() - 1, b'Z')) {
        return None;
    }
    if with_seconds && !sep(16, b':') {
        return None;
    }
    let (y, mo, d, h, mi) = (num(0, 4)?, num(5, 7)?, num(8, 10)?, num(11, 13)?, num(14, 16)?);
    let sec = if with_seconds { num(17, 19)? } else { 0 };
    if !(1..=12).contains(&mo) || d < 1 || d > days_in_month(y, mo) || h > 23 || mi > 59 || sec > 59 {
        return None;
    }
    Some(((days_from_civil(y, mo, d) * 24 + h) * 60 + mi) * 60_000 + sec * 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_hour_is_the_hour_of_the_entry_moment() {
        assert_eq!(utc_hour(1_790_910_900, 845.0), 3); // the window of 2026-10-02 03:15
        assert_eq!(utc_hour(1_790_923_500, 899.0), 6); // the window of 06:45
        assert_eq!(utc_hour(1_790_923_500, 900.0), 7);
    }

    #[test]
    fn entry_ms_truncates_like_pandas() {
        assert_eq!(entry_ms(1_792_115_100_000, 841.716), 1_792_115_941_716);
        assert_eq!(entry_ms(1_792_029_600_000, 840.0), 1_792_030_440_000);
    }

    #[test]
    fn parse_utc_ms_reads_both_forms() {
        assert_eq!(parse_utc_ms("2026-10-15T00:00:00Z"), Some(1_792_022_400_000));
        assert_eq!(parse_utc_ms("2026-08-19T04:00Z"), Some(1_787_112_000_000));
        assert_eq!(parse_utc_ms("2026-09-07T00:00:00Z"), Some(1_788_739_200_000));
        assert_eq!(parse_utc_ms("2026-13-01T00:00Z"), None);
        assert_eq!(parse_utc_ms("2026-10-15"), None);
    }

    #[test]
    fn coins_parse_and_print() {
        assert_eq!(Coin::parse("btc"), Some(Coin::Btc));
        assert_eq!(Coin::parse("xrp"), None);
        assert_eq!(Coin::Sol.as_str(), "sol");
        assert_eq!(Coin::Eth.binance_symbol(), "ETHUSDT");
    }
}
