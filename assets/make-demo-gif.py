#!/usr/bin/env python3
"""Generate assets/demo.gif: an animated diagram of circular scrolling.

Requires ImageMagick 7 (`magick`) with librsvg. Usage:
    python3 assets/make-demo-gif.py
"""

import math
import os
import shutil
import subprocess
import sys
import tempfile

W, H = 560, 260
FPS = 25
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "demo.gif")

# Colours
BG = "#1e2030"
PAD = "#2a2e44"
RING = "#7aa2f7"
FINGER = "#f5c2e7"
WIN = "#24273a"
WIN_BAR = "#363a4f"
TEXT = "#cad3f5"
DIM = "#6e738d"
LINE = "#494d64"
ACCENT = "#a6da95"

# Touchpad geometry
PCX, PCY, PR = 130, 135, 92
RING_START = 0.70

# Window geometry
WX, WY, WW, WH = 290, 38, 240, 194
LINES = 40          # content lines in the mock document
LINE_H = 16

# Timeline: (seconds, phase)
PHASES = [
    (3.2, "cw"),     # circle clockwise -> scroll down
    (0.5, "lift"),
    (2.6, "ccw"),    # anticlockwise -> scroll up
    (0.5, "lift"),
    (2.6, "point"),  # touch in middle -> normal pointer
    (0.6, "lift"),
]
DEG_PER_SEC = 240    # finger angular speed
PX_PER_DEG = 0.55    # document scroll per degree


def ease(t):
    return 0.5 - 0.5 * math.cos(math.pi * max(0.0, min(1.0, t)))


def frames():
    """Yield per-frame state dicts."""
    scroll = 0.0
    angle = -90.0
    cursor = (WX + 150, WY + 120)
    for dur, phase in PHASES:
        n = int(dur * FPS)
        start_angle = angle
        for i in range(n):
            t = i / n
            st = {"phase": phase, "scroll": scroll, "finger": None, "trail": None, "cursor": cursor}
            if phase in ("cw", "ccw"):
                sign = 1 if phase == "cw" else -1
                # ramp speed in and out
                swept = sign * DEG_PER_SEC * dur * ease(t) * 0.5 + sign * DEG_PER_SEC * dur * t * 0.5
                angle = start_angle + swept
                r = PR * 0.86
                st["finger"] = (PCX + r * math.cos(math.radians(angle)), PCY + r * math.sin(math.radians(angle)))
                st["trail"] = (angle - sign * min(abs(swept), 110), angle)
                st["scroll"] = scroll + swept * PX_PER_DEG
            elif phase == "point":
                # small loop in the middle of the pad, cursor mirrors it
                a = 2 * math.pi * t
                fx = PCX + 26 * math.sin(a)
                fy = PCY + 14 * math.sin(2 * a)
                st["finger"] = (fx, fy)
                st["cursor"] = (WX + 150 + (fx - PCX) * 2.2, WY + 120 + (fy - PCY) * 2.2)
            yield st
        if phase in ("cw", "ccw"):
            sign = 1 if phase == "cw" else -1
            scroll += sign * DEG_PER_SEC * dur * PX_PER_DEG
            angle = start_angle + sign * DEG_PER_SEC * dur


def arc_path(a0, a1, r):
    if a1 < a0:
        a0, a1 = a1, a0
    x0, y0 = PCX + r * math.cos(math.radians(a0)), PCY + r * math.sin(math.radians(a0))
    x1, y1 = PCX + r * math.cos(math.radians(a1)), PCY + r * math.sin(math.radians(a1))
    large = 1 if (a1 - a0) > 180 else 0
    return f"M{x0:.1f},{y0:.1f} A{r},{r} 0 {large} 1 {x1:.1f},{y1:.1f}"


def svg(st):
    p = st["phase"]
    out = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" font-family="DejaVu Sans">',
           f'<rect width="{W}" height="{H}" fill="{BG}"/>']

    # --- touchpad
    ring_in = PR * RING_START
    out.append(f'<circle cx="{PCX}" cy="{PCY}" r="{PR}" fill="{PAD}" stroke="{LINE}" stroke-width="2"/>')
    active_ring = p in ("cw", "ccw")
    ring_op = 0.38 if active_ring else 0.16
    out.append(f'<circle cx="{PCX}" cy="{PCY}" r="{(PR + ring_in) / 2}" fill="none" stroke="{RING}" '
               f'stroke-opacity="{ring_op}" stroke-width="{PR - ring_in}"/>')
    out.append(f'<circle cx="{PCX}" cy="{PCY}" r="{ring_in}" fill="none" stroke="{RING}" '
               f'stroke-opacity="0.5" stroke-dasharray="4 5"/>')
    if st["trail"]:
        a0, a1 = st["trail"]
        out.append(f'<path d="{arc_path(a0, a1, PR * 0.86)}" fill="none" stroke="{FINGER}" '
                   f'stroke-opacity="0.45" stroke-width="7" stroke-linecap="round"/>')
    if st["finger"]:
        fx, fy = st["finger"]
        out.append(f'<circle cx="{fx:.1f}" cy="{fy:.1f}" r="13" fill="{FINGER}" fill-opacity="0.25"/>')
        out.append(f'<circle cx="{fx:.1f}" cy="{fy:.1f}" r="8" fill="{FINGER}"/>')

    caption = {
        "cw": "Circle the edge clockwise: scroll down",
        "ccw": "Anticlockwise: scroll up",
        "point": "Start in the middle: normal pointer",
    }.get(p)
    out.append(f'<text x="{W / 2}" y="22" text-anchor="middle" font-size="14" font-weight="bold" '
               f'fill="{TEXT}">{caption or "&#160;"}</text>')
    out.append(f'<text x="{PCX}" y="{H - 8}" text-anchor="middle" font-size="11" fill="{DIM}">'
               f'touchpad (shaded = scroll ring)</text>')

    # --- window
    out.append(f'<clipPath id="c"><rect x="{WX}" y="{WY + 18}" width="{WW}" height="{WH - 18}"/></clipPath>')
    out.append(f'<rect x="{WX}" y="{WY}" width="{WW}" height="{WH}" rx="8" fill="{WIN}" stroke="{LINE}"/>')
    out.append(f'<path d="M{WX},{WY + 18} V{WY + 8} a8,8 0 0 1 8,-8 H{WX + WW - 8} a8,8 0 0 1 8,8 V{WY + 18} Z" '
               f'fill="{WIN_BAR}"/>')
    for i, c in enumerate(("#ed8796", "#eed49f", "#a6da95")):
        out.append(f'<circle cx="{WX + 12 + i * 13}" cy="{WY + 9}" r="4" fill="{c}"/>')

    max_scroll = LINES * LINE_H - (WH - 30)
    sc = max(0.0, min(max_scroll, st["scroll"]))
    out.append('<g clip-path="url(#c)">')
    for i in range(LINES):
        y = WY + 32 + i * LINE_H - sc
        if y < WY + 10 or y > WY + WH + 10:
            continue
        if i % 9 == 0:
            out.append(f'<rect x="{WX + 14}" y="{y - 8:.1f}" width="{90 + (i * 7) % 50}" height="9" rx="3" '
                       f'fill="{ACCENT}" fill-opacity="0.8"/>')
        else:
            width = 120 + (i * 37) % 85
            out.append(f'<rect x="{WX + 14}" y="{y - 6:.1f}" width="{width}" height="6" rx="3" fill="{LINE}"/>')
    out.append('</g>')
    # scrollbar
    track_h = WH - 26
    thumb_h = track_h * (WH - 30) / (LINES * LINE_H)
    thumb_y = WY + 22 + (track_h - thumb_h) * (sc / max_scroll)
    out.append(f'<rect x="{WX + WW - 9}" y="{thumb_y:.1f}" width="4" height="{thumb_h:.1f}" rx="2" '
               f'fill="{RING}" fill-opacity="{0.9 if active_ring else 0.4}"/>')

    # cursor arrow
    cx, cy = st["cursor"]
    out.append(f'<path transform="translate({cx:.1f},{cy:.1f})" d="M0,0 L0,16 L4,12 L7,19 L10,18 L7,11 L12,11 Z" '
               f'fill="#ffffff" stroke="#000" stroke-width="1"/>')
    out.append(f'<text x="{WX + WW / 2}" y="{H - 8}" text-anchor="middle" font-size="11" fill="{DIM}">screen</text>')
    out.append('</svg>')
    return "\n".join(out)


def main():
    if not shutil.which("magick"):
        sys.exit("ImageMagick 7 (magick) is required")
    tmp = tempfile.mkdtemp(prefix="cs-demo-")
    try:
        names = []
        for i, st in enumerate(frames()):
            name = os.path.join(tmp, f"f{i:04d}.svg")
            with open(name, "w") as f:
                f.write(svg(st))
            names.append(name)
        print(f"rendering {len(names)} frames...")
        subprocess.run(
            ["magick", "-delay", f"{100 // FPS}", *names, "-layers", "Optimize", "-loop", "0", OUT],
            check=True,
        )
        print(f"wrote {OUT} ({os.path.getsize(OUT) // 1024} KiB)")
    finally:
        shutil.rmtree(tmp)


if __name__ == "__main__":
    main()
