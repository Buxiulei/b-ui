#!/usr/bin/env python3
"""v4 M5：Hysteria2 http 鉴权（auth.type=http）定速压测。

多线程 + http.client 长连接，按固定速率 POST /auth，逐次记录微秒级延迟与结果，
最后打印 p50/p95/p99、失败数、实际速率并给出 PASS/FAIL（判据 p99 < 20ms）。

只用标准库，不出网：默认打 127.0.0.1:18789（守护进程只监听回环）。

生产用法（bwg-rick，root）：
    printf 'alice:<密码>\\n' > /root/.bench-cred && chmod 600 /root/.bench-cred
    nohup python3 /opt/b-ui/ops/authhttp-bench.py --cred-file /root/.bench-cred \\
        --out /var/log/bui-bench-http >/dev/null 2>&1 &

凭据只从 --cred-file 读（一行 `user:password`），**绝不进 argv**：`ps` 会泄露。

产物与 authhook-bench.sh 同格式，所以 authhook-report.sh 可以直接判读同一个目录：
    <out>/lat-<worker>.csv   每行 `worker,序号,延迟微秒,结果码,返回的 id`
                             结果码 0 = ok:true，1 = ok:false / HTTP 非 200 / 连接错
    <out>/DONE               `finished=… target_rate=… workers=… seconds=… calls=… failures=…`
"""

import argparse
import http.client
import json
import os
import statistics
import sys
import threading
import time

DEFAULT_PORT = 18789  # bui_schema::render::hysteria::AUTH_HTTP_PORT
P99_LIMIT_MS = 20.0  # 判据（spec §3.2）
MIN_RATE_PCT = 95.0  # 实际速率低于目标的这个百分比 ⇒ 压测机自己是瓶颈，结果不可用


def read_cred(path):
    """返回 `user:password`。文件权限不对只警告不拒绝（本机压测，不值得卡住）。"""
    with open(path, encoding="utf-8") as f:
        line = f.readline().strip()
    if ":" not in line:
        raise SystemExit("--cred-file 第一行应为 user:password")
    if os.stat(path).st_mode & 0o077:
        print(f"警告：{path} 对同机其他用户可读", file=sys.stderr)
    return line


def worker(idx, rate, seconds, addr, auth, host, port, path, rows, errors):
    """一个 worker：长连接 + 按 1/rate 的节拍发请求，逐次记 (延迟微秒, 结果码, id)。"""
    body = json.dumps({"addr": addr, "auth": auth, "tx": 0})
    headers = {"Content-Type": "application/json", "Content-Length": str(len(body))}
    conn = http.client.HTTPConnection(host, port, timeout=5)
    interval = 1.0 / rate
    start = time.monotonic()
    n = 0
    while True:
        due = start + n * interval
        now = time.monotonic()
        if due > now:
            time.sleep(due - now)
        if time.monotonic() - start >= seconds:
            break
        t0 = time.perf_counter()
        code, ident = 1, ""
        try:
            conn.request("POST", path, body=body, headers=headers)
            res = conn.getresponse()
            payload = res.read()
            if res.status == 200:
                got = json.loads(payload)
                if got.get("ok") is True:
                    code, ident = 0, str(got.get("id", ""))
        except Exception as e:  # 连接被掐 / 超时：记一次失败并重建连接
            errors.append(f"{type(e).__name__}: {e}")
            try:
                conn.close()
            except Exception:
                pass
            conn = http.client.HTTPConnection(host, port, timeout=5)
        rows.append((idx, n, int((time.perf_counter() - t0) * 1_000_000), code, ident))
        n += 1
    conn.close()


def percentile(sorted_us, p):
    if not sorted_us:
        return 0.0
    i = min(len(sorted_us) - 1, max(0, int(len(sorted_us) * p / 100 + 0.999) - 1))
    return sorted_us[i] / 1000.0


def main():
    ap = argparse.ArgumentParser(description="Hysteria2 http 鉴权定速压测")
    ap.add_argument("--rate", type=int, default=200, help="目标速率（次/秒）")
    ap.add_argument("--seconds", type=int, default=300, help="持续秒数")
    ap.add_argument("--workers", type=int, default=8, help="并发线程数")
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=DEFAULT_PORT)
    ap.add_argument("--path", default="/auth")
    ap.add_argument("--addr", default="203.0.113.9:54321", help="伪造的客户端地址")
    ap.add_argument("--cred-file", required=True, help="一行 user:password（凭据不进 argv）")
    ap.add_argument("--out", default="", help="产物目录（与 authhook-report.sh 同格式）")
    ap.add_argument("--p99-ms", type=float, default=P99_LIMIT_MS)
    args = ap.parse_args()

    auth = read_cred(args.cred_file)
    workers = max(1, args.workers)
    per_worker = max(1, args.rate // workers)
    rows, errors, threads = [], [], []
    began = time.monotonic()
    for i in range(1, workers + 1):
        # list.append 在 CPython 里是原子的，多线程共写一个列表不会丢样本
        t = threading.Thread(
            target=worker,
            args=(i, per_worker, args.seconds, args.addr, auth,
                  args.host, args.port, args.path, rows, errors),
            daemon=True,
        )
        t.start()
        threads.append(t)
    for t in threads:
        t.join()
    elapsed = time.monotonic() - began

    if not rows:
        print("没有采到任何样本（连不上鉴权端口？）", file=sys.stderr)
        return 1
    lat = sorted(r[2] for r in rows)
    fails = sum(1 for r in rows if r[3] != 0)
    empty = sum(1 for r in rows if r[3] == 0 and not r[4])
    actual = len(rows) / elapsed if elapsed > 0 else 0.0
    pct = actual / args.rate * 100 if args.rate else 0.0
    p50, p95, p99 = (percentile(lat, p) for p in (50, 95, 99))

    if args.out:
        os.makedirs(args.out, exist_ok=True)
        for i in range(1, workers + 1):
            with open(f"{args.out}/lat-{i}.csv", "w", encoding="utf-8") as f:
                for r in (x for x in rows if x[0] == i):
                    f.write("%d,%d,%d,%d,%s\n" % r)
        with open(f"{args.out}/DONE", "w", encoding="utf-8") as f:
            f.write(
                "finished=%s target_rate=%d workers=%d seconds=%d calls=%d failures=%d\n"
                % (time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                   args.rate, workers, args.seconds, len(rows), fails)
            )

    print("调用数 %d  失败 %d  空 id %d" % (len(rows), fails, empty))
    print("延迟 p50=%.2fms p95=%.2fms p99=%.2fms max=%.2fms 均值=%.2fms"
          % (p50, p95, p99, lat[-1] / 1000.0, statistics.fmean(lat) / 1000.0))
    print("实际速率 %.1f/s（目标 %d/s，%.1f%%）" % (actual, args.rate, pct))
    for e in sorted(set(errors))[:5]:
        print("连接错误 %s" % e)
    bad = []
    if p99 >= args.p99_ms:
        bad.append("FAIL p99 %.2fms 不低于阈值 %.1fms" % (p99, args.p99_ms))
    if fails:
        bad.append("FAIL 失败 %d 次（放行路径必须 100%% 成功）" % fails)
    if empty:
        bad.append("FAIL %d 次没拿到 id（应答里缺 user_id）" % empty)
    if pct < MIN_RATE_PCT:
        bad.append("FAIL 实际速率只有目标的 %.1f%%（压测机自身是瓶颈，结果不可用）" % pct)
    for line in bad:
        print(line)
    print("\n结论：%s" % ("FAIL" if bad else "PASS"))
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
