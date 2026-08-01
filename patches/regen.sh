#!/usr/bin/env bash
#
# Regenerate the patches in this directory from the current vendor/ tree.
#
# Edit vendor/library or vendor/patches/<crate> by hand, run this, and the
# corresponding .patch is rewritten. Timestamps are stripped so unrelated runs
# don't churn the files.
#
# Nothing is written until it round-trips: applying the fresh patch to a
# pristine upstream copy has to reproduce vendor/ byte for byte. If it doesn't,
# the old patch is left alone and this exits non-zero.
#
#   ./patches/regen.sh              # everything vendor/.stamps knows about
#   ./patches/regen.sh async-io     # just one
#
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

channel=$(sed -n 's/^ *channel *= *"\([^"]*\)".*/\1/p' "$root/rust-toolchain.toml")
[ -n "$channel" ] || { echo "could not read the channel out of rust-toolchain.toml" >&2; exit 1; }

vendored() {
    case $1 in
        library) echo "$root/vendor/library" ;;
        *) echo "$root/vendor/patches/$1" ;;
    esac
}

# Lay down the pristine upstream copy at $work/a/<name>.
pristine() {
    local name=$1
    mkdir -p "$work/a"
    if [ "$name" = library ]; then
        local src
        src="$(rustc "+$channel" --print sysroot)/lib/rustlib/src/rust/library"
        [ -d "$src" ] || {
            echo "no rust-src for $channel. try run 'rustup component add rust-src --toolchain $channel'" >&2
            exit 1
        }
        cp -a "$src" "$work/a/library"
    else
        # The stamp records the exact version that was unpacked, e.g.
        # "async-io-2.6.0 patch:<sha>", which is also the .crate filename.
        local stamp="$root/vendor/.stamps/$name" nv
        [ -f "$stamp" ] || { echo "no vendor/.stamps/$name. run 'cargo xtask setup'" >&2; exit 1; }
        nv=$(awk '{print $1}' "$stamp")
        tar -xzf "$root/cache/$nv.crate" -C "$work/a"
        mv "$work/a/$nv" "$work/a/$name"
    fi
}

regen() {
    local name=$1
    local out="$work/$name.patch"
    rm -rf "$work/a" "$work/b" "$work/rt"
    pristine "$name"
    mkdir -p "$work/b"
    cp -a "$(vendored "$name")" "$work/b/$name"

    # diff exits 1 when the files differ; that's expected here.
    (cd "$work" && diff -ruN "a/$name" "b/$name" || true) |
        perl -pe 's/^(---|\+\+\+) ([^\t]*)\t.*/$1 $2/; s/^diff -ruN? /diff /' >"$out"

    if [ ! -s "$out" ]; then
        printf '%-16s no local changes, leaving the existing patch alone\n' "$name"
        return
    fi

    mkdir -p "$work/rt"
    cp -a "$work/a/$name" "$work/rt/$name"
    (cd "$work/rt" && patch -p1 -s --no-backup-if-mismatch <"$out")
    if ! diff -ru "$work/rt/$name" "$(vendored "$name")" >/dev/null; then
        echo "$name: the generated patch does not reproduce vendor/, refusing to write it" >&2
        exit 1
    fi

    cp "$out" "$root/patches/$name.patch"
    printf '%-16s %s lines\n' "$name" "$(wc -l <"$out")"
}

names=("$@")
if [ ${#names[@]} -eq 0 ]; then
    names=(library)
    for stamp in "$root"/vendor/.stamps/*; do
        n=$(basename "$stamp")
        # cosmocc is used as shipped; library is already first.
        case $n in library | cosmocc) continue ;; esac
        names+=("$n")
    done
fi

for n in "${names[@]}"; do regen "$n"; done
echo "all regenerated patches round-trip"
