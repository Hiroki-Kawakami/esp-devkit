#!/usr/bin/env python3
"""Drive the esp-devkit test harness (libs/harness) from the PC.

Runs a script of UI actions against either a simulator binary (stdin/stdout)
or a board on its log console (pyserial), using the same line protocol on
both. The firmware only exposes primitives (touch/button injection, an idle
probe, a JPEG capture); timing and scripting live here.

    harness.py --sim path/to/simulator script.txt
    harness.py --port /dev/cu.usbserial-XXXX [--baud 921600] script.txt

Script: one command per line, '#' comments.
    wait <ms>                  sleep
    settle [<max_ms>]          wait until the UI reports idle (default 5000)
    capture <path> [quality]   JPEG of the panel (path relative to --out)
    tap <x> <y> [id]           press + release
    down <x> <y> [id]          press / drag (call again to move)
    move <x> <y> [id]
    up [id]                    release
    btn <id> [click|down|up]   physical button (default click)
    quit                       stop (implicit at end of script)
    <anything else>            passed through verbatim (app commands)
"""

import argparse
import base64
import os
import queue
import subprocess
import sys
import threading
import time


class HarnessError(Exception):
    pass


class SimLink:
    def __init__(self, exe, headless=True, log=None):
        env = dict(os.environ)
        if headless:
            env["SIMULATOR_HEADLESS"] = "1"
        self.proc = subprocess.Popen(
            [exe], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=None if log else subprocess.DEVNULL, env=env, bufsize=0)
        self.lines = queue.Queue()
        threading.Thread(target=self._reader, daemon=True).start()

    def _reader(self):
        for raw in self.proc.stdout:
            self.lines.put(raw.decode("utf-8", "replace").rstrip("\r\n"))
        self.lines.put(None)

    def send(self, line):
        self.proc.stdin.write((line + "\n").encode())
        self.proc.stdin.flush()

    def readline(self, timeout):
        try:
            return self.lines.get(timeout=timeout)
        except queue.Empty:
            return ""

    def close(self):
        try:
            self.proc.stdin.close()
        except OSError:
            pass
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()


class SerialLink:
    def __init__(self, port, baud):
        import serial
        self.ser = serial.Serial()
        self.ser.port = port
        self.ser.baudrate = baud
        self.ser.timeout = 0.1
        self.ser.dtr = False
        self.ser.rts = False
        self.ser.open()
        self.ser.reset_input_buffer()
        self.buf = b""

    def send(self, line):
        self.ser.write((line + "\n").encode())
        self.ser.flush()

    def readline(self, timeout):
        deadline = time.monotonic() + timeout
        while True:
            nl = self.buf.find(b"\n")
            if nl >= 0:
                line, self.buf = self.buf[:nl], self.buf[nl + 1:]
                return line.decode("utf-8", "replace").rstrip("\r")
            if time.monotonic() >= deadline:
                return ""
            chunk = self.ser.read(4096)
            if chunk:
                self.buf += chunk

    def close(self):
        self.ser.close()


class Harness:
    TOUCH_HOLD_S = 0.08

    def __init__(self, link, log_stream=None):
        self.link = link
        self.log_stream = log_stream

    def _log(self, line):
        if self.log_stream:
            print(line, file=self.log_stream)

    def _read_reply(self, timeout):
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise HarnessError("timeout waiting for reply")
            line = self.link.readline(remaining)
            if line is None:
                raise HarnessError("link closed")
            if line == "":
                continue
            if line.startswith("#"):
                return line[1:]
            self._log(line)

    def cmd(self, line, timeout=5.0):
        self.link.send(line)
        reply = self._read_reply(timeout)
        parts = reply.split()
        if not parts or parts[0] != "OK":
            raise HarnessError(f"{line!r} -> {reply}")
        return parts[1:]

    def wait_ready(self, timeout=20.0):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                self.cmd("ping", timeout=1.0)
                return
            except HarnessError:
                continue
        raise HarnessError("harness did not answer ping")

    def info(self):
        p = self.cmd("info")
        return {"width": int(p[1]), "height": int(p[2]), "pixfmt": p[3],
                "touch": p[4] == "1", "buttons": int(p[5]), "capture": p[6] == "1"}

    def down(self, x, y, tid=0):
        self.cmd(f"down {tid} {x} {y}")

    def move(self, x, y, tid=0):
        self.cmd(f"move {tid} {x} {y}")

    def up(self, tid=0):
        self.cmd(f"up {tid}")

    def tap(self, x, y, tid=0):
        self.down(x, y, tid)
        time.sleep(self.TOUCH_HOLD_S)
        self.up(tid)
        time.sleep(self.TOUCH_HOLD_S)

    def btn(self, bid, action="click"):
        if action == "click":
            self.cmd(f"btn {bid} down")
            time.sleep(self.TOUCH_HOLD_S)
            self.cmd(f"btn {bid} up")
            time.sleep(self.TOUCH_HOLD_S)
        else:
            self.cmd(f"btn {bid} {action}")

    def idle(self):
        return self.cmd("idle")[1] == "1"

    def settle(self, max_ms=5000):
        deadline = time.monotonic() + max_ms / 1000.0
        quiet = 0
        while time.monotonic() < deadline:
            quiet = quiet + 1 if self.idle() else 0
            if quiet >= 3:
                return True
            time.sleep(0.05)
        return False

    def capture(self, path, quality=0):
        head = self.cmd(f"snap {quality}" if quality else "snap", timeout=30.0)
        width, height = int(head[1]), int(head[2])
        chunks = []
        while True:
            reply = self._read_reply(30.0)
            if reply.startswith("D "):
                chunks.append(reply[2:])
            elif reply.startswith("END "):
                expected = int(reply[4:])
                break
            else:
                raise HarnessError(f"snap -> {reply}")
        data = base64.b64decode("".join(chunks))
        if len(data) != expected:
            raise HarnessError(f"snap: got {len(data)} bytes, expected {expected}")
        os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
        with open(path, "wb") as f:
            f.write(data)
        return width, height, len(data)

    def quit(self):
        try:
            self.cmd("quit", timeout=2.0)
        except HarnessError:
            pass

    def run_script(self, lines, out_dir="."):
        for raw in lines:
            line = raw.strip()
            if not line or line.startswith("#"):
                continue
            argv = line.split()
            cmd, args = argv[0], argv[1:]
            if cmd == "wait":
                time.sleep(int(args[0]) / 1000.0)
            elif cmd == "settle":
                if not self.settle(int(args[0]) if args else 5000):
                    print(f"[harness] settle: UI still busy", file=sys.stderr)
            elif cmd == "capture":
                path = os.path.join(out_dir, args[0])
                w, h, n = self.capture(path, int(args[1]) if len(args) > 1 else 0)
                print(f"[harness] captured {path} ({w}x{h}, {n} bytes)", file=sys.stderr)
            elif cmd == "tap":
                self.tap(int(args[0]), int(args[1]), int(args[2]) if len(args) > 2 else 0)
            elif cmd in ("down", "move"):
                getattr(self, cmd)(int(args[0]), int(args[1]), int(args[2]) if len(args) > 2 else 0)
            elif cmd == "up":
                self.up(int(args[0]) if args else 0)
            elif cmd == "btn":
                self.btn(int(args[0]), args[1] if len(args) > 1 else "click")
            elif cmd == "quit":
                return
            else:
                self.cmd(line)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    target = ap.add_mutually_exclusive_group(required=True)
    target.add_argument("--sim", metavar="EXE", help="simulator binary to launch")
    target.add_argument("--port", metavar="DEV", help="serial port of the board's console")
    ap.add_argument("--baud", type=int, default=115200)
    ap.add_argument("--window", action="store_true", help="simulator: show the SDL window")
    ap.add_argument("--out", default=".", help="directory captures are written under")
    ap.add_argument("--log", action="store_true", help="echo the target's log lines to stderr")
    ap.add_argument("script", nargs="?", default="-", help="script file ('-' = stdin)")
    args = ap.parse_args()

    log_stream = sys.stderr if args.log else None
    if args.sim:
        link = SimLink(args.sim, headless=not args.window, log=args.log)
    else:
        link = SerialLink(args.port, args.baud)

    h = Harness(link, log_stream)
    status = 0
    try:
        h.wait_ready()
        script = sys.stdin if args.script == "-" else open(args.script)
        with script:
            h.run_script(script.readlines(), args.out)
    except HarnessError as e:
        print(f"[harness] error: {e}", file=sys.stderr)
        status = 1
    finally:
        h.quit()
        link.close()
    return status


if __name__ == "__main__":
    sys.exit(main())
