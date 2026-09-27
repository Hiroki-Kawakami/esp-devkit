#!/usr/bin/env python3
"""SDIO link bench over the coprocessor test channel (no Wi-Fi involved).

    sdiobench.py --port /dev/cu.usbmodemXXXX [--json out.json] [--c6-log] [--sd KB]

Needs the esp_hosted_enhanced host and coprocessor firmware on both ends.
"""

import argparse
import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from netbench import open_harness, wait_connected  # noqa: E402

SIZES = (1524, 64)
COUNT = {1524: 2000, 64: 5000}


def mbps(nbytes, us):
    return nbytes * 8 / us if us else 0.0


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--port", required=True, help="host console (USB-Serial-JTAG)")
    ap.add_argument("--json", help="write the results here")
    ap.add_argument("--c6-log", action="store_true", help="forward and echo the coprocessor log")
    ap.add_argument("--sd", type=int, metavar="KB",
                    help="also write and verify KB on the SD card while streaming to the coprocessor")
    args = ap.parse_args()

    h, link = open_harness(args.port, sys.stderr if args.c6_log else None)
    results = {}
    try:
        wait_connected(h, 60)
        results["ext_caps"] = h.cmd("sdio caps")[1]
        if args.c6_log:
            h.cmd("sdio log on")
        for size in SIZES:
            n = COUNT[size]
            p = h.cmd(f"sdio sink {size} {n}", timeout=60)
            results[f"sink_{size}_mbps"] = mbps(int(p[2]), int(p[3]))
            p = h.cmd(f"sdio source {size} {n}", timeout=60)
            results[f"source_{size}_mbps"] = mbps(int(p[2]), int(p[3]))
            results[f"source_{size}_lost"] = int(p[4])
            p = h.cmd(f"sdio echo {size} 500", timeout=60)
            results[f"echo_{size}_us"] = dict(zip(("p50", "p99", "max", "lost"), map(int, p[2:6])))
        p = h.cmd("sdio sink 1524 500 1000", timeout=60)
        results["sink_slow_consumer_mbps"] = mbps(int(p[2]), int(p[3]))
        p = h.cmd("sdio share 2000", timeout=60)
        results["share"] = {"idf_ok": int(p[2]), "idf_err": int(p[3]), "mbps": mbps(int(p[4]), int(p[5]))}
        if args.sd:
            p = h.cmd(f"sdio sd {args.sd}", timeout=300)
            nbytes = int(p[2])
            results["sd"] = {"write_mbps": mbps(nbytes, int(p[3])), "read_mbps": mbps(nbytes, int(p[4])),
                             "bad_chunks": int(p[5]), "sdio_mbps": mbps(int(p[6]), int(p[7]))}
    finally:
        link.close()

    print(f"ext caps 0x{results['ext_caps']}")
    for size in SIZES:
        e = results[f"echo_{size}_us"]
        print(f"{size:5d} B  host->c6 {results[f'sink_{size}_mbps']:6.1f} Mbps   "
              f"c6->host {results[f'source_{size}_mbps']:6.1f} Mbps (lost {results[f'source_{size}_lost']})   "
              f"echo p50 {e['p50']} p99 {e['p99']} max {e['max']} us (lost {e['lost']})")
    print(f"host->c6 with a 1 ms/packet consumer: {results['sink_slow_consumer_mbps']:.1f} Mbps "
          f"(ceiling {1524 * 8 / 1000:.1f})")
    sh = results["share"]
    print(f"host->c6 while IDF commands share the controller: {sh['mbps']:.1f} Mbps, "
          f"IDF ok {sh['idf_ok']} err {sh['idf_err']}")
    if "sd" in results:
        sd = results["sd"]
        print(f"SD card write {sd['write_mbps']:.1f} / read {sd['read_mbps']:.1f} Mbps "
              f"(bad chunks {sd['bad_chunks']}) while host->c6 {sd['sdio_mbps']:.1f} Mbps")

    if args.json:
        with open(args.json, "w") as f:
            json.dump(results, f, indent=2)
    return 0


if __name__ == "__main__":
    sys.exit(main())
