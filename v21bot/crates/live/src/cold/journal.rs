//! The journal of the bot and its restart state (spec §8): the port of `paper/journal.py` and of the
//! restart rule of `paper/engine.py` (`_reconcile`).
//!
//! - **the journal** (`journal.jsonl`): every event, a JSON object per line, appended only. Strict JSON
//!   (a NaN or an infinity cannot get into a `serde_json::Value`: it is null there), UTF-8 with Cyrillic
//!   as it is; `kind` and `ts_utc` (when it was appended) come first. A line a crash cut short is ended
//!   before the next record and skipped when read. A record is synced to the disk at once unless the
//!   last sync was less than a second ago; the cold runtime calls `sync` every second.
//! - **the state** (`state.json`): what a restart needs, replaced whole and atomically. The journal is
//!   written before it, so on load the trades and settlements it missed are taken from the journal.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use v21_core::money::{DemoAccount, DemoPosition};
use v21_core::window::{Coin, Side};

use crate::shared::utc::now_iso;

/// A record waits this long at most for its sync.
const SYNC_EVERY: Duration = Duration::from_secs(1);
/// Attempts to replace the state file (a virus scanner can hold it for a moment).
const STATE_ATTEMPTS: u32 = 3;
/// How a line of a `bbo` record begins.
const BBO_LINE: &[u8] = br#"{"kind":"bbo""#;

pub struct Journal {
    path: PathBuf,
    file: File,
    /// The file does not end with a line break: a crash cut its last line.
    torn: bool,
    dirty: bool,
    synced_at: Instant,
}

impl Journal {
    /// The journal at `path`, made (with its folder) if it is not there.
    pub fn open(path: &Path) -> Result<Journal> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir).with_context(|| format!("{}", dir.display()))?;
        }
        let mut file = OpenOptions::new().create(true).read(true).append(true).open(path).with_context(|| format!("{}", path.display()))?;
        let mut torn = false;
        if file.metadata()?.len() > 0 {
            let mut last = [0u8];
            file.seek(SeekFrom::End(-1))?;
            file.read_exact(&mut last)?;
            torn = last[0] != b'\n';
        }
        Ok(Journal { path: path.to_path_buf(), file, torn, dirty: false, synced_at: Instant::now() })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Adds a record: `kind`, `ts_utc` (now), then the fields of `rec` (an object), whose own `kind`
    /// and `ts_utc` give way.
    pub fn append(&mut self, kind: &str, rec: Value) -> Result<()> {
        let Value::Object(fields) = rec else { bail!("a journal record is a JSON object, not {rec}") };
        let mut line = format!("{{\"kind\":{},\"ts_utc\":{}", serde_json::to_string(kind)?, serde_json::to_string(&now_iso())?);
        for (k, v) in fields.iter().filter(|(k, _)| *k != "kind" && *k != "ts_utc") {
            line.push(',');
            line.push_str(&serde_json::to_string(k)?);
            line.push(':');
            line.push_str(&serde_json::to_string(v)?);
        }
        line.push_str("}\n");
        if self.torn {
            self.file.write_all(b"\n")?;
            self.torn = false;
        }
        self.file.write_all(line.as_bytes()).with_context(|| format!("{}", self.path.display()))?;
        self.dirty = true;
        if self.synced_at.elapsed() >= SYNC_EVERY {
            self.sync()?;
        }
        Ok(())
    }

    /// Writes what was appended through to the disk.
    pub fn sync(&mut self) -> Result<()> {
        if self.dirty {
            self.file.sync_data().with_context(|| format!("{}", self.path.display()))?;
            self.dirty = false;
        }
        self.synced_at = Instant::now();
        Ok(())
    }

    /// Every record, in the order appended; a line that is not a JSON object (a torn last line) is
    /// skipped.
    pub fn all(&self) -> Result<Vec<Value>> {
        let raw = fs::read(&self.path).with_context(|| format!("{}", self.path.display()))?;
        Ok(raw
            .split(|b| *b == b'\n')
            .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
            .filter(Value::is_object)
            .collect())
    }

    /// Every record but the `bbo` rows, in the order appended. The rows are most of the journal and
    /// only the reconciliation reads them: their lines are skipped unread (`kind` comes first in every
    /// line), and the file is read a line at a time. A torn line is skipped.
    pub fn all_but_bbo(&self) -> Result<Vec<Value>> {
        let file = File::open(&self.path).with_context(|| format!("{}", self.path.display()))?;
        let mut out = Vec::new();
        for line in BufReader::new(file).split(b'\n') {
            let line = line.with_context(|| format!("{}", self.path.display()))?;
            if line.starts_with(BBO_LINE) {
                continue;
            }
            if let Ok(rec) = serde_json::from_slice::<Value>(&line)
                && rec.is_object()
            {
                out.push(rec);
            }
        }
        Ok(out)
    }

    /// The records of `kind`, in the order appended.
    pub fn records(&self, kind: &str) -> Result<Vec<Value>> {
        let recs = if kind == "bbo" { self.all()? } else { self.all_but_bbo()? };
        Ok(recs.into_iter().filter(|r| r["kind"] == kind).collect())
    }
}

impl Drop for Journal {
    fn drop(&mut self) {
        let _ = self.sync();
    }
}

/// A window whose outcomes are still awaited: its coins, and those settled.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowWait {
    pub coins: Vec<Coin>,
    pub done: Vec<Coin>,
}

/// What a restart needs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub account: DemoAccount,
    /// (window start, coin) of every trade: one per coin and window.
    pub traded: Vec<(i64, Coin)>,
    pub unsettled: BTreeMap<i64, WindowWait>,
    /// The UTC date of the last daily summary.
    pub last_summary_date: Option<String>,
    pub started_utc: String,
    pub clock_offset_ms: Option<i64>,
}

impl State {
    pub fn new(bank_usd: f64) -> State {
        State {
            account: DemoAccount::new(bank_usd),
            traded: vec![],
            unsettled: BTreeMap::new(),
            last_summary_date: None,
            started_utc: now_iso(),
            clock_offset_ms: None,
        }
    }
}

/// `path` with `.tmp` after its name.
fn tmp_of(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

/// Makes `path` hold `bytes` through a file next to it and a rename, so that it is the old file or the
/// new one, never half of one; `durable` syncs the new one first. The file next to it does not outlive
/// an error.
pub(crate) fn write_atomically(path: &Path, bytes: &[u8], durable: bool) -> io::Result<()> {
    let tmp = tmp_of(path);
    let result = (|| {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        if durable {
            f.sync_all()?;
        }
        drop(f);
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Replaces the state file, atomically.
pub fn save_state(path: &Path, st: &State) -> Result<()> {
    let text = serde_json::to_string_pretty(st)?;
    let mut attempt = 1;
    loop {
        match write_atomically(path, text.as_bytes(), true) {
            Ok(()) => return Ok(()),
            Err(e) if attempt >= STATE_ATTEMPTS => return Err(e).with_context(|| format!("{}: {STATE_ATTEMPTS} attempts", path.display())),
            Err(_) => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// The state saved last (a new account of `bank_usd` if there is none), with the trades and the
/// settlements of the journal it missed. A damaged file is an error: starting afresh would forget the
/// money and the positions.
pub fn load_state(path: &Path, journal: &Journal, bank_usd: f64) -> Result<State> {
    let mut st = match fs::read(path) {
        Ok(raw) => serde_json::from_slice::<State>(&raw).with_context(|| format!("{}: damaged", path.display()))?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => State::new(bank_usd),
        Err(e) => return Err(e).with_context(|| format!("{}", path.display())),
    };
    reconcile(&mut st, journal)?;
    Ok(st)
}

/// The position an `entry` record opened.
fn position_of(e: &Value) -> Option<DemoPosition> {
    Some(DemoPosition {
        id: e["trade_id"].as_str()?.to_string(),
        s: e["s"].as_i64()?,
        coin: Coin::parse(e["coin"].as_str()?)?,
        side: serde_json::from_value::<Side>(e["side"].clone()).ok()?,
        shares: e["shares"].as_f64()?,
        cost_usd: e["cost_usd"].as_f64()?,
        fee_usd: e["fee_usd"].as_f64()?,
        avg_price: e["avg_price"].as_f64()?,
    })
}

/// The trades of the journal the state does not know are opened, and the settlements it missed applied.
fn reconcile(st: &mut State, journal: &Journal) -> Result<()> {
    let known = st.traded.clone();
    let recs = journal.all_but_bbo()?;
    for e in recs.iter().filter(|r| r["kind"] == "entry") {
        let Some(pos) = position_of(e) else { bail!("an entry of the journal cannot be read: {e}") };
        let key = (pos.s, pos.coin);
        if !st.traded.contains(&key) {
            st.traded.push(key);
        }
        if !known.contains(&key) && !st.account.positions.iter().any(|p| p.id == pos.id) {
            st.account.open(pos);
        }
    }
    let mut paid: HashMap<&str, f64> = HashMap::new();
    let mut order = vec![];
    for r in recs.iter().filter(|r| r["kind"] == "settle") {
        let (Some(id), Some(payout)) = (r["trade_id"].as_str(), r["payout_usd"].as_f64()) else { bail!("a settlement of the journal cannot be read: {r}") };
        if paid.insert(id, payout).is_none() {
            order.push(id);
        }
    }
    for id in order {
        if let Some(shares) = st.account.positions.iter().find(|p| p.id == id).map(|p| p.shares) {
            st.account.settle(id, paid[id] / shares);
        }
    }
    Ok(())
}
