#!/bin/zsh
# Rasterises the VST Compatible logo for the About tab: both SVG variants in
# docs/images/vst/, at 1x and 2x, to raw unpremultiplied RGBA beside them
# (<name>@1x.rgba, <name>@2x.rgba), which the app loads with include_bytes!.
# egui can't draw SVG without pulling in resvg's tree, so this runs by hand,
# once, and its output is committed (docs/archive/330-macos-app.md, decision 7).
#
#   scripts/make-vst-logo.sh
#
# Needs resvg (cargo install resvg) and python3, whose standard library turns
# resvg's PNGs into raw pixels. The height is in points; the width follows the
# logo's aspect, and the app reads it back from each file's length, so keep
# HEIGHT in step with VST_LOGO_HEIGHT in src/view/display/about_tab.rs.
set -euo pipefail

repo=${0:A:h:h}
dir=$repo/docs/images/vst
height=48

command -v resvg >/dev/null || { echo "needs resvg (cargo install resvg)" >&2; exit 1; }

work=$(mktemp -d)
trap 'rm -rf $work' EXIT

for svg in $dir/*.svg; do
  name=${svg:t:r}
  for scale in 1 2; do
    png=$work/$name@${scale}x.png
    resvg -h $((height * scale)) $svg $png
    python3 -I - $png $dir/$name@${scale}x.rgba <<'EOF'
# Decodes an 8-bit RGBA, non-interlaced PNG (what resvg writes) to raw RGBA.
import struct, sys, zlib

src, dst = sys.argv[1], sys.argv[2]
data = open(src, "rb").read()
assert data[:8] == b"\x89PNG\r\n\x1a\n", "not a PNG"
pos, idat = 8, b""
while pos < len(data):
    length, kind = struct.unpack(">I4s", data[pos:pos + 8])
    body = data[pos + 8:pos + 8 + length]
    if kind == b"IHDR":
        width, height, depth, color, _, _, interlace = struct.unpack(">IIBBBBB", body)
        assert (depth, color, interlace) == (8, 6, 0), "expected 8-bit RGBA, not interlaced"
    elif kind == b"IDAT":
        idat += body
    pos += 12 + length
raw, bpp, stride = zlib.decompress(idat), 4, width * 4
out, prev = bytearray(), bytearray(stride)
for y in range(height):
    start = y * (stride + 1)
    kind, line = raw[start], bytearray(raw[start + 1:start + 1 + stride])
    for x in range(stride):
        a = line[x - bpp] if x >= bpp else 0
        b = prev[x]
        c = prev[x - bpp] if x >= bpp else 0
        if kind == 1:
            line[x] = (line[x] + a) & 0xFF
        elif kind == 2:
            line[x] = (line[x] + b) & 0xFF
        elif kind == 3:
            line[x] = (line[x] + (a + b) // 2) & 0xFF
        elif kind == 4:
            p = a + b - c
            pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
            pred = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
            line[x] = (line[x] + pred) & 0xFF
    out += line
    prev = line
open(dst, "wb").write(out)
print(f"wrote {dst} ({width}x{height})")
EOF
  done
done
