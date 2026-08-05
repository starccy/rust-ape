# rust-ape

[![CI](https://github.com/starccy/rust-ape/actions/workflows/ci.yml/badge.svg)](https://github.com/starccy/rust-ape/actions/workflows/ci.yml)

Build Rust programs into Cosmopolitan APE binaries: One file that runs on
Linux, macOS, Windows and BSD, on both x86-64 and arm64.

> **Status: experimental.** Only the breakage I've run into myself is fixed;
> everything beyond that is untested territory. Not for production use.

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
ordinary Rust ([**with some limitations**](#limits-and-gotchas), for
sufficiently large values of "some" 🙃) and get one binary out.

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

To confirm that worked, you can just build the bundled examples now:

```sh
cargo xtask build --project examples
./examples/target/ape/platform.com
```

That leaves all the binaries in `examples/target/ape/`, each one a scenario you
can copy to another machine and run. They double as the test suite; see
[What works](#what-works).

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
one, `--release` for an optimized build. Feature selection works like it does
in `cargo build`: `--features` (`-F`), `--all-features` and
`--no-default-features` are passed through as-is.

Plain `cargo build` inside the project will **not** work. The build needs a
patched std via `-Z build-std`, two `--cfg` flags, and `CC`/`CXX`/`AR` pointing
at cosmocc. `cargo xtask build` sets all of that up.

**4. Run it.**

Copy the `.com` file to any supported machine and execute it. On
Unix you need the `./` prefix (or you might use `sh -c ./name.com` in some situations, see [this](https://github.com/jart/cosmopolitan#shells));
on Windows, `.\name.com`.

## What works

Everything in `examples/` is a self-contained scenario that exits non-zero on
failure. Including TCP/UDP server/client, blocking and async I/O, clocks, hashing,
and an HTTPS client and so on. Build them all with `cargo xtask build --project examples`;
they are the shortest way to see what does and doesn't work here.
CI builds them **once on Linux** and runs those same files on five platforms:

| | x86-64 | arm64 |
| --- | --- | --- |
| Linux | ✅ | ✅ |
| Windows | ✅ | ✅ |
| macOS | untested | ✅ |
| others | untested | untested |

> The untested cells would probably work. However, the
> [compile-time-constant mismatches](#why-things-break-in-this-particular-way)
> that break things here are per-platform, so those platforms may fail in ways
> the tested ones no longer do. I rarely use them and don't plan to test them;
> reports welcome.

## Limits and gotchas

### Gotchas you can work around

**Prefer `ape::` over `std::` where they overlap.** `std::env::current_exe()`
returns the APE loader rather than your program, and on Windows it fails
outright looking for `/proc/self/exe`. Use `ape::program_executable_name()`.

**Async means smol, not tokio.** Cosmopolitan
[dropped epoll in 2024](https://github.com/jart/cosmopolitan/commit/2ec413b5a9b5d88d363cf5657a8c3ddce4d7feb1),
and mio hardwires epoll on Linux targets, so tokio cannot run here. smol works
because its `polling` crate leaves a compile-time escape hatch
(`--cfg polling_test_poll_backend`) that switches it to plain `poll`, which
this project sets.

**C dependencies must be compiled from source.** Vendored C, C++ or
hand-written assembly built through the `cc` crate works: the examples link
`ring` (30 native objects) and `blake3`'s SIMD backends, both compiled by
cosmocc. What does not work: `-sys` crates that expect a prebuilt system
library (e.g. openssl-sys, which will find none for this target).

### Known broken, with no fix yet

What follows is only what has been hit so far, not a complete inventory.

**No async subprocesses, on any platform.** async-process needs SIGCHLD or
pidfd/waitid to reap children, and cosmo has neither everywhere, so
`async-process.patch` disables its driver. Synchronous `std::process::Command`
works fine, pipes included.

**Anonymous temp files fail on Windows.** `tempfile::tempfile()` does not work
on Windows. The good news is it returns `Err` rather than crashing, see [temp_files.rs](./examples/src/bin/temp_files.rs).
`NamedTempFile` and `tempdir()` are fine.

**Runtime CPU detection is blind off Linux on arm64.** `AT_HWCAP` reads as 0
there, so crates that dispatch on it fall back to scalar code: correct, slower.
x86 is unaffected, since CPUID is a hardware instruction.

### Will it port? A cheat sheet

To size up an existing project (or a design you're about to start), scan its
dependency tree (`cargo tree`) against this table before investing time:

| If it involves | Verdict |
| --- | --- |
| tokio, mio, or anything else epoll-only | ❌ no epoll under cosmo, and mio can't be told to use anything else |
| spawning processes from async (async-process) | ❌ disabled here; sync `std::process::Command` is the only way |
| `-sys` crates that link a prebuilt system library (openssl-sys, …) | ❌ no such library exists for this target |
| C/C++/asm vendored in the crate, built via `cc` | ⚠️ works if the code sticks to APIs cosmo has: `ring` and `blake3` do, OpenSSL's `dladdr()` use doesn't |
| comparing `raw_os_error()` (or any raw OS value) against `libc::` constants | ⚠️ compiles, then silently misbehaves off Linux. see below |
| smol, async-io, rustls, and pure-Rust crates in general | ✅ works, some via `patches/` |

### Why things break in this particular way

Almost every problem above has the same root cause: Rust's std pins platform
constants at compile time for a fixed target (`x86_64-unknown-linux-musl`
here), while Cosmopolitan resolves them **at runtime** to whatever the host
uses. For example, `libc::EBADF` compiles to Linux's 9, but on Windows the real value is 6,
so code comparing `raw_os_error()` against `libc::` constants takes the wrong
branch on every OS except Linux, and the same goes for flag constants in
general. Nearly everything under `patches/` exists to fix exactly this.

The coverage is certainly incomplete. If something works on Linux but hangs or
errors on another platform, suspect a compile-time constant first.

## Notes

Some design issues that you should know about:

**Nightly is required.** `-Z build-std` is the only way to build a patched std,
and it has been unstable for years.

**The patches are tied to one exact Rust version.** They apply to the
`rust-src` of the pinned nightly, so upgrading Rust means redoing every patch.

**Generated projects reference this directory by absolute path.** Moving this
directory, or taking a generated project to another machine, breaks its build
until the paths in its `Cargo.toml` are fixed up.

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
