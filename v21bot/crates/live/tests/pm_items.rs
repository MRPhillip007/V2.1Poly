//! The frame reader of the Polymarket market channel and the feed items it makes (spec §5.2): the
//! ask ladders of pmbot's scanner on 2 425 real messages, the bbo rows of `paper/feeds.parse_ws` on
//! 3 000 frames recorded on 2026-10-19, and the rules on small frames.

use std::collections::HashMap;
use std::io::Read;

use serde::Deserialize;
use v21_live::book::{Book, BookSide};
use v21_live::hot::items::{read_frame, FeedItem};
use v21_live::hot::wire::Frame;

fn ids(ids: &[&str]) -> Vec<Box<str>> {
    ids.iter().map(|id| Box::from(*id)).collect()
}

/// The items of one frame and the count of its unreadable parts.
fn items(raw: &str, tokens: &[Box<str>]) -> (Vec<FeedItem>, u32) {
    let (mut frame, mut out) = (Frame::default(), vec![]);
    let bad = read_frame(raw.as_bytes(), tokens, &mut frame, &mut out);
    (out, bad)
}

// ---------------------------------------------------------------- the scanner's ask ladders

#[derive(Deserialize)]
struct Vectors {
    sources: Vec<Source>,
}

#[derive(Deserialize)]
struct Source {
    name: String,
    tokens: Vec<String>,
    levels: usize,
    messages: Vec<String>,
    /// For each message, the ladder updates the scanner made of it.
    updates: Vec<Vec<Want>>,
}

/// `[token, venue_ts, [[price, shares], ...]]`.
#[derive(Deserialize)]
struct Want(String, i64, Vec<(f64, f64)>);

#[test]
fn ask_ladders_match_the_scanner_on_real_messages() {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/feed_vectors.json")).unwrap();
    let vectors: Vectors = serde_json::from_str(&text).unwrap();
    let mut compared = 0;
    for src in &vectors.sources {
        let tokens: Vec<Box<str>> = src.tokens.iter().map(|t| Box::from(t.as_str())).collect();
        let mut books: Vec<Book> = tokens.iter().map(|_| Book::new()).collect();
        // as the scanner: a token's changes count only once a snapshot of it has come
        let mut ready = vec![false; tokens.len()];
        let (mut frame, mut out, mut ladder) = (Frame::default(), vec![], vec![]);
        for (i, (raw, wanted)) in src.messages.iter().zip(&src.updates).enumerate() {
            out.clear();
            read_frame(raw.as_bytes(), &tokens, &mut frame, &mut out);
            for item in &out {
                match *item {
                    FeedItem::Snapshot { token, .. } => {
                        books[usize::from(token)].clear();
                        ready[usize::from(token)] = true;
                    }
                    FeedItem::SnapLevel { token, side, tick, size } => books[usize::from(token)].set(side, tick, size),
                    FeedItem::Level { token, side, tick, size, best_bid, best_ask, .. } if ready[usize::from(token)] => {
                        let book = &mut books[usize::from(token)];
                        if let Some(tick) = tick {
                            book.set(side, tick, size);
                        }
                        book.trim(best_bid, best_ask);
                    }
                    _ => {}
                }
            }
            // the last update of a token in the message is its ladder after the message
            let last: HashMap<&str, &Want> = wanted.iter().map(|w| (w.0.as_str(), w)).collect();
            for (token, want) in last {
                let ix = src.tokens.iter().position(|t| t == token).unwrap();
                // the update is stamped with the venue time of one of the token's items
                let stamped = out.iter().any(|item| match *item {
                    FeedItem::Snapshot { token, ts_ms } | FeedItem::Level { token, ts_ms, .. } => usize::from(token) == ix && ts_ms == want.1,
                    _ => false,
                });
                assert!(stamped, "{} message {i}: no item of {token} at {}", src.name, want.1);
                books[ix].asks_upto(1.0, &mut ladder);
                ladder.truncate(src.levels);
                assert_eq!(ladder, want.2, "{} message {i}: token {token}\n{raw}", src.name);
                compared += 1;
            }
        }
    }
    assert!(compared > 1000, "{compared} ladders compared");
}

// ---------------------------------------------------------------- paper's bbo rows

#[derive(Deserialize)]
struct BboVectors {
    tokens: Vec<String>,
    frames: Vec<BboFrame>,
}

#[derive(Deserialize)]
struct BboFrame {
    raw: String,
    /// `[token, venue ms, bid, ask]` as `paper/feeds.parse_ws` gives them.
    rows: Vec<(String, i64, f64, f64)>,
}

#[test]
fn bbo_matches_paper_parse_ws_on_real_frames() {
    let file = std::fs::File::open(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/bbo_vectors.json.gz")).unwrap();
    let mut text = String::new();
    flate2::read::GzDecoder::new(file).read_to_string(&mut text).unwrap();
    let v: BboVectors = serde_json::from_str(&text).unwrap();
    let tokens: Vec<Box<str>> = v.tokens.iter().map(|t| Box::from(t.as_str())).collect();
    let mut rows = 0;
    for (i, f) in v.frames.iter().enumerate() {
        let (out, bad) = items(&f.raw, &tokens);
        let got: Vec<(String, i64, f64, f64)> = out
            .iter()
            .filter_map(|item| match *item {
                FeedItem::Bbo { token, ts_ms, bid, ask } => Some((v.tokens[usize::from(token)].clone(), ts_ms, bid, ask)),
                _ => None,
            })
            .collect();
        assert_eq!(got, f.rows, "frame {i}: {}", f.raw);
        assert_eq!(bad, 0, "frame {i}");
        rows += got.len();
    }
    assert!(rows > 5000, "{rows} rows");
}

// ---------------------------------------------------------------- the rules

#[test]
fn a_book_gives_snapshot_levels_then_its_bbo() {
    let raw = r#"{"event_type":"book","asset_id":"B","timestamp":"7",
                  "bids":[{"price":"0.49","size":"2"},{"price":"0.48","size":"0"}],"asks":[{"price":"0.51","size":"5"}]}"#;
    let (got, bad) = items(raw, &ids(&["A", "B"]));
    assert_eq!(bad, 0);
    assert_eq!(
        got,
        [
            FeedItem::Snapshot { token: 1, ts_ms: 7 },
            FeedItem::SnapLevel { token: 1, side: BookSide::Bid, tick: 490, size: 2.0 },
            FeedItem::SnapLevel { token: 1, side: BookSide::Ask, tick: 510, size: 5.0 },
            FeedItem::Bbo { token: 1, ts_ms: 7, bid: 0.49, ask: 0.51 },
        ]
    );
    // an empty side reads as bid 0.0 / ask 1.0
    let (got, _) = items(r#"{"event_type":"book","asset_id":"A","timestamp":"8","bids":[],"asks":[]}"#, &ids(&["A"]));
    assert_eq!(got, [FeedItem::Snapshot { token: 0, ts_ms: 8 }, FeedItem::Bbo { token: 0, ts_ms: 8, bid: 0.0, ask: 1.0 }]);
}

#[test]
fn a_price_change_gives_a_level_then_its_bbo_with_defaults_0_and_1() {
    let raw = r#"{"event_type":"price_change","timestamp":"8","price_changes":[
                  {"asset_id":"A","price":"0.52","size":"4","side":"SELL","best_bid":"0.5","best_ask":"0.51"},
                  {"asset_id":"A","price":"0.4","size":"0","side":"BUY"}]}"#;
    let (got, bad) = items(raw, &ids(&["A"]));
    assert_eq!(bad, 0);
    assert_eq!(
        got,
        [
            FeedItem::Level { token: 0, ts_ms: 8, side: BookSide::Ask, tick: Some(520), size: 4.0, best_bid: 0.5, best_ask: 0.51 },
            FeedItem::Bbo { token: 0, ts_ms: 8, bid: 0.5, ask: 0.51 },
            FeedItem::Level { token: 0, ts_ms: 8, side: BookSide::Bid, tick: Some(400), size: 0.0, best_bid: 0.0, best_ask: 1.0 },
            FeedItem::Bbo { token: 0, ts_ms: 8, bid: 0.0, ask: 1.0 },
        ]
    );
}

#[test]
fn an_off_grid_snapshot_is_not_taken_but_its_bbo_is() {
    let raw = r#"{"event_type":"book","asset_id":"A","timestamp":"9","bids":[],"asks":[{"price":"0.5004","size":"5"}]}"#;
    let (got, bad) = items(raw, &ids(&["A"]));
    assert_eq!(got, [FeedItem::Bbo { token: 0, ts_ms: 9, bid: 0.0, ask: 0.5004 }]);
    assert_eq!(bad, 1);
    // an off-grid change keeps its best prices for the trim
    let raw = r#"{"event_type":"price_change","timestamp":"10","price_changes":[
                  {"asset_id":"A","price":"0.5004","size":"5","side":"SELL","best_bid":"0.4","best_ask":"0.5"}]}"#;
    let (got, bad) = items(raw, &ids(&["A"]));
    assert_eq!(bad, 1);
    assert_eq!(got[0], FeedItem::Level { token: 0, ts_ms: 10, side: BookSide::Ask, tick: None, size: 5.0, best_bid: 0.4, best_ask: 0.5 });
}

#[test]
fn not_json_gives_no_items_and_pong_is_not_unreadable() {
    let tokens = ids(&["A"]);
    assert_eq!(items("{oops", &tokens), (vec![], 1));
    for idle in ["PONG", "pong", "", " \n"] {
        assert_eq!(items(idle, &tokens), (vec![], 0), "{idle:?}");
    }
    // events that cannot be read are counted, the rest of the frame still counts
    let raw = r#"[{"event_type":"book","asset_id":"A","asks":[]},{"event_type":"book","asset_id":"A","timestamp":"3","asks":[]}]"#;
    let (got, bad) = items(raw, &tokens);
    assert_eq!((got.len(), bad), (2, 1));
}

#[test]
fn tokens_outside_the_list_give_no_items() {
    let tokens = ids(&["A"]);
    let raw = r#"[{"event_type":"book","asset_id":"Z","timestamp":"1","asks":[{"price":"0.5","size":"1"}]},
                  {"event_type":"price_change","timestamp":"2","price_changes":[{"asset_id":"Z","price":"0.5","size":"1","side":"SELL"}]},
                  {"event_type":"tick_size_change","asset_id":"Z","timestamp":"3","new_tick_size":"0.001"}]"#;
    assert_eq!(items(raw, &tokens), (vec![], 0));
}

#[test]
fn a_tick_size_change_is_an_item() {
    let raw = r#"{"event_type":"tick_size_change","asset_id":"A","timestamp":"5","old_tick_size":"0.01","new_tick_size":"0.001"}"#;
    assert_eq!(items(raw, &ids(&["A"])), (vec![FeedItem::TickSize { token: 0, ts_ms: 5, tick: 0.001 }], 0));
}
