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

shopt -s globstar nullglob
shim_hdrs=("$SDK_ROOT"/shim/**/*.h)
for shim_src in "$SDK_ROOT"/shim/**/*.c; do
    # subdirectory sources keep their directory in the cache name
    # (procfs/core/tree.c -> shim-procfs-core-tree-...) so basenames cannot collide
    rel=${shim_src#"$SDK_ROOT"/shim/}
    rel=${rel%.c}
    shim_obj="$SDK_ROOT/generated/shim-${rel//\//-}-$ARCH$mode.o"
    stale=0
    # a regenerated tables.h must rebuild every object, not just edited .c files
    for dep in "$shim_src" "${shim_hdrs[@]}"; do
        [ ! -f "$shim_obj" ] || [ "$dep" -nt "$shim_obj" ] && stale=1
    done
    if [ "$stale" = 1 ]; then
        tmp=$(mktemp "$shim_obj.XXXXXX")
        # A unit replacing a cosmo-internal compilation unit declares its
        # own flags on a "// cflags:" line, since _COSMO_SOURCE must be
        # live before the -include'd normalize.inc and only a command-line
        # define can arrange that. SYSDEBUG=1 keeps its STRACE lines
        # alive the way libc.a was built; the tiny runtime never prints
        # them and skips the dead code.
        extra=$(sed -n 's|^// cflags: ||p' "$shim_src")
        case "$shim_src" in
            */dlmalloc.c)
                # the allocator is built the way upstream builds it:
                # freestanding, -O3, and with general registers only so a
                # malloc reached from any context never touches vector state
                extra="-D_COSMO_SOURCE -ffreestanding -fdata-sections -ffunction-sections"
                if [ "$tiny" = 0 ]; then
                    extra="$extra -O3 -mgeneral-regs-only"
                fi ;;
        esac
        if [ "$tiny" = 0 ] && [[ "$extra" == *_COSMO_SOURCE* ]]; then
            extra="$extra -DSYSDEBUG=1"
        fi
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
