#!/usr/bin/env python3
"""Draw the app icon (1024x1024 PNG) with no third-party dependencies."""
import math, struct, sys, zlib

N = 1024
out = sys.argv[1] if len(sys.argv) > 1 else "icon.png"
cx = cy = N / 2

def mix(a, b, t):
    return tuple(int(a[i] + (b[i] - a[i]) * t) for i in range(3))

def clamp(v):
    return 0.0 if v < 0 else 1.0 if v > 1 else v

TOP, BOT = (52, 60, 78), (16, 18, 24)
BLADE_IN, BLADE_OUT = (56, 189, 248), (10, 132, 255)
rows = []
for y in range(N):
    row = bytearray([0])
    for x in range(N):
        # rounded-square background (macOS icon grid: 824px body, radius ~185)
        m, rad = 100, 185
        qx = max(abs(x + 0.5 - cx) - (N / 2 - m - rad), 0)
        qy = max(abs(y + 0.5 - cy) - (N / 2 - m - rad), 0)
        d = math.hypot(qx, qy) - rad
        a = clamp(0.5 - d)
        if a == 0:
            row += b"\0\0\0\0"
            continue
        col = mix(TOP, BOT, (y - m) / (N - 2 * m))
        dx, dy = x + 0.5 - cx, y + 0.5 - cy
        r = math.hypot(dx, dy)
        th = math.atan2(dy, dx)
        # housing
        col = mix(col, (12, 13, 17), clamp(330.5 - r))
        col = mix(col, (90, 98, 116), clamp(8 - abs(r - 336)) )
        # blades
        if 70 < r < 318:
            phi = (th - r * 0.0042) % (2 * math.pi / 7) - math.pi / 7
            width = 0.33 * (0.55 + 0.45 * (r - 70) / 248)
            edge = (width - abs(phi)) * r
            t = clamp(edge)
            if t > 0:
                col = mix(col, mix(BLADE_IN, BLADE_OUT, (r - 70) / 248), t * clamp(318 - r))
        # hub
        col = mix(col, (200, 210, 225), clamp(92.5 - r))
        col = mix(col, (120, 132, 150), clamp(60.5 - r))
        row += bytes(col) + bytes([int(255 * a)])
    rows.append(bytes(row))

def chunk(t, d):
    return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)

png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", N, N, 8, 6, 0, 0, 0))
png += chunk(b"IDAT", zlib.compress(b"".join(rows), 9)) + chunk(b"IEND", b"")
open(out, "wb").write(png)
