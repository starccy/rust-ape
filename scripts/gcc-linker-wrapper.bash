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

$COSMO/bin/$ARCH-unknown-cosmo-cc "${args[@]}"
