# v21bot на сервере: демо-режим (веха 2)

Сервер — QuantVPS в Дублине, Ubuntu 24.04, тот же, где работает сканер. Все команды — от root. Бот в вехе 2
ордеров не ставит: он смотрит ленты, принимает оба решения и ведёт демо-счёт на $100. Ключи биржи ему не нужны.

## 1. Копия репозитория

```bash
apt update && apt install -y git build-essential pkg-config chrony
git clone https://github.com/MRPhillip007/V2.1Poly.git /opt/src/V2.1Poly
```

## 2. Rust и сборка

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
cd /opt/src/V2.1Poly/v21bot
RUSTFLAGS="-C target-cpu=native" cargo build --release
mkdir -p /opt/v21bot
cp target/release/v21 /opt/v21bot/v21
```

Сборка нужна именно на сервере: `target-cpu=native` подгоняет код под его процессор.

## 3. Часы

```bash
systemctl enable --now chrony
chronyc tracking
```

В выводе `System time` должен быть меньше 0.05 с. Бот сам меряет поправку по NTP (`time.google.com`), а без неё — по
Binance. Если поправка больше `clock_max_offset_ms` (50 мс) или не измерена — пауза, выстрелов нет, в Telegram
тревога.

## 4. Конфиг и секреты

```bash
mkdir -p /etc/v21bot
cp /opt/src/V2.1Poly/v21bot/v21bot.example.toml /etc/v21bot/v21bot.toml
cp /opt/src/V2.1Poly/v21bot/secrets.example.toml /etc/v21bot/secrets.toml
chmod 600 /etc/v21bot/secrets.toml
nano /etc/v21bot/secrets.toml
```

В `secrets.toml` впиши только `telegram_token` и `telegram_chat_id` — ключи вехи 3 оставь пустыми. Права файла
должны быть 600: иначе бот не запустится и скажет почему. Значения секретов не попадают в журнал, логи и Telegram.

В `v21bot.toml` для демо менять ничего не нужно: `mode = "demo"`, ставка $20, банк $100.

## 5. Файлы стратегии

`strategy_dir` в `v21bot.toml` — папка с `strategy_v2.json` и `strategy_v2_1.json`:

- **если на сервере есть копия исследований `/root/15mbacktest`** — оставь `strategy_dir = "/root/15mbacktest"`;
- **иначе** — положи файлы из репозитория рядом с ботом:

  ```bash
  mkdir -p /opt/v21bot/strategy
  cp /opt/src/V2.1Poly/v21bot/crates/core/tests/data/strategy_v2*.json /opt/v21bot/strategy/
  sed -i 's|^strategy_dir = .*|strategy_dir = "/opt/v21bot/strategy"|' /etc/v21bot/v21bot.toml
  ```

Хеш файлов бот проверяет сам: с другими файлами он не запустится.

## 6. Проверка связи

```bash
/opt/v21bot/v21 check --config /etc/v21bot/v21bot.toml
```

Около минуты: геоблок, часы, путь до биржи, Binance (основной адрес и зеркало), обе копии ленты Polymarket на
рынках текущего окна, правила рынков. Последняя строка — итог, код выхода:

| Код | Итог |
|---|---|
| 0 | можно работать |
| 1 | есть предупреждения — они названы выше; бот работать будет, но с паузами |
| 2 | работать нельзя (API Polymarket закрыт для страны, нет биржи, нет Binance, нет ленты) |

Ирландия у Polymarket закрыта только на сайте: API, через который бот ставит ордера, открыт
([документация Polymarket](https://docs.polymarket.com/developers/CLOB/geoblock), «Close-Only on Frontend»; так же
Нидерланды, Япония, Южная Корея, Мальта). Поэтому из Дублина `check` пишет «геоблок: сайт закрыт, API открыт (IE)», и
это не мешает.

## 7. Служба

```bash
cp /opt/src/V2.1Poly/v21bot/deploy/v21bot.service /etc/systemd/system/v21bot.service
systemctl daemon-reload
systemctl enable --now v21bot
journalctl -u v21bot -f
```

Файлы бота — в `/var/lib/v21bot/`:

- `journal.jsonl` — все события;
- `state.json` — состояние для перезапуска;
- `trades.csv`, `signals.csv`, `windows.csv` — таблицы, пересобираются из журнала.

Служба перезапускает бота после сбоя. После перезапуска посреди окна бот поднимает демо-счёт из `state.json` и
журнала и не входит второй раз в монету, уже купленную в этом окне.

**Ядра.** Горячий поток бота закреплён за ядром 1, остальное — за ядром 0 (`[cores]` в конфиге). Сканеру стоит
добавить `CPUAffinity=0` в `scanner.service`, чтобы он не мешал горячему потоку. Это меняет службу сканера — только
с твоего согласия.

## 8. Обновление и остановка

Обновление:

```bash
cd /opt/src/V2.1Poly && git pull
cd v21bot && RUSTFLAGS="-C target-cpu=native" cargo build --release
systemctl stop v21bot
cp target/release/v21 /opt/v21bot/v21
systemctl start v21bot
```

Остановка: `systemctl stop v21bot`. Бот получает SIGTERM, сохраняет `state.json` и журнал и выходит.

Новый демо-счёт с нуля: остановить службу, удалить `/var/lib/v21bot/state.json`, запустить.

## 9. Утром

Сводка дня приходит в Telegram в 07:30 UTC. Подробный отчёт:

```bash
/opt/v21bot/v21 report --journal /var/lib/v21bot/journal.jsonl --config /etc/v21bot/v21bot.toml --from 2026-10-21
```

Сверка с архивом — на ПК, когда архив догонит ночь (после `forward.update()`):

```bash
scp root@СЕРВЕР:/var/lib/v21bot/journal.jsonl C:\Users\filip\Documents\15mbacktest\v21_journal.jsonl
v21 reconcile --journal C:\Users\filip\Documents\15mbacktest\v21_journal.jsonl --root C:\Users\filip\Documents\15mbacktest
```

Таблица `reconcile_ГГГГММДД.csv` ляжет рядом с журналом.

## Ворота вехи 2

Решение принимаешь ты. Ворота пройдены, если:

- демо отработало одну торговую ночь (00–07 UTC), и в журнале нет записей `error`;
- утром `v21 report` готов;
- когда архив догонит ночь, `v21 reconcile` не находит расхождений точных решений, кроме окон, помеченных
  пропусками ленты.
