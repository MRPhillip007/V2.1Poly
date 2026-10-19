"""A short stretch of both copies of the Polymarket market channel, for crates/live/tests: the dedupe of
the copies, the time order of the exact engine, the allocations of the hot path.

usage: py -3.11 tools/make_two_copies.py --rec FEEDS_JSONL --out crates/live/tests/data/two_copies.jsonl.gz [--seconds 15]

FEEDS_JSONL is a recording of tools/record_feeds.py. The stretch is chosen by venue time, so that both
copies give the same frames: among the stretches where both copies hold the same frames, in the same
order for each market, the one where copy a has the most late events (a change older than an event
already received for its token). Output: a meta line, then {"copy": 0 | 1 | "bn", "rx_ns": ..., "raw":
...} in the order of receipt; the Binance messages received during the stretch come along.
"""
import argparse
import bisect
import gzip
import json
import statistics
import sys
from collections import Counter, defaultdict

COPIES = {'pm_a': 0, 'pm_b': 1}


def events(raw):
    try:
        msg = json.loads(raw)
    except ValueError:
        return []
    return [x for x in (msg if isinstance(msg, list) else [msg]) if isinstance(x, dict)]


def assets_of(ev):
    if ev.get('event_type') == 'price_change':
        return [pc.get('asset_id') for pc in ev.get('price_changes') or []]
    return [ev.get('asset_id')]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument('--rec', required=True)
    ap.add_argument('--out', required=True)
    ap.add_argument('--seconds', type=int, default=15)
    a = ap.parse_args()

    frames = {0: [], 1: []}        # copy -> [(rx_ns, frame venue ms, market, raw)]
    binance = []
    late_ts = []                   # venue ms of copy a's frames that hold a late event
    latest = {}
    with open(a.rec, encoding='utf-8') as f:
        meta = json.loads(f.readline())['meta']
        for line in f:
            r = json.loads(line)
            raw = r.get('raw') or ''
            if r['copy'] == 'bn_mirror':
                binance.append((r['rx_ns'], raw))
                continue
            copy = COPIES.get(r['copy'])
            evs = events(raw)
            if copy is None or not evs:
                continue
            stamps = [int(ev.get('timestamp') or 0) for ev in evs]
            frames[copy].append((r['rx_ns'], min(stamps), evs[0].get('market'), raw))
            if copy == 0:
                late = False
                for ev, ts in zip(evs, stamps):
                    for asset in assets_of(ev):
                        if asset in latest and ts < latest[asset]:
                            late = True
                        latest[asset] = max(ts, latest.get(asset, 0))
                if late:
                    late_ts.append(min(stamps))
    # receipt minus venue time: the PC's clock offset and the delivery, to find the frames by receipt
    lag_ms = statistics.median(rx / 1e6 - ts for rx, ts, _, _ in frames[0][::50])
    rx_of = {c: [x[0] for x in frames[c]] for c in (0, 1)}
    late_ts.sort()
    span = a.seconds * 1000

    def stretch(c, t0):
        lo = bisect.bisect_left(rx_of[c], (t0 + lag_ms - 10_000) * 1e6)
        hi = bisect.bisect_right(rx_of[c], (t0 + span + lag_ms + 10_000) * 1e6)
        return [x for x in frames[c][lo:hi] if t0 <= x[1] < t0 + span]

    def by_market(rows):
        out = defaultdict(list)
        for x in rows:
            out[x[2]].append(x[3])
        return out

    candidates = sorted({ts - span // 2 for ts in late_ts},
                        key=lambda t0: -(bisect.bisect_left(late_ts, t0 + span) - bisect.bisect_left(late_ts, t0)))
    for t0 in candidates[:200]:
        sa, sb = stretch(0, t0), stretch(1, t0)
        if Counter(x[3] for x in sa) != Counter(x[3] for x in sb) or by_market(sa) != by_market(sb):
            continue
        n_late = bisect.bisect_left(late_ts, t0 + span) - bisect.bisect_left(late_ts, t0)
        first = min(x[0] for x in sa + sb)
        last = max(x[0] for x in sa + sb)
        rows = [(rx, c, raw) for c, s in ((0, sa), (1, sb)) for rx, _, _, raw in s]
        rows += [(rx, 'bn', raw) for rx, raw in binance if first <= rx <= last]
        rows.sort(key=lambda x: x[0])
        with gzip.open(a.out, 'wt', encoding='utf-8') as g:
            g.write(json.dumps({'meta': {'windows': meta['windows'], 'tokens': list(meta['tokens'].values()),
                                         'venue_ms': [t0, t0 + span], 'late_frames_a': n_late}}) + '\n')
            for rx, c, raw in rows:
                g.write(json.dumps({'copy': c, 'rx_ns': rx, 'raw': raw}) + '\n')
        print(f'{a.out}: venue {t0}..{t0 + span} ms, {len(sa)} frames per copy, {n_late} late frames of copy a, '
              f'{sum(1 for x in rows if x[1] == "bn")} Binance messages')
        return 0
    print('no stretch where both copies agree', file=sys.stderr)
    return 1


if __name__ == '__main__':
    sys.exit(main())
