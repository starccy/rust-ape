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

# A -l left on rustc's link line is a library some crate links
# dynamically, since static ones are bundled into the rlibs (or one built
# with -bundle, which sits in the crate's own -L directory). cosmo has no
# dynamic linking, so each one cosmo doesn't have itself is replaced by an
# archive of stubs that load the host's copy at runtime, see
# xtask/src/host/link/. libc's own pieces stay with cosmo. The system's
# library directories that pkg-config hands out are only searched for
# the shared objects: their static archives are built for glibc, and ld
# would take them over cosmo's own libc.a, libdl.a and so on.
is_sysdir() {
    case $1 in
        /lib|/lib/*|/lib64|/lib64/*|/usr/lib|/usr/lib/*|/usr/lib64|/usr/lib64/*|/usr/local/lib|/usr/local/lib/*) return 0;;
    esac
    return 1
}
declare -A hostlib=()
hostnames=()
search=()
prev=
for o in "$@"; do
    if [ "$prev" = "-L" ]; then
        search+=("$o")
    else
        case $o in
            -L?*) search+=("${o#-L}");;
        esac
    fi
    prev=$o
done
for o in "$@"; do
    case $o in
        -l?*)
            name=${o#-l}
            case $name in
                c|m|dl|pthread|rt|util|resolv|crypt|anl|nsl|mvec|gcc|gcc_s|gcc_eh|atomic|stdc++|c++|c++abi|unwind) continue;;
            esac
            [ -e "$COSMO/$ARCH-linux-cosmo/lib/lib$name.a" ] && continue
            local_archive=0
            for dir in "${search[@]}"; do
                if ! is_sysdir "$dir" && [ -e "$dir/lib$name.a" ]; then
                    local_archive=1
                fi
            done
            [ "$local_archive" = 1 ] || hostnames+=("$name");;
    esac
done
if [ ${#hostnames[@]} -gt 0 ] && [ -n "${RUST_APE_XTASK:-}" ]; then
    while IFS=$'\t' read -r name archive; do
        hostlib[$name]=$archive
    done < <("$RUST_APE_XTASK" hostlink --arch "$ARCH" \
        --cc "$COSMO/bin/$ARCH-unknown-cosmo-cc" \
        --ar "$SDK_ROOT/generated/ar-$ARCH.bash" \
        --out-dir "$SDK_ROOT/generated/hostlink" \
        "${search[@]/#/-L}" -- $(printf '%s\n' "${hostnames[@]}" | sort -u))
fi

ldir=0
for o in "$@"; do
    if [ "$ldir" = 1 ]; then
        ldir=0
        is_sysdir "$o" || args+=("-L" "$o")
        continue
    fi
    case $o in
        "-L")
            # the directory is the next argument
            ldir=1
            continue;;
        -L?*)
            is_sysdir "${o#-L}" && continue;;
        "-mtiny") tiny=1;;
        "-lunwind") continue;;
        "-Wl,-Bdynamic") continue;;
        "-Wl,-Bstatic") continue;;
        "-Wl,--strip-all" | "-Wl,-s" | "-Wl,-S" | "-Wl,--strip-debug") continue;;
        -l?*)
            if [ -n "${hostlib[${o#-l}]:-}" ]; then
                args+=("${hostlib[${o#-l}]}")
                continue
            fi;;
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

# The fork's /proc and /sys emulation (libc/procfs/) is reached through weak
# references, so nothing pulls it in unless something names the anchor. Every
# binary gets it: std's current_exe and the monitoring crates read /proc.
args+=("-Wl,-u,procfs")

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
    # (a/b.c -> shim-a-b-...) so basenames cannot collide
    rel=${shim_src#"$SDK_ROOT"/shim/}
    rel=${rel%.c}
    shim_obj="$SDK_ROOT/generated/shim-${rel//\//-}-$ARCH$mode.o"
    stale=0
    # an edited header must rebuild every object, not just edited .c files
    # a repackaged toolchain can change the structs the shim indexes into
    for dep in "$shim_src" "${shim_hdrs[@]}" "$SDK_ROOT/vendor/cosmocc/$ARCH-linux-cosmo/lib/libcosmo.a"; do
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
        if [ "$tiny" = 0 ] && [[ "$extra" == *_COSMO_SOURCE* ]]; then
            extra="$extra -DSYSDEBUG=1"
        fi
        if [ "$tiny" = 1 ]; then
            extra="$extra -mtiny"
        fi
        "$COSMO/bin/$ARCH-unknown-cosmo-cc" -c -O2 -fno-stack-protector \
            -ffunction-sections -fdata-sections $extra \
            -o "$tmp" "$shim_src"
        mv -f "$tmp" "$shim_obj"
    fi
    args+=("$shim_obj")
done

$COSMO/bin/$ARCH-unknown-cosmo-cc "${args[@]}"
