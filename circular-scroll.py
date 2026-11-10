#!/usr/bin/env python3
"""circular-scroll: ring-style circular scrolling for round touchpads on Linux.

Works under Wayland and X11 (anything using libinput). Grabs the physical
touchpad and re-emits its events on a virtual clone, so normal pointer, tap
and gesture behaviour is untouched. A single-finger touch that *starts* in
the outer ring of the pad is taken over: moving around the ring emits
mouse-wheel events instead (clockwise = scroll down), until the finger lifts.

Usage (as root):
  circular-scroll                      run the daemon
  circular-scroll --debug              run the daemon with verbose logging
  circular-scroll --calibrate 15       trace the pad edge for 15 s, print geometry
  circular-scroll --calibrate 15 --write   ...and save it to the config file
  circular-scroll --list               list input devices that look like touchpads
"""

import argparse
import math
import os
import select
import sys
import time
import tomllib

import evdev
from evdev import UInput, ecodes as e

CONFIG_PATH = "/etc/circular-scroll.toml"

DEFAULTS = {
    # Exact device name; empty = auto-detect the first touchpad.
    "device_name": "",
    # Pad geometry in raw touchpad units. 0 = derive from the device's reported range.
    "center_x": 0,
    "center_y": 0,
    "radius_x": 0,
    "radius_y": 0,
    # A touch starting at or beyond this fraction of the radius enters scroll mode.
    "ring_start": 0.70,
    # Degrees of rotation per wheel notch (smaller = faster scrolling).
    "degrees_per_notch": 20.0,
    # Flip direction (true: clockwise scrolls up).
    "reverse": False,
    # Ignore angle updates when the finger drifts this close to the centre.
    "min_track_radius": 0.35,
}

COMMENTS = {
    "device_name": "exact input device name; empty = auto-detect touchpad",
    "center_x": "pad geometry in raw touchpad units (set by calibration)",
    "ring_start": "touches starting beyond this fraction of the radius scroll",
    "degrees_per_notch": "smaller = faster scrolling",
    "reverse": "true: clockwise scrolls up",
    "min_track_radius": "ignore movement this close to the centre while scrolling",
}

VIRTUAL_SUFFIX = "(circular-scroll)"
log_debug = False


def dbg(*a):
    if log_debug:
        print(*a, file=sys.stderr, flush=True)


def load_config():
    cfg = dict(DEFAULTS)
    if os.path.exists(CONFIG_PATH):
        try:
            with open(CONFIG_PATH, "rb") as f:
                user = tomllib.load(f)
        except tomllib.TOMLDecodeError as err:
            sys.exit(f"error: cannot parse {CONFIG_PATH}: {err}")
        unknown = set(user) - set(DEFAULTS)
        if unknown:
            print(f"warning: unknown config keys ignored: {sorted(unknown)}", file=sys.stderr)
        cfg.update({k: v for k, v in user.items() if k in DEFAULTS})
    return cfg


def toml_value(v):
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, str):
        return '"' + v.replace("\\", "\\\\").replace('"', '\\"') + '"'
    return str(v)


def write_config(cfg):
    lines = ["# circular-scroll settings", "# apply changes with: sudo systemctl restart circular-scroll", ""]
    for k in DEFAULTS:
        line = f"{k} = {toml_value(cfg[k])}"
        if k in COMMENTS:
            line = f"{line:<40} # {COMMENTS[k]}"
        lines.append(line)
    tmp = CONFIG_PATH + ".tmp"
    with open(tmp, "w") as f:
        f.write("\n".join(lines) + "\n")
    os.replace(tmp, CONFIG_PATH)
    print(f"Saved to {CONFIG_PATH}")


def is_touchpad(d):
    caps = d.capabilities()
    keys = set(caps.get(e.EV_KEY, []))
    abs_codes = {c if isinstance(c, int) else c[0] for c in caps.get(e.EV_ABS, [])}
    return (
        e.BTN_TOUCH in keys
        and e.BTN_TOOL_FINGER in keys
        and e.BTN_TOOL_PEN not in keys
        and {e.ABS_X, e.ABS_Y} <= abs_codes
        and not d.name.endswith(VIRTUAL_SUFFIX)
    )


def touchpads():
    found = []
    for path in evdev.list_devices():
        try:
            d = evdev.InputDevice(path)
        except OSError:
            continue
        if is_touchpad(d):
            found.append(d)
        else:
            d.close()
    return found


def find_touchpad(name, wait=True):
    while True:
        pads = touchpads()
        match = next((d for d in pads if not name or d.name == name), None)
        for d in pads:
            if d is not match:
                d.close()
        if match:
            return match
        if not wait:
            sys.exit(f"error: no touchpad found" + (f" named '{name}'" if name else ""))
        time.sleep(1)


def geometry(dev, cfg):
    ax = dev.absinfo(e.ABS_X)
    ay = dev.absinfo(e.ABS_Y)
    cx = cfg["center_x"] or (ax.min + ax.max) / 2
    cy = cfg["center_y"] or (ay.min + ay.max) / 2
    rx = cfg["radius_x"] or (ax.max - ax.min) / 2
    ry = cfg["radius_y"] or (ay.max - ay.min) / 2
    return cx, cy, rx, ry


# --------------------------------------------------------------------------- calibrate

def calibrate(cfg, seconds, write):
    dev = find_touchpad(cfg["device_name"], wait=False)
    print(f"Touchpad: {dev.name} ({dev.path})")
    ax, ay = dev.absinfo(e.ABS_X), dev.absinfo(e.ABS_Y)
    print(f"Reported range: X {ax.min}..{ax.max}  Y {ay.min}..{ay.max}\n")
    print("Trace slowly around the VERY EDGE of the touchpad, lap after lap,")
    print(f"until the timer runs out ({seconds} s).")
    if sys.stdin.isatty():
        input("Press Enter when ready... ")
    for n in (3, 2, 1):
        print(f"  {n}...", flush=True)
        time.sleep(1)
    print("  GO - trace the edge now!", flush=True)
    while dev.read_one():  # discard events queued before the start
        pass
    xs, ys = [], []
    # Seed state from the device: if a finger landed during the countdown, its
    # BTN_TOUCH/position events were just discarded and won't be re-sent.
    x, y = dev.absinfo(e.ABS_X).value, dev.absinfo(e.ABS_Y).value
    touching = e.BTN_TOUCH in dev.active_keys()
    start = time.monotonic()
    end = start + seconds
    last_shown = None
    while (now := time.monotonic()) < end:
        left = int(end - now) + 1
        if left != last_shown:
            print(f"\r  {left:2d} s left, {len(xs)} samples ", end="", flush=True)
            last_shown = left
        r, _, _ = select.select([dev.fd], [], [], 0.2)
        if not r:
            continue
        for ev in dev.read():
            if ev.type == e.EV_ABS and ev.code == e.ABS_X:
                x = ev.value
            elif ev.type == e.EV_ABS and ev.code == e.ABS_Y:
                y = ev.value
            elif ev.type == e.EV_KEY and ev.code == e.BTN_TOUCH:
                touching = bool(ev.value)
            elif ev.type == e.EV_SYN and touching and x is not None and y is not None:
                xs.append(x)
                ys.append(y)
    print()
    if len(xs) < 200:
        sys.exit(f"Only {len(xs)} samples captured - run it again and keep tracing the edge the whole time.")
    # Use percentiles to reject outliers/noise at the extremes.
    xs.sort()
    ys.sort()
    p = lambda v, q: v[min(len(v) - 1, int(q * len(v)))]
    xmin, xmax = p(xs, 0.01), p(xs, 0.99)
    ymin, ymax = p(ys, 0.01), p(ys, 0.99)
    geom = {
        "device_name": dev.name,
        "center_x": round((xmin + xmax) / 2),
        "center_y": round((ymin + ymax) / 2),
        "radius_x": round((xmax - xmin) / 2),
        "radius_y": round((ymax - ymin) / 2),
    }
    print(f"\nSamples: {len(xs)}  observed X {xmin}..{xmax}  Y {ymin}..{ymax}")
    for k, v in geom.items():
        print(f"{k} = {toml_value(v)}")
    if write:
        cfg.update(geom)
        write_config(cfg)


def list_devices():
    pads = touchpads()
    if not pads:
        print("No touchpads found (are you root?)")
    for d in pads:
        ax, ay = d.absinfo(e.ABS_X), d.absinfo(e.ABS_Y)
        print(f"{d.path}: \"{d.name}\"  X {ax.min}..{ax.max}  Y {ay.min}..{ay.max}")


# --------------------------------------------------------------------------- daemon

class Scroller:
    def __init__(self, cfg, geom, wheel):
        self.cfg = cfg
        self.cx, self.cy, self.rx, self.ry = geom
        self.wheel = wheel
        self.last_angle = None
        self.hires_acc = 0.0  # pending hi-res units not yet emitted
        self.notch_acc = 0    # emitted hi-res units not yet reported as full notches

    def norm(self, x, y):
        dx = (x - self.cx) / self.rx
        dy = (y - self.cy) / self.ry
        return math.hypot(dx, dy), math.degrees(math.atan2(dy, dx))

    def in_ring(self, x, y):
        return self.norm(x, y)[0] >= self.cfg["ring_start"]

    def start(self, x, y):
        self.last_angle = self.norm(x, y)[1]
        self.hires_acc = 0.0
        self.notch_acc = 0

    def move(self, x, y):
        r, ang = self.norm(x, y)
        if r < self.cfg["min_track_radius"]:
            self.last_angle = None  # too close to centre: angle is meaningless
            return
        if self.last_angle is None:
            self.last_angle = ang
            return
        d = (ang - self.last_angle + 180) % 360 - 180  # shortest signed delta
        self.last_angle = ang
        # Touchpad Y grows downward, so positive d is clockwise on screen.
        # Clockwise -> scroll down -> negative wheel values.
        sign = 1 if self.cfg["reverse"] else -1
        self.hires_acc += sign * d / self.cfg["degrees_per_notch"] * 120
        hires = int(self.hires_acc)
        if hires == 0:
            return
        self.hires_acc -= hires
        self.notch_acc += hires
        notches = int(self.notch_acc / 120)
        self.notch_acc -= notches * 120
        self.wheel.write(e.EV_REL, e.REL_WHEEL_HI_RES, hires)
        if notches:
            self.wheel.write(e.EV_REL, e.REL_WHEEL, notches)
        self.wheel.syn()
        dbg(f"scroll d={d:+.1f} hires={hires} notches={notches}")


BUTTON_CODES = {e.BTN_LEFT, e.BTN_RIGHT, e.BTN_MIDDLE}
TOOL_FINGERS = {
    e.BTN_TOOL_FINGER: 1,
    e.BTN_TOOL_DOUBLETAP: 2,
    e.BTN_TOOL_TRIPLETAP: 3,
    e.BTN_TOOL_QUADTAP: 4,
    e.BTN_TOOL_QUINTTAP: 5,
}


def run(cfg):
    dev = find_touchpad(cfg["device_name"])
    geom = geometry(dev, cfg)
    print(f"circular-scroll: using {dev.name} ({dev.path}) "
          f"center=({geom[0]:.0f},{geom[1]:.0f}) radius=({geom[2]:.0f},{geom[3]:.0f})",
          flush=True)

    caps = dev.capabilities(absinfo=True)
    caps.pop(e.EV_SYN, None)
    clone = UInput(
        caps,
        name=f"{dev.name} {VIRTUAL_SUFFIX}",
        vendor=dev.info.vendor,
        product=dev.info.product,
        version=dev.info.version,
        bustype=dev.info.bustype,
        input_props=dev.input_props(),
    )
    wheel = UInput(
        {
            e.EV_KEY: [e.BTN_LEFT, e.BTN_RIGHT, e.BTN_MIDDLE],
            e.EV_REL: [e.REL_X, e.REL_Y, e.REL_WHEEL, e.REL_WHEEL_HI_RES],
        },
        name="circular-scroll wheel",
    )
    scroller = Scroller(cfg, geom, wheel)

    dev.grab()
    try:
        mode = "idle"  # idle | pass | scroll
        x = y = None
        fingers = 0
        tools = {}
        frame = []
        for ev in dev.read_loop():
            if ev.type != e.EV_SYN:
                frame.append(ev)
                if ev.type == e.EV_ABS:
                    if ev.code == e.ABS_X:
                        x = ev.value
                    elif ev.code == e.ABS_Y:
                        y = ev.value
                elif ev.type == e.EV_KEY and ev.code in TOOL_FINGERS:
                    tools[ev.code] = ev.value
                    fingers = max((n for c, n in TOOL_FINGERS.items() if tools.get(c)), default=0)
                continue
            if ev.code != e.SYN_REPORT:
                frame.clear()  # SYN_DROPPED etc.: discard partial frame
                continue

            touch_down = any(f.type == e.EV_KEY and f.code == e.BTN_TOUCH and f.value == 1 for f in frame)
            touch_up = any(f.type == e.EV_KEY and f.code == e.BTN_TOUCH and f.value == 0 for f in frame)

            if mode == "idle" and touch_down:
                if fingers <= 1 and x is not None and y is not None and scroller.in_ring(x, y):
                    mode = "scroll"
                    scroller.start(x, y)
                    dbg(f"scroll start at ({x},{y})")
                else:
                    mode = "pass"

            if mode == "scroll":
                if x is not None and y is not None:
                    scroller.move(x, y)
                # Still pass physical button presses through while scrolling.
                buttons = [f for f in frame if f.type == e.EV_KEY and f.code in BUTTON_CODES]
                for f in buttons:
                    clone.write(f.type, f.code, f.value)
                if buttons:
                    clone.syn()
            else:
                for f in frame:
                    clone.write(f.type, f.code, f.value)
                clone.syn()

            if touch_up:
                if mode == "scroll":
                    dbg("scroll end")
                mode = "idle"
            frame.clear()
    finally:
        try:
            dev.ungrab()
        except OSError:
            pass
        clone.close()
        wheel.close()


def main():
    global log_debug
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--calibrate", type=int, metavar="SECONDS", help="record the pad edge and print geometry")
    ap.add_argument("--write", action="store_true", help="with --calibrate: save geometry to " + CONFIG_PATH)
    ap.add_argument("--list", action="store_true", help="list detected touchpads")
    ap.add_argument("--debug", action="store_true", help="verbose logging")
    args = ap.parse_args()
    log_debug = args.debug
    cfg = load_config()
    if args.list:
        list_devices()
    elif args.calibrate:
        calibrate(cfg, args.calibrate, args.write)
    else:
        run(cfg)


if __name__ == "__main__":
    main()
