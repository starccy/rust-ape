#!/usr/bin/env python3
"""Sweep every struct/union the patched libc crate declares and compare its
size against what cosmo's public headers say, to catch the whole
cosmo-outgrows-musl class of bugs (utsname 900v390, statfs 144v120,
pthread_attr_t 64v56) in one pass instead of one crashing project at a time.

Rust side: enumerate `pub struct`/`pub union` names in vendor/patches/libc,
generate a probe binary that prints size_of for each, prune the names that
don't exist on x86_64-musl by iterating on rustc errors, run it.

Cosmo side: compile scripts/struct-probe.c (a static file -- dynamically
generated C gets caught by the disk-encryption layer) with
-fno-eliminate-unused-debug-types and read every type's size out of DWARF.

Verdicts:
  DANGER   cosmo > musl: any out-param use writes past the caller's buffer
  SHORT    cosmo < musl: short writes, safe direction, listed for awareness
  KNOWN    size differs but the delta is understood (see ALLOWLIST)
  MISSING  cosmo's public headers have no such type (calls using it must be
           shimmed or unsupported; informational)
  ok       identical size (note: equal size does NOT prove equal field
           layout -- shim/layouts.c pins offsets for the curated ones)
"""

import re
import os
import subprocess
import sys
from typing import List, Dict
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LIBC_SRC = ROOT / "vendor/patches/libc/src"
PROBE_C = ROOT / "scripts/struct-probe.c"
WORK = ROOT / "target/struct-scan"
LIB_FILE = WORK / "src/lib.rs"
MAIN_FILE = WORK / "src/main.rs"
OBJ_FILE = WORK / "struct-probe.o"
COSMO_CC = ROOT / "vendor/cosmocc/bin/x86_64-unknown-cosmo-cc"
RUST_TARGET = "x86_64-unknown-linux-musl"

# Size mismatches that are understood and handled. Value is the reason shown
# in the report. Add entries only with an explanation of where the delta is
# absorbed.
ALLOWLIST = {
    "termios": "shim/termios.c repacks between musl and cosmo layouts",
    "sigaction": "shim/signal.c repacks via lin_sigaction, musl's shape",
}


def sh(cmd, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, **kw)


def enumerate_rust_names() -> List[str]:
    names = set()
    pat = re.compile(r"pub (?:struct|union) ([A-Za-z_][A-Za-z0-9_]*)")
    for f in LIBC_SRC.rglob("*.rs"):
        names.update(pat.findall(f.read_text(errors="replace")))
    return sorted(names)


def write_probe_crate(names: List[str]):
    WORK.mkdir(parents=True, exist_ok=True)
    (WORK / "src").mkdir(exist_ok=True)
    (WORK / "Cargo.toml").write_text(f"""\
[package]
name = "struct-scan-probe"
version = "0.0.0"
edition = "2024"

[dependencies]
libc = {{ path = "{LIBC_SRC.parent}", default-features = false }}

[workspace]
""")
    lines = [f"    acc += core::mem::size_of::<libc::{n}>();" for n in names]
    LIB_FILE.write_text(
        "#![no_std]\n#![allow(deprecated)]\n"
        "pub fn probe() -> usize {\n    let mut acc = 0usize;\n"
        + "\n".join(lines) + "\n    acc\n}\n")

    if MAIN_FILE.exists():
        MAIN_FILE.unlink()


def build_probe(extra=()):
    # -Zprint-type-sizes makes rustc report the
    # layout of every type probe() instantiates
    env = dict(os.environ, CARGO_INCREMENTAL="0")
    return sh(["cargo", "rustc", "--lib", "--target", RUST_TARGET,
               "--quiet", *extra, "--", "-Zprint-type-sizes"],
              cwd=WORK, env=env)


def rust_sizes(names: List[str]) -> Dict[str, int]:
    """
    Build the probe, dropping names rustc can't find, and read the sizes
    out of -Zprint-type-sizes
    """
    build_std = []
    r = None
    for _ in range(12):
        write_probe_crate(names)
        r = build_probe(build_std)
        if r.returncode == 0:
            break
        if re.search(r"can.t find crate for `(?:std|core)`", r.stderr) and not build_std:
            build_std = ["-Zbuild-std=core"]
            continue
        missing = set(re.findall(
            r"cannot find (?:type|value|struct, variant or union type) "
            r"`(\w+)`", r.stderr))
        missing |= set(re.findall(r"(?:struct|union|type) `(\w+)` is private",
                                  r.stderr))
        if not missing:
            sys.exit(f"probe build failed with no prunable names:\n{r.stderr}")
        names = [n for n in names if n not in missing]
    else:
        sys.exit("probe build did not converge after 12 prune rounds")

    wanted = set(names)
    out = {}
    for m in re.finditer(r"print-type-size type: `(?:libc::)?(\w+)`: (\d+) bytes",
                         r.stdout + r.stderr):
        if m.group(1) in wanted:
            out[m.group(1)] = int(m.group(2))
    if not out:
        sys.exit("no print-type-size output captured; rustc flag mismatch?")
    return out


def cosmo_sizes() -> Dict[str, int]:
    if not PROBE_C.exists():
        sys.exit(f"missing {PROBE_C}")
    r = sh(["sh", str(COSMO_CC), "-g", "-fno-eliminate-unused-debug-types",
            "-c", str(PROBE_C), "-o", str(OBJ_FILE)])
    if r.returncode != 0:
        sys.exit(f"cosmo compile failed:\n{r.stderr}")
    r = sh(["readelf", "--debug-dump=info", str(OBJ_FILE)])
    if r.returncode != 0:
        sys.exit(f"readelf failed: {r.stderr}")

    dies = {}
    cur = None
    die_re = re.compile(r"^ <\d+><([0-9a-f]+)>: Abbrev Number: \d+ \((DW_TAG_\w+)\)")
    attr_re = re.compile(r"^    <[0-9a-f]+>\s+(DW_AT_\w+)\s*:\s*(.*)$")
    for line in r.stdout.splitlines():
        m = die_re.match(line)
        if m:
            cur = {"tag": m.group(2), "name": None, "size": None, "ref": None}
            dies[int(m.group(1), 16)] = cur
            continue
        if cur is None:
            continue
        m = attr_re.match(line)
        if not m:
            continue
        key, val = m.group(1), m.group(2).strip()
        if key == "DW_AT_name":
            cur["name"] = val.split(": ")[-1].strip()
        elif key == "DW_AT_byte_size":
            try:
                cur["size"] = int(val)
            except ValueError:
                pass
        elif key == "DW_AT_type":
            rm = re.search(r"<0x([0-9a-f]+)>", val)
            if rm:
                cur["ref"] = int(rm.group(1), 16)

    if not dies:
        sys.exit("no DWARF DIEs parsed")

    def resolve(off, depth=0):
        if off is None or depth > 16 or off not in dies:
            return None
        d = dies[off]
        if d["size"] is not None:
            return d["size"]
        return resolve(d["ref"], depth + 1)

    out = {}
    for d in dies.values():
        if d["name"] is None:
            continue
        if d["tag"] in ("DW_TAG_structure_type", "DW_TAG_union_type"):
            if d["size"] is not None:
                out.setdefault(d["name"], d["size"])
    # typedefs fill in the names structs don't cover
    for d in dies.values():
        if d["tag"] == "DW_TAG_typedef" and d["name"] and d["name"] not in out:
            s = resolve(d["ref"])
            if s is not None:
                out[d["name"]] = s
    return out


def display_row(name: str, musl_size: int, cosmo_size: int, message: str):
    print(f"  {name:32} musl {musl_size:6}  cosmo {cosmo_size:6}  {message}")


def main():
    names = enumerate_rust_names()
    print(f"{len(names)} pub struct/union names found in the libc crate",
          file=sys.stderr)
    musl = rust_sizes(names)
    print(f"{len(musl)} of them exist on {RUST_TARGET}", file=sys.stderr)
    cosmo = cosmo_sizes()
    print(f"{len(cosmo)} named types in cosmo's DWARF", file=sys.stderr)

    danger, short, known, missing, ok, opaque = [], [], [], [], [], []
    for n, ms in sorted(musl.items()):
        cs = cosmo.get(n)
        if cs is None:
            missing.append(n)
        elif ms == 0:
            # zero-sized in the libc crate: an opaque pointer-only type
            # (fpos_t, timezone); callers cannot allocate one, so cosmo's
            # size is irrelevant
            opaque.append(n)
        elif cs == ms:
            ok.append(n)
        elif n in ALLOWLIST:
            known.append((n, ms, cs, ALLOWLIST[n]))
        elif cs > ms:
            danger.append((n, ms, cs))
        else:
            short.append((n, ms, cs))

    if danger:
        print("\nDANGER  cosmo > musl | out-params write past the caller:")
        for n, ms, cs in danger:
            display_row(n, ms, cs, f"(+{cs - ms})")
    if short:
        print("\nSHORT   cosmo < musl | short writes, safe direction:")
        for n, ms, cs in short:
            display_row(n, ms, cs, f"({cs - ms})")
    if known:
        print("\nKNOWN   size differs, handled elsewhere:")
        for n, ms, cs, why in known:
            display_row(n, ms, cs, why)
    print(f"\nok: {len(ok)} identical, opaque pointer-only: {len(opaque)}, "
          f"missing in cosmo headers: {len(missing)}")
    if "-v" in sys.argv:
        print("\nmissing:", " ".join(missing))
    sys.exit(1 if danger else 0)


if __name__ == "__main__":
    main()
