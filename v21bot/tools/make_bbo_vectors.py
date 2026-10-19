"""Golden vectors for the frame reader of v21bot (crates/live/tests/pm_items.rs): real frames of the
Polymarket market channel and the bbo rows that paper/feeds.parse_ws makes of them (spec §5.2).

usage: py -3.11 tools/make_bbo_vectors.py --root 15MBACKTEST --rec FEEDS_JSONL --out crates/live/tests/data/bbo_vectors.json.gz

FEEDS_JSONL is a recording of tools/record_feeds.py. Taken from copy a: the first 1 000 frames (they
hold the snapshots of the subscription) and 2 000 frames from 838 s into the first window on. A row is
[token, venue ms, bid, ask]: a `book` gives its best prices (0.0 / 1.0 for an empty side), each change
of a `price_change` its best_bid / best_ask.
"""
import argparse
import gzip
import json
import sys


def best(rows, pick, empty):
    prices = [float(r['price']) for r in rows if float(r['size']) > 0]
    return pick(prices) if prices else empty


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument('--root', required=True, help='the 15mbacktest folder (paper/ is read from it)')
    ap.add_argument('--rec', required=True)
    ap.add_argument('--out', required=True)
    a = ap.parse_args()
    sys.path.insert(0, a.root)
    from paper.feeds import parse_ws

    first, later = [], []
    with open(a.rec, encoding='utf-8') as f:
        meta = json.loads(f.readline())['meta']
        s = meta['windows'][0]
        tokens = list(meta['tokens'].values())
        for line in f:
            r = json.loads(line)
            raw = r.get('raw') or ''
            if r['copy'] != 'pm_a' or raw.startswith('"__') or raw.startswith('{"__'):
                continue                    # the recorder's own marks of a connection
            if len(first) < 1000:
                first.append(raw)
            elif r['rx_ns'] / 1e9 >= s + 838:   # the PC's clock, a second off: good enough for a minute
                later.append(raw)
                if len(later) == 2000:
                    break
    ours = set(tokens)
    frames = []
    for raw in first + later:
        rows = []
        for ev in parse_ws(raw):
            if ev[1] not in ours:
                continue
            if ev[0] == 'book':
                rows.append([ev[1], ev[2], best(ev[3], max, 0.0), best(ev[4], min, 1.0)])
            elif ev[0] == 'change':
                rows.append([ev[1], ev[2], ev[6], ev[7]])
        frames.append({'raw': raw, 'rows': rows})
    with gzip.open(a.out, 'wt', encoding='utf-8') as g:
        json.dump({'window': s, 'tokens': tokens, 'frames': frames}, g)
    print(f'{a.out}: {len(frames)} frames, {sum(len(x["rows"]) for x in frames)} rows')
    return 0


if __name__ == '__main__':
    sys.exit(main())
