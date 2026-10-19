# v21bot — веха 1: ядро стратегии и прогон по архиву. План реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Цель:** ядро v2.1 на Rust и прогон по архиву, который повторяет Python-бэктест (`ruslan_v2.py` + `ruslan_v2_1.py`)
сигнал в сигнал и сделка в сделку.

**Архитектура.** Воркспейс `v21bot/` из трёх крейтов:
- `v21-core` — вся логика без ввода-вывода: параметры, детектор, z, решение, доходность и статистика;
- `v21-replay` — читает parquet-архив, прогоняет его через ядро и сравнивает с выходом Python;
- `v21-cli` — исполняемый файл `v21` с командой `replay`.

Логика ядра — перенос `paper/strategy.py`, который уже совпал с бэктестом. Тесты ядра — перенос тестов `paper/tests/`.

**Стек:** Rust stable не ниже 1.88 (edition 2024); `serde_json`, `sha2`, `thiserror` — в ядре; `parquet` и `arrow`
(arrow-rs), `rayon`, `csv`, `anyhow` — в прогоне; `clap` — в CLI; `cargo-llvm-cov` — покрытие.

**Спецификация:** `docs/superpowers/specs/2026-10-18-v21bot-design.md` — §2, §4.1, §5.1, §5.4, §10, §14 (веха 1).

**Следующие планы.** Вехи 2–4 — отдельные планы. Каждый пишется после приёмки предыдущей вехи: им нужны интерфейсы
этой вехи и проверки из §15 спецификации. Список — в конце документа.

## Global Constraints

- **Rust** — последняя стабильная версия через rustup, не ниже 1.88, edition 2024.
- **Параметры стратегии** — только из `strategy_v2.json` и `strategy_v2_1.json`. Хеш sha256 канонического JSON файла
  `strategy_v2.json` (как `json.dumps(..., sort_keys=True)` в Python) должен совпасть с `base.sha256_canonical` из
  `strategy_v2_1.json`. Иначе — ошибка, бот не работает.
- **`v21-core` без ввода-вывода:** ни сети, ни часов, ни файлов. JSON приходит строкой, данные — аргументами.
- **Точность:**
  - `t_trig`, сторона и `px` — точное равенство;
  - `z`, `cushion_bp`, `sigma_bp` и `h` — до 1e-9;
  - номер в окне — точно;
  - `ret_top` и `ret` — до 1e-12;
  - наборы сделок v2 и v2.1 совпадают.
- **Причины решений** — ровно эти строки: «вход», «пропуск», «ask > 0.88 через 0.3 с», «третья монета», «вне часов»,
  «монета не торгуется», «нет данных Binance», «z < 1.25».
- **Корень данных** — `--root DIR` или переменная `V21_ROOT`: папка `15mbacktest` с обоими JSON, `data/` и
  `data_pre/`. Тесты, которым нужен архив, без `V21_ROOT` печатают `skipped: V21_ROOT is not set` и проходят.
- **Скорость:** полный прогон `--period main` в сборке release — не дольше 120 с на ПК пользователя.
- **Покрытие** (`cargo llvm-cov`): `v21-core` — не меньше 95% строк, весь код вехи — не меньше 85%.
- **Коммиты — только по просьбе пользователя** (спецификация §1, решение 12), поэтому шагов «коммит» в задачах нет.
  Если пользователь разрешит коммиты в этой ветке — коммит в конце каждой задачи с её заголовком как сообщением.
- **Язык:** идентификаторы и комментарии в коде — английские, как в `pmbot`; README — по-русски.
- **Где работать:** все команды `cargo` выполняются из `v21bot/` в корне рабочей копии.

## Review Focus

1. **Эталон старше архива.** Пользователь докачал новые часы, а `ruslan_v2.py` не перезапускал. Сигналы в окнах после
   последнего окна эталона должны попадать в отдельный список «новее эталона — перезапусти `ruslan_v2.py`», а не в
   расхождения. Тест — в задаче 9.
2. **Две схемы архива.**
   - основной `bbo`: `asset_id` — binary, цены — Decimal(9,4);
   - `bbo` периода 07–18.08 (PRE): `asset_id` — fixed_size_binary[32], цены — double;
   - `start` в `meta_15m` — в мкс, в `meta_15m_pre` — в мс.

   Обе схемы читаются одинаково. Тест — в задаче 7.
3. **Одновременные сигналы на 840.000 с** (в архиве 156 окон). Равенство времени решает порядок, в котором бэктест
   впервые встретил окно: порядок файлов, внутри файла — по возрастанию id токена Up. Тест — в задаче 8.
4. **Сигнал окна из более раннего часового файла,** заменённый сигналом из более позднего. Окно сохраняет первую
   позицию, как ключ словаря Python. Тест — в задаче 8.
5. **Неверный `--root` или нет файла.** Ошибка называет недостающий путь, код выхода 2, без паники. Тест — в задаче 10.

## Карта файлов

| Файл | Что в нём |
|---|---|
| `v21bot/Cargo.toml` | воркспейс, профиль release |
| `v21bot/README.md` | как запускать тесты и прогон, итоги приёмки и покрытия |
| `crates/core/src/window.rs` | константы окна, `Coin`, `Side`, час UTC, момент входа, разбор времени ISO |
| `crates/core/src/numpy.rs` | `round3` — как `np.round(x, 3)` |
| `crates/core/src/pyjson.rs` | `repr(float)` и канонический `json.dumps(sort_keys=True)` как в Python, sha256 |
| `crates/core/src/params.rs` | `Params`, `Strategy`, `load_strategy` |
| `crates/core/src/detector.rs` | `Check`, `CoinDetector` |
| `crates/core/src/closes.rs` | `Closes`, `ArrayCloses` |
| `crates/core/src/zscore.rs` | `ZInfo`, `p0_of`, `z_score` |
| `crates/core/src/decision.rs` | `Reason`, `Decision`, `WindowEngine` |
| `crates/core/src/returns.rs` | `win_of`, `trade_return` |
| `crates/core/src/stats.rs` | `cl_t`, `Stats`, `stats`, периоды |
| `crates/core/tests/data/strategy_v2*.json` | побайтные копии замороженных файлов |
| `crates/replay/src/archive.rs` | окна, сбои, файлы `bbo`, секунды Binance |
| `crates/replay/src/run.rs` | прогон: сигналы, z, номера, решения |
| `crates/replay/src/compare.rs` | эталон Python и сравнение |
| `crates/replay/src/report.rs` | таблица по периодам, CSV |
| `crates/cli/src/main.rs` | `v21 replay` |

---

### Task 1: Тулчейн, воркспейс и помощники ядра

**Files:**
- Create: `v21bot/Cargo.toml`, `v21bot/.gitignore`, `v21bot/crates/core/Cargo.toml`, `v21bot/crates/core/src/lib.rs`
- Create: `v21bot/crates/core/src/window.rs`, `v21bot/crates/core/src/numpy.rs` (тесты — в модулях)

**Interfaces:**
- Produces:
```rust
// window.rs
pub const WINDOW_S: i64 = 900;
pub const TWAP_S: i64 = 60;
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)] pub enum Side { Up, Down }
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)] pub enum Coin { Btc, Eth, Sol }
impl Coin {
    pub fn parse(s: &str) -> Option<Coin>;          // "btc" | "eth" | "sol"
    pub fn as_str(self) -> &'static str;             // "btc" ...
    pub fn binance_symbol(self) -> &'static str;     // "BTCUSDT" ...
}
pub fn utc_hour(s: i64, t_trig: f64) -> u32;         // как int((s + t_trig) // 3600 % 24)
pub fn entry_ms(s0_ms: i64, t_trig: f64) -> i64;     // s0_ms + (t_trig * 1000.0) as i64, как astype('int64')
pub fn parse_utc_ms(s: &str) -> Option<i64>;         // "YYYY-MM-DDTHH:MM:SSZ" и "YYYY-MM-DDTHH:MMZ"
// numpy.rs
pub fn round3(x: f64) -> f64;                        // (x * 1000.0).round_ties_even() / 1000.0
```

- [ ] **Step 1: Обновить тулчейн**

Run: `rustup update stable; rustup component add llvm-tools-preview; cargo install cargo-llvm-cov --locked; rustc --version`
Expected: `rustc 1.88.0` или новее.

- [ ] **Step 2: Создать воркспейс.** В `v21bot/Cargo.toml`:

```toml
[workspace]
members = ["crates/core"]
resolver = "3"

[workspace.package]
edition = "2024"
rust-version = "1.88"

[profile.release]
opt-level = 3
lto = "fat"
codegen-units = 1
panic = "abort"
```

В `.gitignore` — `/target`. Крейт `v21-core` наследует `edition` и `rust-version` из воркспейса.

- [ ] **Step 3: Написать падающие тесты**

```rust
// numpy.rs — значения проверены на numpy 18.10.2026
#[test]
fn round3_matches_numpy() {
    for (x, want) in [(0.0005, 0.0), (0.0015, 0.002), (0.0025, 0.002), (1.0 - 0.81, 0.19), (0.6996, 0.7),
                      (0.8806, 0.881), (0.8804, 0.88), (0.6994, 0.699), (0.12345, 0.123), (0.8885, 0.888)] {
        assert_eq!(round3(x), want, "{x}");
    }
    for k in 0..=1000 {
        assert_eq!(round3(k as f64 / 1000.0), k as f64 / 1000.0);
        assert_eq!(round3(1.0 - k as f64 / 1000.0), (1000 - k) as f64 / 1000.0);
    }
}

// window.rs
#[test]
fn utc_hour_is_the_hour_of_the_entry_moment() {
    assert_eq!(utc_hour(1_790_910_900, 845.0), 3);   // окно 2026-10-02 03:15
    assert_eq!(utc_hour(1_790_923_500, 899.0), 6);   // окно 06:45
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
```

- [ ] **Step 4: Убедиться, что тесты падают**

Run: `cargo test -p v21-core`
Expected: FAIL — функции не определены.

- [ ] **Step 5: Реализовать** `window.rs` и `numpy.rs` по сигнатурам выше. `parse_utc_ms` — свой разбор фиксированного
формата и перевод даты в дни по алгоритму days-from-civil, без внешних крейтов.

- [ ] **Step 6: Убедиться, что тесты проходят**

Run: `cargo test -p v21-core`
Expected: PASS.

---

### Task 2: Параметры стратегии и проверка замороженных файлов

**Files:**
- Create: `crates/core/src/pyjson.rs`, `crates/core/src/params.rs`, `crates/core/tests/params.rs`
- Create: `crates/core/tests/data/strategy_v2.json`, `crates/core/tests/data/strategy_v2_1.json` — побайтные копии из
  корня `15mbacktest`
- Modify: `crates/core/Cargo.toml` (зависимости `serde_json`, `sha2`, `thiserror`), `crates/core/src/lib.rs`

**Interfaces:**
- Consumes: `Coin`, `parse_utc_ms` (задача 1).
- Produces:
```rust
// pyjson.rs
pub fn py_float_repr(x: f64) -> String;                  // как repr(float) в Python 3
pub fn canonical_json(v: &serde_json::Value) -> String;   // как json.dumps(v, sort_keys=True): ", " и ": ", ensure_ascii
pub fn canonical_sha256(json_text: &str) -> Result<String, serde_json::Error>;   // hex в нижнем регистре
// params.rs
#[derive(Clone, Debug, PartialEq)]
pub struct Params {
    pub watch_coins: Vec<Coin>, pub trade_coins: Vec<Coin>,
    pub lo: f64, pub hi: f64, pub t0: f64, pub t1: f64, pub lat: f64, pub max_fill: f64,
    pub z_min: f64, pub max_coins: u32, pub hours: Vec<(u32, u32)>, pub sig_lb: i64,
    pub fee: f64, pub slip: f64, pub incident_buffer_min: i64,
}
#[derive(Clone, Debug, PartialEq)] pub struct ForwardRules { pub stop_after_trades: u32, pub stop_if_win_below: f64 }
#[derive(Clone, Debug, PartialEq)] pub struct Dates { pub research_split_ms: i64, pub rule_selected_ms: i64, pub v2_forward_ms: i64, pub v21_forward_ms: i64 }
#[derive(Clone, Debug, PartialEq)] pub struct Strategy { pub v2: Params, pub v21: Params, pub forward: ForwardRules, pub dates: Dates }
#[derive(Debug, thiserror::Error)]
pub enum ParamsError {
    #[error("bad json: {0}")] Json(String),
    #[error("strategy_v2.json differs from the one v2.1 was frozen with: expected {expected}, got {got}")] BaseHash { expected: String, got: String },
    #[error("missing or bad field {0}")] Field(&'static str),
}
pub fn load_strategy(v2_json: &str, v21_json: &str) -> Result<Strategy, ParamsError>;
```

**Откуда берётся каждое поле.** В `strategy_v2.json`:
- `markets.coins` → `watch_coins` и `v2.trade_coins`;
- `entry.price_band` → `lo` и `hi`;
- `entry.monitor_from_s` → `t0`, `entry.monitor_to_s` → `t1`, `entry.fill_latency_s` → `lat`;
- `entry.max_fill_price` → `max_fill`;
- `filters.distance_min_sigma` → `z_min`, `filters.max_coins_per_window` → `max_coins`;
- `filters.utc_hours` → `v2.hours`, `filters.sigma_lookback_s` → `sig_lb`;
- `filters.skip_incident_windows_buffer_min` → `incident_buffer_min`;
- `costs.taker_fee_rate` → `fee`, `costs.slippage_per_share` → `slip`;
- `research_split_utc`, `rule_selected_utc`, `forward_start_utc` → `Dates`.

В `strategy_v2_1.json`:
- `changes.trade_coins` → `v21.trade_coins`, `changes.utc_hours` → `v21.hours`;
- `forward_start_utc` → `v21_forward_ms`;
- `forward_rules.*` → `ForwardRules`;
- `base.sha256_canonical` сравнивается с `canonical_sha256(v2_json)`.

Остальные поля `v21` равны полям `v2`. Целое число в JSON (`840`) читается как `f64`, где поле дробное.

- [ ] **Step 1: Написать падающие тесты** (`crates/core/tests/params.rs`)

```rust
const V2: &str = include_str!("data/strategy_v2.json");
const V21: &str = include_str!("data/strategy_v2_1.json");

#[test]
fn py_float_repr_matches_python() {
    for (x, want) in [(0.7, "0.7"), (0.88, "0.88"), (1.25, "1.25"), (0.014, "0.014"), (100.0, "100.0"),
                      (1e-05, "1e-05"), (1e16, "1e+16"), (0.0001, "0.0001"), (0.06, "0.06"), (1207.0, "1207.0"),
                      (1e15, "1000000000000000.0"), (123456789012345.0, "123456789012345.0"), (1.5e-07, "1.5e-07"),
                      (-0.5, "-0.5"), (0.0, "0.0")] {
        assert_eq!(py_float_repr(x), want);
    }
}
#[test]
fn canonical_json_matches_json_dumps_sort_keys() {
    let v: serde_json::Value = serde_json::from_str(r#"{"b": 1, "a": [0.70, "x\u00e9"], "c": {"z": true, "y": null}}"#).unwrap();
    assert_eq!(canonical_json(&v), r#"{"a": [0.7, "x\u00e9"], "b": 1, "c": {"y": null, "z": true}}"#);
}
#[test]
fn frozen_v2_hash_is_the_registered_one() {
    assert_eq!(canonical_sha256(V2).unwrap(), "0e4ee96e6e2e4e6ec678e587a5fe57a78ff1393abd679f8dfd7972ead843e135");
}
#[test]
fn strategy_values() {
    let s = load_strategy(V2, V21).unwrap();
    use Coin::*;
    assert_eq!(s.v21.watch_coins, vec![Btc, Eth, Sol]);
    assert_eq!(s.v21.trade_coins, vec![Btc, Eth]);
    assert_eq!(s.v21.hours, vec![(0, 7)]);
    assert_eq!(s.v2.trade_coins, vec![Btc, Eth, Sol]);
    assert_eq!(s.v2.hours, vec![(0, 7), (13, 16)]);
    let p = &s.v21;
    assert_eq!((p.lo, p.hi, p.t0, p.t1, p.lat, p.max_fill, p.z_min, p.max_coins, p.sig_lb),
               (0.70, 0.88, 840.0, 870.0, 0.3, 0.88, 1.25, 2, 300));
    assert_eq!((p.fee, p.slip, p.incident_buffer_min), (0.07, 0.014, 30));
    assert_eq!(s.forward, ForwardRules { stop_after_trades: 100, stop_if_win_below: 0.80 });
    assert_eq!(s.dates, Dates { research_split_ms: 1_788_739_200_000, rule_selected_ms: 1_790_848_800_000,
                                v2_forward_ms: 1_790_881_200_000, v21_forward_ms: 1_792_022_400_000 });
}
#[test]
fn a_changed_v2_is_refused() {
    let tampered = V2.replace("\"distance_min_sigma\": 1.25", "\"distance_min_sigma\": 1.3");
    assert!(matches!(load_strategy(&tampered, V21), Err(ParamsError::BaseHash { .. })));
}
#[test]
fn every_field_is_read_from_its_own_key() { /* см. шаг ниже */ }
#[test]
fn root_files_equal_the_vendored_ones() { /* V21_ROOT: оба файла из корня дают тот же Strategy */ }
```

Тест `every_field_is_read_from_its_own_key` строит синтетический `v2` с попарно разными значениями:
- `price_band` [0.6, 0.9], `monitor_from_s` 100, `monitor_to_s` 200, `fill_latency_s` 0.5, `max_fill_price` 0.95;
- `distance_min_sigma` 2.5, `max_coins_per_window` 1, `utc_hours` [[1,2],[5,6]], `sigma_lookback_s` 120;
- `taker_fee_rate` 0.05, `slippage_per_share` 0.02, `skip_incident_windows_buffer_min` 15;
- даты и монеты — любые.

К нему — `v21` с `base.sha256_canonical = canonical_sha256(v2)`. Тест проверяет каждое поле, как
`test_load_params_reads_every_field_from_its_own_key` в `paper/tests/test_closes_z.py`.

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p v21-core --test params`
Expected: FAIL — модули не определены.

- [ ] **Step 3: Реализовать `pyjson.rs`.**
- **`py_float_repr`:** кратчайшие цифры, которые читаются обратно в то же число, — из `format!("{:e}", x)`. Затем
  правило Python: десятичный порядок `e` в диапазоне −4 ≤ e < 16 — обычная запись, при целом значении с `.0`;
  иначе — `d[.ddd]e±XX`, порядок не меньше двух цифр.
- **`canonical_json`:** ключи по возрастанию (`serde_json::Map` без `preserve_order` уже сортирует), разделители `", "`
  и `": "`. Не-ASCII → `\uXXXX` строчными; вне BMP — суррогатная пара. Кавычка, `\` и управляющие символы — как в
  `json.dumps`. Целые печатаются как целые, дробные — через `py_float_repr`.

- [ ] **Step 4: Реализовать `params.rs`** по таблице полей выше. Сначала проверка хеша, потом поля.

- [ ] **Step 5: Убедиться, что тесты проходят**

Run: `cargo test -p v21-core`
Expected: PASS.

---

### Task 3: Детектор монеты (`CoinDetector`)

**Files:**
- Create: `crates/core/src/detector.rs`, `crates/core/tests/detector.rs`

**Interfaces:**
- Consumes: `Params`, `Coin`, `Side`, `round3`.
- Produces:
```rust
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Check { pub coin: Coin, pub t_trig: f64, pub side: Side, pub ask_at_trig: f64, pub px: f64, pub ok: bool }
pub struct CoinDetector { /* lo, hi, t0, t1, lat, max_fill копируются из Params */ }
impl CoinDetector {
    pub fn new(p: &Params, coin: Coin, s0_ms: i64) -> CoinDetector;
    pub fn update(&mut self, ts_ms: i64, bid: f64, ask: f64) -> Option<Check>;   // обновления — по времени
    pub fn flush(&mut self, now_ms: i64) -> Option<Check>;
    pub fn reset(&mut self);
    pub fn pending(&self) -> Option<(f64, Side, f64)>;    // (t_trig, сторона, ask в момент сигнала)
    pub fn is_done(&self) -> bool;
    pub fn ua(&self) -> Option<f64>;                       // ask Up, округлённый
    pub fn da(&self) -> Option<f64>;                       // 1 − bid Up, округлённый
}
```

Поведение — ровно `paper/strategy.py::CoinDetector`, включая описание в его docstring. Время — `(ts_ms − s0_ms) as f64
/ 1000.0`. Цены — `round3(ask)` и `round3(1.0 - bid)`. Порядок внутри `update` — как там: `_start_840` → `_resolve`
→ новые цены → проверка коридора.

- [ ] **Step 1: Перенести тесты.** Каждый тест классов `DetectorTests` и `DetectorResetTests` из
`paper/tests/test_strategy.py` переносится один к одному: имя в snake_case, те же входы и ожидаемые значения.
Константы: `P` — `load_strategy(V2, V21).v2`, `QUIET = (830_000, 0.45, 0.50)`, `S = 1_790_910_900`. Два примера формы:

```rust
#[test]
fn signal_from_pre840_state() {
    let mut det = CoinDetector::new(&p(), Coin::Btc, 0);
    assert_eq!(det.update(839_500, 0.10, 0.75), None);
    let got = det.update(840_500, 0.10, 0.70);
    assert_eq!(got, Some(Check { coin: Coin::Btc, t_trig: 840.0, side: Side::Up, ask_at_trig: 0.75, px: 0.75, ok: true }));
    assert!(det.is_done());
}
#[test]
fn failed_check_then_resume() {
    let mut det = CoinDetector::new(&p(), Coin::Btc, 0);
    det.update(830_000, 0.45, 0.50);
    assert_eq!(det.update(841_000, 0.10, 0.85), None);
    assert_eq!(det.pending(), Some((841.0, Side::Up, 0.85)));
    assert_eq!(det.update(841_200, 0.10, 0.90), None);
    assert_eq!(det.update(841_400, 0.10, 0.86),
               Some(Check { coin: Coin::Btc, t_trig: 841.0, side: Side::Up, ask_at_trig: 0.85, px: 0.90, ok: false }));
    assert_eq!(det.pending(), Some((841.4, Side::Up, 0.86)));
    assert!(!det.is_done());
}
```

- [ ] **Step 2: Перенести тест на случайных лентах** `detector_matches_book_triggers_on_random_streams`:
- `backtest_checks` и `random_stream` из `paper/tests/test_strategy.py` (строки 328–392) переписываются на Rust;
- генератор — xorshift64* с seed 20261002 (как `pmbot/src/hot/mod.rs::testing::Rng`);
- 6000 лент, окна `S` и `S + 900`;
- пороги видов лент те же: ok ≥ 1000, ended failed ≥ 100, no check ≥ 300, no book at t0 ≥ 300, more than one
  check ≥ 300.

- [ ] **Step 3: Убедиться, что тесты падают**

Run: `cargo test -p v21-core --test detector`
Expected: FAIL — `CoinDetector` не определён.

- [ ] **Step 4: Реализовать `detector.rs`.**

- [ ] **Step 5: Убедиться, что тесты проходят**

Run: `cargo test -p v21-core --test detector`
Expected: PASS, включая тест на 6000 лентах.

---

### Task 4: Цены Binance и z (`Closes`, `ArrayCloses`, `z_score`)

**Files:**
- Create: `crates/core/src/closes.rs`, `crates/core/src/zscore.rs`, `crates/core/tests/zscore.rs`

**Interfaces:**
- Consumes: `Params`, `Side`, `WINDOW_S`, `TWAP_S`.
- Produces:
```rust
pub trait Closes {
    fn close(&self, q: i64) -> Option<f64>;
    fn sum_closes(&self, q0: i64, q1: i64) -> Option<f64>;        // q0 > q1 — panic (ошибка вызова)
    fn sum_sq_returns(&self, q0: i64, q1: i64) -> Option<f64>;    // нужна и секунда q0 − 1
}
pub struct ArrayCloses { /* t0, c, cs, c1 */ }
impl ArrayCloses {
    pub fn new(t0: i64, closes: Vec<f64>) -> ArrayCloses;           // close[i] — секунда t0 + i
    pub fn covers(&self, q0: i64, q1: i64) -> bool;
    pub fn t0(&self) -> i64;
    pub fn len(&self) -> usize;
}
impl Closes for ArrayCloses { .. }
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZInfo { pub z: f64, pub p0: f64, pub ef: f64, pub sigma_bp: f64, pub h: f64, pub cushion_bp: f64 }
pub fn p0_of(c: &impl Closes, s: i64) -> Option<f64>;
pub fn z_score(c: &impl Closes, s: i64, t_trig: f64, side: Side, p: &Params) -> Option<ZInfo>;
```

**Накопленные суммы — как в бэктесте, последовательным сложением:**
- `cs[0] = 0`, `cs[i+1] = cs[i] + c[i]`;
- `r[0] = 0`, `r[i] = ln(c[i]) − ln(c[i−1])`;
- `c1[0] = 0`, `c1[i+1] = c1[i] + r[i]·r[i]`;
- `sum_closes(q0, q1) = cs[q1−t0+1] − cs[q0−t0]`;
- `sum_sq_returns(q0, q1) = c1[q1−t0+1] − c1[q0−t0]` — только если `covers(q0 − 1, q1)`.

**`z_score` — порядок операций как в `paper/strategy.py::z_score`.** Он фиксирует биты результата:

```text
end = s + 900; t = s + floor(t_trig); first = end - 61; last = min(t - 1, end - 2)
known = max(last - first + 1, 0); nf = 60 - known
p0 = sum_closes(s - 61, s - 2) / 60
seen = if known > 0 { sum_closes(first, last) } else { 0.0 }
price = close(t - 1); squares = sum_sq_returns(t - sig_lb, t - 1)       // любое None → None
ef = (seen + nf as f64 * price) / 60.0
a = max(first - (t - 1), 1)
h = ((nf * nf * a) as f64 + ((nf - 1) * nf * (2 * nf - 1)) as f64 / 6.0) / 3600.0
sigma = sqrt(squares / sig_lb as f64); cushion = sign * ln(ef / p0)     // sign: Up = 1.0, Down = -1.0
z = if sigma > 0.0 && h > 0.0 { cushion / (sigma * sqrt(h)) } else { NaN }
ZInfo { z, p0, ef, sigma_bp: sigma * 1e4, h, cushion_bp: cushion * 1e4 }
```

- [ ] **Step 1: Написать падающие тесты.** Перенести из `paper/tests/test_closes_z.py`:
- `ArrayClosesTests` — без чтения parquet: `get`/`covers` → `close`/`covers`/суммы;
- `ZScoreTests` — варианты с источником `ArrayCloses`.

Для них пишутся тестовые помощники:
- `random_closes(first, last, seed)` — случайное блуждание цены около 100, 20% секунд без сделок (цена повторяется);
- `naive_z` — формулы `STRATEGY_V2.md` §3 простыми суммами.

Обязательные проверки:

```rust
#[test]
fn h_at_the_golden_moments() {
    // h из ruslan_rule_all_triggers.parquet: t_trig 840.0 и 841.716
    assert_eq!(z_on_random(840.0).h, 19.502777777777776);
    assert_eq!(z_on_random(841.716).h, 18.535833333333333);
}
#[test] fn z_matches_the_spec_formulas()         { /* t_trig 840.0, 840.4, 843.7, 855.5, 869.9, 870.0 × Up/Down: |Δ| ≤ 1e-9 по 6 полям */ }
#[test] fn z_before_the_final_minute()           { /* t_trig 500.0, 700.5, 838.9, 839.0, 839.99, 840.0: множитель a */ }
#[test] fn z_down_mirrors_up()                    { /* z и cushion_bp меняют знак, остальное равно */ }
#[test] fn z_uses_whole_seconds_of_t_trig()       { /* 843.0 == 843.999, 843.999 != 844.0 */ }
#[test] fn z_is_none_without_the_prices()         { /* ArrayCloses начинается в S − 50 или кончается на S + 299 */ }
#[test] fn z_is_nan_when_sigma_is_zero()          { /* 1301 цена 100.0: sigma_bp 0, cushion_bp 0, p0 100, ef 100, h > 0 */ }
#[test] fn z_is_nan_not_inf_when_sigma_is_zero_but_the_price_moved() { /* 100.0 до S + 500, дальше 101.0 */ }
#[test] fn z_after_the_final_minute_is_over()     { /* t_trig 905.0: h == 0, z NaN, ef = среднее секунд S+839..S+898 */ }
#[test] fn running_sums_equal_the_prefix_arithmetic() { /* sum_closes == cs[q1−t0+1] − cs[q0−t0] побитно */ }
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p v21-core --test zscore`
Expected: FAIL.

- [ ] **Step 3: Реализовать `closes.rs` и `zscore.rs`.**

- [ ] **Step 4: Убедиться, что тесты проходят**

Run: `cargo test -p v21-core --test zscore`
Expected: PASS.

---

### Task 5: Решение окна (`WindowEngine`)

**Files:**
- Create: `crates/core/src/decision.rs`, `crates/core/tests/decision.rs`

**Interfaces:**
- Consumes: `Check` (задача 3), `ZInfo` (задача 4), `Params`, `utc_hour`, `py_float_repr`.
- Produces:
```rust
pub const ENTER: &str = "вход";
pub const SKIP: &str = "пропуск";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason { Enter, FailedCheck, ThirdCoin, OutOfHours, CoinNotTraded, NoBinance, ZBelow }
impl Reason { pub fn label(self, p: &Params) -> String; }
#[derive(Clone, Debug, PartialEq)]
pub struct Decision { pub check: Check, pub counted: bool, pub rank: Option<u32>, pub z: Option<ZInfo>,
                      pub hour_ok: bool, pub trade: bool, pub reason: Reason }
pub struct WindowEngine<'p> { /* p, s, counted */ }
impl<'p> WindowEngine<'p> {
    pub fn new(p: &'p Params, s: i64) -> Self;
    pub fn decide(&mut self, chk: &Check, z: Option<&ZInfo>) -> Decision;   // вызывать в порядке номеров
}
```

**Порядок правил:**
1. `!chk.ok` → `FailedCheck`: не засчитан, номер `None`.
2. Иначе счётчик +1, `rank = счётчик`.
3. `rank > max_coins` → `ThirdCoin`.
4. Час не в `hours` → `OutOfHours`.
5. Монета не в `trade_coins` → `CoinNotTraded`.
6. `z` нет или NaN → `NoBinance`.
7. `z < z_min` → `ZBelow`.
8. Иначе `Enter`.

**Подписи** (`label`): «вход», «ask > 0.88 через 0.3 с» (собирается как `ask > {max_fill} через {lat} с` через
`py_float_repr`), «третья монета», «вне часов», «монета не торгуется», «нет данных Binance», «z < 1.25» (как
`z < {z_min}`).

- [ ] **Step 1: Перенести тесты** `EngineTests` из `paper/tests/test_strategy.py` (9 тестов) с параметрами `v2`, один
к одному. Помощники — как `up_check` и `zinfo` там:
- `up_check(t, coin)` → `Check { coin, t_trig: t, side: Up, ask_at_trig: 0.80, px: 0.80, ok: true }`;
- `zi(z)` → `ZInfo { z, p0: 100.0, ef: 100.1, sigma_bp: 1.0, h: 20.0, cushion_bp: 5.0 }`.

Добавить тесты v2.1:

```rust
#[test]
fn v21_sol_is_counted_but_not_traded() {
    let s = strategy();
    let mut eng = WindowEngine::new(&s.v21, 1_790_910_900);
    let d = [eng.decide(&up_check(845.0, Coin::Sol), Some(&zi(2.0))),
             eng.decide(&up_check(846.0, Coin::Btc), Some(&zi(2.0))),
             eng.decide(&up_check(847.0, Coin::Eth), Some(&zi(2.0)))];
    assert_eq!(d.iter().map(|x| x.reason).collect::<Vec<_>>(), [Reason::CoinNotTraded, Reason::Enter, Reason::ThirdCoin]);
    assert_eq!(d.iter().map(|x| x.rank).collect::<Vec<_>>(), [Some(1), Some(2), Some(3)]);
}
#[test]
fn v21_hours_are_00_07_only() {
    let s = strategy();
    // окно 13:00 (1_790_946_000): у v2 разрешено, у v2.1 — нет
    assert_eq!(WindowEngine::new(&s.v2, 1_790_946_000).decide(&up_check(845.0, Coin::Btc), Some(&zi(2.0))).reason, Reason::Enter);
    assert_eq!(WindowEngine::new(&s.v21, 1_790_946_000).decide(&up_check(845.0, Coin::Btc), Some(&zi(2.0))).reason, Reason::OutOfHours);
}
#[test]
fn v21_reason_order_puts_the_coin_after_the_hours() {
    let s = strategy();
    // SOL вне часов → «вне часов»; SOL в часах без z → «монета не торгуется»
}
#[test]
fn labels_are_the_exact_strings() {
    let s = strategy();
    assert_eq!(Reason::FailedCheck.label(&s.v21), "ask > 0.88 через 0.3 с");
    assert_eq!(Reason::ZBelow.label(&s.v21), "z < 1.25");
    assert_eq!(Reason::CoinNotTraded.label(&s.v21), "монета не торгуется");
}
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p v21-core --test decision`
Expected: FAIL.

- [ ] **Step 3: Реализовать `decision.rs`.**

- [ ] **Step 4: Убедиться, что тесты проходят**

Run: `cargo test -p v21-core --test decision`
Expected: PASS.

---

### Task 6: Доходность и статистика

**Files:**
- Create: `crates/core/src/returns.rs`, `crates/core/src/stats.rs`, `crates/core/tests/stats.rs`

**Interfaces:**
- Consumes: `Side`, `Dates`.
- Produces:
```rust
pub fn win_of(side: Side, p_up_final: f64) -> f64;                  // Up: p, Down: 1 − p
pub fn trade_return(px: f64, win: f64, slip: f64, fee: f64) -> f64;
// p = min(px + slip, 0.99); win == 1.0 → 1/(p*(1 + fee*(1 − p))) − 1, иначе −1.0
pub fn cl_t(x: &[f64], groups: &[i64]) -> (f64, f64, usize);         // как alib.cl_t; n < 3 → (NaN, NaN, n)
#[derive(Clone, Copy, Debug)]
pub struct TradeRow { pub s0_ms: i64, pub entry_ms: i64, pub px: f64, pub win: f64, pub ret_top: f64, pub ret: f64 }
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stats { pub trades: usize, pub per_day: f64, pub win_pct: f64, pub avg_price: f64, pub breakeven_pct: f64,
                   pub roi_top_pct: f64, pub t_top: f64, pub roi_slip_pct: f64, pub t_slip: f64, pub pnl_100: f64 }
pub fn stats(rows: &[TradeRow], slip: f64, fee: f64) -> Option<Stats>;  // пустой набор → None
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Half { Pre, P1, P2, P3, V2Forward, V21Forward }   // колонка `half` бэктеста
impl Half { pub fn label(self) -> &'static str; }
pub fn half_of(entry_ms: i64, s0_ms: i64, d: &Dates, pre: bool) -> Half;
```

**`stats` — как `ruslan_v2.stats`:**
- `per_day` = n ÷ дней между первым и последним входом (0 дней → 1);
- `win_pct` — среднее `win` × 100;
- `breakeven_pct` — среднее `(px + slip)·(1 + fee·(1 − px − slip))` × 100;
- `roi_*` и `t_*` — `cl_t` по `s0` при n > 2, иначе (среднее, NaN);
- `pnl_100` = Σ ret × 100.

**Периоды (`Half`) и их подписи** (`label`):

| `Half` | Подпись | Условие |
|---|---|---|
| `Pre` | «0) 07.08-18.08 (PRE)» | данные `pre` |
| `P1` | «1) 18.08-06.09» | вход < `research_split` |
| `P2` | «2) 07.09-01.10 10:00» | вход < `rule_selected` |
| `P3` | «3) 01.10 10:00-19:00 (after selection)» | `s0` < `v2_forward` |
| `V2Forward` | «4) v2 forward 01.10 19:00-15.10» | `s0` < `v21_forward` |
| `V21Forward` | «5) FORWARD v2.1 from 15.10» | остальное |

- [ ] **Step 1: Написать падающие тесты**

```rust
#[test]
fn golden_returns() {   // строки ruslan_rule_all_triggers.parquet
    assert_eq!(trade_return(0.84, 1.0, 0.0, 0.07), 0.1772905364677515);
    assert_eq!(trade_return(0.84, 1.0, 0.014, 0.07), 0.15911404184596445);
    assert_eq!(trade_return(0.88, 1.0, 0.0, 0.07), 0.12689769571959175);
    assert_eq!(trade_return(0.88, 1.0, 0.014, 0.07), 0.1103295871257195);
    assert_eq!(trade_return(0.80, 0.0, 0.014, 0.07), -1.0);
    assert_eq!(trade_return(0.98, 1.0, 0.014, 0.07), 1.0 / (0.99 * (1.0 + 0.07 * (1.0 - 0.99))) - 1.0);
}
#[test]
fn cl_t_matches_alib() {
    let (mu, t, n) = cl_t(&[0.15, -1.0, 0.2, 0.1, -1.0, 0.3], &[1, 1, 2, 3, 3, 3]);
    assert!((mu - -0.2083333333333333).abs() < 1e-12 && (t - -2.0975431183552344).abs() < 1e-12 && n == 6);
    let (mu, t, _) = cl_t(&[0.1, 0.2], &[1, 2]);
    assert!(mu.is_nan() && t.is_nan());
}
#[test]
fn win_of_mirrors_down() { assert_eq!(win_of(Side::Up, 1.0), 1.0); assert_eq!(win_of(Side::Down, 1.0), 0.0); }
#[test]
fn periods_follow_the_frozen_dates() { /* по одной точке на каждую границу Dates, ±1 мс */ }
#[test]
fn stats_of_a_small_set() { /* 3 сделки вручную: trades, win_pct, avg_price, breakeven_pct, pnl_100 */ }
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p v21-core --test stats`
Expected: FAIL.

- [ ] **Step 3: Реализовать `returns.rs` и `stats.rs`.**

- [ ] **Step 4: Убедиться, что все тесты ядра проходят**

Run: `cargo test -p v21-core`
Expected: PASS.

---

### Task 7: Чтение архива

**Files:**
- Create: `crates/replay/Cargo.toml` (`v21-replay`; зависимости: `v21-core`, `parquet` с фичей `arrow`, `arrow`,
  `serde_json`, `anyhow`, `rayon`, `csv`; dev — `tempfile`), `crates/replay/src/lib.rs`,
  `crates/replay/src/archive.rs`, `crates/replay/tests/archive.rs`
- Modify: `v21bot/Cargo.toml` — `members` + `"crates/replay"`

**Interfaces:**
- Consumes: `Coin`, `ArrayCloses`, `parse_utc_ms`.
- Produces:
```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum Period { Main, Pre }
pub const MAIN_START_MS: i64 = 1_787_032_800_000;          // 2026-08-18T06:00Z, ruslan_v2.windows()
#[derive(Clone, Debug, PartialEq)]
pub struct WindowMeta { pub slug: String, pub coin: Coin, pub s0_ms: i64, pub up: [u8; 32], pub p_up_final: f64 }
pub struct Incidents { /* интервалы [начало − буфер, конец + буфер) в мс */ }
impl Incidents {
    pub fn load(path: &Path, buffer_min: i64) -> anyhow::Result<Incidents>;
    pub fn overlaps(&self, s0_ms: i64) -> bool;            // s0 + 900_000 > a && s0 < b хотя бы для одного
    pub fn len(&self) -> usize;
}
pub fn load_windows(root: &Path, period: Period, coins: &[Coin], incidents: Option<&Incidents>) -> anyhow::Result<Vec<WindowMeta>>;
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BboRow { pub ts_ms: i64, pub token: [u8; 32], pub bid: f64, pub ask: f64 }
pub fn bbo_files(root: &Path, period: Period) -> anyhow::Result<Vec<PathBuf>>;   // по имени
pub fn read_bbo_file(path: &Path, tokens: &HashSet<[u8; 32]>) -> anyhow::Result<Vec<BboRow>>;   // порядок файла
pub fn load_closes(root: &Path, period: Period, coin: Coin) -> anyhow::Result<ArrayCloses>;
```

**Пути:**

| Что | `Main` | `Pre` |
|---|---|---|
| Окна | `data/meta_15m.parquet` | `data/meta_15m_pre.parquet` |
| Стаканы | `data/bbo/*.parquet` | `data_pre/bbo/*.parquet` |
| Секунды Binance | `data/bn1s/{SYM}.parquet` | `data_pre/bn1s/{SYM}.parquet` |
| Сбои | `data/INCIDENTS.json` | нет |

**Схемы:**
- **`bbo`:**
  - `timestamp` — Timestamp(ms, UTC);
  - `asset_id` — Binary или FixedSizeBinary(32);
  - `best_bid` и `best_ask` — Decimal128(9, 4), переводится как `value as f64 / 10f64.powi(scale)`, или Float64;
  - null → NaN.
- **`meta`:**
  - `slug`, `coin`, `up` (64 hex-символа → 32 байта) — Utf8;
  - `start` — Timestamp(us или ms, UTC), переводится в мс с округлением вниз;
  - `p_up_final` — Float64.
- **`bn1s`:** `t` — Int64 (мс), `c` — Float64. Секунды подряд с шагом 1000, без null, файл не пустой — иначе ошибка с
  путём файла.
- **Нет колонки** — ошибка, где названы файл и колонка.

**Фильтр окон** — как `ruslan_v2.windows()` и `ruslan_v2_pre.py`:
- монета в `coins`; `p_up_final` равен 0.0 или 1.0;
- для `Main` ещё: `s0 ≥ MAIN_START_MS` и `!incidents.overlaps(s0)`.

- [ ] **Step 1: Написать падающие тесты.** Синтетические файлы пишутся в `tempfile::TempDir` через
`parquet::arrow::ArrowWriter`.

```rust
#[test] fn reads_main_schema_decimal_prices() { /* Decimal(9,4) 8100 → 0.81, 1 − bid точно как f64; null → NaN */ }
#[test] fn reads_pre_schema_double_prices()   { /* FixedSizeBinary(32) + Float64 дают те же BboRow */ }
#[test] fn keeps_file_order_and_filters_tokens() { /* 5 строк, 2 токена из 3 — порядок строк сохраняется */ }
#[test] fn empty_bbo_file_gives_no_rows() {}
#[test] fn missing_column_names_file_and_column() { /* текст ошибки содержит "best_ask" и путь */ }
#[test] fn meta_start_in_us_and_ms_give_the_same_s0() {}
#[test] fn closes_reject_hole_null_and_empty() {}
#[test]
fn incident_buffer_edges() {
    // сбой 04:00–05:30, буфер 30 мин → [03:30, 06:00)
    // окно 03:15 (кончается в 03:30) не пересекает, 03:30 пересекает, 05:45 пересекает, 06:00 не пересекает
}
#[test]
fn real_archive_facts() {      // V21_ROOT
    // Main: окон ≥ 5577 на монету и поровну по BTC/ETH/SOL; Pre: ровно 3240 окон
    // сбоев ≥ 26; файлов bbo Main ≥ 1476, первый — "2026-08-18T06.parquet"; Pre — ровно 270
    // BTCUSDT: t0 = 1786924800, первая цена 62900.0; ETHUSDT — 1876.01; SOLUSDT — 74.62; длины равны
}
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p v21-replay --test archive`
Expected: FAIL.

- [ ] **Step 3: Реализовать `archive.rs`.** Через `ParquetRecordBatchReaderBuilder` читать только нужные колонки.

- [ ] **Step 4: Убедиться, что тесты проходят** (с `V21_ROOT` и без)

Run: `$env:V21_ROOT="C:\Users\filip\Documents\15mbacktest"; cargo test -p v21-replay --test archive`
Expected: PASS. Без переменной архивный тест печатает `skipped: V21_ROOT is not set`.

---

### Task 8: Прогон по архиву (`replay::run`)

**Files:**
- Create: `crates/replay/src/run.rs`, `crates/replay/tests/run.rs`

**Interfaces:**
- Consumes: всё из задач 2–7.
- Produces:
```rust
#[derive(Clone, Debug)]
pub struct SignalRow {
    pub slug: String, pub coin: Coin, pub s0_ms: i64, pub t_trig: f64, pub side: Side, pub px: f64,
    pub z: ZInfo, pub rank: u32, pub p_up_final: f64, pub win: f64, pub ret_top: f64, pub ret: f64,
    pub entry_ms: i64, pub v2: Decision, pub v21: Decision,
}
pub struct ReplayOutput { pub period: Period, pub signals: Vec<SignalRow>, pub windows: usize, pub files: usize }
pub fn signal_for_token(rows: &[BboRow], coin: Coin, s0_ms: i64, p: &Params) -> Option<Check>;
pub fn replay_with(files: &[PathBuf], windows: &[WindowMeta], closes: &HashMap<Coin, ArrayCloses>,
                   strategy: &Strategy) -> anyhow::Result<Vec<SignalRow>>;
pub fn replay(root: &Path, period: Period, strategy: &Strategy) -> anyhow::Result<ReplayOutput>;
```

**`signal_for_token`** — ровно `paper/replay_test.py::_signal`:
- `rel = (ts − s0)/1000`; `j0` — последняя строка с `rel ≤ t0`; её нет — `None`;
- детектор получает строку `j0`, затем строки с `t0 < rel ≤ t1 + lat`;
- первая удачная проверка — ответ;
- иначе `flush(s0_ms + round((t1 + lat)·1000) + 1)` → удачная проверка или `None`.

**`replay_with`** — порядок `ruslan_v2.book_triggers` и `ruslan_v2.py`:
1. Карта «токен Up → окно».
2. Файлы декодируются параллельно (`rayon`), а результаты разбираются строго по порядку файлов. В файле строки
   группируются по токену по возрастанию байтов (`BTreeMap`); внутри токена — устойчивая сортировка по `ts_ms`; затем
   `signal_for_token`.
3. `found` — упорядоченная карта «окно → (токен, Check)» с семантикой `dict` Python: перезапись сохраняет первую
   позицию окна.
4. Для каждой записи `found` по порядку:
   - нет `covers(s − 400, s + 900)` — запись выбрасывается;
   - иначе `z_score`; `None` или NaN — тоже выбрасывается.
5. В каждом окне номер — по (`t_trig`, позиция в `found`). Решения `v2` и `v21` — по `WindowEngine` с
   `strategy.v2` и `strategy.v21`; проверки подаются по порядку номеров.
6. `win`, `ret_top` (проскальзывание 0) и `ret` (проскальзывание `slip`) — по задаче 6. Строки сортируются по
   (`s0`, номер).

**`replay`** собирает файлы, окна (с `Incidents` для `Main`) и секунды Binance трёх монет и вызывает `replay_with`.

- [ ] **Step 1: Написать падающие тесты**

```rust
#[test] fn no_row_before_t0_gives_no_signal() {}
#[test] fn pre840_state_in_the_band_signals_at_840() {}
#[test] fn failed_then_ok_check_returns_the_ok_one() {}
#[test]
fn ties_at_840_follow_the_backtest_insertion_order() {
    // одно окно, один файл: ETH и BTC в коридоре на 840.000, z хорошие; у ETH токен Up меньше →
    // ETH номер 1, BTC номер 2
}
#[test]
fn a_later_file_replaces_the_signal_but_keeps_the_first_position() {
    // файл A: BTC в коридоре на 840 (позиция 0); файл B: ETH на 840.0 (позиция 1), BTC снова на 840.0
    // итог: у BTC проверка из B, номер 1 — хотя её токен больше, чем у ETH
}
#[test] fn signals_without_binance_coverage_are_dropped_before_ranking() {}
#[test]
fn golden_z_on_the_real_archive() {      // V21_ROOT
    // btc s 1792029600, t_trig 840.0, Down: z 3.8125930024497108, cushion_bp 10.732760574468532,
    //   sigma_bp 0.6374450583311702, h 19.502777777777776 (|Δ| ≤ 1e-9)
    // eth s 1792115100, t_trig 841.716, Down: z 1.8981123947449212, cushion_bp 3.8567995888642668,
    //   sigma_bp 0.4719533673912547, h 18.535833333333333
}
#[test]
fn one_day_matches_the_reference() {     // V21_ROOT
    // файлы 2026-10-14T23 … 2026-10-16T00, окна со стартом 15.10 UTC → те же slug, t_trig, side, px, rank и z,
    // что строки ruslan_rule_all_triggers.parquet этого дня
}
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p v21-replay --test run`
Expected: FAIL.

- [ ] **Step 3: Реализовать `run.rs`.**

- [ ] **Step 4: Убедиться, что тесты проходят**

Run: `$env:V21_ROOT="C:\Users\filip\Documents\15mbacktest"; cargo test -p v21-replay --test run`
Expected: PASS.

---

### Task 9: Сравнение с Python и приёмка

**Files:**
- Create: `crates/replay/src/compare.rs`, `crates/replay/tests/acceptance.rs`

**Interfaces:**
- Consumes: `ReplayOutput`, `SignalRow`, `Period`.
- Produces:
```rust
#[derive(Clone, Debug)]
pub struct RefSignal { pub slug: String, pub coin: Coin, pub s0_ms: i64, pub t_trig: f64, pub side: Side, pub px: f64,
                       pub z: f64, pub cushion_bp: f64, pub sigma_bp: f64, pub h: f64, pub rank: u32,
                       pub ret_top: f64, pub ret: f64, pub entry_ms: i64 }
pub struct Reference { pub signals: Vec<RefSignal>, pub v2_trades: BTreeSet<String>, pub v21_trades: BTreeSet<String>,
                       pub last_s0_ms: i64 }
pub fn load_reference(root: &Path, period: Period) -> anyhow::Result<Reference>;
pub struct Comparison { pub problems: Vec<String>, pub newer: Vec<String>, pub verdict: String }
pub fn compare(got: &ReplayOutput, r: &Reference) -> Comparison;
```

**Эталон:**
- `Main`: `data/ruslan_rule_all_triggers.parquet` и `data/ruslan_v2_trades.parquet`;
- `Pre`: `data_pre/ruslan_rule_pre_triggers.parquet` и `data_pre/ruslan_v2_pre_trades.parquet`;
- `v21_trades` — сделки v2 с монетой btc/eth и часом входа UTC < 7, как `ruslan_v2_1.is_v21`;
- `entry` читается в любой единице времени и переводится в мс;
- `last_s0_ms` — наибольший `s0` эталона.

**Проблемы** — по строке на каждое расхождение:
- сигнал есть только в эталоне;
- сигнал есть только в прогоне при `s0 ≤ last_s0_ms`. Если `s0` больше — строка уходит в `newer`, а не в
  проблемы (Review Focus 1);
- `t_trig`, сторона или `px` не равны;
- `z`, `cushion_bp`, `sigma_bp` или `h` расходятся на 1e-9 и больше или NaN;
- номер отличается;
- `ret_top` или `ret` расходятся на 1e-12 и больше;
- набор сделок v2 или v2.1 отличается (по `slug`).

**Вердикт:**
`signals {равных}/{в эталоне} equal, z max diff {макс}, ranks equal, v2 trades {a}/{b}, v2.1 trades {c}/{d}`. Если
есть `newer`, добавляется `, {k} newer than the reference — rerun ruslan_v2.py`.

- [ ] **Step 1: Написать падающие тесты**

```rust
#[test] fn each_kind_of_mismatch_is_one_problem_line() { /* 7 искусственных расхождений → 7 строк */ }
#[test] fn signals_after_the_reference_go_to_newer_not_to_problems() {}
#[test] fn verdict_text() { /* равные наборы → "signals 3/3 equal, z max diff < 1e-9, ranks equal, v2 trades 1/1, v2.1 trades 1/1" */ }
#[test]
fn main_archive_matches_python() {       // V21_ROOT
    // problems пусто; v2 и v2.1 — то же число сделок, что в эталоне; вердикт печатается
}
#[test]
fn pre_archive_matches_python() {        // V21_ROOT
    // problems пусто; ровно 517 сигналов, 69 сделок v2, 55 сделок v2.1
}
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p v21-replay --test acceptance`
Expected: FAIL.

- [ ] **Step 3: Реализовать `compare.rs`.**

- [ ] **Step 4: Пройти приёмку**

Run: `$env:V21_ROOT="C:\Users\filip\Documents\15mbacktest"; cargo test --release -p v21-replay --test acceptance -- --nocapture`
Expected: PASS. Вердикт `Main` при данных на 18.10.2026:
`signals 2748/2748 equal, z max diff < 1e-9, ranks equal, v2 trades 315/315, v2.1 trades 201/201`.
Любое расхождение исправляется до перехода к задаче 10, а его причина записывается в README.

---

### Task 10: Команда `v21 replay`, отчёт по периодам, README, покрытие

**Files:**
- Create: `crates/replay/src/report.rs`, `crates/cli/Cargo.toml` (`v21-cli`, `[[bin]] name = "v21"`; зависимости
  `v21-core`, `v21-replay`, `clap` с `derive`, `anyhow`), `crates/cli/src/main.rs`, `crates/cli/tests/cli.rs`,
  `v21bot/README.md`
- Modify: `v21bot/Cargo.toml` — `members` + `"crates/cli"`

**Interfaces:**
- Consumes: `replay`, `load_reference`, `compare`, `stats`, `half_of`.
- Produces:
```rust
pub fn period_table(signals: &[SignalRow], s: &Strategy, pre: bool) -> String;
pub fn write_csv(out_dir: &Path, signals: &[SignalRow], s: &Strategy) -> anyhow::Result<()>;
// signals.csv, trades_v2.csv, trades_v21.csv
```
CLI: `v21 replay [--root DIR] [--period main|pre] [--out DIR]`. Корень — `--root`, иначе `V21_ROOT`, иначе ошибка.
По умолчанию `--period main`, `--out replay_out`. Коды выхода:
- 0 — расхождений нет;
- 1 — есть расхождения;
- 2 — плохой ввод: нет корня, нет файла, не тот JSON.

**`period_table`** — как `ruslan_v2_1.py::side_by_side`: строка на период, колонки
`v2 n | v2 win% | v2 roi% | v2 t | v2.1 n | ... | dropped n | ... | dropped t`. Доход — с проскальзыванием, t — по
окнам.

**Колонки `signals.csv`:** `slug, coin, s0, t_trig, side, px, z, cushion_bp, sigma_bp, h, rank, v2_decision,
v2_reason, v21_decision, v21_reason, win, ret_top, ret`. В `trades_*.csv` — те же колонки, только строки со сделкой.

- [ ] **Step 1: Написать падающие тесты**

```rust
#[test]
fn bad_root_exits_2_and_names_the_path() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_v21"))
        .args(["replay", "--root", "Z:/no/such/dir"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("strategy_v2.json"));
}
#[test] fn csv_header_is_exact() { /* первая строка signals.csv — колонки выше через запятую */ }
#[test] fn period_table_has_a_row_per_period_and_the_three_column_groups() {}
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p v21-cli`
Expected: FAIL.

- [ ] **Step 3: Реализовать `report.rs` и `main.rs`.** Команда печатает:
- число окон, файлов и сигналов;
- вердикт сравнения и строки проблем (не больше 40 и хвост «... and N more»);
- `newer`;
- таблицу периодов;
- время прогона в секундах.

- [ ] **Step 4: Убедиться, что тесты проходят**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: Полный прогон и время**

Run: `cargo run --release -p v21-cli -- replay --root C:\Users\filip\Documents\15mbacktest`
Expected: код 0, вердикт как в задаче 9, время не больше 120 с.

Run: `cargo run --release -p v21-cli -- replay --root C:\Users\filip\Documents\15mbacktest --period pre`
Expected: код 0, `v2 trades 69/69, v2.1 trades 55/55`.

Сверка таблицы периодов с Python (критерий «итоги v2.1» из §5.4 спецификации). Команда только читает файлы — в
отличие от `python ruslan_v2_1.py`, она не перезаписывает Excel и график. Запуск из корня `15mbacktest`:

Run: `py -3.11 -c "import ruslan_v2_1 as R; V=R.v2_trades(); M=R.is_v21(V); print(R.side_by_side([(p, g, M[g.index]) for p, g in V.groupby('half')]).to_string(index=False))"`
Expected: для каждого периода число сделок, win%, roi% и t у v2, v2.1 и выкинутых совпадают с таблицей `v21 replay`
(main и pre) до округления печати.

- [ ] **Step 6: Покрытие**

Run: `$env:V21_ROOT="C:\Users\filip\Documents\15mbacktest"; cargo llvm-cov --workspace --summary-only`
Expected: `v21-core` — не меньше 95% строк, всего — не меньше 85%. Если меньше, дописываются тесты на непокрытые
ветки. Оставшиеся непокрытые строки перечисляются в README с причиной.

- [ ] **Step 7: README** (`v21bot/README.md`, по-русски):
- как поставить тулчейн;
- как запустить тесты — с `V21_ROOT` и без;
- как запустить прогон и что значат коды выхода;
- вердикты `main` и `pre` на дату прогона, время прогона;
- покрытие и перечень непокрытых строк.

---

## После вехи 1 — следующие планы

Каждый план пишется после приёмки предыдущей вехи, по спецификации (§14).

| План | Что войдёт | Что нужно до него |
|---|---|---|
| Веха 2 — живые данные и демо | живые цены Binance (`aggTrade` → секунды), лента Polymarket на 2 копии (перенос `wire.rs`, `book.rs`, `copies.rs` из `pmbot`), горячий поток, быстрое и точное решения, ожидание номера монеты, демо-исполнение FAK, журнал, CSV, `v21 report`, Telegram, `v21 reconcile`, служба на сервере | веха 1; записанные сообщения двух копий ленты (способ распознать одно событие) |
| Веха 3 — боевой режим | подпись Deposit Wallet заранее (официальный клиент), своя отправка, ответ и канал «мои сделки», баланс, пределы в коде, паузы и стопы, погашение через ретранслятор, `/stop` и `STOP`, `setup`, `check`, `test-order`, `bench`, сквозные тесты с поддельной биржей | веха 2 |
| Веха 4 — запуск | `setup` → `check` → пробный ордер → `live` на сервере | веха 3; новый аккаунт и ключи от пользователя |
