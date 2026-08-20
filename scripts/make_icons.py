#!/usr/bin/env python3
"""Generate the app and menu bar icons without any image dependencies.

The artwork is a rounded square with a "Z" cut through it. The tray variant is
pure black with alpha so macOS can treat it as a template image.

Run: python3 scripts/make_icons.py
"""

from __future__ import annotations

import os
import struct
import subprocess
import sys
import zlib

ICON_DIR = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "src-tauri", "icons")


def write_png(path: str, width: int, height: int, pixels: bytearray) -> None:
    """pixels is RGBA, row major."""
    raw = bytearray()
    stride = width * 4
    for y in range(height):
        raw.append(0)  # filter type 0
        raw.extend(pixels[y * stride : (y + 1) * stride])

    def chunk(tag: bytes, data: bytes) -> bytes:
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    header = struct.pack(">2I5B", width, height, 8, 6, 0, 0, 0)
    with open(path, "wb") as fh:
        fh.write(b"\x89PNG\r\n\x1a\n")
        fh.write(chunk(b"IHDR", header))
        fh.write(chunk(b"IDAT", zlib.compress(bytes(raw), 9))) 
        fh.write(chunk(b"IEND", b""))


def blend(pixels: bytearray, width: int, x: int, y: int, rgba: tuple[int, int, int, int]) -> None:
    r, g, b, a = rgba
    if a == 0:
        return
    i = (y * width + x) * 4
    dst_a = pixels[i + 3]
    if a >= 255 or dst_a == 0:
        pixels[i : i + 4] = bytes((r, g, b, max(a, dst_a) if a < 255 else 255))
        return
    alpha = a / 255.0
    for k, value in enumerate((r, g, b)):
        pixels[i + k] = int(pixels[i + k] * (1 - alpha) + value * alpha)
    pixels[i + 3] = max(dst_a, a)


def fill_polygon(pixels: bytearray, width: int, height: int, points, rgba, samples: int = 3) -> None:
    """Scanline fill with vertical supersampling for smooth edges."""
    ys = [p[1] for p in points]
    y0, y1 = max(0, int(min(ys))), min(height - 1, int(max(ys)) + 1)
    for y in range(y0, y1 + 1):
        coverage = [0.0] * width
        for s in range(samples):
            sample_y = y + (s + 0.5) / samples
            crossings = []
            n = len(points)
            for i in range(n):
                ax, ay = points[i]
                bx, by = points[(i + 1) % n]
                if (ay <= sample_y < by) or (by <= sample_y < ay):
                    t = (sample_y - ay) / (by - ay)
                    crossings.append(ax + t * (bx - ax))
            crossings.sort()
            for i in range(0, len(crossings) - 1, 2):
                left, right = crossings[i], crossings[i + 1]
                lx, rx = int(max(0, left)), int(min(width - 1, right))
                for x in range(lx, rx + 1):
                    span = min(x + 1.0, right) - max(float(x), left)
                    if span > 0:
                        coverage[x] += span / samples
        for x, c in enumerate(coverage):
            if c > 0.002:
                r, g, b, a = rgba
                blend(pixels, width, x, y, (r, g, b, int(min(1.0, c) * a)))


def rounded_rect(width: int, height: int, radius: float, steps: int = 24):
    pts = []
    corners = [
        (width - radius, height - radius, 0),
        (radius, height - radius, 90),
        (radius, radius, 180),
        (width - radius, radius, 270),
    ]
    import math

    for cx, cy, start in corners:
        for i in range(steps + 1):
            angle = math.radians(start + 90 * i / steps)
            pts.append((cx + radius * math.cos(angle), cy + radius * math.sin(angle)))
    return pts


def z_glyph(size: int, inset: float, thickness: float):
    """Three bars forming a Z, returned as separate polygons."""
    left = inset
    right = size - inset
    top = inset
    bottom = size - inset
    t = thickness
    return [
        [(left, top), (right, top), (right, top + t), (left, top + t)],
        [(right, top + t * 0.2), (right - t * 0.9, top + t * 0.2), (left + t * 0.1, bottom - t * 0.2), (left, bottom - t * 0.2)],
        [(left, bottom - t), (right, bottom - t), (right, bottom), (left, bottom)],
    ]


def app_icon(size: int) -> bytearray:
    pixels = bytearray(size * size * 4)
    # Background: vertical gradient from indigo to violet, drawn as bands.
    body = rounded_rect(size, size, size * 0.22)
    bands = 64
    for i in range(bands):
        y0 = size * i / bands
        y1 = size * (i + 1) / bands
        t = i / (bands - 1)
        color = (
            int(78 + (139 - 78) * t),
            int(70 + (92 - 70) * t),
            int(229 + (246 - 229) * t),
            255,
        )
        clipped = [(x, min(max(y, y0), y1)) for x, y in body]
        fill_polygon(pixels, size, size, clipped, color)
    for poly in z_glyph(size, size * 0.3, size * 0.11):
        fill_polygon(pixels, size, size, poly, (255, 255, 255, 255))
    return pixels


def tray_icon(size: int) -> bytearray:
    pixels = bytearray(size * size * 4)
    for poly in z_glyph(size, size * 0.16, size * 0.17):
        fill_polygon(pixels, size, size, poly, (0, 0, 0, 255))
    return pixels


def sips(src: str, dst: str, size: int) -> None:
    subprocess.run(
        ["sips", "-z", str(size), str(size), src, "--out", dst],
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def main() -> int:
    os.makedirs(ICON_DIR, exist_ok=True)
    source = os.path.join(ICON_DIR, "icon.png")
    write_png(source, 512, 512, app_icon(512))

    for name, size in [
        ("32x32.png", 32),
        ("128x128.png", 128),
        ("128x128@2x.png", 256),
        ("Square107x107Logo.png", 107),
    ]:
        sips(source, os.path.join(ICON_DIR, name), size)

    write_png(os.path.join(ICON_DIR, "tray.png"), 44, 44, tray_icon(44))

    # icns for the bundle
    iconset = os.path.join(ICON_DIR, "icon.iconset")
    os.makedirs(iconset, exist_ok=True)
    for size in (16, 32, 64, 128, 256, 512):
        sips(source, os.path.join(iconset, f"icon_{size}x{size}.png"), size)
        sips(source, os.path.join(iconset, f"icon_{size//2}x{size//2}@2x.png"), size)
    subprocess.run(
        ["iconutil", "-c", "icns", iconset, "-o", os.path.join(ICON_DIR, "icon.icns")],
        check=True,
    )
    subprocess.run(["rm", "-rf", iconset], check=True)
    print("icons written to", ICON_DIR)
    return 0


if __name__ == "__main__":
    sys.exit(main())
