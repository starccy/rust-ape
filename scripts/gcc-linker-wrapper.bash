#!/usr/bin/env bash
# rustc's linker: drop the flags cosmo can't take, add a few symbol aliases,
# hand the rest to cosmocc. COSMO and ARCH can be overridden; the per-arch
# shims in generated/ (written by `cargo xtask setup`) set ARCH.

set -eu

SDK_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COSMO="${COSMO:-$SDK_ROOT/vendor/cosmocc}"
ARCH="${ARCH:-x86_64}"

declare -a args

args=()

for o in "$@"; do
    case $o in
        "-lunwind") continue;;
        "-Wl,-Bdynamic") continue;;
        "-Wl,-Bstatic") continue;;
        "-Wl,--strip-all" | "-Wl,-s" | "-Wl,-S" | "-Wl,--strip-debug") continue;;
    esac
    args+=("$o")
done

# glibc 2.38+ headers pull in __isoc23_* aliases under C23 (gnu-gcc-built deps
# like aws-lc drag them along); cosmo has none, so point them at the plain ones.
args+=("-Wl,--defsym,__isoc23_strtol=strtol")
args+=("-Wl,--defsym,__isoc23_sscanf=sscanf")

# std's pidfd.rs references waitid and cosmo has no such symbol, so anything
# built against this std fails to link unless --gc-sections happens to drop it.
# Alias it to cosmo's enosys; pidfd is unusable here anyway, so this only ever
# has to satisfy the linker.
args+=("-Wl,--defsym,waitid=enosys")

# The Linux-personality shim (shim/*.c): the patched std/libc/errno crates
# resolve __ape_shim_* to it. Compiled here, cached per arch, so an edited
# shim is always what gets linked; the temp + mv keeps parallel links from
# reading a half-written object.
for shim_src in "$SDK_ROOT"/shim/*.c; do
    shim_obj="$SDK_ROOT/generated/shim-$(basename "$shim_src" .c)-$ARCH.o"
    stale=0
    # a regenerated tables.h must rebuild every object, not just edited .c files
    for dep in "$shim_src" "$SDK_ROOT"/shim/*.h; do
        [ ! -f "$shim_obj" ] || [ "$dep" -nt "$shim_obj" ] && stale=1
    done
    if [ "$stale" = 1 ]; then
        tmp=$(mktemp "$shim_obj.XXXXXX")
        "$COSMO/bin/$ARCH-unknown-cosmo-cc" -c -O2 -fno-stack-protector \
            -o "$tmp" "$shim_src"
        mv -f "$tmp" "$shim_obj"
    fi
    args+=("$shim_obj")
done

$COSMO/bin/$ARCH-unknown-cosmo-cc "${args[@]}"
