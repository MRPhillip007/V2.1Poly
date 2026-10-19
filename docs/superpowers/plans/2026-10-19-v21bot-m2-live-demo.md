# v21bot — веха 2: живые данные и демо-режим. План реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Цель:** `v21 run` в режиме `demo` на живых лентах Polymarket и Binance. По каждому сигналу бот принимает быстрое и
точное решения и считает демо-исполнение ордера $20 по стакану в момент, когда ордер исполнился бы. Ещё он ведёт
демо-счёт $100, журнал и CSV, делает отчёт и сверку с архивом, шлёт сообщения в Telegram. Ключи биржи не нужны:
боевые ордера — веха 3.

**Архитектура.** Новый крейт `v21-live` поверх ядра вехи 1; папки — как в §13 спецификации:
- **`hot/` — горячий поток.** Свой поток ОС на ядре `cores.hot` со своим tokio `current_thread`. В нём:
  - две копии ленты Polymarket и поток Binance;
  - отсев дубликатов и разбор;
  - быстрые решения и таймер 840.000 с.
- **`cold/` — неспешный рантайм** (tokio, ядро `cores.cold`):
  - Gamma и CLOB, поправка часов;
  - стаканы и точный движок;
  - демо-исполнение и демо-счёт;
  - журнал и Telegram.
- **`shared/`** — конфиг, секреты, часы биржи, сообщения между потоками.
- **Связь** — очереди SPSC (`rtrb`) и атомики. Горячий поток пересылает неспешному:
  - каждое событие ленты;
  - каждую сделку Binance;
  - каждое быстрое решение.

  Сам он неспешный рантайм никогда не ждёт.

Быстрый и точный движки — структуры без сети и часов: их проверяют синтетические сценарии и архив. Точный движок —
перенос `paper/engine.py` с тем же порядком работы, но события он берёт по времени биржи, через буфер 500 мс.

**Стек:**
- Rust stable не ниже 1.88;
- основные крейты: `tokio`, `tokio-tungstenite` (rustls), `reqwest` (rustls, json), `bytes`, `rtrb`, `core_affinity`, `xxhash-rust` (xxh3);
- данные и прочее: `serde` и `serde_json` (`float_roundtrip`), `toml`, `zeroize`, `tracing`, `csv`, `anyhow`;
- в тестах: `tempfile`, `proptest`, `flate2`; поддельные серверы — на `axum` и `tokio-tungstenite`.

**Спецификация:** `docs/superpowers/specs/2026-10-18-v21bot-design.md` — §3, §4, §5.1–5.3, §5.5, §6.2, §6.3, §7.1,
§7.4, §8–§12, §14 (веха 2).

**Основа:**
- план вехи 1 `docs/superpowers/plans/2026-10-18-v21bot-m1-core-replay.md` и код `v21bot/`;
- что переносится — из корня `15mbacktest` (`V21_ROOT`):
  - `paper/`: `engine.py`, `feeds.py`, `closes.py`, `book.py`, `journal.py`, `notify.py`, `reconcile.py`, `run_paper.py` и их тесты;
  - `pmbot/`: `src/hot/wire.rs`, `src/hot/book.rs`, `src/shared/fees.rs`, `src/secrets.rs`, `tests/golden_feed.rs`, `tests/no_alloc.rs`, `tests/data/feed_vectors.json`.

## Факты, на которых стоит план

Запись 19.10.2026, 03:23–03:46 UTC, домашний ПК: две копии ленты Polymarket на токены двух окон и зеркало Binance.

| Что | Замер | Следствие |
|---|---|---|
| Две копии ленты | кадры совпадают байт в байт: 99.5%, остальное потеряно в разрыве копии. Порядок внутри рынка тот же: 4 перестановки на 788 тыс. пар | дубликат отсеивается по хешу байтов кадра и номеру повтора, без разбора |
| Поле `hash` события | одно и то же у нескольких разных сообщений одной миллисекунды | ключ — все байты кадра, а не `hash` |
| Выигрыш от первой копии | медиана 1.1 мс, p95 64 мс | две копии срезают хвост задержки |
| Нагрузка на соединение (12 токенов) | кадров в секунду: медиана 563, p99 1 913, максимум 2 737. Размер кадра: медиана 612 байт, максимум 36 КБ | медленного читателя биржа закрывает кодом 1013 «slow consumer» |
| Поздние события | 261 `price_change` старше уже пришедшего события своего токена (медиана на 114 мс, максимум 295 мс); 5 снимков `book` (медиана 21 мс, максимум 27 мс) | быстрый путь их отбрасывает, точный ставит на место по времени (буфер 500 мс) |
| Binance | `stream.binance.com` с домашнего ПК отвечает HTTP 451; зеркало `data-stream.binance.vision` работает | запасной адрес обязателен |
| Часы ПК | отстают от биржи на 1.04 с: приход события минус время биржи — медиана −1 042 мс | моменты окна — по часам биржи: местные часы плюс поправка |

## Global Constraints

- **Всё из Global Constraints вехи 1 в силе:**
  - Rust не ниже 1.88, edition 2024;
  - параметры — только из двух замороженных JSON с проверкой sha256;
  - `v21-core` без ввода-вывода;
  - строки причин вехи 1 — без изменений;
  - архивные тесты — через `V21_ROOT` (после задачи 1 — и через `V21_DATA`);
  - идентификаторы и комментарии — по-английски, README — по-русски;
  - команды `cargo` выполняются из `v21bot/`.
- **Пути** файлов в задачах — от `v21bot/`, кроме `docs/`.
- **Режим.** Веха 2 делает только `mode = "demo"`. При `mode = "live"` бот не запускается: «боевой режим будет в
  вехе 3», код выхода 2.
- **Горячий путь** — от байтов кадра или сделки Binance до быстрого решения и записи в очередь. В него входят отсев
  дубликатов, разбор, события, детектор, `LiveCloses` и решение. После прогрева он:
  - не выделяет память (тест со считающим аллокатором, как `pmbot/tests/no_alloc.rs`);
  - не блокируется и не берёт блокировок, общих с неспешным рантаймом: только SPSC и атомики.

  Ввод-вывод библиотеки вебсокетов в эту границу не входит.
- **Время окна** — по часам биржи: местные часы плюс поправка (задача 11). Если поправки нет или она больше
  `clock_max_offset_ms`, включается пауза (§7.4).
- **Новые ключи конфига** — в дополнение к §12:

| Ключ | По умолчанию | Что |
|---|---|---|
| `[feeds] polymarket_close_s` | `880` | отключение копий ленты |
| `[feeds] reorder_hold_ms` | `500` | буфер точного движка |
| `[feeds] binance_rest_fallback` | `"https://data-api.binance.vision"` | запасной REST Binance |
| `[exec] taker_delay_ms` | `250` | задержка тейкера в демо, пока нет живых ордеров (§6.2) |
| `[risk] ntp_server` | `"time.google.com:123"` | источник поправки часов; `""` — только Binance |
| `[cores] pin` | `true` | закреплять ли потоки за ядрами; в тестах — `false` |
| `[urls] gamma` | `"https://gamma-api.polymarket.com"` | |
| `[urls] clob` | `"https://clob.polymarket.com"` | |
| `[urls] polymarket_ws` | `"wss://ws-subscriptions-clob.polymarket.com/ws/market"` | |
| `[urls] geoblock` | `"https://polymarket.com/api/geoblock"` | |
| `[urls] telegram_api` | `"https://api.telegram.org"` | тесты подставляют поддельные серверы во все `[urls]` |

- **Постоянное расписание окна** — как в `paper/run_paper.py`:
  - **Gamma:** с 540-й секунды, повтор раз в 20 с до 700-й;
  - **тик быстрого движка:** раз в 50 мс в 835–872 с и ровно в 840.000;
  - **исходы:** с 930-й секунды раз в 30 с, через 2 часа — раз в 5 минут; окно без позиций перестаёт ждать через 6 часов.
- **Демо-счёт:**
  - старт — `demo_bank_usd`, ставка — `stake_usd` до комиссии, предел цены — 0.88;
  - комиссия сверху — `fee()` из `pmbot/src/shared/fees.rs`: `round5(контракты · rate · (p(1−p))^exponent)`;
  - денежные правила — §7.1 (`stake_usd + 0.50`).
- **Секреты:**
  - в вехе 2 нужны только токен и chat_id Telegram (`secrets.toml`, права 600 на Unix);
  - ключи вехи 3 в файле не мешают;
  - значения секретов не попадают в журнал, логи и Telegram.
- **Коммиты.** Пользователь разрешил коммиты и пуш в `github.com/MRPhillip007/V2.1Poly`:
  - коммит — в конце каждой задачи; сообщение — заголовок задачи плюс строка
    `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`;
  - пуш в `v21poly main` — после финального ревью вехи.
- **Покрытие:**
  - `v21-core` — не меньше 95% строк, весь код — не меньше 85%;
  - строки, которые проверяются только вживую, перечисляются в README поимённо.

## Review Focus

1. **Часы машины сбиты** (дома — на ~1 с). Таймер 840.000 и все моменты окна считаются по часам биржи: поправка по
   NTP, запасной источник — Binance `/api/v3/time`. Если поправки нет или она больше `clock_max_offset_ms`, это пауза:
   выстрелов нет, тревога. Тесты — задачи 5, 13 и все сквозные сценарии задачи 18: бот в них живёт на сдвинутых
   часах.
2. **Обе копии ленты оборвались в решающую минуту** (или биржа закрыла их кодом 1013):
   - детекторы сбрасываются, до свежего снимка выстрелов нет;
   - окно помечается «пропуски данных Polymarket»; обрыв дольше 10 с — тревога;
   - старый стакан сигнала не даёт.

   Тесты — задачи 12, 13 и 18.
3. **Поздние события** — время старше уже применённого:
   - быстрый путь их отбрасывает;
   - точный ставит на место по времени;
   - опоздавшее дальше буфера отбрасывается и считается в записи окна.

   Тесты — задачи 12 и 13.
4. **Binance недоступен.**
   - Основной адрес отвечает 451 — бот работает через зеркало.
   - Сделок нет дольше `feed_silent_ms` в 835–870 с — пауза, выстрелов нет.
   - Точное решение ждёт дозаполнения дыр свечами.

   Тесты — задачи 10, 13 и 18.
5. **Gamma не дала рынок монеты, или у рынка другие правила.**
   - Монета без рынка считается возможной более ранней проверкой: стреляет только монета, чей номер не больше 2 при
     любом исходе этой проверки.
   - Монета с другими правилами не торгуется (пауза по ней), но участвует в нумерации.

   Тесты — задачи 11 и 13.

## Карта файлов

| Файл | Что в нём |
|---|---|
| `crates/core/src/decision.rs` | `WindowEngine` без ссылки на `Params`; `reason_for` — причина без счётчика |
| `crates/core/src/ranks.rs` | `rank_state` — номер монеты для быстрого решения |
| `crates/core/src/live_closes.rs` | `LiveCloses` — секунды Binance из сделок и свечей; кольцо без выделений памяти |
| `crates/core/src/fill.rs` | комиссия рынка, демо-исполнение FAK по уровням |
| `crates/core/src/money.rs` | денежные правила §7.1 и демо-счёт |
| `crates/live/src/shared/{config,secrets,clock,msgs}.rs` | конфиг, секреты Telegram, часы биржи, сообщения между потоками |
| `crates/live/src/book.rs` | стакан токена на сетке 0.001, обе стороны |
| `crates/live/src/health.rs` | гистограммы и запись `health` |
| `crates/live/src/hot/{wire,items,copies,ws,binance,engine,timer,mod}.rs` | разбор кадров, события ленты, отсев дубликатов, соединения, Binance, быстрый движок, расписание, горячий поток |
| `crates/live/src/cold/{binance_rest,gamma,clob_info,ntp,exact,demo,journal,csv,telegram,mod}.rs` | свечи и время Binance, рынки окна, настройки рынка, поправка часов, точный движок, демо-счёт, журнал, CSV, Telegram, неспешный рантайм |
| `crates/live/src/{run,report,reconcile,check}.rs` | запуск бота, `v21 report`, `v21 reconcile`, `v21 check` |
| `crates/live/tests/support/` | поддельные Polymarket, Binance, Gamma, CLOB, Telegram; сценарии |
| `crates/cli/src/main.rs` | + `run`, `report`, `reconcile`, `check` |
| `tools/{record_feeds,make_bbo_vectors}.py` | запись двух копий ленты; эталон разбора из `paper/feeds.parse_ws` |
| `deploy/v21bot.service`, `deploy/README.md` | служба и инструкция для сервера |

**Отличия от §13 спецификации:**
- **Стакан** — общий модуль `book.rs`, а не `hot/book.rs`. Быстрому решению хватает лучших цен сервера (§5.2); полный
  стакан нужен точному движку и демо.
- **`hot/copies.rs`** отсеивает дубликаты кадров, а не выбирает копию.
- **Часы:** сами часы — в `shared/clock.rs`, их замер — в `cold/ntp.rs`.

---

### Task 1: Долги вехи 1 (по итогам ревью)

**Files:**
- Modify: `crates/replay/src/{archive,compare,run}.rs`, `crates/cli/src/main.rs`, `.gitignore`, `README.md`
- Test: `crates/replay/tests/{run,acceptance}.rs`, `crates/cli/tests/cli.rs`

Ничего нового — десять отложенных мелочей из журнала вехи 1 (строки `Final: minor (deferred)`).

**Interfaces:**
- Produces:
  - `replay_with(...)` возвращает ошибку `no Binance closes for <coin>`, если монеты нет в `closes`;
  - в `archive.rs` — переименования `ts_ms` → `int_column` и `prices` → `float_column`, поведение то же;
  - `--data` — синоним `--root`, `V21_DATA` — синоним `V21_ROOT` (так их называют §4.4 и §10 спецификации).

- [ ] **Step 1: Написать падающие тесты**
```rust
#[test] fn an_earlier_files_signal_survives_when_the_windows_own_hour_has_none() { /* файл A с сигналом, файл B без строк окна */ }
#[test] fn a_coin_missing_from_closes_is_an_error() { /* replay_with без Coin::Sol в closes → Err с "sol" */ }
#[test] fn verdict_z_max_diff_counts_every_z_field_and_keeps_nan() { /* сбой только в sigma_bp → не "< 1e-9"; NaN не теряется */ }
#[test] fn newer_lines_are_capped_at_40_with_a_tail() {}
#[test] fn a_valid_root_without_a_data_file_exits_2_and_names_it() { /* временный корень с JSON, без data/meta_15m.parquet */ }
#[test] fn data_and_v21_data_are_aliases() {}
#[test] fn a_closed_stdout_is_a_clean_exit() { /* stdout закрыт сразу → код 0 или 1, в stderr нет "panicked" */ }
```
- [ ] **Step 2: Убедиться, что тесты падают:** `cargo test --workspace` → FAIL.
- [ ] **Step 3: Реализовать:**
  - вывод CLI — через `writeln!`; ошибка «broken pipe» — обычный выход;
  - эталон загружается до прогона;
  - «z max diff» считается по всем четырём полям z, NaN сохраняется;
  - в `.gitignore` — `/replay_out`;
  - в README — граница «новее эталона».
- [ ] **Step 4: Убедиться, что тесты проходят:** `cargo test --workspace` (с `V21_ROOT`) → PASS.
- [ ] **Step 5: Коммит.**

---

### Task 2: Решение без ссылки на параметры и номер монеты для быстрого решения

**Files:**
- Create: `crates/core/src/ranks.rs`
- Modify: `crates/core/src/{decision,lib}.rs`, `crates/replay/src/run.rs`
- Test: `crates/core/tests/ranks.rs`, `crates/core/tests/decision.rs`

**Interfaces:**
- Produces:
```rust
// decision.rs — WindowEngine больше не держит &Params: живые движки хранят его между окнами
pub struct WindowEngine { /* s, counted */ }
impl WindowEngine {
    pub fn new(s: i64) -> WindowEngine;
    pub fn decide(&mut self, p: &Params, chk: &Check, z: Option<&ZInfo>) -> Decision;   // counted += 1 у удачной проверки, затем reason_for
}
/// Причина и hour_ok проверки с известным номером: порядок причин WindowEngine::decide без счётчика.
pub fn reason_for(p: &Params, s: i64, chk: &Check, rank: Option<u32>, z: Option<&ZInfo>) -> (Reason, bool);
// ranks.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Earlier { Ok, Failed, Pending }      // проверки других монет окна с ключом (t_trig, id токена Up) меньше нашего
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RankState { Rank(u32), Wait }
pub fn rank_state(earlier: &[Earlier], max_coins: u32) -> RankState;
```
**Правило (§5.3).** `k` — число `Ok`, `n` — число `Pending`:
- `k ≥ max_coins` → `Rank(k + 1)`: третья монета при любых идущих проверках;
- `k + n < max_coins` → `Rank(k + 1)`: номер не больше `max_coins` при любом исходе идущих проверок;
- иначе → `Wait`.

`Rank(r)` — номер, по которому принимается решение. Идущая проверка может поднять настоящий номер, но только в
пределах `max_coins`, и решение от этого не меняется.

- [ ] **Step 1: Написать падающие тесты**
```rust
#[test] fn no_earlier_check_is_rank_1() { assert_eq!(rank_state(&[], 2), RankState::Rank(1)); }
#[test] fn two_settled_ok_make_a_third_coin() { assert_eq!(rank_state(&[Earlier::Ok, Earlier::Ok], 2), RankState::Rank(3)); }
#[test] fn two_ok_and_a_pending_are_still_a_third_coin() { assert_eq!(rank_state(&[Earlier::Ok, Earlier::Pending, Earlier::Ok], 2), RankState::Rank(3)); }
#[test] fn one_ok_and_one_pending_must_wait() { assert_eq!(rank_state(&[Earlier::Ok, Earlier::Pending], 2), RankState::Wait); }
#[test] fn one_pending_alone_cannot_make_a_third_coin() { assert_eq!(rank_state(&[Earlier::Pending], 2), RankState::Rank(1)); }
#[test] fn failed_checks_do_not_count() { assert_eq!(rank_state(&[Earlier::Failed, Earlier::Ok], 2), RankState::Rank(2)); }
// decision.rs
#[test] fn reason_for_matches_window_engine_on_every_combination() {
    // номер 1..=3 × час в часах и вне × BTC и SOL × z None / NaN / 0.5 / 2.0 × удачная и неудачная проверка, для v2 и v2.1:
    // reason_for(p, s, chk, rank, z) == причина и hour_ok из WindowEngine::decide
}
```
- [ ] **Step 2: Убедиться, что тесты падают:** `cargo test -p v21-core --test ranks --test decision` → FAIL.
- [ ] **Step 3: Реализовать.** Вызовы в `crates/replay/src/run.rs` передают `&strategy.v2` и `&strategy.v21`.
- [ ] **Step 4: Убедиться, что тесты проходят:** `cargo test --workspace` (с `V21_ROOT`: приёмка вехи 1 тоже) → PASS.
- [ ] **Step 5: Коммит.**

---

### Task 3: Живые секунды Binance (`LiveCloses`)

**Files:**
- Create: `crates/core/src/live_closes.rs`
- Test: `crates/core/tests/live_closes.rs`

**Interfaces:**
- Consumes: `Closes`, `ArrayCloses`, `z_score` (веха 1).
- Produces:
```rust
pub const CAPACITY_S: i64 = 8192;
pub struct LiveCloses { /* кольцо секунд: цена, время последней сделки, дыра; lo; floor */ }
impl LiveCloses {
    pub fn new() -> LiveCloses;                                    // кольцо выделяется здесь, один раз
    pub fn add_trade(&mut self, ts_ms: i64, price: f64);
    pub fn add_klines(&mut self, rows: &[(i64, f64)]);            // (open_ms, close) одного ответа REST
    pub fn mark_gap(&mut self, q0: i64, q1: i64);
    pub fn prune(&mut self, before: i64);
    pub fn get(&self, q0: i64, q1: i64, out: &mut Vec<f64>) -> bool;   // get() из paper; false — None
    pub fn latest_trade_ms(&self) -> Option<i64>;
    pub fn is_final(&self, q: i64, now_ms: i64) -> bool;
}
impl Closes for LiveCloses { .. }
```
**Поведение** — `paper/closes.py::SecondCloses` строка в строку; её docstring — это спецификация:
- **Сделки.** Закрытие секунды — её последняя по времени сделка; более ранняя сделка, пришедшая позже, не побеждает.
- **Свечи:**
  - заполняют только секунды без собственных сделок;
  - закрывают дыры в своём диапазоне;
  - секунды после последней строки ответа остаются как были.
- **Тихие секунды и дыры.** Секунда без сделок повторяет прежнюю цену. Диапазон, задевший дыру, неизвестен, пока её
  не заполнят сделки или свечи.
- **`prune`** переносит действующую цену в первую оставшуюся секунду; более старые данные с этого момента
  игнорируются.

**Кольцо:**
- ячейка — `q mod CAPACITY_S`;
- секунда `q ≥ lo + CAPACITY_S` сначала делает `prune(q − CAPACITY_S + 1)`, поэтому оборот кольца не меняет ни одного
  ответа о сохранённых секундах;
- после `new()` ничего не выделяется.

**Суммы** — прямым проходом по диапазону: «вживую — в скользящем окне» (§5.1). Логарифмические приращения — как в
`SecondCloses`.

**`is_final(q, now_ms)`** (§5.3) — истина, если выполнено одно из трёх:
- пришла сделка со временем ≥ `(q+1)·1000`;
- пришла свеча секунды ≥ `q`;
- `now_ms ≥ (q+1)·1000 + 1500`.

Пока секунда `t−1` не закрыта, её закрытие — цена последней пришедшей сделки. Это и есть быстрый z (§5.3).

- [ ] **Step 1: Написать падающие тесты.**
  - Перенести `SecondClosesTests` из `paper/tests/test_closes_z.py` один к одному: 21 тест, от
    `forward_fill_quiet_seconds` до `random_trades_match_a_naive_model`.
  - `prune_frees_the_old_seconds` проверяет, что секунды до среза больше не отвечаются.
  - Добавить:
```rust
#[test] fn sums_agree_with_array_closes() { /* как paper test_sums_agree_between_the_live_buffer_and_the_archive: относительная |Δ| ≤ 1e-9 */ }
#[test] fn z_on_live_and_array_closes_agree() { /* z_score на обоих: |Δz| ≤ 1e-6 при t_trig от 840.0 до 870.0 через 0.1 */ }
#[test] fn a_second_is_final_after_a_later_trade_a_kline_or_1_5_s() {}
#[test] fn an_open_second_answers_with_the_last_trade() { /* сделка в q, в q+1 нет: close(q) — её цена; is_final(q, (q+1)·1000 + 1499) = false, +1500 → true */ }
#[test] fn a_wrap_of_the_ring_answers_like_prune() { /* случайные сделки за 3 × CAPACITY_S: ответы о последних 4 000 с == наивной модели с явным prune */ }
```
- [ ] **Step 2: Убедиться, что тесты падают:** `cargo test -p v21-core --test live_closes` → FAIL.
- [ ] **Step 3: Реализовать.**
- [ ] **Step 4: Убедиться, что тесты проходят:** `cargo test -p v21-core` → PASS.
- [ ] **Step 5: Коммит.**

---

### Task 4: Комиссия, демо-исполнение FAK и денежные правила

**Files:**
- Create: `crates/core/src/{fill,money}.rs`
- Modify: `crates/core/Cargo.toml` (`serde` с `derive`), `crates/core/src/window.rs` (`Coin` и `Side` сериализуются как
  `"btc"`/`"eth"`/`"sol"` и `"Up"`/`"Down"`, как в журнале `paper`)
- Test: `crates/core/tests/{fill,money}.rs`

**Interfaces:**
- Produces:
```rust
// fill.rs — fee() и round5: перенос pmbot/src/shared/fees.rs вместе с его тестами
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeeSchedule { pub rate: f64, pub exponent: f64 }
pub fn fee(shares: f64, p: f64, fs: &FeeSchedule) -> f64;
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Fill { pub shares: f64, pub notional_usd: f64, pub fee_usd: f64, pub avg_price: f64, pub levels_used: u32, pub unfilled_usd: f64 }
pub fn fak_buy(asks: &[(f64, f64)], notional_usd: f64, limit: f64, fs: &FeeSchedule) -> Fill;
// money.rs — правила §7.1, общие с боевым счётом вехи 3
pub const RESERVE_USD: f64 = 0.50;
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StakeAnswer { Stake(f64), NotEnough, Stop }
pub fn stake_answer(free_usd: f64, stake_usd: f64, open_positions: usize) -> StakeAnswer;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DemoPosition { pub id: String, pub s: i64, pub coin: Coin, pub side: Side, pub shares: f64, pub cost_usd: f64, pub fee_usd: f64, pub avg_price: f64 }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DemoAccount { pub cash: f64, pub positions: Vec<DemoPosition>, pub stopped: bool }
impl DemoAccount {
    pub fn new(bank_usd: f64) -> DemoAccount;
    pub fn stake(&mut self, stake_usd: f64) -> StakeAnswer;    // stake_answer(cash, ...); Stop → stopped = true
    pub fn open(&mut self, pos: DemoPosition);                  // cash −= cost_usd
    pub fn settle(&mut self, id: &str, payout_per_share: f64) -> Option<(f64, f64)>;   // (выплата, P&L); None — позиции нет
}
```
**`fak_buy`:**
- уровни берутся от дешёвых к дорогим, до `limit`;
- на уровне берётся `min(size, остаток / p)`, пока не потрачено `notional_usd` до комиссии;
- комиссия — сумма `fee(взято, p)` по уровням;
- `avg_price` — средневзвешенная цена без комиссии (NaN, если ничего не куплено);
- `unfilled_usd` — непотраченная часть `notional_usd`.

`cost_usd` позиции = `notional_usd + fee_usd`.

- [ ] **Step 1: Написать падающие тесты**
```rust
#[test] fn fee_rounds_like_python() { /* наборы из тестов pmbot/src/shared/fees.rs */ }
#[test] fn one_level_fills_whole() { /* [(0.80, 100.0)], $20 → 25 шт., notional 20, fee 0.28, avg 0.80, уровней 1 */ }
#[test] fn levels_are_walked_cheapest_first_up_to_the_limit() { /* [(0.85, 10), (0.80, 10), (0.90, 100)], предел 0.88 → 20 шт., notional 16.5, unfilled 3.5 */ }
#[test] fn the_budget_ends_inside_a_level() { /* [(0.80, 10), (0.85, 100)] → 10 по 0.80 и 12/0.85 по 0.85; notional ровно 20 */ }
#[test] fn nothing_under_the_limit_is_no_fill() { /* shares 0, avg NaN, unfilled 20 */ }
#[test] fn stake_rules() { /* (20.49, 20, 1) → NotEnough; (20.49, 20, 0) → Stop; (20.50, 20, 0) → Stake(20) */ }
#[test] fn settle_pays_1_0_or_half() { /* 25 шт., cost 20.28: 1 → выплата 25, P&L +4.72; 0 → −20.28; 0.5 → −7.78; cash сходится */ }
```
- [ ] **Step 2: Убедиться, что тесты падают:** `cargo test -p v21-core --test fill --test money` → FAIL.
- [ ] **Step 3: Реализовать.**
- [ ] **Step 4: Убедиться, что тесты проходят:** `cargo test -p v21-core` → PASS.
- [ ] **Step 5: Коммит.**

---

### Task 5: Крейт `v21-live`: конфиг, секреты, часы биржи

**Files:**
- Create: `crates/live/Cargo.toml`, `crates/live/src/{lib,run}.rs` (`run` — заглушка до задачи 16),
  `crates/live/src/shared/{mod,config,secrets,clock}.rs`, `v21bot.example.toml`, `secrets.example.toml`
- Modify: `Cargo.toml` (`members` + `"crates/live"`), `crates/cli/{Cargo.toml,src/main.rs}`
  (`v21 run [--config FILE] [--secrets FILE]`, по умолчанию `/etc/v21bot/v21bot.toml` и `/etc/v21bot/secrets.toml`)
- Test: `crates/live/tests/config.rs`, `crates/cli/tests/cli.rs`

**Interfaces:**
- Produces:
```rust
// shared/config.rs — поля разделов: ключи §12 и таблицы Global Constraints, те же имена
pub enum Mode { Demo, Live }
pub struct Config { pub mode: Mode, pub stake_usd: f64, pub demo_bank_usd: f64, pub strategy_dir: PathBuf,
                    pub feeds: Feeds, pub exec: Exec, pub risk: Risk, pub cores: Cores,
                    pub telegram: TelegramCfg, pub paths: Paths, pub urls: Urls }
impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Config>;
    pub fn strategy(&self) -> anyhow::Result<Strategy>;     // оба JSON из strategy_dir через load_strategy вехи 1
}
// shared/secrets.rs — перенос pmbot/src/secrets.rs: свой разбор без цитат значений, Debug → "***", zeroize, права 600
pub struct Secrets { pub telegram_token: Option<String>, pub telegram_chat_id: Option<String> }
impl Secrets { pub fn load(path: &Path) -> anyhow::Result<Secrets>; }
// shared/clock.rs
pub struct ExchangeClock { /* поправка в мс (AtomicI64), известна ли (AtomicBool) */ }
impl ExchangeClock {
    pub fn new() -> ExchangeClock;
    pub fn now_ms(&self) -> i64;            // местные часы + поправка
    pub fn now_us(&self) -> i64;
    pub fn set_offset_ms(&self, ms: i64);
    pub fn offset_ms(&self) -> Option<i64>;
}
/// None — часы в порядке; иначе причина паузы: «пауза: часы не измерены» или «пауза: часы сбиты».
pub fn clock_pause(offset_ms: Option<i64>, max_offset_ms: i64) -> Option<&'static str>;
```
**Правила:**
- `stake_usd > 20` — ошибка «stake_usd выше предела в коде ($20)»;
- неизвестный ключ — ошибка с его именем;
- пропущенный ключ получает значение по умолчанию.

- [ ] **Step 1: Написать падающие тесты**
```rust
#[test] fn example_config_loads_with_the_spec_defaults() { /* v21bot.example.toml: Demo, 20, 100, open 600, close 880, hold 500, taker 250, silent 2000, clock 50, ядра 1/0, "07:30" */ }
#[test] fn an_empty_config_is_all_defaults() {}
#[test] fn stake_above_the_code_limit_is_refused() {}
#[test] fn an_unknown_key_is_named() {}
#[test] fn secrets_debug_hides_values_and_errors_quote_no_line() {}
#[test] fn exchange_clock_adds_the_offset() { /* поправка −1042 → now_ms = местные − 1042 (± 1); до замера offset_ms() = None */ }
#[test] fn clock_pause_rules() { /* None → «пауза: часы не измерены»; Some(51) при пределе 50 → «пауза: часы сбиты»; Some(−50) → None */ }
// cli.rs
#[test] fn live_mode_is_refused_in_milestone_2() { /* mode = "live" → код 2, в stderr «вехе 3» */ }
```
- [ ] **Step 2: Убедиться, что тесты падают:** `cargo test -p v21-live -p v21-cli` → FAIL.
- [ ] **Step 3: Реализовать.**
- [ ] **Step 4: Убедиться, что тесты проходят:** `cargo test --workspace` → PASS.
- [ ] **Step 5: Коммит.**

---

### Task 6: Стакан токена

**Files:**
- Create: `crates/live/src/book.rs`
- Test: `crates/live/tests/book.rs`

**Interfaces:**
- Produces:
```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BookSide { Bid, Ask }
pub fn grid_tick(price: f64) -> Option<u16>;          // перенос pmbot/src/hot/book.rs: 0..=1000, вне сетки 0.001 — None
pub struct Book { /* объём на тик для bids и asks, лучшие тики */ }
impl Book {
    pub fn new() -> Book;
    pub fn clear(&mut self);                                      // и для снимка: стакан пустеет, уровни приходят через set
    pub fn set(&mut self, side: BookSide, tick: u16, size: f64);  // объём уровня — новое значение; 0 убирает уровень
    pub fn trim(&mut self, best_bid: f64, best_ask: f64);         // paper Book.trim: уровни лучше цен сервера убираются
    pub fn best_bid(&self) -> f64;                                // 0.0, если bids пусты
    pub fn best_ask(&self) -> f64;                                // 1.0, если asks пусты
    pub fn asks_upto(&self, limit: f64, out: &mut Vec<(f64, f64)>);   // от дешёвых; out очищается
    pub fn matches(&self, best_bid: f64, best_ask: f64) -> bool;  // сверка §5.2: обе стороны совпадают с точностью 1e-9
}
```
- [ ] **Step 1: Написать падающие тесты**
```rust
#[test] fn books_behave_like_paper_book() { /* перенос тестов Book из paper/tests/test_book_fills.py */ }
#[test] fn empty_sides_read_as_0_and_1() {}
#[test] fn asks_upto_is_cheapest_first_and_stops_at_the_limit() {}
proptest! {
    #[test] fn book_equals_a_naive_model(ops in /* случайные снимки, set, trim */) {
        // модель на BTreeMap: best_bid, best_ask и asks_upto(0.88) совпадают после каждого шага (§10)
    }
}
```
- [ ] **Step 2–4:** тесты падают → реализация → `cargo test -p v21-live --test book` → PASS.
- [ ] **Step 5: Коммит.**

---

### Task 7: Разбор кадров Polymarket и события ленты

**Files:**
- Create: `crates/live/src/hot/{mod,wire,items}.rs`, `crates/live/tests/pm_items.rs`
- Create: `crates/live/tests/data/feed_vectors.json` — копия из `pmbot/tests/data/`
- Create: `crates/live/tests/data/bbo_vectors.json.gz`
- Create (инструменты): `tools/record_feeds.py`, `tools/make_bbo_vectors.py`

**Инструменты:**
- **`tools/record_feeds.py`** — записывающий скрипт 19.10, лежит в папке сессии: `scratchpad/record_feeds.py`. Если его
  там нет, написать заново:
  - две копии ленты на 6 токенов текущего окна и 6 следующего, плюс зеркало Binance;
  - кадры копятся в памяти и сбрасываются на диск раз в секунду;
  - при разрыве — переподключение.
- **`tools/make_bbo_vectors.py --root $V21_ROOT --rec FILE --out FILE`** берёт 3 000 кадров копии a и записывает ответы
  `paper/feeds.parse_ws`.

**Interfaces:**
- Consumes: `BookSide`, `grid_tick` (задача 6).
- Produces:
```rust
// hot/wire.rs — перенос pmbot/src/hot/wire.rs с тестами модуля; добавлено: Change.best_bid (0.0, если нет), строки bids у book.
// read и Frame — pub: их зовут интеграционные тесты (здесь и в задаче 16)
pub fn read(raw: &[u8], tokens: &[Box<str>], frame: &mut Frame) -> Result<(), serde_json::Error>;
// hot/items.rs
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FeedItem {
    /// Строка архива bbo (§5.2): после снимка — его лучшие цены, после изменения — best_bid / best_ask сервера.
    Bbo { token: u8, ts_ms: i64, bid: f64, ask: f64 },
    /// Снимок: стакан токена заменяется уровнями SnapLevel, которые идут следом.
    Snapshot { token: u8, ts_ms: i64 },
    SnapLevel { token: u8, side: BookSide, tick: u16, size: f64 },
    /// Изменение уровня. tick None — цена вне сетки: уровень пропускается, обрезка по лучшим ценам остаётся.
    Level { token: u8, ts_ms: i64, side: BookSide, tick: Option<u16>, size: f64, best_bid: f64, best_ask: f64 },
    TickSize { token: u8, ts_ms: i64, tick: f64 },
}
/// Добавляет события кадра в out; возвращает число нечитаемых частей.
pub fn frame_items(frame: &Frame, out: &mut Vec<FeedItem>) -> u32;
```
**Правила** — как `paper/feeds.parse_ws`; токены — только из списка подписки:
- **`book`:**
  - даёт `Snapshot`, затем `SnapLevel` на каждый уровень с объёмом > 0;
  - затем `Bbo`: bid — максимум цен с ненулевым объёмом (0.0, если таких нет), ask — минимум (1.0);
  - если в снимке есть цена вне сетки, снимок не берётся: нет `Snapshot` и `SnapLevel`, но `Bbo` есть, и это
    нечитаемая часть.
- **Изменение `price_change`:** `Level` (BUY — `Bid`, SELL — `Ask`), затем `Bbo` с его `best_bid` (0.0, если нет) и
  `best_ask` (1.0).
- **Нечитаемое** — как в `pmbot/src/hot/feed.rs`: считается и пропускается. PONG и пустой кадр нечитаемыми не
  считаются.

- [ ] **Step 1: Сделать эталон и написать падающие тесты**
```rust
#[test] fn ask_ladders_match_the_scanner_on_real_messages() {
    // feed_vectors.json, 2 425 настоящих сообщений: события → Book (снимок, Level, trim по лучшим ценам);
    // в каждом сообщении, где сканер прислал обновление лестницы, самые дешёвые `levels` asks == его лестница
}
#[test] fn bbo_matches_paper_parse_ws_on_real_frames() { /* bbo_vectors.json.gz: Bbo по порядку == строки parse_ws (токен, ts, bid, ask) */ }
#[test] fn a_book_gives_snapshot_levels_then_its_bbo() {}
#[test] fn a_price_change_gives_a_level_then_its_bbo_with_defaults_0_and_1() {}
#[test] fn an_off_grid_snapshot_is_not_taken_but_its_bbo_is() {}
#[test] fn not_json_gives_no_items_and_pong_is_not_unreadable() {}
#[test] fn tokens_outside_the_list_give_no_items() {}
```
- [ ] **Step 2–4:** тесты падают → реализация → `cargo test -p v21-live --test pm_items` и тесты модуля `wire` → PASS.
- [ ] **Step 5: Коммит.**

---

### Task 8: Отсев дубликатов двух копий ленты

**Files:**
- Create: `crates/live/src/hot/copies.rs`, `crates/live/tests/dedupe.rs`
- Create: `crates/live/tests/data/two_copies.jsonl.gz` — 30 с обеих копий из записи 19.10. Отрезок выбирается так,
  чтобы в нём были поздние события (их находит `analyze_more.py` из папки сессии); он нужен и задачам 12 и 16.

**Interfaces:**
- Produces:
```rust
pub struct FrameDedupe { /* открытая адресация, таблица фиксированного размера: xxh3 → повторы по копиям, принято, время */ }
impl FrameDedupe {
    pub fn new(capacity: usize, copies: u8, ttl_ms: i64) -> FrameDedupe;   // в боте: 32 768, 2, 3 000
    pub fn admit(&mut self, copy: u8, raw: &[u8], now_ms: i64) -> bool;    // true — первое появление этого вхождения кадра
    pub fn dropped(&self) -> u64;
}
```
**Правила:**
- **Принятие кадра.** n-е появление одинаковых байтов в копии `c` принимается, если ни одна копия ещё не приносила
  n-е появление этих байтов.
- **Устаревание.** Запись старше `ttl_ms` считается свободной.
- **Переполнение.** Если в пределах пробы свободной записи нет, кадр принимается и считается: дубликат,
  применённый дважды, безвреден, а потерянный кадр — нет.

- [ ] **Step 1: Написать падающие тесты**
```rust
#[test] fn the_second_copy_of_a_frame_is_dropped() {}
#[test] fn identical_frames_repeated_inside_a_copy_are_kept() { /* A,A от копии 0 и A,A от копии 1 → принято 2 */ }
#[test] fn a_frame_only_one_copy_had_is_kept() {}
#[test] fn a_full_table_admits_rather_than_drops() {}
#[test] fn merged_stream_of_the_recording_equals_each_copy_per_market() {
    // two_copies.jsonl.gz: склейка по времени прихода через admit() == кадры копии a; порядок внутри рынка тот же
}
#[test] fn no_allocation_after_warm_up() { /* считающий аллокатор, как pmbot/tests/no_alloc.rs */ }
```
- [ ] **Step 2–4:** тесты падают → реализация → `cargo test -p v21-live --test dedupe` → PASS.
- [ ] **Step 5: Коммит.**

---

### Task 9: Соединения с лентой Polymarket

**Files:**
- Create: `crates/live/src/hot/ws.rs`, `crates/live/tests/support/{mod,fake_pm}.rs`, `crates/live/tests/pm_ws.rs`

**Interfaces:**
- Produces:
```rust
pub enum CopyEvent { Frame { copy: u8, raw: bytes::Bytes }, Up { copy: u8 }, Down { copy: u8, code: Option<u16>, reason: String } }
pub struct PmCopy { .. }
impl PmCopy {
    pub fn new(url: &str, assets: &[String], copy: u8) -> PmCopy;
    pub async fn next(&mut self) -> CopyEvent;   // отмена безопасна: стоит в select! с другой копией, Binance и таймером
}
```
**Поведение** — как `paper/feeds.MarketStream`:
- подписка `{"assets_ids": [...], "type": "market"}`;
- текстовый `PING` раз в 10 с; молчание 30 с — разрыв;
- переподключение: пауза 0.1 с, дальше ×2 до 5 с;
- каждое закрытие, в том числе 1013, — `Down` с кодом; каждое подключение — `Up`;
- чтение ничего другого не ждёт: медленного читателя биржа закрывает.

**Поддельный сервер** (`fake_pm`) раздаёт кадры по сценарию: момент по часам биржи, маска копий, задержка копии,
закрытие с кодом. Задача 18 переиспользует его.

- [ ] **Step 1: Написать падающие тесты**
```rust
#[tokio::test] async fn subscribes_with_the_window_tokens_and_pings_every_10_s() { /* часы теста ускорены: tokio::time::pause */ }
#[tokio::test] async fn reconnects_after_close_1013_and_reports_the_code() {}
#[tokio::test] async fn thirty_seconds_of_silence_break_the_connection() {}
#[tokio::test] async fn next_is_cancel_safe() { /* в цикле select! с таймером 1 мс не теряется ни один кадр */ }
```
- [ ] **Step 2–4:** тесты падают → реализация → `cargo test -p v21-live --test pm_ws` → PASS.
- [ ] **Step 5: Коммит.**

---

### Task 10: Binance: поток сделок, дыры, свечи и время

**Files:**
- Create: `crates/live/src/hot/binance.rs`, `crates/live/src/cold/{mod,binance_rest}.rs`
- Create: `crates/live/tests/support/fake_binance.rs`, `crates/live/tests/binance.rs`

**Interfaces:**
- Produces:
```rust
// hot/binance.rs
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Trade { pub coin: Coin, pub ts_ms: i64, pub price: f64 }
pub fn parse_agg(raw: &[u8]) -> Option<Trade>;     // поток combined: data.s, data.T, data.p; без выделений памяти
pub enum BinanceEvent { Trade(Trade), Up { fallback: bool }, Down { reason: String } }
pub struct BinanceStream { .. }
impl BinanceStream {
    pub fn new(urls: [String; 2], coins: &[Coin]) -> BinanceStream;   // основной и запасной адреса
    pub async fn next(&mut self) -> BinanceEvent;                      // отмена безопасна
}
pub struct GapTracker { .. }      // перенос paper/feeds.BinanceFeed: on_trade, on_disconnect, очередь дозаполнения
impl GapTracker {
    pub fn new(coins: &[Coin]) -> GapTracker;
    pub fn on_trade(&mut self, t: &Trade) -> Option<(Coin, i64, i64)>;           // первая сделка после обрыва → дозаполнить q0..q1
    pub fn on_disconnect(&mut self, at_ms: i64, out: &mut Vec<(Coin, i64, i64)>); // дыры для mark_gap
}
// cold/binance_rest.rs — адреса по очереди: при ошибке или HTTP 451 — следующий
pub async fn klines(http: &reqwest::Client, bases: &[String], symbol: &str, q0: i64, q1: i64) -> anyhow::Result<Vec<(i64, f64)>>;   // страницы по 1000
pub async fn server_time_ms(http: &reqwest::Client, bases: &[String]) -> anyhow::Result<(i64, i64, i64)>;   // (время сервера, отправка, приход) по местным часам
```
**Поведение:**
- **Адреса потока.** Сначала основной адрес; при ошибке, в том числе HTTP 451 при рукопожатии, — запасной. После
  обрыва запасного снова пробуется основной.
- **Дыры и дозаполнение** — как `paper/feeds.BinanceFeed`:
  - обрыв — дыра от секунды последней сделки монеты до секунды обрыва;
  - первая сделка после переподключения ставит в очередь дозаполнение до секунды перед ней;
  - дозаполнение — свечами, до 5 попыток через 2 с;
  - если ответ кончается раньше `q1`, добавляется замыкающая строка (`paper` `backfill`).

- [ ] **Step 1: Написать падающие тесты**
```rust
#[test] fn parse_agg_reads_a_real_message() { /* сообщение из записи 19.10 */ }
#[tokio::test] async fn falls_back_when_the_primary_answers_451() {}
#[tokio::test] async fn returns_to_the_primary_after_the_fallback_drops() {}
#[test] fn a_drop_marks_a_gap_and_the_next_trade_asks_for_a_backfill() { /* перенос тестов BinanceFeed из paper/tests/test_feeds.py */ }
#[tokio::test] async fn klines_page_by_1000_and_fall_back_on_451() {}
#[tokio::test] async fn server_time_tries_the_fallback() {}
```
- [ ] **Step 2–4:** тесты падают → реализация → `cargo test -p v21-live --test binance` → PASS.
- [ ] **Step 5: Коммит.**

---

### Task 11: Gamma, настройки рынка, путь до биржи, поправка часов

**Files:**
- Create: `crates/live/src/cold/{gamma,clob_info,ntp}.rs`
- Create: `crates/live/tests/data/{gamma_event,gamma_closed,clob_market,clob_book}.json` — настоящие ответы,
  сохраняются при выполнении задачи
- Create: `crates/live/tests/support/{fake_gamma,fake_clob}.rs`, `crates/live/tests/{gamma,clock}.rs`

**Interfaces:**
- Consumes: `FeeSchedule` (задача 4).
- Produces:
```rust
// cold/gamma.rs — перенос paper/feeds.py: parse_market, parse_outcome, fetch_markets, fetch_outcomes
pub struct Market { pub coin: Coin, pub s: i64, pub slug: String, pub condition_id: String, pub up: String, pub down: String,
                    pub fee: FeeSchedule, pub rules_ok: bool, pub note: String }
pub fn parse_market(event: &serde_json::Value) -> anyhow::Result<Market>;
pub struct Outcome { pub payout_up: f64, pub ptb: Option<f64>, pub final_price: Option<f64> }
pub fn parse_outcome(event: &serde_json::Value) -> Option<Outcome>;
pub async fn fetch_window(http: &reqwest::Client, gamma: &str, s: i64, coins: &[Coin]) -> anyhow::Result<Vec<Market>>;   // уникальный параметр `_`
pub async fn fetch_outcomes(http: &reqwest::Client, gamma: &str, s: i64, coins: &[Coin]) -> anyhow::Result<Vec<(Coin, Outcome)>>;
// cold/clob_info.rs
pub struct MarketInfo { pub taker_delay: bool, pub tick: f64, pub min_size: f64, pub fee: FeeSchedule, pub taker_only: bool }
pub fn parse_market_info(v: &serde_json::Value) -> anyhow::Result<MarketInfo>;       // itode, mts, mos, fd
/// None — правила как в §7.4; иначе — какое правило не то.
pub fn rules_note(m: &Market, info: Option<&MarketInfo>) -> Option<String>;
pub async fn fetch_market_info(http: &reqwest::Client, clob: &str, condition_id: &str) -> anyhow::Result<MarketInfo>;
pub async fn fetch_book(http: &reqwest::Client, clob: &str, token: &str) -> anyhow::Result<(i64, Vec<(f64, f64)>, Vec<(f64, f64)>)>;   // (timestamp, bids, asks)
pub struct Rtt { .. }                                                                   // последние 20 замеров
impl Rtt { pub fn push(&mut self, ms: f64); pub fn median_ms(&self) -> Option<f64>; }
// cold/ntp.rs
pub async fn sntp_offset_ms(server: &str) -> anyhow::Result<(i64, i64)>;              // (поправка, путь): лучший по пути из 5 запросов
pub fn clock_offset_ms(server_ms: i64, sent_ms: i64, received_ms: i64) -> i64;        // время сервера − середина запроса
```
**Правила рынка** (§7.4):
- TWAP включён, окно 60 с;
- комиссия: ставка 0.07, степень 1, только тейкер;
- `itode` — `true`.

Если настроек CLOB нет, это «нет настроек рынка».

- [ ] **Step 1: Написать падающие тесты**
```rust
#[test] fn a_real_15m_market_parses_with_its_rules() { /* gamma_event.json: rules_ok, комиссия 0.07/1, токены Up и Down по outcomes */ }
#[test] fn a_changed_rule_is_named_in_the_note() { /* степень 2 → "exponent"; TWAP 30 с → "TWAP"; itode false → "задержка тейкера" */ }
#[test] fn outcomes_need_a_closed_market_and_final_prices() { /* перенос тестов parse_outcome из paper/tests/test_feeds.py */ }
#[test] fn market_info_reads_itode_tick_min_size_and_fees() { /* clob_market.json */ }
#[tokio::test] async fn fetch_window_asks_each_coin_with_a_cache_buster() {}
#[test] fn rtt_median_of_the_last_20() {}
#[test] fn clock_offset_is_the_server_minus_the_middle() {}
#[tokio::test] async fn sntp_offset_against_a_local_server() { /* UDP-сокет отвечает с часами +1042 мс → поправка 1042 ± 2 */ }
```
- [ ] **Step 2–4:** тесты падают → реализация → `cargo test -p v21-live --test gamma --test clock` → PASS.
- [ ] **Step 5: Коммит.**

---

### Task 12: Точный движок

**Files:**
- Create: `crates/live/src/shared/msgs.rs` (типы окна и выстрела), `crates/live/src/cold/exact.rs`
- Test: `crates/live/tests/exact.rs`

**Interfaces:**
- Consumes:
  - `FeedItem` (задача 7), `Book` (6), `LiveCloses` (3), `Trade` (10), `fak_buy` (4);
  - `CoinDetector`, `z_score`, `WindowEngine` и `reason_for` (2).
- Produces:
```rust
// shared/msgs.rs
pub struct CoinArm { pub coin: Coin, pub up_ix: u8, pub down_ix: u8, pub up_key: [u8; 32], pub fee: FeeSchedule,
                     pub tradable: bool, pub note: String }       // up_key — id токена Up, 32 байта big-endian, как в вехе 1
pub struct WindowArm { pub s: i64, pub tokens: Vec<String>, pub coins: Vec<CoinArm>, pub missing: Vec<Coin>,
                       pub resumed: bool,          // старт после 835-й секунды (задача 16)
                       pub traded: Vec<Coin> }     // монеты, уже купленные в этом окне: из state.json после перезапуска
#[derive(Clone, Copy, Debug)]
pub struct ShotRecord { pub s: i64, pub coin: Coin, pub side: Side, pub t_trig: f64, pub event_ts_ms: i64, pub send_ms: i64,
                        pub rank: u32, pub z: Option<ZInfo>, pub frame_to_decision_ns: u64 }
// cold/exact.rs
pub const GRID_MS: [i64; 13] = [0, 50, 100, 150, 200, 250, 300, 350, 400, 450, 500, 550, 600];
pub struct DemoTiming { pub stake_usd: f64, pub limit: f64, pub half_rtt_ms: f64, pub taker_delay_ms: f64 }
#[derive(Clone, Debug)]
pub struct DemoFill { pub match_ms: i64, pub at_match: Fill, pub no_latency: Fill, pub by_delay: Option<[Fill; 13]> }
pub struct ExactSignal { pub s: i64, pub check: Check, pub rank: Option<u32>, pub z: Option<ZInfo>, pub v2: Decision, pub v21: Decision,
                         pub lag_ms: i64, pub send_ms: i64, pub shot: bool, pub demo: Option<DemoFill> }
pub struct ShotFill { pub shot: ShotRecord, pub demo: DemoFill }        // by_delay = None: ответ нужен быстро
pub struct WindowSummary { pub s: i64, pub ws_ok: bool, pub binance_ok: bool, pub n_signals: u32, pub lag_max_ms: i64,
                           pub late_dropped: u32, pub book_reloads: u32, pub coverage_lost_ms: i64,
                           pub p0: Vec<(Coin, Option<f64>)>, pub bbo: Vec<(Coin, Vec<(i64, f64, f64)>)> }
pub enum ExactOut { Signal(ExactSignal), ShotFill(ShotFill), Reload { token: u8 }, WindowDone(WindowSummary) }
pub struct ExactEngine { .. }
impl ExactEngine {
    pub fn new(strategy: Strategy, hold_ms: i64, timing: DemoTiming) -> ExactEngine;
    pub fn arm(&mut self, arm: &WindowArm);
    pub fn on_item(&mut self, rx_ms: i64, item: FeedItem);
    pub fn on_rest_book(&mut self, token: u8, ts_ms: i64, bids: &[(f64, f64)], asks: &[(f64, f64)]);   // только стакан, без Bbo
    pub fn on_coverage_lost(&mut self, at_ms: i64);
    pub fn on_trade(&mut self, t: &Trade);
    pub fn on_klines(&mut self, coin: Coin, rows: &[(i64, f64)]);
    pub fn on_gap(&mut self, coin: Coin, q0: i64, q1: i64);
    pub fn on_shot(&mut self, shot: &ShotRecord);
    pub fn set_timing(&mut self, timing: DemoTiming);
    pub fn advance(&mut self, now_ms: i64, out: &mut Vec<ExactOut>);
}
```
**Правила.** Перенос `paper/engine.py`; ниже — только то, что отличается или добавлено.

1. **Буфер:**
   - граница отпуска `W = min(now_ms, самое новое пришедшее время) − hold_ms`;
   - события со временем ≤ `W` отпускаются по (время биржи, порядок прихода) — это порядок архива: устойчивая
     сортировка по времени;
   - событие старше последнего отпущенного события своего токена опоздало: оно отбрасывается, `late_dropped += 1`.
2. **Отпущенное событие** — в порядке `paper`:
   - `Bbo` токена Up идёт в детектор монеты, кроме токенов, которые после обрыва ждут свежего снимка;
   - события стакана идут в `Book`; `Snapshot` снимает ожидание снимка;
   - сверка стакана (§5.2) — после всех отпущенных событий с одним временем биржи. Каждый токен, изменённый `Level`,
     сравнивается с лучшими ценами последнего `Level` (`Book::matches`). Расхождение → `Reload` не чаще раза в 5 с
     времени биржи на токен. Это сверка `paper` по сообщению: лучшие цены изменения — состояние после всей пачки.
3. **Проверки:**
   - детекторы сбрасываются (`flush`) до `W`;
   - решения — в порядке ключа (`t_trig`, `up_key`), когда `W ≥ t_trig + lat` и ни у одной монеты нет идущей проверки
     с меньшим ключом (`paper` `_release`);
   - монеты из `missing` никого не задерживают;
   - решения v2 и v2.1 — два `WindowEngine` с `decide(&strategy.v2, …)` и `decide(&strategy.v21, …)`.
4. **Точный z** (§5.3):
   - считается, когда `is_final(t − 1, now_ms)`;
   - если `z_score` ещё `None` (дыра не дозаполнена), решение ждёт до `s + 1020 с`, затем z — `None` («нет данных
     Binance»);
   - решения ждут в порядке ключа.
5. **Демо-исполнение** (§6.2) считается для каждой удачной проверки и для каждой проверки с выстрелом.
   - **Момент отправки:** у выстрела — его `send_ms`. Иначе — позже из двух: `s + 840 000` и `rx_ms` события, начавшего
     проверку.
   - **Выстрел и проверка** — выстрел относится к проверке своей монеты с ближайшим `t_trig` в пределах ±0.3 с.
   - **Стакан на момент** — стакан купленного токена после всех событий со временем ≤ этого момента; цена —
     `fak_buy(asks_upto(0.88), stake_usd, 0.88, комиссия рынка)`.
   - **Моменты:**
     - исполнение: отправка + `half_rtt_ms` + `taker_delay_ms` → `at_match`;
     - стакан сразу после события, начавшего проверку → `no_latency`;
     - отправка + `GRID_MS[k]` → `by_delay`: по этой сетке история пересчитывается, когда задержка тейкера будет
       измерена (§6.2).
   - **`ShotFill`** уходит, как только `W` прошла момент исполнения: горячий поток ждёт ответа на выстрел.
6. **Обрыв обеих копий** (`paper` `_reset`):
   - метка встаёт в буфер на время `at_ms`; все токены ждут свежего снимка, стаканы чистятся, детекторы сбрасываются;
   - детектор, закончивший окно из-за отсутствия стакана на 840 с, начинается заново;
   - `ws_ok = false`, если обрыв на 835-й секунде или позже;
   - `coverage_lost_ms` копится, пока у всех токенов не будет свежего снимка;
   - `WindowArm.resumed` (старт посреди решающей минуты) действует так же, как обрыв в начале окна.
7. **Окно:**
   - паузы времени биржи между событиями монеты дольше 5 с в 840–870 с → `ws_ok = false`;
   - `lag_max_ms` — по событиям 840–870 с;
   - строки `Bbo` токенов Up в 835–871 с собираются для журнала;
   - `WindowDone` — когда `W ≥ s + 871 000` и все проверки решены.

- [ ] **Step 1: Написать падающие тесты**
```rust
#[test] fn archive_day_through_the_exact_engine_equals_the_replay() {
    // V21_ROOT, день 15.10.2026. Если в нём есть окно со строками в двух часовых файлах, взять другой день без таких окон
    // и записать дату в тест. Для каждого окна: секунды bn1s с s − 1000 по s + 900 → on_klines; строки bbo окна → FeedItem::Bbo
    // с rx = ts; advance до конца окна → сигналы, номера, решения v2 и v2.1 == replay_with этого дня; |Δz| ≤ 1e-6
}
#[test] fn late_events_are_put_back_in_time_order() { /* two_copies.jsonl.gz: порядок отпуска == устойчивая сортировка по ts; опоздавшее дальше буфера → late_dropped */ }
#[test] fn demo_fill_uses_the_book_at_the_match_moment() { /* уровни меняются между t_trig и t_trig + 0.26 с: at_match видит второй стакан, no_latency — первый */ }
#[test] fn the_delay_grid_has_13_fills_from_the_send() {}
#[test] fn a_check_without_a_shot_is_sent_at_840_or_at_its_arrival() {}
#[test] fn exact_z_waits_for_the_closed_second() { /* сделка секунды t−1 приходит после сигнала: z считается по ней */ }
#[test] fn decisions_wait_for_earlier_pending_checks_of_other_coins() {}
#[test] fn a_book_that_differs_from_the_server_asks_a_reload_once_per_5_s() {}
#[test] fn after_coverage_loss_changes_wait_for_a_fresh_snapshot() { /* Review Focus 2 */ }
```
- [ ] **Step 2–4:** тесты падают → реализация → `cargo test -p v21-live --test exact` → PASS.
- [ ] **Step 5: Коммит.**

---

### Task 13: Быстрый движок

**Files:**
- Create: `crates/live/src/hot/engine.rs`
- Modify: `crates/live/src/shared/msgs.rs` (`FastReason`, `FastSkip`, `FastOut`)
- Test: `crates/live/tests/fast.rs`

**Interfaces:**
- Consumes: `CoinDetector`, `rank_state` и `reason_for` (задача 2), `LiveCloses` (3), `FeedItem` (7), `Trade` (10),
  `WindowArm` и `ShotRecord` (12).
- Produces:
```rust
// shared/msgs.rs
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FastReason {
    Rule(Reason),             // причины вехи 1
    Paused(&'static str),     // «пауза: лента Polymarket молчит», «пауза: Binance молчит», причины clock_pause, «мало денег», «стоп демо-счёта»
    RulesChanged,             // «правила рынка изменились»
    StaleBook,                // «после обрыва ленты нет свежего снимка»
    RankUnknown,              // «номер монеты неизвестен» (нет рынка монеты окна)
    ShotLimit,                // «уже 2 выстрела в окне»
}
impl FastReason { pub fn label(&self, p: &Params) -> String; }
#[derive(Clone, Copy, Debug)]
pub struct FastSkip { pub s: i64, pub coin: Coin, pub side: Side, pub t_trig: f64, pub rank: Option<u32>, pub z: Option<ZInfo>, pub reason: FastReason }
#[derive(Clone, Copy, Debug)]
pub enum FastOut { Shot(ShotRecord), Skip(FastSkip), Wait { s: i64, coin: Coin, t_trig: f64 } }
// hot/engine.rs
pub struct FastEngine { .. }
impl FastEngine {
    pub fn new(p: Params, feed_silent_ms: i64) -> FastEngine;
    pub fn arm(&mut self, arm: &WindowArm);
    pub fn on_item(&mut self, now_ms: i64, item: &FeedItem, out: &mut Vec<FastOut>);
    pub fn on_trade(&mut self, now_ms: i64, t: &Trade);
    pub fn on_klines(&mut self, coin: Coin, rows: &[(i64, f64)]);
    pub fn on_gap(&mut self, coin: Coin, q0: i64, q1: i64);
    pub fn on_tick(&mut self, now_ms: i64, out: &mut Vec<FastOut>);
    pub fn on_coverage_lost(&mut self);
    pub fn on_fill(&mut self, coin: Coin, filled: bool);
    pub fn set_pause(&mut self, pause: Option<&'static str>);   // часы, деньги, стоп — от неспешного рантайма
}
```
**Правила** (§5.2, §5.3, §6.1):
1. **События** — в порядке прихода. `Bbo` старше последнего применённого события своего токена отбрасывается. После
   `on_coverage_lost` (и при `resumed`) `Bbo` от изменений не применяются, пока не придёт `Snapshot` токена.
2. **Решение** принимается в начале проверки: на `Bbo`, попавшем в коридор, а для состояния на 840 с — на тике
   840.000. Порядок:
   - **номер:** `rank_state` по проверкам других монет (`Ok`/`Failed`/`Pending`). Каждая наблюдаемая монета без рынка —
     `Pending`. `Wait` → решение откладывается до событий и тиков. Если проверка закрылась, а ответ всё ещё `Wait`, —
     `Skip(RankUnknown)`;
   - **причина стратегии:** `reason_for` с этим номером и быстрым z на своих `LiveCloses`. Если стратегия говорит
     «пропуск», это `Skip(Rule(..))`;
   - **иначе по очереди:** паузы, правила рынка, ожидание снимка, лимит выстрелов, деньги (`set_pause`) → `Skip` или
     `Shot`.
3. **Лимиты.** Один выстрел на монету и два на окно; монеты из `WindowArm.traded` уже куплены.
   - После выстрела монета ждёт `on_fill`; новая проверка этой монеты до ответа — `Wait`.
   - `on_fill(false)` при неудачной проверке открывает монету снова (таблица §6.1); в остальных случаях монета
     закончена.
4. **Тишина лент.** Если ленты Polymarket (`on_item`) или Binance (`on_trade`) нет дольше `feed_silent_ms` в 835–870 с —
   `Paused(..)`.
5. **Поля выстрела.** `send_ms` — `now_ms` решения. `frame_to_decision_ns` заполняет горячий цикл (задача 16).

- [ ] **Step 1: Написать падающие тесты**
```rust
#[test] fn the_book_at_840_fires_on_the_840_tick() {}
#[test] fn a_band_hit_mid_minute_fires_on_its_event() {}
#[test] fn a_third_coin_does_not_fire() {}
#[test] fn an_open_rank_waits_and_then_fires() {}
#[test] fn sol_counts_but_never_fires() {}
#[test] fn out_of_hours_and_low_z_do_not_fire() {}
#[test] fn an_old_event_does_not_change_the_state() { /* Review Focus 3 */ }
#[test] fn after_coverage_loss_no_shot_until_a_fresh_snapshot() { /* Review Focus 2 */ }
#[test] fn binance_silence_pauses_the_shots() { /* Review Focus 4 */ }
#[test] fn a_coin_without_a_market_lets_only_rank_1_fire() { /* Review Focus 5 */ }
#[test] fn a_changed_rule_blocks_the_coin_but_it_still_counts() { /* Review Focus 5 */ }
#[test] fn a_pause_from_the_cold_side_blocks_shots() { /* Review Focus 1: set_pause(Some("пауза: часы не измерены")) */ }
#[test] fn an_empty_fill_after_a_failed_check_rearms_the_coin() {}
#[test] fn a_band_hit_waits_for_the_answer_to_the_previous_shot() {}
#[test] fn fast_equals_exact_on_the_archive_for_successful_checks() {
    // V21_ROOT, день задачи 12, без задержек (now = ts события): у удачных проверок номер и причина v2.1 быстрого == точного
}
```
- [ ] **Step 2–4:** тесты падают → реализация → `cargo test -p v21-live --test fast` → PASS.
- [ ] **Step 5: Коммит.**

---

### Task 14: Журнал, состояние, CSV

**Files:**
- Create: `crates/live/src/cold/{journal,csv}.rs`, `crates/live/tests/journal.rs`

**Interfaces:**
- Consumes: `DemoAccount` (задача 4).
- Produces:
```rust
pub struct Journal { .. }
impl Journal {
    pub fn open(path: &Path) -> anyhow::Result<Journal>;
    pub fn append(&mut self, kind: &str, rec: serde_json::Value) -> anyhow::Result<()>;   // + kind, ts_utc; NaN и inf → null; fsync не реже раза в секунду
    pub fn records(&self, kind: &str) -> anyhow::Result<Vec<serde_json::Value>>;
}
#[derive(Serialize, Deserialize)]
pub struct WindowWait { pub coins: Vec<Coin>, pub done: Vec<Coin> }
#[derive(Serialize, Deserialize)]
pub struct State { pub account: DemoAccount, pub traded: Vec<(i64, Coin)>, pub unsettled: BTreeMap<i64, WindowWait>,
                   pub last_summary_date: Option<String>, pub started_utc: String, pub clock_offset_ms: Option<i64> }
pub fn save_state(path: &Path, st: &State) -> anyhow::Result<()>;                 // атомарно: .tmp → fsync → rename; 3 попытки
pub fn load_state(path: &Path, journal: &Journal, bank_usd: f64) -> anyhow::Result<State>;   // + сверка с журналом (paper Engine._reconcile)
pub fn rebuild_tables(journal: &Journal, dir: &Path) -> anyhow::Result<bool>;      // trades.csv, signals.csv, windows.csv; false — файл занят
```
**Записи журнала** — §8 плюс то, что нужно сверке и отчёту. Имена — как в `paper/journal.py`, где такое имя есть.

| kind | поля |
|---|---|
| `window` | s, markets_ok, rules_ok, ws_ok_840_870, binance_ok, n_signals, n_trades, note, feed_lag_max_ms, late_dropped, book_reloads, coverage_lost_ms, binance_fallback, p0_binance_btc / eth / sol |
| `window_result` | s, outcome_&lt;coin&gt;, official_ptb_&lt;coin&gt;, official_final_&lt;coin&gt; |
| `bbo` | s, coin, rows: `[[ts_ms, bid, ask], …]` — события токена Up в 835–871 с |
| `signal` | s, coin, side, t_trig_s, ask_at_trig, px, counted, rank, z, cushion_bp, sigma_bp, h_s, p0_binance, ef_binance, hour_ok, decision, reason, v2_decision, v2_reason, fast_decision, fast_reason, fast_rank, fast_z, fast_wait_ms, lag_ms, send_ms, shot, demo (`Fill`), demo_grid `[[offset_ms, shares, notional_usd, avg_price], …]`, price_no_latency |
| `shot` | s, coin, side, t_trig_s, event_ts_ms, send_ms, rank, z, frame_to_decision_ns |
| `demo` | s, coin, side, t_trig_s, match_ms, shares, notional_usd, fee_usd, avg_price, levels_used, unfilled_usd, price_no_latency — демо-исполнение выстрела |
| `entry` | trade_id (`"{s}-{coin}"`), s, coin, side, t_trig_s, rank, z, stake_usd, cost_usd, fee_usd, shares, avg_price, levels_used, unfilled_usd, price_no_latency, balance_after |
| `settle` | trade_id, outcome, payout_usd, pnl_usd, roi, balance_after, official_ptb, official_final, settled_utc |
| `health`, `alarm` | задача 15 |
| `error` | s (если есть), where, error |

**Таблицы CSV:**
- пересобираются из журнала целиком, UTF-8 с BOM;
- файл, занятый Excel, пропускается до следующего события — как `paper/journal.py`.

| Таблица | Колонки |
|---|---|
| `trades.csv` | trade_id, window_start_utc, coin, side, t_trig_s, rank, z, stake_usd, cost_usd, fee_usd, shares, avg_price, levels_used, unfilled_usd, px_backtest, price_no_latency, status, outcome, payout_usd, pnl_usd, roi, balance_after, settled_utc |
| `signals.csv` | window_start_utc, coin, t_trig_s, side, ask_at_trig, px, counted, rank, z, hour_ok, decision, reason, fast_decision, fast_reason, fast_rank, fast_z, lag_ms, demo_filled_usd, demo_shares, demo_avg_price, price_no_latency |
| `windows.csv` | window_start_utc, markets_ok, rules_ok, ws_ok_840_870, binance_ok, n_signals, n_trades, outcome_btc / eth / sol, official_ptb_btc / eth / sol, p0_binance_btc / eth / sol, feed_lag_max_ms, late_dropped, book_reloads, coverage_lost_ms, binance_fallback, note |

В `trades.csv`:
- `px_backtest` берётся из `signal` того же окна, монеты и `t_trig_s`;
- `status` — как в `paper`: open, win, loss или half.

- [ ] **Step 1: Написать падающие тесты**
```rust
#[test] fn records_are_strict_json_with_kind_and_time() { /* NaN → null, кириллица как есть */ }
#[test] fn a_trade_in_the_journal_missing_from_the_state_is_restored() { /* перенос paper/tests/test_broker_journal.py */ }
#[test] fn a_settlement_in_the_journal_is_applied_on_load() {}
#[test] fn tables_have_these_columns_and_a_bom() {}
#[test] fn trades_csv_takes_px_backtest_from_the_signal() {}
#[test] fn a_locked_csv_does_not_stop_the_journal() {}
```
- [ ] **Step 2–4:** тесты падают → реализация → `cargo test -p v21-live --test journal` → PASS.
- [ ] **Step 5: Коммит.**

---

### Task 15: Telegram и здоровье

**Files:**
- Create: `crates/live/src/cold/telegram.rs`, `crates/live/src/health.rs`, `crates/live/tests/support/fake_telegram.rs`,
  `crates/live/tests/telegram.rs`

**Interfaces:**
- Consumes: `Fill` (задача 4), `Secrets` (5).
- Produces:
```rust
// cold/telegram.rs — тексты §8 для режима demo; минус — знак U+2212, как в paper/notify.py
pub fn fmt_entry(coin: Coin, side: Side, s: i64, fill: &Fill, z: Option<f64>) -> String;
pub fn fmt_result(coin: Coin, side: Side, payout_per_share: f64, pnl: f64, balance: f64) -> String;
pub fn fmt_missed(coin: Coin, side: Side, s: i64) -> String;
pub struct DaySummary { pub date: String, pub n: u32, pub wins: u32, pub pnl: f64, pub balance: f64, pub signals: u32,
                        pub full_fill_pct: f64, pub mean_filled_usd: f64, pub price_vs_backtest_c: Option<f64>,
                        pub missed: u32, pub fast_ne_exact: u32, pub alarms: Vec<String> }
pub fn fmt_summary(d: &DaySummary) -> String;
pub fn fmt_alarm(text: &str) -> String;
pub struct Telegram { .. }      // очередь, 3 попытки; без токена — только лог; токен не пишется никуда (paper notify._safe)
impl Telegram {
    pub fn new(secrets: &Secrets, api: &str) -> Telegram;
    pub async fn send(&self, text: &str) -> bool;
}
// health.rs
#[derive(Clone, Copy, Default)]
pub struct Hist { /* корзины по степеням двойки, без выделений */ }
impl Hist { pub fn add(&mut self, v: u64); pub fn quantile(&self, q: f64) -> u64; pub fn max(&self) -> u64; }
#[derive(Clone, Copy, Default)]
pub struct HotHealth { pub frames: u64, pub dup_frames: u64, pub bad_parts: u64, pub trades: u64, pub dropped_msgs: u64,
                       pub decide_ns: Hist, pub pm_lag_ms: Hist, pub bn_lag_ms: Hist, pub copies_up: u8 }
pub fn health_record(h: &HotHealth, clock_offset_ms: Option<i64>, rtt_ms: Option<f64>, cpu_pct: Option<f64>) -> serde_json::Value;
```
**Тексты.** Входные данные теста:
- `fill` — 20 / 0.809 = 24.72188 шт. на $20.00 по цене 0.809;
- комиссия — `fee(24.72188, 0.809)` = $0.26740, стоимость позиции — $20.26740, баланс до входа — $100.

| Сообщение | Текст |
|---|---|
| вход | `🟢 BTC Down · окно 03:15–03:30 · демо: $20.00 по 0.809 → 24.72 шт. · z 1.70` |
| итог: выигрыш | `✅ BTC Down +$4.45 · баланс $104.45` |
| итог: проигрыш | `❌ BTC Down −$20.27 · баланс $79.73` |
| итог: 50/50 | `➖ BTC Down 50/50 −$7.91 · баланс $92.09` |
| упущенная сделка | `⚪ ETH Up · окно 03:15–03:30 · сигнал был, демо-исполнения нет` |
| тревога | `⚠️ <текст>` |

**Сводка** в `summary_utc`:
```
📊 Сводка за 19.10 (UTC) · демо
Сделок: 3 · выигрыши 3 / проигрыши 0 · P&L +$12.40 · баланс $112.40
Сигналов: 41 · ордер $20 исполнился полностью в 85% · в среднем $17.90 из $20
Цена демо − цена бэктеста: +0.4¢
Упущенные: 1 · быстрое ≠ точное: 0
⚠️ <тревога>                       ← по строке на каждую тревогу дня
```
**Запись `health`** — раз в минуту:
- задержки лент: p50 и p99 по Polymarket и Binance;
- внутренняя задержка «кадр → решение»: p50, p99, максимум;
- копии на связи, кадры и дубликаты, потерянные сообщения очереди;
- поправка часов, путь до биржи;
- CPU — из `/proc/self/stat` на Linux, иначе `null`.

- [ ] **Step 1: Написать падающие тесты**
```rust
#[test] fn message_texts_are_exact() { /* таблица и сводка выше на тех же входных данных */ }
#[test] fn the_token_never_reaches_the_log() { /* ошибка запроса с токеном в URL → в логе "***" */ }
#[tokio::test] async fn send_retries_and_gives_up_without_stopping() { /* fake_telegram: 500, 500, 200 → true; 500 ×3 → false */ }
#[test] fn hist_quantiles() {}
#[test] fn health_record_has_the_spec_fields() {}
```
- [ ] **Step 2–4:** тесты падают → реализация → `cargo test -p v21-live --test telegram` → PASS.
- [ ] **Step 5: Коммит.**

---

### Task 16: Потоки, очереди, расписание окон, демо-счёт

**Files:**
- Create: `crates/live/src/hot/{timer,mod}.rs`, `crates/live/src/cold/demo.rs`
- Modify: `crates/live/src/cold/mod.rs`, `crates/live/src/run.rs`, `crates/live/src/shared/msgs.rs` (`HotMsg`, `ColdMsg`),
  `crates/cli/src/main.rs` (`v21 run` запускает бота; Ctrl-C и SIGTERM ставят `stop`)
- Test: `crates/live/tests/{timer,demo_desk,no_alloc_hot}.rs`

**Interfaces:**
- Consumes: всё из задач 5–15.
- Produces:
```rust
// shared/msgs.rs
pub enum HotMsg { Feed { rx_ms: i64, item: FeedItem }, Trade(Trade), Gap { coin: Coin, q0: i64, q1: i64 }, Fast(FastOut),
                  Copy { copy: u8, up: bool, code: Option<u16> }, Coverage { lost: bool, at_ms: i64 },
                  Binance { up: bool, fallback: bool }, Health(HotHealth) }
pub enum ColdMsg { Arm(WindowArm), Klines { coin: Coin, rows: Vec<(i64, f64)> }, Fill { coin: Coin, filled: bool }, Pause(Option<&'static str>) }
// hot/timer.rs
pub struct WindowPlan { pub s: i64, pub gamma_at_ms: i64, pub connect_at_ms: i64, pub t840_ms: i64, pub close_at_ms: i64, pub resumed: bool }
/// Текущее окно, если до 870-й секунды ещё есть время, иначе следующее; обработанное окно не повторяется.
/// resumed — старт после 835-й секунды: окно начинается как после обрыва ленты.
pub fn window_plan(now_ms: i64, last_s: Option<i64>, open_s: i64, close_s: i64) -> WindowPlan;
// hot/mod.rs
pub fn spawn_hot(cfg: &Config, p: Params, to_cold: rtrb::Producer<HotMsg>, from_cold: rtrb::Consumer<ColdMsg>,
                 clock: Arc<ExchangeClock>, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<()>;
// cold/demo.rs — демо-счёт по быстрым решениям (§6.3)
pub enum DeskOut { Entry { pos: DemoPosition, balance: f64 }, Missed, Settled { id: String, coin: Coin, side: Side, payout: f64, pnl: f64, balance: f64 }, Stopped }
pub struct DemoDesk { .. }
impl DemoDesk {
    pub fn new(account: DemoAccount, stake_usd: f64) -> DemoDesk;
    pub fn on_shot_fill(&mut self, f: &ShotFill) -> DeskOut;                  // позиция, если куплено > 0
    pub fn on_outcome(&mut self, s: i64, coin: Coin, payout_up: f64) -> Vec<DeskOut>;
    pub fn pause(&self) -> Option<&'static str>;                              // «мало денег» или «стоп демо-счёта» → ColdMsg::Pause
    pub fn account(&self) -> &DemoAccount;
}
// cold/mod.rs, run.rs
pub async fn run_cold(cfg: Config, strategy: Strategy, secrets: Secrets, from_hot: rtrb::Consumer<HotMsg>, to_hot: rtrb::Producer<ColdMsg>,
                      clock: Arc<ExchangeClock>, stop: Arc<AtomicBool>) -> anyhow::Result<()>;
pub fn run(cfg: Config, secrets: Secrets, stop: Arc<AtomicBool>) -> anyhow::Result<()>;   // потоки и очереди; возврат после stop
```
**Горячий поток** — один `tokio::select!` по двум копиям, Binance и таймеру; состояние — одна структура потока:
- **закрепление:** за `cores.hot`, если `cores.pin`;
- **кадр:** `FrameDedupe` → `wire::read` → `frame_items` → `FastEngine::on_item` и `HotMsg::Feed` на каждое событие;
- **Binance:**
  - сделка → `FastEngine::on_trade` и `HotMsg::Trade`;
  - `GapTracker` → `FastEngine::on_gap` и `HotMsg::Gap`;
- **копии:**
  - подключение — в `max(connect_at, приход Arm)`, отключение — в `close_at`;
  - ни одной копии на связи → `FastEngine::on_coverage_lost` и `HotMsg::Coverage`;
- **таймер:** раз в 50 мс в 835–872 с и ровно в 840.000 по `ExchangeClock` → `FastEngine::on_tick`; очередь от
  неспешного рантайма читается на каждом тике;
- **`FastOut` → `HotMsg::Fast`;** `frame_to_decision_ns` — от `Instant` прихода кадра;
- **раз в минуту** → `HotMsg::Health`;
- **очередь к неспешному полна** (ёмкость 65 536) → сообщение отбрасывается и считается в `HotHealth.dropped_msgs`;
  горячий поток не ждёт.

**Неспешный рантайм:**
- **Часы.**
  - Раз в 10 минут — NTP (`sntp_offset_ms`); без него — Binance `/api/v3/time` через `clock_offset_ms`.
  - Результат → `ExchangeClock::set_offset_ms`.
  - `clock_pause` → `ColdMsg::Pause`; начало паузы — тревога.
- **Путь до биржи.**
  - `GET {clob}/time` по одному прогретому соединению раз в `warm_interval_s` → `Rtt`.
  - `ExactEngine::set_timing`: `half_rtt_ms` — медиана / 2; до первого замера — 6 мс.
- **Binance.**
  - Разогрев: свечи за `binance_warmup_s` в оба движка (`ColdMsg::Klines` и `on_klines`).
  - Дозаполнение — на каждый `HotMsg::Gap`.
- **Окно.**
  - Gamma и настройки CLOB → `WindowArm`: `tradable` — когда `rules_note` равен `None`; монеты без рынка — в
    `missing`; `resumed` — из `window_plan`; `traded` — из `State.traded`.
  - Затем `ExactEngine::arm` и `ColdMsg::Arm`.
- **Точный движок.**
  - `advance` раз в 50 мс в 835–900 с, иначе раз в секунду.
  - `Reload` → `fetch_book` → `on_rest_book`.
- **Пары решений.** Быстрое решение ищется для каждого `ExactSignal` по (s, монета, `t_trig` ± 0.3 с) — оба идут в
  запись `signal`.
- **Демо-счёт.**
  - `ShotFill` → `DemoDesk::on_shot_fill` → записи `demo` и `entry`, `ColdMsg::Fill`, Telegram.
  - Упущенная сделка: точное решение v2.1 — «вход», а демо-исполнение пустое → `fmt_missed`.
- **Исходы** — по расписанию из Global Constraints → `DemoDesk::on_outcome` → `settle` и `window_result`, Telegram.
- **Файлы и сводка.**
  - Сводка — в `summary_utc`; `health` — раз в минуту.
  - `state.json` — после каждого изменения счёта.
  - Таблицы — после каждого окна и расчёта.
- **Тревоги:**
  - обе копии недоступны дольше 10 с;
  - начало паузы: часы, тишина ленты в решающую минуту, правила рынка;
  - стоп демо-счёта;
  - рост `dropped_msgs`.

- [ ] **Step 1: Написать падающие тесты**
```rust
#[test] fn window_plan_times() {
    // now = s+300 → gamma s+540, connect s+600, t840 s+840, close s+880, resumed false;
    // now = s+851 → то же окно, resumed true; now = s+871 → следующее окно; last_s = s → следующее окно
}
#[test] fn the_desk_opens_a_position_only_for_a_fill_and_stops_when_money_runs_out() {}
#[test] fn the_desk_settles_by_the_official_outcome() {}
#[test] fn the_hot_path_allocates_nothing_after_warm_up() {
    // считающий аллокатор (как pmbot/tests/no_alloc.rs): 10 000 кадров two_copies.jsonl.gz и сделки Binance через
    // FrameDedupe → wire::read → frame_items → FastEngine → rtrb; первые 1 000 кадров — прогрев, дальше 0 выделений
}
```
- [ ] **Step 2–4:** тесты падают → реализация → `cargo test -p v21-live --test timer --test demo_desk --test no_alloc_hot` → PASS.
- [ ] **Step 5: Коммит.**

---

### Task 17: Отчёт, сверка с архивом, проверка связи

**Files:**
- Create: `crates/live/src/{report,reconcile,check}.rs`, `crates/live/tests/{report,reconcile,check}.rs`
- Modify: `crates/live/Cargo.toml` (+ `v21-replay`), `crates/cli/src/main.rs`:
  - `v21 report --journal FILE [--from YYYY-MM-DD] [--to YYYY-MM-DD]`;
  - `v21 reconcile --journal FILE [--root DIR]`;
  - `v21 check [--config FILE]`.

**Interfaces:**
- Consumes: `Journal` (задача 14); из `v21-replay` — `load_windows`, `read_bbo_file`, `load_closes`, `replay_with`,
  `Incidents`.
- Produces:
```rust
// report.rs (§8)
pub fn report(journal: &Journal, strategy: &Strategy, from: Option<i64>, to: Option<i64>) -> String;
// reconcile.rs — перенос paper/reconcile.py поверх v21-replay
pub struct ReconcileRow { pub s: i64, pub coin: Coin, pub status: &'static str /* both | live_only | archive_only */,
                          pub side_eq: bool, pub rank_eq: bool, pub exact_eq: bool, pub fast_eq: bool,
                          pub dt_trig_ms: Option<f64>, pub dpx: Option<f64>, pub dz: Option<f64>,
                          pub bbo_same: u32, pub bbo_live_only: u32, pub bbo_archive_only: u32, pub incident: bool }
pub fn reconcile(journal: &Journal, root: &Path, strategy: &Strategy) -> anyhow::Result<Vec<ReconcileRow>>;
pub fn reconcile_summary(rows: &[ReconcileRow]) -> String;
// check.rs (§4.4, часть без ключей)
pub async fn check(cfg: &Config) -> (String, i32);   // текст и код выхода: 0 — в порядке, 1 — предупреждения, 2 — работать нельзя
```
**Отчёт** (режим demo). Строки в таком порядке:
```
Отчёт v21bot · демо · 15.10–19.10.2026 (UTC)
Сигналов базового правила: 41 · ордер $20 исполнился полностью в 85% · в среднем $17.90 из $20
Цена демо − цена бэктеста: +0.4¢ · без задержки − бэктест: −0.2¢
Упущенные сделки: 1 · быстрое ≠ точное: 0
Задержка тейкера в демо: 250 мс (по документации; живых ордеров ещё нет)
P&L демо-счёта: +$12.40 · баланс $112.40 · сделок 3, выигрышей 3
Бэктест-эквивалент v2.1: сделок 4 · доход на сделку +9.1% (проскальзывание 1.4¢)
Проверка на 100 сделках: 3/100 · вин-рейт 100%
```
Ниже — по строке на каждый случай «быстрое ≠ точное».

Бэктест-эквивалент — точные решения v2.1 «вход» с известным исходом; доходность — `trade_return` вехи 1.

**`reconcile`:**
- **Окна:** из журнала, которые уже есть в архиве; данные — функциями `v21-replay`.
- **Для каждого окна и монеты:**
  - точное решение v2.1 и быстрое против `replay_with`;
  - строки `bbo` 835–871 с против строк архива.
- **Сбои:** окна из `INCIDENTS.json` помечаются.
- **Итог:** CSV `reconcile_YYYYMMDD.csv` рядом с журналом и сводка на экран.

**`check`:**
- **Геоблок:** закрыто → код 2.
- **Часы:** поправка по NTP и по Binance; больше `clock_max_offset_ms` → 1.
- **Путь до CLOB:** 10 запросов `/time`, медиана.
- **Binance:** основной и запасной адреса, первая сделка и её задержка. Основной недоступен, а запасной работает → 1;
  недоступны оба → 2.
- **Polymarket:** обе копии 30 с на токенах текущего окна — кадры, доля дубликатов, выигрыш первой копии.
- **Рынки текущего окна в Gamma и их правила:** правила не те → 1.

- [ ] **Step 1: Написать падающие тесты**
```rust
#[test] fn report_of_a_small_journal() { /* журнал из 3 окон: ровно строки выше */ }
#[test] fn reconcile_of_a_journal_made_from_the_archive_is_clean() { /* V21_ROOT: журнал точного движка на дне задачи 12 → все both, все _eq true */ }
#[test] fn reconcile_names_a_changed_decision_and_a_missing_signal() {}
#[tokio::test] async fn check_against_fake_servers_is_0_and_a_geoblock_is_2() {}
```
- [ ] **Step 2–4:** тесты падают → реализация → `cargo test -p v21-live --test report --test reconcile --test check` → PASS.
- [ ] **Step 5: Коммит.**

---

### Task 18: Сквозные тесты демо-режима

**Files:**
- Create: `crates/live/tests/e2e.rs`, `crates/live/tests/support/scenario.rs`

**Стенд:**
- **Поддельные серверы** — из `tests/support`: Polymarket, Binance (поток, свечи, `/api/v3/time`, ответ 451 по
  заказу), Gamma, CLOB, Telegram.
- **Бот** — `run(cfg, secrets, stop)` в отдельном потоке:
  - конфиг — во временной папке, `[urls]` указывают на поддельные серверы;
  - `ntp_server = ""`, `clock_max_offset_ms = 10^13`, `cores.pin = false`.
- **Часы.** Поддельный `/api/v3/time` отдаёт время со сдвигом: бот видит время биржи `s + 830 с`, где
  `s = 1 790 910 900` (03:15 UTC — торговые часы v2.1). Так все сценарии проверяют и поправку часов (Review Focus 1).
- **Сценарий** кончается, когда в журнале появились ожидаемые записи, или через 120 с — тогда тест падает.
- **Время и запуск.** Сценарий идёт около 75 с; сценарии идут параллельно. Все они `#[ignore]` и запускаются командой
  `cargo test -p v21-live --test e2e -- --ignored`.

**Сценарии** (демо-часть §10):

| Тест | Что происходит | Что проверяем |
|---|---|---|
| `shot_at_840_and_a_mid_minute_signal` | BTC в коридоре к 840 с, ETH входит в 842.3, SOL вне коридора | выстрелы BTC на 840.0 и ETH на 842.3; `demo`, `entry`, `signal` (быстрое = точное = «вход»); после исхода — `settle`, строки `trades.csv`, сообщения входа и итога |
| `failed_check_then_a_second_shot` | BTC: коридор в 841.0, ask 0.90 с 841.2, снова коридор в 843.0 | первое демо-исполнение пустое, монета открыта снова, второй выстрел исполнен |
| `third_coin_and_waiting_for_the_rank` | SOL удачна с 840.0; BTC входит в 840.1, ETH — в 840.2 | BTC стреляет (номер 1); ETH ждёт номер и пропускается как третья монета; точное решение то же |
| `one_copy_drops_mid_minute` | копия 0 закрыта кодом 1013 в 845 с | лента не прерывается; выстрел в 846 с есть; `ws_ok_840_870` true |
| `both_copies_drop_for_11_s` | обе копии закрыты с 845 по 856 с | выстрелов нет до свежего снимка; окно помечено; тревога |
| `partial_fill_and_a_missed_trade` | BTC: под 0.88 только $12. ETH: ask 0.85 с 840.0, 0.90 с 840.20, снова 0.85 с 840.28; исполнение — около 840.25 | у BTC частичное исполнение; у ETH проверка удачна, а демо-исполнение пустое — «упущенная» в журнале и в Telegram |
| `money_below_the_stake_stops_the_demo_account` | `demo_bank_usd = 20.40` | выстрела нет, «стоп демо-счёта», тревога |
| `binance_primary_451_uses_the_fallback` | основной адрес отвечает 451 | z считается по зеркалу, выстрел есть, `binance_fallback` true |
| `restart_mid_window_restores_from_the_journal` | выстрел BTC на 840.0, остановка в 850 с, новый запуск; BTC снова в коридоре в 860, ETH — в 862 | позиция BTC восстановлена, второго выстрела BTC нет, ETH стреляет, расчёт после исхода |

- [ ] **Step 1: Написать сценарии** — тесты таблицы.
- [ ] **Step 2: Убедиться, что они падают там, где поведение ещё не доделано:**
  `cargo test -p v21-live --test e2e -- --ignored` → FAIL.
- [ ] **Step 3: Исправить найденное.** Каждое исправление идёт в модуль, где ошибка, вместе с модульным тестом.
- [ ] **Step 4: Убедиться, что всё проходит:** `cargo test --workspace` и
  `cargo test -p v21-live --test e2e -- --ignored` → PASS.
- [ ] **Step 5: Коммит.**

---

### Task 19: Сервер и ночь демо (ворота вехи 2)

**Files:**
- Create: `deploy/v21bot.service`, `deploy/README.md`
- Modify: `README.md` — веха 2: запуск демо, отчёт, сверка, покрытие

**`deploy/README.md`** — по-русски, по шагам (§11):
1. Копия репозитория `V2.1Poly` на сервер.
2. Rust через rustup; сборка `RUSTFLAGS="-C target-cpu=native" cargo build --release`; файл `v21` — в `/opt/v21bot/`.
3. chrony.
4. `/etc/v21bot/v21bot.toml` из примера и `secrets.toml` с Telegram, права 600.
5. `strategy_dir` — папка с `strategy_v2.json` и `strategy_v2_1.json`:
   - `/root/15mbacktest`, если копия исследований там есть;
   - иначе — `/opt/v21bot/strategy` с файлами из `crates/core/tests/data/`.

   Хеш проверяет сам бот.
6. `v21 check`.
7. Служба: `systemctl enable --now v21bot`; логи — `journalctl -u v21bot -f`.
8. Обновление и остановка.
9. Утром — `v21 report`; копия журнала на ПК для `v21 reconcile`, когда архив догонит ночь.

Рекомендация из §11: `CPUAffinity=0` в `scanner.service` — только с согласия пользователя.

**Служба:** `Restart=always`, `KillMode=mixed`, `ExecStart=/opt/v21bot/v21 run --config /etc/v21bot/v21bot.toml`.

- [ ] **Step 1: Покрытие.** `cargo llvm-cov --workspace --summary-only` с `V21_ROOT`:
  - ядро — не меньше 95%, весь код — не меньше 85%;
  - непокрытые строки — в README поимённо.
- [ ] **Step 2: Проверка дома.**
  - `v21 check`, затем час `v21 run` на ПК пользователя с `clock_max_offset_ms = 5000`: часы ПК сбиты на ~1 с,
    поправка их исправляет.
  - В журнале есть окна, сигналы и демо-исполнения; записей `error` нет.
- [ ] **Step 3: Коммит, финальное ревью вехи, пуш в `v21poly main`.**
- [ ] **Step 4: Ворота.** Пользователь ставит бота на сервер по `deploy/README.md`. Ворота пройдены, если:
  - демо работает одну торговую ночь (00–07 UTC) без записей `error`;
  - утром `v21 report` готов;
  - когда архив догонит ночь, `v21 reconcile` не находит расхождений точных решений, кроме помеченных окон с
    пропусками ленты.

  Решение принимает пользователь, бот сам ничего не делает.

---

## После вехи 2

- **Веха 3 — боевой режим:**
  - **ордер:** подпись Deposit Wallet заранее, своя отправка FAK;
  - **горячий поток:** опрос сокетов без сна в решающую минуту;
  - **после выстрела:** ответ биржи и канал «мои сделки», баланс pUSD;
  - **риск:** пределы, паузы (техработы биржи), стопы (§7.5), файл `STOP`, команды Telegram, погашение через
    ретранслятор;
  - **команды:** `setup`, часть `check` с ключами, `test-order`, `bench`;
  - **сквозные сценарии боевого режима.**

  Демо вехи 2 работает на сервере всё это время.
- **Веха 4 — запуск на реальные деньги** (§14).
