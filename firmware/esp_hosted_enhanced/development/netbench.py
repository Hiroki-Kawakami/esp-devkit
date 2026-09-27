#!/usr/bin/env python3
"""Wi-Fi throughput / latency bench against the development firmware.

Talks to the board over the harness console for its IP and heap figures, then
measures from the PC over the LAN (the PC must reach the board's network):

    netbench.py --port /dev/cu.usbmodemXXXX [--seconds 10] [--json out.json]
                [--udp] [--udp-rate MBPS]

Tests, in order:
    latency idle    UDP echo RTT, one packet every --interval ms
    tcp down        PC -> board throughput (board sink)
    tcp up          board -> PC throughput (board source)
    latency + down  the same RTT probe while the downlink stream runs
    latency + up    ... while the uplink stream runs
    udp down / up   with --udp: datagrams at --udp-rate (0: unpaced), received
                    rate and loss
"""

import argparse
import json
import os
import socket
import struct
import sys
import threading
import time

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                "..", "..", "..", "tools", "harness"))
from harness import Harness, HarnessError, SerialLink  # noqa: E402

SINK_PORT = 5001
SOURCE_PORT = 5002
ECHO_PORT = 5003
UDP_PORT = 5004
UDP_SIZE = 1472
UDP_DATA, UDP_RESET, UDP_REPORT, UDP_SEND = range(4)
UDP_HDR = struct.Struct("<B3xIII")
CHUNK = 64 * 1024


def open_harness(port, log_stream):
    link = SerialLink(port, 115200)
    # Opening the USB-Serial-JTAG port resets the board; drop the boot chatter
    # so late ping replies do not shift every later reply by one.
    time.sleep(4)
    link.ser.reset_input_buffer()
    link.buf = b""
    h = Harness(link, log_stream)
    h.wait_ready()
    return h, link


def wait_connected(h, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        p = h.cmd("wifi")
        if p[1] == "connected":
            return p[2], int(p[3])
        time.sleep(0.5)
    raise HarnessError("wifi did not connect")


def heap(h):
    p = h.cmd("heap")
    keys = ("int_free", "int_min", "int_largest", "dma_free", "psram_free",
            "int_free_at_main")
    return dict(zip(keys, map(int, p[1:7])))


def tcp_down(ip, seconds):
    s = socket.create_connection((ip, SINK_PORT), timeout=10)
    payload = bytes(CHUNK)
    sent = 0
    t0 = time.monotonic()
    while time.monotonic() - t0 < seconds:
        s.sendall(payload)
        sent += len(payload)
    s.shutdown(socket.SHUT_WR)
    s.settimeout(30)
    while s.recv(4096):
        pass
    elapsed = time.monotonic() - t0
    s.close()
    return sent * 8 / elapsed / 1e6


def tcp_up(ip, seconds):
    s = socket.create_connection((ip, SOURCE_PORT), timeout=10)
    s.settimeout(10)
    received = 0
    t0 = time.monotonic()
    while time.monotonic() - t0 < seconds:
        n = len(s.recv(CHUNK))
        if n == 0:
            break
        received += n
    elapsed = time.monotonic() - t0
    s.close()
    return received * 8 / elapsed / 1e6


def udp_report(s, ip):
    for _ in range(10):
        s.sendto(UDP_HDR.pack(UDP_REPORT, 0, 0, 0), (ip, UDP_PORT))
        try:
            while True:
                cmd, a0, a1, a2 = UDP_HDR.unpack_from(s.recv(2048))
                if cmd == UDP_REPORT:
                    return a0, a1, a2
        except socket.timeout:
            pass
    raise HarnessError("udp down: no report")


def udp_down(ip, seconds, rate_mbps):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.settimeout(0.3)
    for _ in range(3):
        s.sendto(UDP_HDR.pack(UDP_RESET, 0, 0, 0), (ip, UDP_PORT))
    time.sleep(0.2)
    payload = bytearray(UDP_SIZE)
    UDP_HDR.pack_into(payload, 0, UDP_DATA, 0, 0, 0)
    interval = UDP_SIZE * 8 / (rate_mbps * 1e6) if rate_mbps else 0
    sent = 0
    t0 = time.monotonic()
    while True:
        now = time.monotonic()
        if now - t0 >= seconds:
            break
        if interval and now < t0 + sent * interval:
            continue
        try:
            s.sendto(payload, (ip, UDP_PORT))
            sent += 1
        except OSError:
            time.sleep(0.0005)
    elapsed = time.monotonic() - t0
    time.sleep(0.5)
    packets, nbytes, span_us = udp_report(s, ip)
    s.close()
    return {"offered_mbps": sent * UDP_SIZE * 8 / elapsed / 1e6,
            "mbps": nbytes * 8 / span_us if span_us else 0.0,
            "sent": sent, "lost": sent - packets}


def udp_up(ip, seconds, rate_mbps):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4 * 1024 * 1024)
    s.settimeout(2.0)
    s.sendto(UDP_HDR.pack(UDP_SEND, int(seconds * 1000), UDP_SIZE, int(rate_mbps * 1000)),
             (ip, UDP_PORT))
    seen = set()
    first = last = report = None
    while True:
        try:
            data = s.recv(2048)
        except socket.timeout:
            break
        cmd, a0, a1, a2 = UDP_HDR.unpack_from(data)
        if cmd == UDP_REPORT:
            report = (a0, a1)
            break
        last = time.monotonic()
        first = first or last
        seen.add(a0)
    s.close()
    if report is None or not seen:
        raise HarnessError("udp up: no report")
    span = last - first
    return {"mbps": len(seen) * UDP_SIZE * 8 / span / 1e6 if span else 0.0,
            "sent": report[0], "lost": report[0] - len(seen), "send_errors": report[1]}


def fmt_udp(name, r):
    s = f"{name:16s} {r['mbps']:5.1f} Mbps  lost {r['lost']}/{r['sent']}"
    if "offered_mbps" in r:
        s += f"  (offered {r['offered_mbps']:.1f} Mbps)"
    if r.get("send_errors"):
        s += f"  (send errors {r['send_errors']})"
    return s


def latency(ip, count, interval_ms, size):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.settimeout(0.2)
    rtts = {}
    done = threading.Event()

    def receiver():
        while not done.is_set():
            try:
                data = s.recv(2048)
            except socket.timeout:
                continue
            now = time.monotonic_ns()
            seq, sent_ns = struct.unpack_from("<IQ", data)
            rtts[seq] = (now - sent_ns) / 1e6

    rx = threading.Thread(target=receiver, daemon=True)
    rx.start()
    pad = bytes(max(0, size - 12))
    next_t = time.monotonic()
    for seq in range(count):
        s.sendto(struct.pack("<IQ", seq, time.monotonic_ns()) + pad, (ip, ECHO_PORT))
        next_t += interval_ms / 1000.0
        delay = next_t - time.monotonic()
        if delay > 0:
            time.sleep(delay)
    time.sleep(2.0)
    done.set()
    rx.join()
    s.close()

    values = sorted(rtts.values())
    if not values:
        return {"sent": count, "lost": count}

    def pct(p):
        return values[min(len(values) - 1, int(len(values) * p / 100.0))]

    return {
        "sent": count,
        "lost": count - len(values),
        "p50": pct(50), "p90": pct(90), "p99": pct(99), "p999": pct(99.9),
        "max": values[-1],
        "over10": sum(v > 10 for v in values),
        "over50": sum(v > 50 for v in values),
        "over100": sum(v > 100 for v in values),
    }


def latency_under(load, ip, seconds, args):
    result = {}
    t = threading.Thread(target=lambda: result.setdefault("mbps", load(ip, seconds)))
    t.start()
    time.sleep(1.0)
    count = int((seconds - 2) * 1000 / args.interval)
    lat = latency(ip, count, args.interval, args.size)
    t.join()
    lat["load_mbps"] = result.get("mbps")
    return lat


def fmt_lat(name, r):
    if "p50" not in r:
        return f"{name:16s} all {r['sent']} lost"
    s = (f"{name:16s} p50 {r['p50']:6.2f}  p90 {r['p90']:6.2f}  p99 {r['p99']:7.2f}  "
         f"p99.9 {r['p999']:7.2f}  max {r['max']:7.2f} ms  "
         f">10ms {r['over10']}  >50ms {r['over50']}  >100ms {r['over100']}  "
         f"lost {r['lost']}/{r['sent']}")
    if r.get("load_mbps") is not None:
        s += f"  (load {r['load_mbps']:.1f} Mbps)"
    return s


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--port", required=True, help="board console (USB-Serial-JTAG)")
    ap.add_argument("--seconds", type=float, default=10.0, help="per throughput test")
    ap.add_argument("--count", type=int, default=1000, help="idle latency probes")
    ap.add_argument("--interval", type=float, default=10.0, help="probe interval (ms)")
    ap.add_argument("--size", type=int, default=64, help="probe payload bytes")
    ap.add_argument("--json", help="write the results here")
    ap.add_argument("--log", action="store_true", help="echo the board's log lines to stderr")
    ap.add_argument("--udp", action="store_true", help="also measure UDP throughput")
    ap.add_argument("--udp-rate", type=float, default=0.0, help="UDP offered rate (Mbps, 0: unpaced)")
    args = ap.parse_args()

    h, link = open_harness(args.port, sys.stderr if args.log else None)
    try:
        ip, rssi = wait_connected(h, 60)
        h.cmd("wifi ps none")
        mem = heap(h)
        time.sleep(1.0)

        results = {"ip": ip, "rssi": rssi, "heap": mem}
        results["latency_idle"] = latency(ip, args.count, args.interval, args.size)
        results["tcp_down_mbps"] = tcp_down(ip, args.seconds)
        results["tcp_up_mbps"] = tcp_up(ip, args.seconds)
        results["latency_down"] = latency_under(tcp_down, ip, args.seconds, args)
        results["latency_up"] = latency_under(tcp_up, ip, args.seconds, args)
        if args.udp:
            results["udp_down"] = udp_down(ip, args.seconds, args.udp_rate)
            results["udp_up"] = udp_up(ip, args.seconds, args.udp_rate)
        results["heap_after"] = heap(h)
    finally:
        link.close()

    print(f"board {ip}  rssi {rssi} dBm")
    m = results["heap"]
    print(f"internal heap free: at app_main {m['int_free_at_main']}  connected {m['int_free']}  "
          f"min {results['heap_after']['int_min']}  largest {m['int_largest']}  "
          f"dma {m['dma_free']}  psram {m['psram_free']}")
    print(f"tcp down {results['tcp_down_mbps']:.1f} Mbps   tcp up {results['tcp_up_mbps']:.1f} Mbps")
    print(fmt_lat("latency idle", results["latency_idle"]))
    print(fmt_lat("latency + down", results["latency_down"]))
    print(fmt_lat("latency + up", results["latency_up"]))
    if args.udp:
        print(fmt_udp("udp down", results["udp_down"]))
        print(fmt_udp("udp up", results["udp_up"]))

    if args.json:
        with open(args.json, "w") as f:
            json.dump(results, f, indent=2)
    return 0


if __name__ == "__main__":
    sys.exit(main())
