#!/usr/bin/env bash
#
# Regenerate the patches in this directory from the current vendor/ tree.
#
# Edit vendor/patches/<crate> by hand, run this, and the
# corresponding .patch is rewritten. Timestamps are stripped so unrelated runs
# don't churn the files.
#
# Nothing is written until it round-trips: applying the fresh patch to a
# pristine upstream copy has to reproduce vendor/ byte for byte. If it doesn't,
# the old patch is left alone and this exits non-zero.
#
#   ./patches/regen.sh              # everything vendor/.stamps knows about
#   ./patches/regen.sh errno        # just one
#
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT


vendored() {
    echo "$root/vendor/patches/$1"
}

# Lay down the pristine upstream copy at $work/a/<name>.
pristine() {
    local name=$1
    mkdir -p "$work/a"
    # The stamp records the exact version that was unpacked, e.g.
    # "async-io-2.6.0 patch:<sha>", which is also the .crate filename.
    local stamp="$root/vendor/.stamps/$name" nv
    [ -f "$stamp" ] || { echo "no vendor/.stamps/$name. run 'cargo xtask setup'" >&2; exit 1; }
    nv=$(awk '{print $1}' "$stamp")
    tar -xzf "$root/cache/$nv.crate" -C "$work/a"
    mv "$work/a/$nv" "$work/a/$name"
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

# Only the patches that already exist are regenerated. Going by what sits
# under vendor/ instead would turn any leftover checkout there into a new
# patch, and this project does not patch third-party crates.
names=("$@")
if [ ${#names[@]} -eq 0 ]; then
    names=()
    for p in "$root"/patches/*.patch; do
        names+=("$(basename "$p" .patch)")
    done
fi
for n in "${names[@]}"; do
    [ -f "$root/patches/$n.patch" ] || {
        echo "no patches/$n.patch; new patches are not created here" >&2
        exit 1
    }
done

for n in "${names[@]}"; do regen "$n"; done
echo "all regenerated patches round-trip"
