use v21_core::params::{load_strategy, Dates, ForwardRules, ParamsError};
use v21_core::pyjson::{canonical_json, canonical_sha256, py_float_repr};
use v21_core::window::Coin;

const V2: &str = include_str!("data/strategy_v2.json");
const V21: &str = include_str!("data/strategy_v2_1.json");

#[test]
fn py_float_repr_matches_python() {
    for (x, want) in [
        (0.7, "0.7"),
        (0.88, "0.88"),
        (1.25, "1.25"),
        (0.014, "0.014"),
        (100.0, "100.0"),
        (1e-05, "1e-05"),
        (1e16, "1e+16"),
        (0.0001, "0.0001"),
        (0.06, "0.06"),
        (1207.0, "1207.0"),
        (1e15, "1000000000000000.0"),
        (123456789012345.0, "123456789012345.0"),
        (1.5e-07, "1.5e-07"),
        (-0.5, "-0.5"),
        (0.0, "0.0"),
    ] {
        assert_eq!(py_float_repr(x), want, "{x:e}");
    }
}

#[test]
fn canonical_json_matches_json_dumps_sort_keys() {
    let v: serde_json::Value =
        serde_json::from_str(r#"{"b": 1, "a": [0.70, "xé"], "c": {"z": true, "y": null}}"#).unwrap();
    // the backslash is a separate piece so that no editor turns the escape into the letter itself
    let want = [r#"{"a": [0.7, "x"#, "\\", r#"u00e9"], "b": 1, "c": {"y": null, "z": true}}"#].concat();
    assert_eq!(canonical_json(&v), want);
}

#[test]
fn canonical_json_escapes_like_python() {
    // json.dumps of: quote, backslash, newline, tab, U+0001, U+007F and U+1F600 (a grinning face,
    // beyond the BMP) — checked in Python on 2026-10-18; backslashes kept as separate pieces
    let v = serde_json::Value::String("q\"b\\\n\t\u{1}\u{7f}\u{1F600}".to_string());
    let want = [r#""q\"b\\\n\t\u0001\u007f"#, "\\", "ud83d", "\\", r#"ude00""#].concat();
    assert_eq!(canonical_json(&v), want);
    assert_eq!(canonical_json(&serde_json::json!([])), "[]");
    assert_eq!(canonical_json(&serde_json::json!({})), "{}");
    assert_eq!(canonical_json(&serde_json::json!([-5, 1.0, false])), "[-5, 1.0, false]");
}

#[test]
fn frozen_v2_hash_is_the_registered_one() {
    assert_eq!(
        canonical_sha256(V2).unwrap(),
        "0e4ee96e6e2e4e6ec678e587a5fe57a78ff1393abd679f8dfd7972ead843e135"
    );
}

#[test]
fn strategy_values() {
    let s = load_strategy(V2, V21).unwrap();
    use Coin::*;
    assert_eq!(s.v21.watch_coins, vec![Btc, Eth, Sol]);
    assert_eq!(s.v21.trade_coins, vec![Btc, Eth]);
    assert_eq!(s.v21.hours, vec![(0, 7)]);
    assert_eq!(s.v2.watch_coins, vec![Btc, Eth, Sol]);
    assert_eq!(s.v2.trade_coins, vec![Btc, Eth, Sol]);
    assert_eq!(s.v2.hours, vec![(0, 7), (13, 16)]);
    for p in [&s.v2, &s.v21] {
        assert_eq!(
            (p.lo, p.hi, p.t0, p.t1, p.lat, p.max_fill, p.z_min, p.max_coins, p.sig_lb),
            (0.70, 0.88, 840.0, 870.0, 0.3, 0.88, 1.25, 2, 300)
        );
        assert_eq!((p.fee, p.slip, p.incident_buffer_min), (0.07, 0.014, 30));
    }
    assert_eq!(s.forward, ForwardRules { stop_after_trades: 100, stop_if_win_below: 0.80 });
    assert_eq!(
        s.dates,
        Dates {
            research_split_ms: 1_788_739_200_000,
            rule_selected_ms: 1_790_848_800_000,
            v2_forward_ms: 1_790_881_200_000,
            v21_forward_ms: 1_792_022_400_000,
        }
    );
}

#[test]
fn a_changed_v2_is_refused() {
    let tampered = V2.replace("\"distance_min_sigma\": 1.25", "\"distance_min_sigma\": 1.3");
    assert_ne!(tampered, V2);
    assert!(matches!(load_strategy(&tampered, V21), Err(ParamsError::BaseHash { .. })));
}

#[test]
fn bad_json_is_an_error_not_a_panic() {
    assert!(matches!(load_strategy("{", V21), Err(ParamsError::Json(_))));
    assert!(matches!(load_strategy(V2, "not json"), Err(ParamsError::Json(_))));
}

/// A v2 file with pairwise different values, so a swap of two keys cannot go unseen, and the v2.1
/// file registered on top of it.
fn synthetic() -> (String, String) {
    let v2 = r#"{
      "markets": {"coins": ["eth"]},
      "entry": {"price_band": [0.6, 0.9], "monitor_from_s": 100, "monitor_to_s": 200,
                "fill_latency_s": 0.5, "max_fill_price": 0.95},
      "filters": {"distance_min_sigma": 2.5, "max_coins_per_window": 1, "utc_hours": [[1, 2], [5, 6]],
                  "sigma_lookback_s": 120, "skip_incident_windows_buffer_min": 15},
      "costs": {"taker_fee_rate": 0.05, "slippage_per_share": 0.02},
      "research_split_utc": "2026-01-02T03:04:05Z",
      "rule_selected_utc": "2026-02-03T04:05:06Z",
      "forward_start_utc": "2026-03-04T05:06:07Z"
    }"#
    .to_string();
    let v21 = format!(
        r#"{{"base": {{"sha256_canonical": "{}"}},
            "changes": {{"trade_coins": ["eth"], "utc_hours": [[3, 4]]}},
            "forward_start_utc": "2026-04-05T06:07:08Z",
            "forward_rules": {{"stop_after_trades": 7, "stop_if_win_below": 0.6}}}}"#,
        canonical_sha256(&v2).unwrap()
    );
    (v2, v21)
}

#[test]
fn every_field_is_read_from_its_own_key() {
    let (v2, v21) = synthetic();
    let s = load_strategy(&v2, &v21).unwrap();
    let p = &s.v2;
    assert_eq!(p.watch_coins, vec![Coin::Eth]);
    assert_eq!(p.trade_coins, vec![Coin::Eth]);
    assert_eq!(
        (p.lo, p.hi, p.t0, p.t1, p.lat, p.max_fill, p.z_min, p.max_coins, p.sig_lb),
        (0.6, 0.9, 100.0, 200.0, 0.5, 0.95, 2.5, 1, 120)
    );
    assert_eq!(p.hours, vec![(1, 2), (5, 6)]);
    assert_eq!((p.fee, p.slip, p.incident_buffer_min), (0.05, 0.02, 15));
    assert_eq!(s.v21.hours, vec![(3, 4)]);
    assert_eq!(s.v21.lo, 0.6);
    assert_eq!(s.forward, ForwardRules { stop_after_trades: 7, stop_if_win_below: 0.6 });
    assert_eq!(
        s.dates,
        Dates {
            research_split_ms: v21_core::window::parse_utc_ms("2026-01-02T03:04:05Z").unwrap(),
            rule_selected_ms: v21_core::window::parse_utc_ms("2026-02-03T04:05:06Z").unwrap(),
            v2_forward_ms: v21_core::window::parse_utc_ms("2026-03-04T05:06:07Z").unwrap(),
            v21_forward_ms: v21_core::window::parse_utc_ms("2026-04-05T06:07:08Z").unwrap(),
        }
    );
}

#[test]
fn a_missing_or_unknown_field_is_named() {
    let (v2, v21) = synthetic();
    let no_band = v2.replace(r#""price_band": [0.6, 0.9], "#, "");
    let v21_for = |text: &str| {
        let wrong = canonical_sha256(&v2).unwrap();
        v21.replace(&wrong, &canonical_sha256(text).unwrap())
    };
    match load_strategy(&no_band, &v21_for(&no_band)) {
        Err(ParamsError::Field(name)) => assert_eq!(name, "entry.price_band"),
        other => panic!("{other:?}"),
    }
    let xrp = v2.replace(r#"["eth"]"#, r#"["xrp"]"#);
    match load_strategy(&xrp, &v21_for(&xrp)) {
        Err(ParamsError::Field(name)) => assert_eq!(name, "markets.coins"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn root_files_equal_the_vendored_ones() {
    let Ok(root) = std::env::var("V21_ROOT") else {
        println!("skipped: V21_ROOT is not set");
        return;
    };
    let read = |name: &str| std::fs::read_to_string(std::path::Path::new(&root).join(name)).unwrap();
    assert_eq!(load_strategy(&read("strategy_v2.json"), &read("strategy_v2_1.json")).unwrap(),
               load_strategy(V2, V21).unwrap());
}
