#!/bin/zsh
# Builds the downloadable macOS app: a universal (x86_64 + aarch64) Stev.app,
# ad-hoc signed, zipped for a release (docs/archive/330-macos-app.md, phase 1).
#
#   scripts/macos/bundle.sh [--version x.y.z]
#
# Writes target/macos/Stev.app and target/macos/Stev-<version>-macos-universal.zip.
# The version comes from Cargo.toml; --version (the release workflow passes the
# tag) must agree with it, or the script fails before building anything.
# Needs Xcode's command-line tools (lipo, codesign, plutil, ditto).
set -euo pipefail

repo=${0:A:h:h:h}
cargo_version=$(sed -n 's/^version = "\(.*\)"$/\1/p' $repo/Cargo.toml | head -n 1)
version=$cargo_version

while (( $# > 0 )); do
  case $1 in
    --version) version=${2:?--version needs a value}; shift 2 ;;
    *) sed -n '5,10p' "$0"; exit 1 ;;
  esac
done

if [[ $version != $cargo_version ]]; then
  echo "version $version doesn't match Cargo.toml's $cargo_version" >&2
  exit 1
fi

targets=(x86_64-apple-darwin aarch64-apple-darwin)
out_dir=$repo/target/macos
app=$out_dir/Stev.app
zip=$out_dir/Stev-$version-macos-universal.zip

# The Apple targets stay out of rust-toolchain.toml, which would make the Linux
# and Windows CI jobs download them for nothing.
rustup target add $targets

# 11.0 is the floor for aarch64 anyway, so both slices share it, matching
# LSMinimumSystemVersion in the plist.
export MACOSX_DEPLOYMENT_TARGET=11.0
for target in $targets; do
  cargo build --manifest-path $repo/Cargo.toml --release --package stev --bin stev --target $target
done

rm -rf $app $zip
mkdir -p $app/Contents/MacOS $app/Contents/Resources

# Named Stev, not stev: winit titles the app menu's items with the
# executable's name ("About Stev", "Quit Stev").
lipo -create -output $app/Contents/MacOS/Stev $repo/target/${^targets}/release/stev
sed "s/@VERSION@/$version/g" $repo/assets/macos/Info.plist.in > $app/Contents/Info.plist

# The licences travel with the binary: Stev's own, its dependencies', the
# compiled-in Plex font's, and the VST Compatible logo's (shown in About).
res=$app/Contents/Resources
cp $repo/assets/macos/Stev.icns $res/
cp $repo/LICENSE-MIT $repo/LICENSE-APACHE $repo/THIRD-PARTY-NOTICES.md $res/
cp $repo/assets/fonts/IBMPlexSans/OFL.txt $res/IBMPlexSans-OFL.txt
cp $repo/docs/images/vst/LICENSE.txt $res/VST-Compatible-Logo-LICENSE.txt

# Required, not cosmetic: Apple Silicon won't run an unsigned arm64 binary,
# and lipo invalidates the linker's own signature. Ad-hoc signing doesn't make
# Gatekeeper trust the app (that needs a Developer ID); it lets it run at all.
codesign --force --sign - $app

archs=$(lipo -archs $app/Contents/MacOS/Stev)
for arch in x86_64 arm64; do
  [[ " $archs " == *" $arch "* ]] || { echo "binary lacks $arch (has: $archs)" >&2; exit 1; }
done
codesign --verify --deep --strict $app
plutil -lint $app/Contents/Info.plist

# ditto, not zip, which loses the bundle's extended attributes and symlinks.
ditto -c -k --keepParent $app $zip
echo "wrote $app ($archs)"
echo "wrote $zip"
