"""Record two copies of the Polymarket market feed and the Binance aggTrade mirror side by side.

For the milestone-2 plan: how to tell one event in two copies (spec §15). The receive loops do nothing but
append (copy, receive time ns, raw text) to a list, so that the venue does not drop us as a slow consumer
(close code 1013); a writer task flushes the list every second. A closed connection is logged and reopened.
usage: python record_feeds.py OUT_FILE MINUTES
"""
import asyncio
import json
import sys
import time

import requests
import websockets

OUT, MINUTES = sys.argv[1], float(sys.argv[2])
CLOB_WS = 'wss://ws-subscriptions-clob.polymarket.com/ws/market'
BN_MIRROR = 'wss://data-stream.binance.vision/stream?streams=btcusdt@aggTrade/ethusdt@aggTrade/solusdt@aggTrade'
buf = []


def tokens_of(s):
    out = {}
    for coin in ('btc', 'eth', 'sol'):
        r = requests.get('https://gamma-api.polymarket.com/events',
                         params={'slug': f'{coin}-updown-15m-{s}', '_': time.time_ns()}, timeout=20)
        ev = r.json()
        if ev:
            m = ev[0]['markets'][0]
            outs, ids = json.loads(m['outcomes']), json.loads(m['clobTokenIds'])
            out[f'{coin}-{s}-Up'] = ids[outs.index('Up')]
            out[f'{coin}-{s}-Down'] = ids[outs.index('Down')]
    return out


async def pinger(ws, stop):
    while time.time() < stop:
        await asyncio.sleep(10)
        await ws.send('PING')


async def polymarket(name, assets, stop):
    while time.time() < stop:
        try:
            async with websockets.connect(CLOB_WS, ping_interval=None, max_size=None, open_timeout=15) as ws:
                buf.append((name, time.time_ns(), '"__connected__"'))
                await ws.send(json.dumps({'assets_ids': assets, 'type': 'market'}))
                ping = asyncio.create_task(pinger(ws, stop))
                try:
                    while time.time() < stop:
                        raw = await ws.recv()
                        buf.append((name, time.time_ns(), raw))
                finally:
                    ping.cancel()
        except Exception as err:
            buf.append((name, time.time_ns(), json.dumps({'__closed__': repr(err)})))
            await asyncio.sleep(0.5)


async def binance(name, url, stop):
    while time.time() < stop:
        try:
            async with websockets.connect(url, max_size=None, open_timeout=15) as ws:
                while time.time() < stop:
                    buf.append((name, time.time_ns(), await ws.recv()))
        except Exception as err:
            buf.append((name, time.time_ns(), json.dumps({'__closed__': repr(err)})))
            await asyncio.sleep(1)


async def writer(sink, stop):
    while True:
        await asyncio.sleep(1)
        rows, buf[:] = buf[:], []
        for name, rx, raw in rows:
            sink.write(f'{{"copy": "{name}", "rx_ns": {rx}, "raw": {json.dumps(raw)}}}\n')
        if time.time() >= stop + 2:
            return


async def main():
    now = time.time()
    s = int(now // 900) * 900
    toks = {**tokens_of(s), **tokens_of(s + 900)}
    stop = now + MINUTES * 60
    with open(OUT, 'w', encoding='utf-8') as sink:
        sink.write(json.dumps({'meta': {'windows': [s, s + 900], 'tokens': toks, 'start_ns': time.time_ns()}}) + '\n')
        assets = list(toks.values())
        tasks = [polymarket('pm_a', assets, stop), polymarket('pm_b', assets, stop), binance('bn_mirror', BN_MIRROR, stop)]
        await asyncio.gather(writer(sink, stop), *[asyncio.wait_for(t, MINUTES * 60 + 30) for t in tasks],
                             return_exceptions=True)
    print('done', OUT)


asyncio.run(main())
