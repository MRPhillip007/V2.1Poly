//! `v21 check` (spec §4.4, the part without keys): can the bot work from here? The geoblock, the clock,
//! the path to the CLOB, Binance on both addresses, the copies of the market channel on the current
//! window's tokens, and the rules of its markets. The exit code: 0 — all is well, 1 — warnings, 2 — the
//! bot cannot work.

use std::time::{Duration, Instant};

use v21_core::window::Coin;

use crate::cold::binance_rest::server_time_ms;
use crate::cold::clob_info::{fetch_market_info, measure_rtt, rules_note};
use crate::cold::gamma::fetch_window;
use crate::cold::http_client;
use crate::cold::ntp::{clock_offset_ms, sntp_offset_ms};
use crate::hot::binance::{BinanceEvent, BinanceStream};
use crate::hot::copies::FrameDedupe;
use crate::hot::ws::{CopyEvent, PmCopy};
use crate::shared::clock::local_ms;
use crate::shared::config::Config;
use crate::shared::utc::fmt_utc;

/// How long the copies of the market channel are listened to.
const LISTEN: Duration = Duration::from_secs(30);
/// Requests that measure the path to the CLOB.
const RTT_TRIES: usize = 10;
/// How long a Binance address may take to give its first trade.
const FIRST_TRADE_WAIT: Duration = Duration::from_secs(5);
const NTP_WAIT: Duration = Duration::from_secs(4);
const COINS: [Coin; 3] = [Coin::Btc, Coin::Eth, Coin::Sol];

/// The text of the check and its exit code.
pub async fn check(cfg: &Config) -> (String, i32) {
    check_with(cfg, LISTEN).await
}

/// `check` on a runtime of its own, for the command line.
pub fn check_blocking(cfg: &Config) -> anyhow::Result<(String, i32)> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    Ok(rt.block_on(check(cfg)))
}

/// The check, the copies listened to for `listen` (the tests listen less).
pub async fn check_with(cfg: &Config, listen: Duration) -> (String, i32) {
    let http = http_client();
    let mut lines = vec![];
    let mut level = 0;

    // the geoblock
    match async { http.get(&cfg.urls.geoblock).send().await?.json::<serde_json::Value>().await }.await {
        Ok(v) => {
            let country = v["country"].as_str().unwrap_or("?").to_string();
            if v["blocked"] == true {
                raise(&mut level, &mut lines, 2, format!("геоблок: закрыто ({country})"));
            } else {
                raise(&mut level, &mut lines, 0, format!("геоблок: открыто ({country})"));
            }
        }
        Err(e) => raise(&mut level, &mut lines, 1, format!("геоблок: нет ответа ({e})")),
    }

    // the clock: NTP first, Binance's time as the fallback
    let bases = [cfg.feeds.binance_rest.clone(), cfg.feeds.binance_rest_fallback.clone()];
    let ntp = if cfg.risk.ntp_server.is_empty() { None } else { tokio::time::timeout(NTP_WAIT, sntp_offset_ms(&cfg.risk.ntp_server)).await.ok().and_then(Result::ok) };
    let binance = server_time_ms(&http, &bases).await.ok().map(|(server, sent, received)| clock_offset_ms(server, sent, received));
    let mut clock = vec![];
    if let Some((offset, rtt)) = ntp {
        clock.push(format!("NTP {offset:+} мс (путь {rtt} мс)"));
    } else if !cfg.risk.ntp_server.is_empty() {
        clock.push(format!("NTP {}: нет ответа", cfg.risk.ntp_server));
    }
    clock.push(binance.map_or("Binance: нет ответа".to_string(), |o| format!("Binance {o:+} мс")));
    let offset = ntp.map(|(o, _)| o).or(binance);
    match offset {
        Some(o) if o.abs() <= cfg.risk.clock_max_offset_ms => raise(&mut level, &mut lines, 0, format!("часы: {}", clock.join(", "))),
        Some(_) => raise(&mut level, &mut lines, 1, format!("часы: {} — больше {} мс, бот будет на паузе", clock.join(", "), cfg.risk.clock_max_offset_ms)),
        None => raise(&mut level, &mut lines, 1, format!("часы: {} — поправка не измерена, бот будет на паузе", clock.join(", "))),
    }

    // the path to the CLOB
    let mut rtt = vec![];
    for _ in 0..RTT_TRIES {
        if let Ok(ms) = measure_rtt(&http, &cfg.urls.clob).await {
            rtt.push(ms);
        }
    }
    rtt.sort_by(f64::total_cmp);
    match rtt.get(rtt.len() / 2) {
        Some(m) => raise(&mut level, &mut lines, 0, format!("путь до CLOB: медиана {m:.1} мс ({} из {RTT_TRIES} запросов)", rtt.len())),
        None => raise(&mut level, &mut lines, 2, "путь до CLOB: нет ответа".to_string()),
    }

    // Binance on both addresses
    let mut up = [false; 2];
    for (k, (what, url)) in [("основной", &cfg.feeds.binance_ws), ("запасной", &cfg.feeds.binance_ws_fallback)].into_iter().enumerate() {
        let started = Instant::now();
        let mut stream = BinanceStream::new([url.clone(), url.clone()], &COINS);
        let first = tokio::time::timeout(FIRST_TRADE_WAIT, async {
            loop {
                match stream.next().await {
                    BinanceEvent::Trade(t) => return Ok(t),
                    BinanceEvent::Up { .. } => {}
                    BinanceEvent::Down { reason } => return Err(reason),
                }
            }
        })
        .await;
        match first {
            Ok(Ok(t)) => {
                up[k] = true;
                lines.push(format!("Binance {what}: первая сделка через {} мс, задержка {} мс", started.elapsed().as_millis(), local_ms() - t.ts_ms));
            }
            Ok(Err(reason)) => lines.push(format!("Binance {what}: недоступен ({reason})")),
            Err(_) => lines.push(format!("Binance {what}: недоступен (нет сделок за {} с)", FIRST_TRADE_WAIT.as_secs())),
        }
    }
    match up {
        [true, _] => {}
        [false, true] => level = level.max(1),
        [false, false] => level = level.max(2),
    }

    // the markets of the current window and their rules
    let now = local_ms() + offset.unwrap_or(0);
    let s = now.div_euclid(900_000) * 900;
    let mut tokens = vec![];
    match fetch_window(&http, &cfg.urls.gamma, s, &COINS).await {
        Ok(markets) if !markets.is_empty() => {
            let mut bad = vec![];
            for m in &markets {
                let info = fetch_market_info(&http, &cfg.urls.clob, &m.condition_id).await.ok();
                if let Some(note) = rules_note(m, info.as_ref()) {
                    bad.push(format!("{}: {note}", m.coin.as_str()));
                }
                tokens.push(m.up.clone());
                tokens.push(m.down.clone());
            }
            let missing: Vec<&str> = COINS.iter().filter(|c| !markets.iter().any(|m| m.coin == **c)).map(|c| c.as_str()).collect();
            let names = markets.iter().map(|m| m.coin.as_str()).collect::<Vec<_>>().join(", ");
            let when = &fmt_utc(s)[11..16];
            if bad.is_empty() && missing.is_empty() {
                raise(&mut level, &mut lines, 0, format!("рынки окна {when}: {names} — правила в порядке"));
            } else {
                let mut why = bad;
                if !missing.is_empty() {
                    why.push(format!("нет рынка: {}", missing.join(", ")));
                }
                raise(&mut level, &mut lines, 1, format!("рынки окна {when}: {names} — {}", why.join("; ")));
            }
        }
        Ok(_) => raise(&mut level, &mut lines, 1, "рынки текущего окна: их нет в Gamma".to_string()),
        Err(e) => raise(&mut level, &mut lines, 1, format!("рынки текущего окна: Gamma не ответила ({e:#})")),
    }

    // the copies of the market channel
    if tokens.is_empty() {
        raise(&mut level, &mut lines, 1, "Polymarket: проверить нельзя — нет токенов текущего окна".to_string());
    } else {
        let (frames, dups, first) = listen_copies(&cfg.urls.polymarket_ws, &tokens, listen).await;
        let total = frames[0] + frames[1];
        if total == 0 {
            raise(&mut level, &mut lines, 2, "Polymarket: кадров нет ни от одной копии".to_string());
        } else {
            let pct = |a: u64, b: u64| if b == 0 { 0.0 } else { a as f64 * 100.0 / b as f64 };
            let line = format!(
                "Polymarket: копия 1 — {} кадров, копия 2 — {} кадров; дубликатов {:.0}%; первой приходила копия 1 в {:.0}% случаев",
                frames[0],
                frames[1],
                pct(dups, total),
                pct(first[0], first[0] + first[1])
            );
            raise(&mut level, &mut lines, if frames.contains(&0) { 1 } else { 0 }, line);
        }
    }

    lines.push(
        match level {
            0 => "итог: можно работать",
            1 => "итог: есть предупреждения",
            _ => "итог: работать нельзя",
        }
        .to_string(),
    );
    (lines.join("\n"), level)
}

/// A line of the check, and the level it raises the check to.
fn raise(level: &mut i32, lines: &mut Vec<String>, to: i32, line: String) {
    *level = (*level).max(to);
    lines.push(line);
}

/// Two copies of the market channel for `listen`: frames of each, duplicates, and how often each
/// copy brought a frame first.
async fn listen_copies(url: &str, tokens: &[String], listen: Duration) -> ([u64; 2], u64, [u64; 2]) {
    let mut copies = [PmCopy::new(url, tokens, 0), PmCopy::new(url, tokens, 1)];
    let mut dedupe = FrameDedupe::new(4096, 2, 3_000);
    let (mut frames, mut dups, mut first) = ([0u64; 2], 0u64, [0u64; 2]);
    let until = tokio::time::Instant::now() + listen;
    loop {
        let [a, b] = &mut copies;
        let ev = tokio::select! {
            ev = a.next() => ev,
            ev = b.next() => ev,
            () = tokio::time::sleep_until(until) => break,
        };
        if let CopyEvent::Frame { copy, raw } = ev {
            let k = usize::from(copy.min(1));
            frames[k] += 1;
            if dedupe.admit(copy, &raw, local_ms()) {
                first[k] += 1;
            } else {
                dups += 1;
            }
        }
    }
    (frames, dups, first)
}
