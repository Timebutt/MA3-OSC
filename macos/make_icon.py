#!/usr/bin/env python3
"""Draw the app icon as a 1024x1024 PNG: the tray icon's grandMA3-style "3"
badge (white disc, grey rim, dark "3"), without the status dot.

The "3" geometry mirrors `on_three` in src/tray.rs; keep them in sync.

Standard library only. Usage: python3 macos/make_icon.py out.png
"""
import math
import struct
import sys
import zlib

N = 1024
C = N / 2  # disc centre
R = 412  # disc radius: fills Apple's 824 px icon grid
RIM = 0.0625 * R  # grey edge, same proportion as the tray icon
SS = 4  # supersampling per axis for the "3"

WHITE, GREY, INK = 0xF7, 0x9A, 0x1C


def seg_dist(p, a, b):
    abx, aby = b[0] - a[0], b[1] - a[1]
    t = ((p[0] - a[0]) * abx + (p[1] - a[1]) * aby) / (abx * abx + aby * aby)
    t = min(max(t, 0.0), 1.0)
    return math.hypot(p[0] - a[0] - t * abx, p[1] - a[1] - t * aby)


def on_three(x, y):
    """Same as src/tray.rs: (x, y) in disc radii from its centre, y down."""
    x += 0.04  # optical centring
    half = 0.095
    if -0.26 <= x <= 0.34 and -0.50 - half <= y <= -0.50 + half:
        return True
    if seg_dist((x, y), (0.26, -0.46), (0.0, -0.10)) <= half:
        return True
    dx, dy = x, y - 0.20
    a = math.degrees(math.atan2(dy, dx))
    return abs(math.hypot(dx, dy) - 0.30) <= half and -90.0 <= a <= 150.0


def clamp01(v):
    return min(max(v, 0.0), 1.0)


# bounding box of the "3" in pixels (with a margin), where we supersample
X0, X1 = int(C + (-0.47) * R), int(C + 0.40 * R)
Y0, Y1 = int(C + (-0.62) * R), int(C + 0.62 * R)

rows = []
for py in range(N):
    row = bytearray([0])  # PNG filter: none
    for px in range(N):
        d = math.hypot(px + 0.5 - C, py + 0.5 - C)
        alpha = clamp01(R - d + 0.5)
        if alpha == 0.0:
            row += b"\0\0\0\0"
            continue
        # white disc with an anti-aliased grey rim
        k = clamp01(d - (R - RIM) + 0.5)
        v = WHITE * (1 - k) + GREY * k
        if X0 <= px <= X1 and Y0 <= py <= Y1:
            hits = 0
            for sy in range(SS):
                for sx in range(SS):
                    x = (px + (sx + 0.5) / SS - C) / R
                    y = (py + (sy + 0.5) / SS - C) / R
                    hits += on_three(x, y)
            ink = hits / (SS * SS)
            v = v * (1 - ink) + INK * ink
        g = round(v)
        row += bytes([g, g, g, round(alpha * 255)])
    rows.append(bytes(row))


def chunk(tag, data):
    return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data))


png = b"\x89PNG\r\n\x1a\n"
png += chunk(b"IHDR", struct.pack(">IIBBBBB", N, N, 8, 6, 0, 0, 0))
png += chunk(b"IDAT", zlib.compress(b"".join(rows), 9))
png += chunk(b"IEND", b"")
with open(sys.argv[1], "wb") as f:
    f.write(png)
