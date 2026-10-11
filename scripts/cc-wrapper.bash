#!/usr/bin/env bash
# The C compiler cc-rs gets for the target: cosmocc, with a few dependency
# sources swapped for a variant of themselves. COSMO and ARCH can be
# overridden; the per-arch shims in generated/ (written by `cargo xtask
# setup`) set ARCH.

SDK_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COSMO="${COSMO:-$SDK_ROOT/vendor/cosmocc}"
ARCH="${ARCH:-x86_64}"
CC="$COSMO/bin/$ARCH-unknown-cosmo-cc"

args=()
src=
out=
prev=
for arg in "$@"; do
  [ "$prev" = -o ] && out=$arg
  prev=$arg
  case $ARCH:$arg in
    # aws-lc on arm64: cpucap.c followed by the Linux CPU feature probe,
    # which reads getauxval(AT_HWCAP) in Linux's bit layout. The kernel gives
    # that on Linux and the APE loader on macOS. The probe is only compiled
    # under OPENSSL_LINUX, which the rest of aws-lc must not see. On an
    # Apple M chip it also gets ARMV8_APPLE_M, which aws-lc's own macOS
    # probe sets and which picks its 8-way AES-GCM.
    aarch64:*/crypto/fipsmodule/cpucap/cpucap.c)
      dir=$(cd "$(dirname "$arg")" && pwd)
      src="#include \"$dir/cpucap.c\"
#define OPENSSL_LINUX
#include \"$dir/cpu_aarch64_linux.c\"
#include <string.h>
int sysctlbyname(const char *, void *, size_t *, void *, size_t);
__attribute__((__constructor__)) static void ape_apple_m(void) {
  char brand[64];
  size_t len = sizeof(brand);
  if (!sysctlbyname(\"machdep.cpu.brand_string\", brand, &len, 0, 0) &&
      !strncmp(brand, \"Apple M\", 7))
    OPENSSL_armcap_P |= ARMV8_APPLE_M;
}"
      args+=(@SRC@) ;;
    *)
      args+=("$arg") ;;
  esac
done

if [ -n "$src" ]; then
  file=${out:-cpucap.o}.src.c
  printf '%s\n' "$src" > "$file"
  args=("${args[@]/@SRC@/$file}")
fi
exec "$CC" "${args[@]}"
