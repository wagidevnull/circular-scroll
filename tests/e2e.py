#!/usr/bin/env python3
"""End-to-end test with a fake touchpad. Needs root and python-evdev.

  sudo python3 tests/e2e.py [path/to/circular-scroll]

Creates a virtual round touchpad, runs circular-scroll against it, and checks
what comes out of the virtual touchpad clone and the virtual wheel. Output
devices are grabbed so nothing reaches your desktop. Your real touchpad and
any running circular-scroll service are not touched.
"""

import math
import os
import select
import subprocess
import sys
import tempfile
import threading
import time

import evdev
from evdev import UInput, AbsInfo, ecodes as e

BIN = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else "target/release/circular-scroll")
NAME = "circular-scroll e2e pad"
failures = 0


def check(ok, what):
    global failures
    print(("  ok    " if ok else "  FAIL  ") + what)
    failures += not ok


def make_pad():
    absinfo = AbsInfo(value=0, min=0, max=6000, fuzz=0, flat=0, resolution=40)
    caps = {
        e.EV_KEY: [e.BTN_LEFT, e.BTN_TOUCH, e.BTN_TOOL_FINGER, e.BTN_TOOL_DOUBLETAP],
        e.EV_ABS: [(e.ABS_X, absinfo), (e.ABS_Y, absinfo)],
    }
    return UInput(caps, name=NAME, input_props=[e.INPUT_PROP_POINTER, e.INPUT_PROP_BUTTONPAD])


def emit(pad, events):
    for t, c, v in events:
        pad.write(t, c, v)
    pad.syn()
    time.sleep(0.004)


def point(r, deg):
    a = math.radians(deg)
    return round(3000 + r * 3000 * math.cos(a)), round(3000 + r * 3000 * math.sin(a))


def find(name, exclude=()):
    for path in sorted(evdev.list_devices(), key=lambda p: int(p.rsplit("event", 1)[1]), reverse=True):
        if path in exclude:
            continue
        d = evdev.InputDevice(path)
        if d.name == name:
            return d
        d.close()
    return None


class Collector:
    """Reads the output devices continuously, like a desktop would. (Reading only
    after a gesture would overflow the kernel's small per-reader buffer.)"""

    def __init__(self, *devs):
        self.devs = devs
        self.events = {d.fd: [] for d in devs}
        self.dropped = False
        self.lock = threading.Lock()
        self.running = True
        self.thread = threading.Thread(target=self.loop, daemon=True)
        self.thread.start()

    def loop(self):
        while self.running:
            for fd in select.select([d.fd for d in self.devs], [], [], 0.05)[0]:
                dev = next(d for d in self.devs if d.fd == fd)
                for ev in dev.read():
                    with self.lock:
                        if ev.type == e.EV_SYN and ev.code == e.SYN_DROPPED:
                            self.dropped = True
                        elif ev.type != e.EV_SYN:
                            self.events[fd].append(ev)

    def take(self, dev, settle=0.3):
        time.sleep(settle)
        with self.lock:
            out, self.events[dev.fd] = self.events[dev.fd], []
        return out

    def stop(self):
        self.running = False
        self.thread.join()


def test_daemon(tmp):
    print("daemon")
    cfg = os.path.join(tmp, "cfg.toml")
    with open(cfg, "w") as f:
        f.write(f'device_name = "{NAME}"\ncenter_x = 3000\ncenter_y = 3000\nradius_x = 3000\nradius_y = 3000\n')
    pad = make_pad()
    time.sleep(0.5)
    before = set(evdev.list_devices())
    proc = subprocess.Popen([BIN, "--config", cfg, "--debug"], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    try:
        clone = wheel = None
        for _ in range(50):
            time.sleep(0.1)
            clone = clone or find(f"{NAME} (circular-scroll)")
            wheel = wheel or find("circular-scroll wheel", exclude=before)
            if clone and wheel:
                break
        check(clone is not None, "virtual touchpad clone appears")
        check(wheel is not None, "virtual wheel appears")
        if not (clone and wheel):
            return
        clone.grab()
        wheel.grab()
        check(set(clone.capabilities().get(e.EV_KEY, [])) >= {e.BTN_TOUCH, e.BTN_TOOL_FINGER, e.BTN_LEFT},
              "clone has the pad's buttons")
        check(e.INPUT_PROP_BUTTONPAD in clone.input_props(), "clone has the pad's properties")
        col = Collector(clone, wheel)
        drain = col.take
        try:
            pad.device.grab()  # should fail: the daemon holds the grab
            check(False, "daemon grabs the real pad")
        except OSError:
            check(True, "daemon grabs the real pad")
        drain(clone), drain(wheel)

        # 1. Touch in the middle: moves the pointer (goes to the clone), no scrolling.
        x, y = point(0.2, 0)
        emit(pad, [(e.EV_KEY, e.BTN_TOUCH, 1), (e.EV_KEY, e.BTN_TOOL_FINGER, 1), (e.EV_ABS, e.ABS_X, x), (e.EV_ABS, e.ABS_Y, y)])
        for i in range(1, 11):
            emit(pad, [(e.EV_ABS, e.ABS_X, x + i * 50)])
        emit(pad, [(e.EV_KEY, e.BTN_TOUCH, 0), (e.EV_KEY, e.BTN_TOOL_FINGER, 0)])
        c, w = drain(clone), drain(wheel)
        check(len([ev for ev in c if ev.code == e.ABS_X]) == 11, f"middle touch reaches the clone ({len(c)} events)")
        check(not w, "middle touch doesn't scroll")

        # 2. Touch in the ring, 90 degrees clockwise: scrolls down 4 notches, pointer untouched.
        x, y = point(0.9, 0)
        emit(pad, [(e.EV_KEY, e.BTN_TOUCH, 1), (e.EV_KEY, e.BTN_TOOL_FINGER, 1), (e.EV_ABS, e.ABS_X, x), (e.EV_ABS, e.ABS_Y, y)])
        for step in range(1, 91):
            x, y = point(0.9, step)
            emit(pad, [(e.EV_ABS, e.ABS_X, x), (e.EV_ABS, e.ABS_Y, y)])
        emit(pad, [(e.EV_KEY, e.BTN_LEFT, 1)])  # a click while scrolling still goes through
        emit(pad, [(e.EV_KEY, e.BTN_LEFT, 0)])
        emit(pad, [(e.EV_KEY, e.BTN_TOUCH, 0), (e.EV_KEY, e.BTN_TOOL_FINGER, 0)])
        c, w = drain(clone), drain(wheel)
        notches = sum(ev.value for ev in w if ev.code == e.REL_WHEEL)
        hires = sum(ev.value for ev in w if ev.code == e.REL_WHEEL_HI_RES)
        check(notches == -4, f"90 degrees clockwise = 4 notches down (got {notches})")
        check(abs(hires + 540) <= 2, f"hi-res total about -540 (got {hires})")
        check([(ev.code, ev.value) for ev in c] == [(e.BTN_LEFT, 1), (e.BTN_LEFT, 0)],
              "ring touch only passes button clicks to the clone")

        # 3. Anticlockwise scrolls up.
        x, y = point(0.9, 0)
        emit(pad, [(e.EV_KEY, e.BTN_TOUCH, 1), (e.EV_KEY, e.BTN_TOOL_FINGER, 1), (e.EV_ABS, e.ABS_X, x), (e.EV_ABS, e.ABS_Y, y)])
        for step in range(1, 41):
            x, y = point(0.9, -step)
            emit(pad, [(e.EV_ABS, e.ABS_X, x), (e.EV_ABS, e.ABS_Y, y)])
        emit(pad, [(e.EV_KEY, e.BTN_TOUCH, 0), (e.EV_KEY, e.BTN_TOOL_FINGER, 0)])
        notches = sum(ev.value for ev in drain(wheel) if ev.code == e.REL_WHEEL)
        check(notches == 2, f"40 degrees anticlockwise = 2 notches up (got {notches})")

        # 4. Two fingers in the ring: normal touchpad behaviour.
        x, y = point(0.9, 0)
        emit(pad, [(e.EV_KEY, e.BTN_TOUCH, 1), (e.EV_KEY, e.BTN_TOOL_DOUBLETAP, 1), (e.EV_ABS, e.ABS_X, x), (e.EV_ABS, e.ABS_Y, y)])
        for step in range(1, 31):
            x, y = point(0.9, step)
            emit(pad, [(e.EV_ABS, e.ABS_X, x), (e.EV_ABS, e.ABS_Y, y)])
        emit(pad, [(e.EV_KEY, e.BTN_TOUCH, 0), (e.EV_KEY, e.BTN_TOOL_DOUBLETAP, 0)])
        c, w = drain(clone), drain(wheel)
        check(not w and len(c) > 30, "two-finger touch in the ring isn't taken over")

        col.stop()
        check(not col.dropped, "no events were dropped while reading")

        # 5. Stopping the daemon releases the pad.
        proc.terminate()
        proc.wait(5)
        time.sleep(0.3)
        try:
            pad.device.grab()
            pad.device.ungrab()
            check(True, "pad is released when the daemon stops")
        except OSError:
            check(False, "pad is released when the daemon stops")
    finally:
        if proc.poll() is None:
            proc.kill()
        log = proc.communicate()[0]
        pad.close()
    check("using circular-scroll e2e pad" in log, "startup message names the device")
    check("scroll start at" in log and "scroll end" in log, "--debug logs scrolling")


def test_calibrate(tmp):
    print("calibrate")
    cfg = os.path.join(tmp, "cal.toml")
    with open(cfg, "w") as f:
        f.write(f'device_name = "{NAME}"\ndegrees_per_notch = 15.0\n')
    pad = make_pad()
    time.sleep(0.5)
    proc = subprocess.Popen([BIN, "--config", cfg, "--calibrate", "3", "--write"],
                            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    # A finger already down during the countdown must still be counted.
    time.sleep(1.5)
    x, y = point(0.8, 0)
    emit(pad, [(e.EV_KEY, e.BTN_TOUCH, 1), (e.EV_KEY, e.BTN_TOOL_FINGER, 1), (e.EV_ABS, e.ABS_X, x), (e.EV_ABS, e.ABS_Y, y)])
    time.sleep(1.8)
    end, deg = time.monotonic() + 3.2, 0.0
    while time.monotonic() < end and proc.poll() is None:
        deg += 3
        a = math.radians(deg)
        # ellipse: centre (3100, 2900), radii 2500 x 2000
        emit(pad, [(e.EV_ABS, e.ABS_X, round(3100 + 2500 * math.cos(a))), (e.EV_ABS, e.ABS_Y, round(2900 + 2000 * math.sin(a)))])
    emit(pad, [(e.EV_KEY, e.BTN_TOUCH, 0), (e.EV_KEY, e.BTN_TOOL_FINGER, 0)])
    out = proc.communicate(timeout=10)[0]
    pad.close()
    check(proc.returncode == 0, "calibration succeeds" + ("" if proc.returncode == 0 else ":\n" + out))
    text = open(cfg).read()
    vals = {k.strip(): v.split("#")[0].strip() for k, v in (l.split("=", 1) for l in text.splitlines() if "=" in l)}
    near = lambda k, want: abs(float(vals.get(k, "nan")) - want) <= 60
    check(near("center_x", 3100) and near("center_y", 2900), f"centre found ({vals.get('center_x')}, {vals.get('center_y')})")
    check(near("radius_x", 2500) and near("radius_y", 2000), f"radius found ({vals.get('radius_x')}, {vals.get('radius_y')})")
    check(vals.get("degrees_per_notch") == "15.0", "other settings are kept")
    check(vals.get("device_name") == f'"{NAME}"', "device name saved")


def main():
    if os.geteuid() != 0:
        sys.exit("run as root: sudo python3 tests/e2e.py")
    if not os.access(BIN, os.X_OK):
        sys.exit(f"binary not found: {BIN} (run cargo build --release first)")
    subprocess.run(["modprobe", "uinput"], check=False)
    with tempfile.TemporaryDirectory() as tmp:
        test_daemon(tmp)
        test_calibrate(tmp)
    print("\nall passed" if not failures else f"\n{failures} FAILED")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
