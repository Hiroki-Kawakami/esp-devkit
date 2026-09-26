#!/usr/bin/env python3
"""Flash the coprocessor through the development firmware (esp-hosted OTA).

    c6flash.py --port /dev/cu.usbmodemXXXX image.bin
    c6flash.py --port ... --recovery image.bin     enter safe mode first (IO2 low)

--reset-cmd runs a shell command wherever the coprocessor has to be reset and
the host cannot do it itself (a coprocessor without its EN wired to the host).
"""

import argparse
import base64
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                "..", "..", "..", "tools", "harness"))
from harness import Harness, HarnessError, SerialLink  # noqa: E402

LINE_BYTES = 372
WINDOW = 3


def open_harness(port, log_stream):
    deadline = time.monotonic() + 20
    while True:
        try:
            link = SerialLink(port, 115200)
            break
        except Exception:
            if time.monotonic() > deadline:
                raise
            time.sleep(0.5)
    # Opening the USB-Serial-JTAG port resets the board; drop the boot chatter
    # so late ping replies do not shift every later reply by one.
    time.sleep(4)
    link.ser.reset_input_buffer()
    link.buf = b""
    link.ser.timeout = 0.005
    h = Harness(link, log_stream)
    h.wait_ready()
    return h, link


def restart_host(h, link, mode, args, log_stream):
    h.cmd(f"c6 {mode}")
    link.close()
    if args.reset_cmd:
        subprocess.run(args.reset_cmd, shell=True, check=True)
    return open_harness(args.port, log_stream)


def expect_ok(h):
    reply = h._read_reply(10.0)
    if not reply.startswith("OK c6"):
        raise HarnessError(f"c6 data -> {reply}")


def wait_coprocessor(h, timeout):
    deadline = time.monotonic() + timeout
    while True:
        try:
            return h.cmd("c6 ver", timeout=5)[1]
        except HarnessError:
            if time.monotonic() > deadline:
                raise
            time.sleep(1.0)


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--port", required=True, help="host console (USB-Serial-JTAG)")
    ap.add_argument("--recovery", action="store_true", help="flash from safe mode")
    ap.add_argument("--reset-cmd", help="shell command that resets the coprocessor")
    ap.add_argument("--log", action="store_true", help="echo the host's log lines to stderr")
    ap.add_argument("image")
    args = ap.parse_args()

    data = open(args.image, "rb").read()
    log_stream = sys.stderr if args.log else None
    h, link = open_harness(args.port, log_stream)
    try:
        mode = "recovery" if args.recovery else "normal"
        if h.cmd("c6 state")[1] != mode or args.reset_cmd:
            h, link = restart_host(h, link, mode, args, log_stream)
        print(f"coprocessor {wait_coprocessor(h, 40)} ({mode})")

        h.cmd("c6 begin", timeout=30)
        t0 = time.monotonic()
        offsets = range(0, len(data), LINE_BYTES)
        for i, off in enumerate(offsets):
            chunk = base64.b64encode(data[off:off + LINE_BYTES]).decode()
            link.send(f"c6 data {chunk}")
            if i >= WINDOW:
                expect_ok(h)
            if i % 64 == 0:
                print(f"\r{off * 100 // len(data):3d}%", end="", flush=True)
        for _ in range(min(WINDOW, len(offsets))):
            expect_ok(h)
        h.cmd("c6 end", timeout=30)
        h.cmd("c6 activate", timeout=10)
        print(f"\r100%  {len(data)} bytes in {time.monotonic() - t0:.1f} s")

        time.sleep(2.0)
        h, link = restart_host(h, link, "normal", args, log_stream)
        print(f"coprocessor {wait_coprocessor(h, 40)}")
    finally:
        link.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
