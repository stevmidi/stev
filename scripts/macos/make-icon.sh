#!/bin/zsh
# Builds assets/macos/Stev.icns from assets/macos/icon.svg. Run by hand when
# the icon changes and commit the result, so building the bundle needs nothing
# beyond Xcode's command-line tools (docs/archive/330-macos-app.md).
#
#   scripts/macos/make-icon.sh
#
# Needs resvg (cargo install resvg, pure Rust; Homebrew's librsvg builds LLVM
# from source on an Intel Mac); iconutil ships with macOS.
set -euo pipefail

repo=${0:A:h:h:h}
src=$repo/assets/macos/icon.svg
out=$repo/assets/macos/Stev.icns

command -v resvg >/dev/null || { echo "needs resvg (cargo install resvg)" >&2; exit 1; }

work=$(mktemp -d)
trap 'rm -rf $work' EXIT
iconset=$work/Stev.iconset
mkdir $iconset

# The ten sizes iconutil expects: 16–512 pt, each at 1x and 2x.
for pt in 16 32 128 256 512; do
  resvg -w $pt -h $pt $src $iconset/icon_${pt}x${pt}.png
  resvg -w $((pt * 2)) -h $((pt * 2)) $src $iconset/icon_${pt}x${pt}@2x.png
done

iconutil -c icns $iconset -o $out
echo "wrote $out"
