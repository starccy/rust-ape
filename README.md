# rust-ape

[![CI](https://github.com/starccy/rust-ape/actions/workflows/ci.yml/badge.svg)](https://github.com/starccy/rust-ape/actions/workflows/ci.yml)

Build Rust programs into Cosmopolitan APE binaries: One file that runs on
Linux, macOS, Windows and BSD, on both x86-64 and arm64.

```console
$ cargo xtask build --project hello
==> apelink -> hello/target/ape/hello.com

$ ./hello/target/ape/hello.com          # Linux
hello from rust-ape, running on Linux

C:\> .\hello.com                        # the same file, on Windows
hello from rust-ape, running on Windows
```

## What this is

[Cosmopolitan Libc](https://github.com/jart/cosmopolitan) is Justine Tunney's C
library that compiles to an **Actually Portable Executable**: a single file that
is simultaneously a valid PE, ELF and Mach-O, carries native code for x86-64 and
arm64, and runs unmodified on six operating systems, with no VM, no interpreter
and no per-platform builds.

This project makes that available from Rust. It installs and drives the
toolchain with a patched standard library, target specifications, linker wrappers,
and a small crate for the APIs cosmo offers beyond libc, so that you can write
ordinary Rust and get one binary out.

Basically it works like an out-of-tree Rust target: all the pieces an official
target would ship, but maintained outside the compiler.

## Getting started

You need a Linux host with `rustup`. The toolchain itself only runs on Linux,
but the binaries it produces run everywhere.

**1. Install the toolchain.**

```shell
cargo xtask setup
```

This downloads the `rust-src` for the nightly pinned in `rust-toolchain.toml`
(reusing your rustup copy if you have one), fetches the several crates that need
cosmo-specific patches, applies every patch in `patches/`, and unpacks cosmocc.
Everything lands under `vendor/` and `cache/` in this directory.

You only need to re-run it when a patch, the cosmocc version, or the pinned
nightly changes. Otherwise it is a no-op.

> **Keep this directory where it is.** Generated projects reference
> `vendor/patches/` and `ape/` by absolute path, so moving the SDK breaks
> projects you already created. Re-running `setup` fixes the SDK itself, but you
> would have to update those projects' `Cargo.toml` by hand.

**2. Create a project.**

```sh
cargo xtask generate /path/to/project
```

Its behavior is very similar to `cargo new`. You get a normal cargo project,
plus a `[patch.crates-io]` section wiring it to the adapted crates and
a `rust-toolchain.toml` matching the SDK. Write Rust the
way you normally would, **with the caveats below**.

**3. Build it.**

```sh
cargo xtask build --project /path/to/project
```

Both architectures get compiled and `apelink` fuses them into one file under
`target/ape/`. Every `[[bin]]` in the package is packed; use `--bin` to pick
one, `--release` for an optimized build.

Plain `cargo build` inside the project will **not** work. The build needs a
patched std via `-Z build-std`, two `--cfg` flags, and `CC`/`CXX`/`AR` pointing
at cosmocc. `cargo xtask build` sets all of that up.

**4. Run it.** Copy the `.com` file to any supported machine and execute it. On
Unix you need the `./` prefix (or you might use `sh -c ./name.com`, see [this](https://github.com/jart/cosmopolitan#shells));
on Windows, `.\name.com`.

## What works

Everything in `examples/` is a self-contained scenario that exits non-zero on
failure. Including TCP/UDP, blocking and async I/O, clocks, hashing, and an HTTPS
client. CI builds them **once on Linux** and runs those same files on five platforms:

| | x86-64 | arm64 |
| --- | --- | --- |
| Linux | ✅ | ✅ |
| Windows | ✅ | ✅ |
| macOS | out of scope | ✅ |

Intel Macs would in fact work since cosmo supports them, and the same file already
carries x86-64 code. They are left out on purpose: Apple stopped shipping them
in 2023 and the platform is dying out, so it doesn't seem worth a CI lane
(for the same reason there is no Windows 7 lane).

Dependencies with C, C++ or hand-written assembly work: the examples link
`ring` (30 native objects) and `blake3`'s SIMD backends, both compiled by
cosmocc.

## Limits and gotchas

**Prefer `ape::` over `std::` where they overlap.** `std::env::current_exe()`
returns the APE loader rather than your program, and on Windows it fails
outright looking for `/proc/self/exe`. Use `ape::program_executable_name()`.

**Async means smol, not tokio.** Cosmopolitan
[removed epoll entirely in 2024](https://github.com/jart/cosmopolitan/commit/2ec413b5a9b5d88d363cf5657a8c3ddce4d7feb1)
— the author's reasoning was that it could not be made to work on XNU and the
BSDs, and that `poll`/`select` should be done well instead. mio picks epoll by
`target_os` with no way to override it, so tokio cannot run here. smol's
`polling` crate has an escape hatch (`--cfg polling_test_poll_backend`) that
forces the poll backend, which is what this project uses.

**No async subprocesses, on any platform.** async-process needs SIGCHLD or
pidfd/waitid to reap children, and cosmo has neither everywhere, so
`async-process.patch` disables its driver. Synchronous `std::process::Command`
works fine, pipes included.

**Anonymous temp files fail on Windows.** `tempfile::tempfile()` not works on
Windows. The good news is it returns `Err` rather than crashing, see [temp_files.rs](./examples/src/bin/temp_files.rs).
`NamedTempFile` and `tempdir()` are fine.

**Runtime CPU detection is blind off Linux on arm64.** `AT_HWCAP` reads as 0
there, so crates that dispatch on it fall back to scalar code: correct, slower.
x86 is unaffected, since CPUID is a hardware instruction.

### Why things break in this particular way

Almost every problem above has the same root cause, and knowing it helps a lot
when debugging.

Rust compiles for a fixed target: it's `x86_64-unknown-linux-musl` here and bakes
platform constants in at compile time. `libc::EBADF` becomes Linux's 9.
Cosmopolitan resolves those same names **at runtime**, to whatever the host
uses, and on Windows `EBADF` is 6. Any code comparing `raw_os_error()` against a
`libc::` constant therefore takes the wrong branch on every OS except Linux.

It is not only errno: `SOCK_CLOEXEC`, the whole `POLL*` family, `SOL_SOCKET` and
the `SO_*` options, `MSG_NOSIGNAL` etc. are all runtime values under cosmo, but
compile-time constants as far as std is concerned. That is what `patches/` is
for, and each hunk has a `cosmo:` comment explaining why it's needed.

These failures are usually silent. A wrong `POLL*` bit makes `poll()` wait on the
wrong event, so `connect_timeout` reports success and then reads zero bytes,
which surfaces much later as a TLS handshake failing with `UnexpectedEof`. If
something works on Linux but misbehaves elsewhere, check compile-time constants
first before suspecting the syscall itself.

This list only covers what has been hit so far. Cosmopolitan has a large surface
area, and so does std; expect to find more.

## Notes

Some design issues that you should know about:

**Patching std pins the whole project to one dated nightly.** The patch applies
to `rust-src` from exactly that toolchain, and upgrading Rust means redoing it.
There is no alternative for now: std freezes those platform constants when it
is compiled, and a target specification cannot change them.

**Nightly is required.** `-Z build-std` is the only way to build a patched std,
and it has been unstable for years.

**cargo cannot see that build-std's sources changed.** std and libc are not
local packages as far as it is concerned, so editing `vendor/library` changes
nothing until something else forces a rebuild. `xtask` keeps a stamp and clears
the build-std cache itself.

**This is not a target you can `rustup target add`.** An official target is a
name you pass to cargo; this one is a directory you have to keep, plus a command
wrapping cargo. `cargo build` in a generated project does not work and cannot
be made to work.

**Generated projects are tied to where the SDK sits.** Their `Cargo.toml`
points at `vendor/patches/` and `ape/` by absolute path, so moving this
directory breaks every project made before the move. Relative paths would just
break in a different way, so neither option is great.

**Every project carries all five patch entries** whether it depends on those
crates or not, because the alternative is asking people to figure out which
ones they need. It is cheap, but it does mean a scaffolded `Cargo.toml` starts
with a block of paths that may have nothing to do with the program.

## Layout

| | |
| --- | --- |
| `patches/` | Every diff against upstream, with rationale. See its README |
| `ape/` | Safe wrappers for cosmo's non-libc APIs (host, CPU, memory, paths) |
| `xtask/` | Core tools for `setup`, `generate` and `build` |
| `targets/` | Target specification templates, rendered into `generated/` |
| `scripts/` | Linker and archiver wrappers around cosmocc |
| `examples/` | Scenario binaries, also the cross-platform test suite |
| `vendor/`, `cache/` | Created by `setup`, not checked in |

## Thanks

* [jart/cosmopolitan](https://github.com/jart/cosmopolitan): the foundation
  this project is built on.
* [ahgamut/rust-ape-example](https://github.com/ahgamut/rust-ape-example): an
  early demonstration of building Rust code with cosmocc, and where the idea
  of this project came from.
