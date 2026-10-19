//! Reading a market-websocket frame into plain values: the port of `pmbot/src/hot/wire.rs`, with the
//! `best_bid` of a change and the bid rows of a book read as well (spec §5.2 needs both).
//!
//! A frame is one JSON object or an array of them. Here it is read in one pass into small values
//! that borrow nothing from it (`Event`, `Change`, `Level`), kept in buffers that the next frame
//! reuses, so that once the buffers have grown to the biggest frame reading allocates only for
//! the odd string with an escape in it. The feed then applies the events one after another.
//!
//! The scanner reads a frame with `json.loads` and picks the fields out of each event with
//! `float()`, `int()` and `==`, and a field it cannot read spoils only its own event. That is
//! kept: whatever the JSON has in a field, reading it does not fail, it gives a value that says
//! "unreadable" (`None`, `Token::Unreadable`, `List::Unreadable`) and the feed decides what that
//! costs. What does fail is JSON that is not JSON, and then no event of the frame is read, as in
//! the scanner. A number too big for a double (`1e999`) counts as not JSON, as it does for
//! `orjson`, which the scanner uses where it is installed.

use std::borrow::Cow;
use std::fmt;
use std::ops::Range;

use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;

/// What an `asset_id` stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Token {
    /// One of the tokens the feed follows: its place in the list.
    Ours(u8),
    /// Another token, or a value of another kind (a number, `null`): not this feed's business.
    #[default]
    Other,
    /// No `asset_id`, or an array or an object in its place: the event cannot be read.
    Unreadable,
}

/// The side of a price change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Buy,
    Sell,
    /// Missing, not text, or neither `BUY` nor `SELL` (the scanner's `Book.change` refuses it).
    Invalid,
}

/// What an event is, by its `event_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Kind {
    Book,
    PriceChange,
    TickSize,
    /// Any other event type, or none: ignored.
    #[default]
    Other,
}

/// One level of a `book` event as the server wrote it.
#[derive(Debug, Clone, Copy)]
pub struct Level {
    pub price: f64,
    pub size: f64,
}

/// One change of a `price_change` event.
#[derive(Debug, Clone, Copy)]
pub struct Change {
    pub token: Token,
    /// `None` if missing or unreadable.
    pub price: Option<f64>,
    pub size: Option<f64>,
    pub side: Side,
    /// The server's best bid after the change; 0.0 (an empty bid side) if missing or unreadable.
    pub best_bid: f64,
    /// The server's best ask after the change; 1.0 (an empty ask side) if missing or unreadable.
    pub best_ask: f64,
}

impl Default for Change {
    /// A change that is not an object: nothing in it can be read.
    fn default() -> Change {
        Change {
            token: Token::Unreadable,
            price: None,
            size: None,
            side: Side::Invalid,
            best_bid: 0.0,
            best_ask: 1.0,
        }
    }
}

/// A list-valued field of an event, as the scanner's `ev.get(name) or ...` sees it.
#[derive(Debug, Clone, Default)]
pub enum List {
    /// Absent, `null`, `false`, 0, `""`, `[]` or `{}`: nothing there, and `or` moves on.
    #[default]
    Empty,
    /// The elements are `rows[range]` (levels) or `changes[range]` (changes) of the frame.
    Items(Range<usize>),
    /// Something that cannot be read as a list of rows: a number, a text, `true`, an object with
    /// entries, or a list with a row that cannot be read.
    Unreadable,
}

/// One event of a frame, with the fields of every kind of event; the ones that do not belong to
/// its kind stay empty.
#[derive(Debug, Clone, Default)]
pub struct Event {
    pub kind: Kind,
    /// The token of a `book` or a `tick_size_change`.
    pub token: Token,
    /// The venue's time; `None` if missing or unreadable.
    pub ts: Option<i64>,
    /// The bids of a `book`, `bids` or else `buys`: `rows[range]`.
    pub bids: List,
    /// The asks of a `book`, `asks` or else `sells`: `rows[range]`.
    pub asks: List,
    /// The changes of a `price_change`: `changes[range]`.
    pub changes: List,
    /// The `new_tick_size` of a `tick_size_change`.
    pub tick: Option<f64>,
}

/// The buffers a frame is read into. The feed keeps one and passes it to every `read`.
#[derive(Debug, Default)]
pub struct Frame {
    pub events: Vec<Event>,
    pub changes: Vec<Change>,
    pub rows: Vec<Option<Level>>,
}

/// Reads `raw` into `frame`, whose earlier contents go. `tokens` are the ids the feed follows.
/// An error is JSON that is not JSON (or has text after the value); `frame` is then of no use.
pub fn read(
    raw: &[u8],
    tokens: &[Box<str>],
    frame: &mut Frame,
) -> Result<(), serde_json::Error> {
    let Frame {
        events,
        changes,
        rows,
    } = frame;
    events.clear();
    changes.clear();
    rows.clear();
    let visitor = ReadFrame {
        events,
        reader: ReadEvent {
            tokens,
            changes,
            rows,
        },
    };
    let mut json = serde_json::Deserializer::from_slice(raw);
    Deserializer::deserialize_any(&mut json, visitor)?;
    json.end()
}

// -- a JSON value cut down to what the scanner reads of it -----------------------------------------

/// A JSON value as the fields of an event are read: text and numbers are kept, anything else is
/// only its kind. Arrays and objects are skipped over, not kept.
enum Scalar<'a> {
    /// The key is not there.
    Missing,
    /// `null` or a boolean.
    Other,
    /// An array or an object.
    Compound,
    Int(i64),
    Float(f64),
    Text(Cow<'a, str>),
}

impl Scalar<'_> {
    /// What `float(x)` makes of it in the scanner: a number, or text that reads as one (spaces
    /// around it are fine). Nothing else.
    fn float(&self) -> Option<f64> {
        match self {
            Scalar::Int(v) => Some(*v as f64),
            Scalar::Float(v) => Some(*v),
            Scalar::Text(text) => text.trim().parse().ok(),
            _ => None,
        }
    }

    /// What `int(x)` makes of it: a whole number, text that reads as one, or a finite number cut
    /// toward zero.
    fn int(&self) -> Option<i64> {
        match self {
            Scalar::Int(v) => Some(*v),
            Scalar::Float(v) if v.is_finite() && v.abs() < 9.2e18 => Some(*v as i64),
            Scalar::Text(text) => text.trim().parse().ok(),
            _ => None,
        }
    }

    fn text(&self) -> Option<&str> {
        match self {
            Scalar::Text(text) => Some(text),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for Scalar<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ScalarVisitor)
    }
}

struct ScalarVisitor;

impl<'de> Visitor<'de> for ScalarVisitor {
    type Value = Scalar<'de>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_unit<E>(self) -> Result<Scalar<'de>, E> {
        Ok(Scalar::Other)
    }

    fn visit_bool<E>(self, _: bool) -> Result<Scalar<'de>, E> {
        Ok(Scalar::Other)
    }

    fn visit_i64<E>(self, v: i64) -> Result<Scalar<'de>, E> {
        Ok(Scalar::Int(v))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Scalar<'de>, E> {
        Ok(i64::try_from(v).map_or(Scalar::Float(v as f64), Scalar::Int))
    }

    fn visit_f64<E>(self, v: f64) -> Result<Scalar<'de>, E> {
        Ok(Scalar::Float(v))
    }

    fn visit_borrowed_str<E>(self, v: &'de str) -> Result<Scalar<'de>, E> {
        Ok(Scalar::Text(Cow::Borrowed(v)))
    }

    /// A string with an escape in it is unescaped into a buffer of the parser, so it is copied.
    fn visit_str<E>(self, v: &str) -> Result<Scalar<'de>, E> {
        Ok(Scalar::Text(Cow::Owned(v.to_owned())))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Scalar<'de>, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(Scalar::Compound)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Scalar<'de>, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(Scalar::Compound)
    }
}

/// Which token an `asset_id` names: `ids` is the list the feed follows.
fn resolve(ids: &[Box<str>], asset_id: &Scalar) -> Token {
    match asset_id {
        Scalar::Text(text) => ids
            .iter()
            .position(|id| &**id == text.as_ref())
            .and_then(|at| u8::try_from(at).ok())
            .map_or(Token::Other, Token::Ours),
        // `ev['asset_id']` is a KeyError when missing, and a TypeError as a dict key when a list
        // or a dict
        Scalar::Missing | Scalar::Compound => Token::Unreadable,
        Scalar::Other | Scalar::Int(_) | Scalar::Float(_) => Token::Other,
    }
}

fn side_of(side: &Scalar) -> Side {
    match side.text() {
        Some("BUY") => Side::Buy,
        Some("SELL") => Side::Sell,
        _ => Side::Invalid,
    }
}

fn kind_of(event_type: &Scalar) -> Kind {
    match event_type.text() {
        Some("book") => Kind::Book,
        Some("price_change") => Kind::PriceChange,
        Some("tick_size_change") => Kind::TickSize,
        _ => Kind::Other,
    }
}

// -- objects and lists, read into the buffers of the frame ---------------------------------------

/// The names of the fields of an event that the feed reads. The rest are skipped.
#[derive(Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum EventField {
    EventType,
    AssetId,
    Timestamp,
    Bids,
    Buys,
    Asks,
    Sells,
    PriceChanges,
    NewTickSize,
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum ChangeField {
    AssetId,
    Price,
    Size,
    Side,
    BestBid,
    BestAsk,
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum RowField {
    Price,
    Size,
    #[serde(other)]
    Other,
}

/// Reads an object. What it makes of anything that is not an object is `Out::default()`.
trait ReadObject {
    type Out: Default;

    fn read<'de, A: MapAccess<'de>>(&mut self, map: A) -> Result<Self::Out, A::Error>;

    /// Whether an element read as `out` counts as read in the list that holds it.
    fn readable(_out: &Self::Out) -> bool {
        true
    }
}

/// The scalar methods of a `Visitor` that all give the same `Value`.
macro_rules! scalars_give {
    ($value:expr) => {
        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok($value)
        }

        fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
            Ok($value)
        }

        fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
            Ok($value)
        }

        fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
            Ok($value)
        }

        fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
            Ok($value)
        }

        fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
            Ok($value)
        }
    };
}

/// A value that should be an object, read by `R`; if it is anything else it is skipped.
struct OrSkip<'r, R>(&'r mut R);

impl<'de, R: ReadObject> DeserializeSeed<'de> for OrSkip<'_, R> {
    type Value = R::Out;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<R::Out, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de, R: ReadObject> Visitor<'de> for OrSkip<'_, R> {
    type Value = R::Out;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<R::Out, A::Error> {
        self.0.read(map)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<R::Out, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(Default::default())
    }

    scalars_give!(Default::default());
}

/// A list of objects read by `R` into `out`. Its elements stay in `out`, and the list says where.
struct ReadList<'r, R: ReadObject> {
    reader: R,
    out: &'r mut Vec<R::Out>,
}

impl<'de, R: ReadObject> DeserializeSeed<'de> for ReadList<'_, R> {
    type Value = List;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<List, D::Error> {
        deserializer.deserialize_any(self)
    }
}

/// What a value that is not a list makes of `ev.get(name) or ...`: falsy is nothing there, and
/// anything else is not a list of rows.
fn not_a_list(truthy: bool) -> List {
    if truthy {
        List::Unreadable
    } else {
        List::Empty
    }
}

impl<'de, R: ReadObject> Visitor<'de> for ReadList<'_, R> {
    type Value = List;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_seq<A: SeqAccess<'de>>(mut self, mut seq: A) -> Result<List, A::Error> {
        let start = self.out.len();
        let mut count = 0;
        let mut all_read = true;
        while let Some(item) = seq.next_element_seed(OrSkip(&mut self.reader))? {
            count += 1;
            all_read &= R::readable(&item);
            self.out.push(item);
        }
        Ok(match (count, all_read) {
            (0, _) => List::Empty,
            (_, true) => List::Items(start..self.out.len()),
            (_, false) => List::Unreadable,
        })
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<List, A::Error> {
        let mut entries = false;
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {
            entries = true;
        }
        Ok(not_a_list(entries))
    }

    fn visit_unit<E>(self) -> Result<List, E> {
        Ok(List::Empty)
    }

    fn visit_bool<E>(self, v: bool) -> Result<List, E> {
        Ok(not_a_list(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<List, E> {
        Ok(not_a_list(v != 0))
    }

    fn visit_u64<E>(self, v: u64) -> Result<List, E> {
        Ok(not_a_list(v != 0))
    }

    fn visit_f64<E>(self, v: f64) -> Result<List, E> {
        Ok(not_a_list(v != 0.0))
    }

    fn visit_str<E>(self, v: &str) -> Result<List, E> {
        Ok(not_a_list(!v.is_empty()))
    }
}

/// A row of a `book` event: `{"price": .., "size": ..}`.
struct ReadRow;

impl ReadObject for ReadRow {
    type Out = Option<Level>;

    fn read<'de, A: MapAccess<'de>>(&mut self, mut map: A) -> Result<Option<Level>, A::Error> {
        let (mut price, mut size) = (Scalar::Missing, Scalar::Missing);
        while let Some(field) = map.next_key::<RowField>()? {
            match field {
                RowField::Price => price = map.next_value()?,
                RowField::Size => size = map.next_value()?,
                RowField::Other => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        // both are needed, as `r['price']` and `r['size']` are in the scanner
        Ok(price
            .float()
            .zip(size.float())
            .map(|(price, size)| Level { price, size }))
    }

    fn readable(row: &Option<Level>) -> bool {
        row.is_some()
    }
}

/// An element of `price_changes`.
struct ReadChange<'r> {
    tokens: &'r [Box<str>],
}

impl ReadObject for ReadChange<'_> {
    type Out = Change;

    fn read<'de, A: MapAccess<'de>>(&mut self, mut map: A) -> Result<Change, A::Error> {
        let mut asset_id = Scalar::Missing;
        let (mut price, mut size, mut side, mut best_bid, mut best_ask) = (
            Scalar::Missing,
            Scalar::Missing,
            Scalar::Missing,
            Scalar::Missing,
            Scalar::Missing,
        );
        while let Some(field) = map.next_key::<ChangeField>()? {
            match field {
                ChangeField::AssetId => asset_id = map.next_value()?,
                ChangeField::Price => price = map.next_value()?,
                ChangeField::Size => size = map.next_value()?,
                ChangeField::Side => side = map.next_value()?,
                ChangeField::BestBid => best_bid = map.next_value()?,
                ChangeField::BestAsk => best_ask = map.next_value()?,
                ChangeField::Other => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(Change {
            token: resolve(self.tokens, &asset_id),
            price: price.float(),
            size: size.float(),
            side: side_of(&side),
            // `_num(pc.get('best_bid'), 0.0)` and `_num(pc.get('best_ask'), 1.0)`
            best_bid: best_bid.float().unwrap_or(0.0),
            best_ask: best_ask.float().unwrap_or(1.0),
        })
    }
}

/// An event.
struct ReadEvent<'r> {
    tokens: &'r [Box<str>],
    changes: &'r mut Vec<Change>,
    rows: &'r mut Vec<Option<Level>>,
}

impl ReadEvent<'_> {
    /// The value of the next entry of `map` as a list of rows.
    fn rows<'de, A: MapAccess<'de>>(&mut self, map: &mut A) -> Result<List, A::Error> {
        map.next_value_seed(ReadList {
            reader: ReadRow,
            out: &mut *self.rows,
        })
    }
}

/// `first`, or `second` if `first` is empty: Python's `first or second`.
fn or_else(first: List, second: List) -> List {
    if matches!(first, List::Empty) {
        second
    } else {
        first
    }
}

impl ReadObject for ReadEvent<'_> {
    type Out = Option<Event>;

    fn read<'de, A: MapAccess<'de>>(&mut self, mut map: A) -> Result<Option<Event>, A::Error> {
        let (mut event_type, mut asset_id, mut timestamp, mut new_tick_size) = (
            Scalar::Missing,
            Scalar::Missing,
            Scalar::Missing,
            Scalar::Missing,
        );
        let (mut bids, mut buys, mut asks, mut sells, mut changes) = (
            List::Empty,
            List::Empty,
            List::Empty,
            List::Empty,
            List::Empty,
        );
        // The same name twice: the last one counts, as in a Python dict.
        while let Some(field) = map.next_key::<EventField>()? {
            match field {
                EventField::EventType => event_type = map.next_value()?,
                EventField::AssetId => asset_id = map.next_value()?,
                EventField::Timestamp => timestamp = map.next_value()?,
                EventField::NewTickSize => new_tick_size = map.next_value()?,
                EventField::Bids => bids = self.rows(&mut map)?,
                EventField::Buys => buys = self.rows(&mut map)?,
                EventField::Asks => asks = self.rows(&mut map)?,
                EventField::Sells => sells = self.rows(&mut map)?,
                EventField::PriceChanges => {
                    changes = map.next_value_seed(ReadList {
                        reader: ReadChange {
                            tokens: self.tokens,
                        },
                        out: &mut *self.changes,
                    })?;
                }
                EventField::Other => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(Some(Event {
            kind: kind_of(&event_type),
            token: resolve(self.tokens, &asset_id),
            ts: timestamp.int(),
            bids: or_else(bids, buys),
            asks: or_else(asks, sells),
            changes,
            tick: new_tick_size.float(),
        }))
    }
}

/// A frame: an array of events, or one event. Whatever else it holds is skipped.
struct ReadFrame<'r> {
    events: &'r mut Vec<Event>,
    reader: ReadEvent<'r>,
}

impl<'de> Visitor<'de> for ReadFrame<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_seq<A: SeqAccess<'de>>(mut self, mut seq: A) -> Result<(), A::Error> {
        // an element that is not an object (a number, a list inside the list) is no event
        while let Some(event) = seq.next_element_seed(OrSkip(&mut self.reader))? {
            self.events.extend(event);
        }
        Ok(())
    }

    fn visit_map<A: MapAccess<'de>>(mut self, map: A) -> Result<(), A::Error> {
        let event = self.reader.read(map)?;
        self.events.extend(event);
        Ok(())
    }

    scalars_give!(());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(ids: &[&str]) -> Vec<Box<str>> {
        ids.iter().map(|id| Box::from(*id)).collect()
    }

    fn scalar(json: &str) -> Scalar<'_> {
        let mut de = serde_json::Deserializer::from_str(json);
        Scalar::deserialize(&mut de).unwrap()
    }

    #[test]
    fn float_is_what_python_float_takes() {
        for (json, want) in [
            (r#""0.51""#, Some(0.51)),
            (r#"" 0.51\n""#, Some(0.51)),
            (r#""1e3""#, Some(1000.0)),
            (r#""-0""#, Some(-0.0)),
            ("0.51", Some(0.51)),
            ("5", Some(5.0)),
            ("-5", Some(-5.0)),
            ("18446744073709551615", Some(u64::MAX as f64)),
            (r#""""#, None),
            (r#""x""#, None),
            (r#""0,5""#, None),
            ("null", None),
            ("true", None),
            ("[]", None),
            ("[1]", None),
            ("{}", None),
        ] {
            assert_eq!(scalar(json).float(), want, "{json}");
        }
        assert!(scalar(r#""nan""#).float().unwrap().is_nan());
        assert_eq!(scalar(r#""inf""#).float(), Some(f64::INFINITY));
        assert_eq!(Scalar::Missing.float(), None);
    }

    #[test]
    fn int_is_what_python_int_takes() {
        for (json, want) in [
            (r#""1790956443498""#, Some(1_790_956_443_498)),
            (r#"" 12 ""#, Some(12)),
            (r#""-7""#, Some(-7)),
            ("1790956443498", Some(1_790_956_443_498)),
            ("12.9", Some(12)),
            ("-12.9", Some(-12)),
            (r#""1.5""#, None),
            (r#""1e3""#, None),
            (r#""""#, None),
            (r#""now""#, None),
            ("18446744073709551615", None),
            ("1e30", None),
            ("null", None),
            ("false", None),
            ("{}", None),
        ] {
            assert_eq!(scalar(json).int(), want, "{json}");
        }
    }

    #[test]
    fn text_is_borrowed_unless_it_has_an_escape() {
        // the text of a field is not copied out of the frame ...
        assert!(matches!(scalar(r#""A""#), Scalar::Text(Cow::Borrowed("A"))));
        // ... unless it has an escape in it ("A" written as \u0041): then it is unescaped into a copy
        let escaped = r#""\u0041""#;
        assert!(
            escaped.contains(char::from(92u8)),
            "the test needs a real escape"
        );
        assert!(matches!(scalar(escaped), Scalar::Text(Cow::Owned(ref text)) if text == "A"));
    }

    #[test]
    fn text_and_tokens() {
        assert_eq!(scalar(r#""BUY""#).text(), Some("BUY"));
        assert_eq!(scalar("5").text(), None);
        let ids = ids(&["up", "down"]);
        assert_eq!(resolve(&ids, &scalar(r#""down""#)), Token::Ours(1));
        assert_eq!(resolve(&ids, &scalar(r#""other""#)), Token::Other);
        assert_eq!(resolve(&ids, &scalar(r#""DOWN""#)), Token::Other);
        assert_eq!(resolve(&ids, &scalar("5")), Token::Other);
        assert_eq!(resolve(&ids, &scalar("null")), Token::Other);
        assert_eq!(resolve(&ids, &scalar("true")), Token::Other);
        assert_eq!(resolve(&ids, &scalar("[]")), Token::Unreadable);
        assert_eq!(resolve(&ids, &scalar(r#"{"a": 1}"#)), Token::Unreadable);
        assert_eq!(resolve(&ids, &Scalar::Missing), Token::Unreadable);
    }

    #[test]
    fn a_frame_is_read_into_values() {
        let tokens = ids(&["up", "down"]);
        let mut frame = Frame::default();
        let raw = br#"[{"event_type":"book","asset_id":"down","timestamp":"7",
                        "asks":[{"price":"0.51","size":"5"}],"bids":[{"price":"0.49","size":"2"}]},
                       {"event_type":"price_change","timestamp":"8","price_changes":[
                        {"asset_id":"up","price":"0.52","size":"4","side":"SELL","best_ask":"0.51"},
                        {"asset_id":"other","side":"HOLD"}, 5]},
                       7, "x", {"event_type":"last_trade_price"}]"#;
        read(raw, &tokens, &mut frame).unwrap();
        assert_eq!(frame.events.len(), 3, "the 7 and the \"x\" are no events");
        let book = &frame.events[0];
        assert_eq!(
            (book.kind, book.token, book.ts),
            (Kind::Book, Token::Ours(1), Some(7))
        );
        assert!(matches!(&book.asks, List::Items(r) if frame.rows[r.clone()].len() == 1));
        let change = &frame.events[1];
        assert_eq!((change.kind, change.ts), (Kind::PriceChange, Some(8)));
        let List::Items(range) = &change.changes else {
            panic!("{:?}", change.changes)
        };
        let changes = &frame.changes[range.clone()];
        assert_eq!(changes.len(), 3);
        assert_eq!(changes[0].token, Token::Ours(0));
        assert_eq!((changes[0].price, changes[0].size), (Some(0.52), Some(4.0)));
        assert_eq!((changes[0].side, changes[0].best_ask), (Side::Sell, 0.51));
        assert_eq!(changes[1].token, Token::Other);
        assert_eq!(
            (changes[1].price, changes[1].side, changes[1].best_ask),
            (None, Side::Invalid, 1.0)
        );
        assert_eq!(
            changes[2].token,
            Token::Unreadable,
            "a change that is no object"
        );
        assert_eq!(frame.events[2].kind, Kind::Other);
    }

    #[test]
    fn best_bid_is_read_and_an_empty_side_is_0() {
        let tokens = ids(&["up"]);
        let mut frame = Frame::default();
        let raw = br#"{"event_type":"price_change","timestamp":"8","price_changes":[
                       {"asset_id":"up","price":"0.52","size":"4","side":"SELL","best_bid":"0.48","best_ask":"0.51"},
                       {"asset_id":"up","price":"0.53","size":"1","side":"SELL","best_bid":"n/a"},
                       {"asset_id":"up","price":"0.54","size":"1","side":"SELL"}]}"#;
        read(raw, &tokens, &mut frame).unwrap();
        let bids: Vec<f64> = frame.changes.iter().map(|c| c.best_bid).collect();
        assert_eq!(bids, [0.48, 0.0, 0.0]);
        // and the bid rows of a book are kept with the asks
        let raw = br#"{"event_type":"book","asset_id":"up","timestamp":"9","bids":[{"price":"0.47","size":"3"}],"asks":[]}"#;
        read(raw, &tokens, &mut frame).unwrap();
        let List::Items(range) = &frame.events[0].bids else {
            panic!("{:?}", frame.events[0].bids)
        };
        assert_eq!(frame.rows[range.clone()].iter().flatten().map(|l| (l.price, l.size)).collect::<Vec<_>>(), [(0.47, 3.0)]);
    }

    #[test]
    fn the_buffers_are_reused_from_frame_to_frame() {
        let tokens = ids(&["up"]);
        let mut frame = Frame::default();
        let big = format!(
            r#"{{"event_type":"book","asset_id":"up","timestamp":"1","asks":[{}],"bids":[]}}"#,
            vec![r#"{"price":"0.5","size":"1"}"#; 100].join(",")
        );
        read(big.as_bytes(), &tokens, &mut frame).unwrap();
        let seen = (
            frame.events.as_ptr(),
            frame.events.capacity(),
            frame.rows.as_ptr(),
            frame.rows.capacity(),
        );
        for _ in 0..10 {
            read(big.as_bytes(), &tokens, &mut frame).unwrap();
            read(br#"{"event_type":"pong"}"#, &tokens, &mut frame).unwrap();
            assert_eq!(frame.events.len(), 1);
            read(big.as_bytes(), &tokens, &mut frame).unwrap();
        }
        let now = (
            frame.events.as_ptr(),
            frame.events.capacity(),
            frame.rows.as_ptr(),
            frame.rows.capacity(),
        );
        assert_eq!(seen, now, "no new allocation after the first frame");
        assert_eq!(frame.rows.len(), 100, "the rows of the last frame only");
    }

    #[test]
    fn a_frame_that_is_not_json_is_an_error() {
        let tokens = ids(&["up"]);
        let mut frame = Frame::default();
        for raw in [
            &b""[..],
            b"PONG",
            b"{oops",
            b"[{}",
            b"{} {}",
            b"\xff",
            b"[1,]",
        ] {
            assert!(read(raw, &tokens, &mut frame).is_err(), "{raw:?}");
        }
        for raw in [&b"{}"[..], b"[]", b" [ ] \n", b"null", b"3", br#""x""#] {
            assert!(read(raw, &tokens, &mut frame).is_ok(), "{raw:?}");
        }
    }

    #[test]
    fn the_last_of_two_equal_names_counts() {
        let tokens = ids(&["up"]);
        let mut frame = Frame::default();
        let raw = br#"{"event_type":"book","asset_id":"up","timestamp":"1","timestamp":"2",
                       "asks":[{"price":"0.5","size":"1"}],"asks":[{"price":"0.6","size":"2"},{"price":"0.7","size":"3"}]}"#;
        read(raw, &tokens, &mut frame).unwrap();
        let event = &frame.events[0];
        assert_eq!(event.ts, Some(2));
        let List::Items(range) = &event.asks else {
            panic!("{:?}", event.asks)
        };
        let asks: Vec<f64> = frame.rows[range.clone()]
            .iter()
            .flatten()
            .map(|l| l.price)
            .collect();
        assert_eq!(asks, [0.6, 0.7]);
    }
}
