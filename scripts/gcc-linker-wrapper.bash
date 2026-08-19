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
tiny=0

for o in "$@"; do
    case $o in
        "-mtiny") tiny=1;;
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

# In the default runtime the backtrace symbol loader yoinks zipos (it reads
# /zip/.symtab.*), so /zip support comes along for free. The tiny runtime has
# no such loader and --gc-sections throws zipos out, which also leaves the
# packed binary without a zip central directory, so `zip foo.com asset` says
# the file isn't an archive. Pull it back in; costs a few KB.
if [ "$tiny" = 1 ]; then
    args+=("-Wl,-u,zipos")
fi

# The Linux-personality shim (shim/*.c): the patched std/libc/errno crates
# resolve __ape_shim_* to it. Compiled here, cached per arch, so an edited
# shim is always what gets linked; the temp + mv keeps parallel links from
# reading a half-written object.
# tiny objects get their own cache names so switching modes never links a
# shim compiled for the other runtime
mode=
if [ "$tiny" = 1 ]; then
    mode="-tiny"
fi

for shim_src in "$SDK_ROOT"/shim/*.c; do
    shim_obj="$SDK_ROOT/generated/shim-$(basename "$shim_src" .c)-$ARCH$mode.o"
    stale=0
    # a regenerated tables.h must rebuild every object, not just edited .c files
    for dep in "$shim_src" "$SDK_ROOT"/shim/*.h; do
        [ ! -f "$shim_obj" ] || [ "$dep" -nt "$shim_obj" ] && stale=1
    done
    if [ "$stale" = 1 ]; then
        tmp=$(mktemp "$shim_obj.XXXXXX")
        # These replace cosmo-internal compilation units and need the
        # _COSMO_SOURCE-gated macros live before the -include'd
        # normalize.inc, which only a command-line define can arrange.
        # SYSDEBUG=1 keeps their STRACE/DATATRACE lines alive (the SDK
        # headers default it to 0, but libc.a was built with 1, so without
        # it a replaced member silently vanishes from --strace output).
        extra=
        case "$shim_src" in
            */commandv.c|*/fchdir-nt.c|*/mkntpath.c|*/mkntpathat.c|*/read.c|*/readlinkat-nt.c|*/realpath.c)
                extra="-D_COSMO_SOURCE"
                # the tiny runtime never prints strace lines, so leave
                # SYSDEBUG at its default 0 there and skip the dead code
                if [ "$tiny" = 0 ]; then
                    extra="$extra -DSYSDEBUG=1"
                fi ;;
        esac
        if [ "$tiny" = 1 ]; then
            extra="$extra -mtiny"
        fi
        "$COSMO/bin/$ARCH-unknown-cosmo-cc" -c -O2 -fno-stack-protector $extra \
            -o "$tmp" "$shim_src"
        mv -f "$tmp" "$shim_obj"
    fi
    args+=("$shim_obj")
done

$COSMO/bin/$ARCH-unknown-cosmo-cc "${args[@]}"
