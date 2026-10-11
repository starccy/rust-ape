#!/bin/sh
# Regenerates ../../src/hostlibs/layout.rs from lp.h, with bindgen for each
# host's data model and then `cargo xtask host-bindings`. lp.h only needs
# the compiler's own headers, so no other host's libc is involved. The
# library itself is built by ../../lp-sys/build.rs.
set -e
cd "$(dirname "$0")"
here=$(pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
gen() {
    bindgen lp.h --allowlist-function 'lp_.*' --allowlist-type 'u|uf|um|hu|lp_hooks' -- --target="$1" > "$tmp/$2"
}
gen x86_64-unknown-linux-gnu lp-unix.rs
gen x86_64-pc-windows-msvc lp-win.rs
gen aarch64-apple-macos lp-mac.rs
lib=/usr/lib/rust-ape-layout-test
cd ../../..
cargo xtask host-bindings "$tmp/lp-unix.rs" --windows-input "$tmp/lp-win.rs" --macos-input "$tmp/lp-mac.rs" --name LP \
    --linux $lib/liblp-x86_64.so --linux $lib/liblp-aarch64.so \
    --macos $lib/liblp.dylib --windows $lib/lp.dll \
    --len lp_sum:arr=n --len lp_scale:arr=n --len lp_wname:buf=n -o "$here/../../src/hostlibs/layout.rs"
