# rust-ape

[![CI](https://github.com/starccy/rust-ape/actions/workflows/ci.yml/badge.svg)](https://github.com/starccy/rust-ape/actions/workflows/ci.yml)

Build Rust programs into Cosmopolitan APE binaries: One file that runs on
Linux, macOS, Windows and BSD, on both x86-64 and arm64.

> **Status: experimental.** Only the breakage I've run into myself is fixed;
> everything beyond that is untested territory. Not for production use.

```console
$ cargo xtask build hello
==> apelink -> hello/target/ape/hello.com

$ ./hello/target/ape/hello.com          # Linux
hello from rust-ape, running on Linux

C:\> .\hello.com                        # the same file, on Windows
hello from rust-ape, running on Windows
```

## In case you haven't heard of Cosmopolitan

[Cosmopolitan Libc](https://github.com/jart/cosmopolitan) is Justine Tunney's C
library that compiles to an **Actually Portable Executable**: a single file that
is simultaneously a valid PE, ELF and Mach-O, carries native code for x86-64 and
arm64, and runs unmodified on six operating systems, without a VM or an
interpreter.

This project makes that available from Rust. It installs and drives the
toolchain with a patched standard library, target specifications, linker wrappers,
and a small crate for the APIs cosmo offers beyond libc, so that you can write
ordinary Rust ([**with some limitations**](#limitations), for
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
cargo xtask build examples
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
cargo xtask build /path/to/project
```

Both architectures get compiled and `apelink` fuses them into one file under
`target/ape/`. Every `[[bin]]` in the package is packed; use `--bin` to pick
one, `--example` to build an example target instead (packed under
`target/ape/examples/`, mirroring cargo's layout), `-p`/`--package` to pick
a workspace member, `--release` for an optimized build. Feature selection works like it does
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
and an HTTPS client and so on. Build them all with `cargo xtask build examples`;
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

## Limitations

### Problems you can work around

Where `ape::` and `std::` overlap, prefer `ape::`. For example,
`std::env::current_exe()` returns the APE loader rather than your program,
and on Windows it fails outright looking for `/proc/self/exe`;
`ape::program_executable_name()` handles both.

For async, use smol rather than tokio. Cosmopolitan
[dropped epoll in 2024](https://github.com/jart/cosmopolitan/commit/2ec413b5a9b5d88d363cf5657a8c3ddce4d7feb1),
and mio hardwires epoll on Linux targets, so tokio cannot run here. smol works
because its `polling` crate has a compile-time escape hatch
(`--cfg polling_test_poll_backend`) that switches it to plain `poll`, which
this project sets.

C dependencies work as long as they are compiled from source. Vendored C, C++
or hand-written assembly built through the `cc` crate is fine; the examples
link `ring` (30 native objects) and `blake3`'s SIMD backends, both compiled by
cosmocc. What does not work is `-sys` crates that expect a prebuilt system
library, like openssl-sys, which will find none for this target.

### Known broken, with no fix yet

What follows is only what has been hit so far, not a complete inventory.

There are no async subprocesses, on any platform. async-process needs SIGCHLD
or pidfd/waitid to reap children, and cosmo has neither everywhere, so
`async-process.patch` disables its driver. Synchronous `std::process::Command`
works fine, pipes included.

Runtime CPU detection doesn't work off Linux on arm64. `AT_HWCAP` reads as 0
there, so crates that dispatch on it fall back to scalar code, which is
correct but slower. x86 is unaffected, since CPUID is a hardware instruction.

TUI support is partial. Some of the escape sequences TUIs rely on work,
others don't; one known case is that setting the cursor position does nothing
in Windows PowerShell.

### Checking whether a project will port

To size up an existing project (or a design you're about to start), scan its
dependency tree (`cargo tree`) against this table before investing time:

| If it involves | Verdict |
| --- | --- |
| tokio, mio, or anything else epoll-only | ❌ no epoll under cosmo, and mio can't be told to use anything else |
| spawning processes from async (async-process) | ❌ disabled here; sync `std::process::Command` is the only way |
| `-sys` crates that link a prebuilt system library (openssl-sys, …) | ❌ no such library exists for this target |
| C/C++/asm vendored in the crate, built via `cc` | ⚠️ works if the code sticks to APIs cosmo has (e.g. `ring` and `blake3` do, OpenSSL's `dladdr()` use doesn't) |
| raw `libc` usage in the domains the shim covers (errno, file/socket/signal flags, IPv6 families, nonblocking writes) | ✅ translated at the libc boundary |
| `libc::` constants outside the shim's tables (uncommon ioctls, `SYS_*` numbers beyond futex/getrandom, …) | ⚠️ compiles, then silently misbehaves off Linux |
| terminal control (`tcgetattr`/`struct termios`, crossterm, ratatui TUIs) | ⚠️ repacked and translated by the shim; TUIs run on Linux terminals and Windows consoles (crossterm needs its `use-dev-tty` feature). Known gaps on the cosmo side: NT never answers the DSR query (`cursor::position()` times out), OPOST/CSIZE report host semantics, arbitrary baud rates unmapped |
| smol, async-io, rustls, and pure-Rust crates in general | ✅ works, some via `patches/` |

### Why things break in this particular way

Almost every problem above has the same root cause. Rust's std pins platform
constants at compile time, while Cosmopolitan resolves them at runtime to
whatever the host uses. See [patches/README.md](patches/README.md) and
[shim/README.md](shim/README.md) for how this project deals with that.

The coverage is certainly incomplete. If something works on Linux but hangs or
errors on another platform, it is often a compile-time constant, so check that
first; it can also be something cosmo itself doesn't support on that host,
which cosmo's [function list](https://justine.lol/cosmopolitan/functions.html)
can tell you per platform.

## Notes

Some design issues that you should know about:

**Nightly Rust is required**, since `-Z build-std` is the only way to build a
patched std and it has been unstable for years.

The patches apply to the `rust-src` of the pinned nightly, which ties them to
one exact Rust version; upgrading Rust means redoing every patch.

Generated projects reference this directory by absolute path, so moving it,
or taking a generated project to another machine, breaks the build until the
paths in `Cargo.toml` are fixed up.

## Layout

| | |
| --- | --- |
| `patches/` | Every diff against upstream, with rationale. See its README |
| `shim/` | The Linux-personality shim: C compiled by cosmocc into every binary, translating Linux-coded values to the host's at the libc boundary |
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
