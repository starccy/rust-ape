#!/usr/bin/env bash
#
# Verify every __ape_shim_* symbol the shim defines is actually referenced
# by a link_name redirect in patches/*.patch or by another shim file.

set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -f -r "$work"' EXIT

tok='__ape_shim_[A-Za-z0-9_]+[*#]?'

syms() { grep -hoE "$tok" "$@" 2>/dev/null | grep -v '[*#]$' | sort -u || true; }

shopt -s globstar
srcs=("$root"/shim/**/*.[ch])

for f in "${srcs[@]}"; do
    syms "$f" | sed "s|\$|\t${f#"$root"/shim/}|"
done | sort -u > "$work/pairs"

grep -h 'static' "${srcs[@]}" | syms /dev/stdin > "$work/static"

grep -h '^+' "$root"/patches/*.patch | syms /dev/stdin > "$work/patched"

bad=0

orphans=$(awk -F'\t' '
    FILENAME ~ /static$/  { st[$1] = 1; next }
    FILENAME ~ /patched$/ { pa[$1] = 1; next }
    { n[$1]++; file[$1] = $2 }
    END {
        for (s in n)
            if (n[s] == 1 && !(s in pa) && !(s in st))
                printf "  %s (shim/%s)\n", s, file[s]
    }
' "$work/static" "$work/patched" "$work/pairs" | sort)
if [ -n "$orphans" ]; then
    echo "orphaned shim symbols found:"
    echo "Add the #[cfg_attr(rust_ape_shim, link_name = ...)] redirect to the patched"
    echo "crate (edit vendor/patches/<crate> or vendor/library, then ./patches/regen.sh)"
    echo "or delete the dead code:"
    echo "$orphans"
    bad=1
fi

ghosts=$(comm -13 <(cut -f1 "$work/pairs" | sort -u) "$work/patched")
if [ -n "$ghosts" ]; then
    echo "patches redirect to these symbols but no shim file defines them:"
    echo "$ghosts" | sed 's/^/  /'
    bad=1
fi

[ "$bad" = 0 ] && echo "all shim symbols referenced"
exit "$bad"
