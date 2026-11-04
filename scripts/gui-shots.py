#!/usr/bin/env python3
"""Drives arcOS's graphical interface in QEMU and takes screenshots.

QEMU is controlled through QMP: a USB tablet gives absolute pointer
positions, keys are sent as key codes, and `screendump` saves the screen.
A script is a list of steps, one per line:

    wait <seconds>
    shot <name>                 save <out>/<name>.png
    click <x> <y>               left click at a screen position
    double <x> <y>              double click
    right <x> <y>               right click
    move <x> <y>
    drag <x0> <y0> <x1> <y1>
    press <x> <y>               left button down (a drag that goes on...)
    glide <x> <y>               move there in small steps (button held or not)
    release <x> <y>             ...and up again, there
    type <text>                 letters, digits, space and simple punctuation
    key <name>                  a QEMU key name: ret, esc, spc, tab, ctrl-spc...
    hold <name> / unhold <name> keep a key down (ctrl, shift, alt) and let go

Usage: scripts/gui-shots.py <image> <script> <out folder> [qemu options...]
Environment: QEMU (default qemu-system-x86_64), UEFI=1 for OVMF, CDROM=1 to
boot the image as a CD, KEEP=<file> to keep the disk as the session left it,
SCREEN=<w>x<h> when the screen is not 1024x768.
"""
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time

# Screen size the pointer positions refer to (SCREEN=1920x1080 for a
# loader that picked another mode).
W, H = map(int, os.environ.get("SCREEN", "1024x768").split("x"))


class Qmp:
    def __init__(self, path):
        for _ in range(100):
            try:
                self.s = socket.socket(socket.AF_UNIX)
                self.s.connect(path)
                break
            except OSError:
                time.sleep(0.1)
        self.f = self.s.makefile("rw")
        self.read()
        self.cmd("qmp_capabilities")

    def read(self):
        while True:
            msg = json.loads(self.f.readline())
            if "event" not in msg:
                return msg

    def cmd(self, name, **args):
        self.f.write(json.dumps({"execute": name, "arguments": args}) + "\n")
        self.f.flush()
        r = self.read()
        if "error" in r:
            raise RuntimeError(f"{name}: {r['error']}")
        return r.get("return")

    def pointer(self, x, y, down=None, button="left"):
        events = [
            {"type": "abs", "data": {"axis": "x", "value": int(x * 32767 / (W - 1))}},
            {"type": "abs", "data": {"axis": "y", "value": int(y * 32767 / (H - 1))}},
        ]
        if down is not None:
            events.append({"type": "btn", "data": {"button": button, "down": down}})
        self.cmd("input-send-event", events=events)

    def key(self, name):
        keys = name.split("-")
        self.cmd("send-key", keys=[{"type": "qcode", "data": k} for k in keys])

    def key_state(self, name, down):
        self.cmd("input-send-event", events=[{"type": "key", "data": {"down": down, "key": {"type": "qcode", "data": name}}}])


CHARS = {" ": "spc", ".": "dot", ",": "comma", "-": "minus", "/": "slash", ":": "shift-semicolon", "!": "shift-1", "+": "kp_add", "*": "kp_multiply", "=": "equal"}


def main():
    image, script, out = sys.argv[1:4]
    extra = sys.argv[4:]
    os.makedirs(out, exist_ok=True)
    work = tempfile.mkdtemp()
    disk = os.path.join(work, "disk.img")
    shutil.copy(image, disk)
    sock = os.path.join(work, "qmp.sock")
    qemu = os.environ.get("QEMU", "qemu-system-x86_64")
    args = [qemu, "-m", "512M", "-display", "none", "-no-reboot", "-serial", f"file:{out}/serial.log",
            "-qmp", f"unix:{sock},server,nowait", "-usb", "-device", "usb-tablet"]
    if os.environ.get("CDROM") == "1":
        args += ["-cdrom", disk]
    else:
        args += ["-drive", f"file={disk},format=raw,if=ide"]
    if os.environ.get("UEFI") == "1":
        vars_ = os.path.join(work, "vars.fd")
        shutil.copy("/usr/share/OVMF/OVMF_VARS_4M.fd", vars_)
        args += ["-drive", "if=pflash,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE_4M.fd",
                 "-drive", f"if=pflash,format=raw,file={vars_}"]
    proc = subprocess.Popen(args + extra)
    try:
        q = Qmp(sock)
        at = (W // 2, H // 2)
        for line in open(script):
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            op, _, rest = line.partition(" ")
            a = rest.split()
            if op == "wait":
                time.sleep(float(a[0]))
            elif op == "shot":
                ppm = os.path.join(work, "shot.ppm")
                q.cmd("screendump", filename=ppm)
                time.sleep(0.3)
                from PIL import Image
                Image.open(ppm).save(os.path.join(out, a[0] + ".png"))
                print("shot", a[0], flush=True)
            elif op in ("click", "double", "right"):
                x, y = int(a[0]), int(a[1])
                button = "right" if op == "right" else "left"
                q.pointer(x, y)
                time.sleep(0.15)
                for _ in range(2 if op == "double" else 1):
                    q.pointer(x, y, True, button)
                    time.sleep(0.08)
                    q.pointer(x, y, False, button)
                    time.sleep(0.12)
            elif op == "move":
                q.pointer(int(a[0]), int(a[1]))
            elif op == "drag":
                x0, y0, x1, y1 = map(int, a)
                q.pointer(x0, y0)
                time.sleep(0.15)
                q.pointer(x0, y0, True)
                for i in range(1, 9):
                    q.pointer(x0 + (x1 - x0) * i // 8, y0 + (y1 - y0) * i // 8)
                    time.sleep(0.1)
                q.pointer(x1, y1, False)
            elif op == "press":
                x, y = int(a[0]), int(a[1])
                q.pointer(x, y)
                time.sleep(0.15)
                q.pointer(x, y, True)
                time.sleep(0.1)
            elif op == "glide":
                x1, y1 = int(a[0]), int(a[1])
                x0, y0 = at
                for i in range(1, 9):
                    q.pointer(x0 + (x1 - x0) * i // 8, y0 + (y1 - y0) * i // 8)
                    time.sleep(0.1)
            elif op == "release":
                q.pointer(int(a[0]), int(a[1]), False)
                time.sleep(0.1)
            elif op in ("hold", "unhold"):
                q.key_state(a[0], op == "hold")
                time.sleep(0.05)
            elif op == "type":
                for ch in rest:
                    if ch.isupper():
                        q.key("shift-" + ch.lower())
                    elif ch in CHARS:
                        q.key(CHARS[ch])
                    else:
                        q.key(ch)
                    time.sleep(0.08)
            elif op == "key":
                q.key(a[0])
            else:
                raise SystemExit(f"unknown step: {line}")
            if op in ("click", "double", "right", "move", "press", "glide", "release"):
                at = (int(a[0]), int(a[1]))
            elif op == "drag":
                at = (int(a[2]), int(a[3]))
    finally:
        proc.kill()
        proc.wait()
        if os.environ.get("KEEP"):
            shutil.copy(disk, os.environ["KEEP"])
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
