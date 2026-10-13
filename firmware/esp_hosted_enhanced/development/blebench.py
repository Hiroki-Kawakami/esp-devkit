#!/usr/bin/env python3
"""BLE bench between two boards running the development firmware.

    blebench.py --periph /dev/cu.usbmodemXXXX --central /dev/cu.usbmodemYYYY [--json out.json]

The peripheral advertises the bench service, the central connects to it
(2M PHY, data length extension, largest MTU) and both sides stream to the
other. Each side's HCI command round trip is measured while connected.
--load keeps the central's SDIO link busy over the test channel during its
measurements (esp_hosted_enhanced host and coprocessor only).
"""

import argparse
import json
import os
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                "..", "..", "..", "tools", "harness"))
from harness import Harness, HarnessError, SerialLink  # noqa: E402


def open_harness(port, log_stream):
    link = SerialLink(port, 115200)
    link.ser.timeout = 0.005
    h = Harness(link, log_stream)
    h.wait_ready()
    return h, link


def kbps(nbytes, us):
    return nbytes * 8 * 1000 / us if us else 0.0


def rtt(h, count):
    p = h.cmd(f"ble rtt {count}", timeout=60)
    return dict(zip(("p50", "p99", "max"), map(int, p[2:5])))


def loaded(h, direction, ms, measure):
    if not direction:
        return measure()
    h.cmd(f"sdio load {direction} {ms}")
    t0 = time.monotonic()
    result = measure()
    time.sleep(max(0.0, ms / 1000 - (time.monotonic() - t0)) + 1.0)
    return result


def stream(sender, receiver, cmd, nbytes):
    receiver.cmd("ble stats")
    sender.cmd(f"ble {cmd} {nbytes}", timeout=120)
    time.sleep(0.5)
    p = receiver.cmd("ble stats")
    return {"bytes": int(p[2]), "kbps": kbps(int(p[2]), int(p[3]))}


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--periph", required=True, help="peripheral board console")
    ap.add_argument("--central", required=True, help="central board console")
    ap.add_argument("--bytes", type=int, default=200_000, help="bytes per direction")
    ap.add_argument("--rtt", type=int, default=500, help="HCI round trips per side")
    ap.add_argument("--load", choices=("up", "down", "both"),
                    help="SDIO test traffic on the central during its measurements")
    ap.add_argument("--json", help="write the results here")
    ap.add_argument("--log", action="store_true", help="echo both boards' logs")
    args = ap.parse_args()

    log = sys.stderr if args.log else None
    per, per_link = open_harness(args.periph, log)
    cen, cen_link = open_harness(args.central, log)
    results = {}
    try:
        p = per.cmd("ble init", timeout=30)
        addr_type, addr = p[1], p[2]
        results["periph_init_ms"] = int(p[3])
        results["central_init_ms"] = int(cen.cmd("ble init", timeout=30)[3])
        results["periph_rtt_idle_us"] = rtt(per, args.rtt)
        per.cmd("ble adv")
        results["scan_reports_per_s"] = int(cen.cmd("ble scan 3000", timeout=30)[2]) / 3
        p = cen.cmd(f"ble connect {addr_type} {addr}", timeout=30)
        results["connect_ms"], results["mtu"] = int(p[2]), int(p[3])
        results["central_rtt_us"] = loaded(cen, args.load, 3000, lambda: rtt(cen, args.rtt))
        results["periph_rtt_us"] = rtt(per, args.rtt)
        results["write"] = loaded(cen, args.load, 5000, lambda: stream(cen, per, "write", args.bytes))
        results["notify"] = loaded(cen, args.load, 5000, lambda: stream(per, cen, "notify", args.bytes))
    finally:
        try:
            cen.cmd("ble disconnect")
        except HarnessError:
            pass
        per_link.close()
        cen_link.close()

    print(f"init {results['periph_init_ms']} / {results['central_init_ms']} ms, "
          f"scan {results['scan_reports_per_s']:.0f} reports/s, "
          f"connect {results['connect_ms']} ms, mtu {results['mtu']}")
    for key in ("periph_rtt_idle_us", "central_rtt_us", "periph_rtt_us"):
        r = results[key]
        print(f"{key:20s} p50 {r['p50']} p99 {r['p99']} max {r['max']} us")
    for key in ("write", "notify"):
        r = results[key]
        print(f"{key:6s} {r['kbps']:7.1f} kbps ({r['bytes']} bytes received)")

    if args.json:
        with open(args.json, "w") as f:
            json.dump(results, f, indent=2)
    return 0


if __name__ == "__main__":
    sys.exit(main())
