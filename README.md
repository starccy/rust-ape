# rust-ape

[![CI](https://github.com/starccy/rust-ape/actions/workflows/ci.yml/badge.svg)](https://github.com/starccy/rust-ape/actions/workflows/ci.yml)

Build Rust programs into Cosmopolitan APE binaries: One file that runs on
Linux, macOS, Windows, on both x86-64 and arm64.

> **Status: experimental.** Only the breakage I've run into myself is fixed;
> everything beyond that is untested territory. Not for production use.

```console
$ cargo xtask build hello
==> apelink -> hello/target/ape/debug/hello.com

$ ./hello/target/ape/debug/hello.com    # Linux
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
./examples/target/ape/debug/platform.com
```

That leaves all the binaries in `examples/target/ape/debug/`, each one a scenario you
can copy to another machine and run.

The [CI runs](https://github.com/starccy/rust-ape/actions) show how each example
did on each platform, and you can download the built binaries there too
(if they haven't expired). They double as the test suite; see
[What works](#what-works).

**2. Create a project.**

```sh
cargo xtask generate /path/to/project
```

The result is very similar to what `cargo new` produces. You get a normal cargo
project, plus a `[patch.crates-io]` section that points it at the adapted crates
and a `rust-toolchain.toml` matching the SDK. Write Rust the
way you normally would, **with the caveats below**.

**3. Build it.**

```sh
cargo xtask build /path/to/project
```

Both architectures get compiled and `apelink` fuses them into one file under
`target/ape/debug/` (`target/ape/release/` with `--release`). The interface is
very similar to `cargo build`: `--bin`, `--example`, `-p`/`--package`,
`--release`, `--features` and friends work the way you would expect; see
`cargo xtask build --help` for the full list.

Plain `cargo build` inside the project will **not** work. The build needs a
patched std via `-Z build-std`, two `--cfg` flags, and `CC`/`CXX`/`AR` pointing
at cosmocc. `cargo xtask build` sets all of that up.

**4. Run it.**

Copy the `.com` file to any supported machine and execute it. On
Unix you need the `./` prefix (or you might use `sh -c ./name.com` in some situations, see [this](https://github.com/jart/cosmopolitan#shells));
on Windows, `.\name.com`.

## What works

Everything in `examples/` is a self-contained scenario that exits non-zero on
failure. Including TCP/UDP server/client, blocking and async I/O on both smol
and tokio, unix sockets, clocks, hashing, unwinding, file watching, a
pseudo-terminal, and an HTTPS client.
Build them all with `cargo xtask build examples`; they are the shortest way to
see what does and doesn't work here.
CI builds them **once on Linux** and runs those same files on five platforms:

| | x86-64 | arm64 |
| --- | --- | --- |
| Linux | :white_check_mark: | :white_check_mark: |
| Windows | :white_check_mark: | :white_check_mark: |
| macOS | :interrobang: | :white_check_mark: |
| others | :interrobang: | :interrobang: |

> The :interrobang: cell means untested. They would probably work. However, the
> [compile-time-constant mismatches](#why-things-break-in-this-particular-way)
> that break things here are per-platform, so those platforms may fail in ways
> the tested ones no longer do. I rarely use them and don't plan to test them;
> reports welcome.

## Limitations

### Where the platforms differ

The following things behave differently depending on the platform the binary
runs on. Everything is measured on Linux and on a Windows VM unless the cell
says otherwise. The macOS column is arm64 and weaker evidence, since I have
no Mac to debug on and can only go by what GitHub Actions reports.

| | Linux | Windows | macOS |
| --- | --- | --- | --- |
| epoll, under mio and tokio | the real syscalls | emulated over `poll()`, and a set past a few dozen descriptors stops blocking and scans on a 10ms tick | emulated over `poll()` |
| inotify, under `notify` | the real syscalls | emulated by scanning every 200ms, so no opens, no closes, and a rename looks like a delete plus a create | the same emulation |
| pseudo-terminal | works | **none at all**, `openpty` is ENOSYS | works, the same unix path |
| `UnixDatagram` | works | **no**, NT's AF_UNIX is stream-only | works |
| system certificate store | the roots from `/etc/ssl` | **empty**, see below | `/etc/ssl` again, which is right here by luck |
| loading a shared library | `cosmo_dlopen` | `cosmo_dlopen`, and it rewrites `.so` to `.dll` for you | arm64 only (seems acceptable) |
| calling host APIs directly | n/a | Win32, either a plain `extern "C"` away or through `GetProcAddress` | n/a |
| jemalloc as the global allocator | clean | works, but writes `Error in munmap(): Operation not supported` to stderr, because NT cannot release part of a mapping | clean |
| SQLite writers contending | real locks, ~25ms for 1000 inserts across 4 threads | emulated locks, ~3.1s for the same | untested |
| runtime CPU feature detection | works | `AT_HWCAP` reads 0 on arm64, so dispatch falls back to scalar | same on arm64 |
| `std::env::current_exe` | the loader, not you | works: the emulated `/proc/self/exe` answers with the real program path | the loader, not you |

### Problems you can work around

#### APE-specific APIs

Where `ape::` and `std::` overlap, prefer `ape::`. For example, on Linux and
macOS `std::env::current_exe()` returns the APE loader rather than your
program.

#### Async

Both smol and tokio work, by different routes. Cosmopolitan
[dropped epoll in 2024](https://github.com/jart/cosmopolitan/commit/2ec413b5a9b5d88d363cf5657a8c3ddce4d7feb1),
and the two runtimes deal with that differently here. smol's `polling` crate
takes a cfg that switches it to plain `poll()`, which this project sets, and
that path has been trouble-free throughout. tokio's `mio` has the same escape
hatch and it is **not** used, because mio's poll backend drops a descriptor's
interest the moment it reports an event and expects it back through an
internal type that `SourceFd` users never reach, so anything registered that
way goes deaf after one event. `tokio::process`'s stdio hits it
([tokio#8042](https://github.com/tokio-rs/tokio/issues/8042)), crossterm's tty
hits it, and the backend has other open bugs besides
([mio#1874](https://github.com/tokio-rs/mio/issues/1874)). So mio is left on
epoll and `shim/epoll.c` answers it: the raw syscalls on Linux, an emulation
over cosmo's `poll()` everywhere else. Read that file's header before relying
on it, the two things it cannot promise are written down there.

One thing trips up existing projects rather than new ones. A `.cargo/config.toml`
that pins `linker` for `aarch64-unknown-linux-musl` (plenty of projects ship one
for cross-compiling) wins over what `cargo xtask build` sets, and the arm64 half
then goes to the wrong linker and fails on `-lunwind`. Move that entry out of the
way.

#### TLS cert

**The system certificate store is empty on Windows**, and it fails in a way
that is hard to recognize. Every crate that reads it
picks its backend at compile time, and this target says unix, so
`rustls-native-certs`, `rustls-platform-verifier` and native-tls all go
looking in `/etc/ssl` no matter which machine they end up on. On Windows
that means finding nothing. Anything that talks to the network over TLS
most likely has to deal with this. rustls-platform-verifier at least says
`No CA certificates were loaded from the system`; rustls-native-certs returns
**zero roots and zero errors**, so nothing looks wrong until every
certificate fails to verify.

Three ways out, cheapest first.

Bundle the roots. `webpki-roots` compiles them in and the question stops
existing. `examples/reqwest_client.rs` does this.

Ship a PEM and point `SSL_CERT_FILE` at it. **No code change at all.** The
unix path those crates take honours the variable, so putting a `cacert.pem`
next to the binary is enough. Verified by copying `/etc/ssl/cert.pem` to the
Windows box and setting nothing but that variable.

Read the real store. cosmo imports no `Crypt*` symbol at all, so this is
`LoadLibraryA` plus `GetProcAddress`, and it is x86-64 only, because the
pointer that comes back wants Microsoft's convention and `extern "win64"`
exists nowhere else.
[`examples/src/bin/win_cert_store.rs`](examples/src/bin/win_cert_store.rs) has
the full version, with the three crypt32 entry points, the enumeration loop
over both the `ROOT` and `CA` stores, and the DER handed straight to a rustls
`RootCertStore`. Do this behind `ape::is_windows()`, and
keep one of the first two options for the other hosts.

C dependencies work as long as they are compiled from source. Vendored C, C++
or hand-written assembly built through the `cc` crate is fine; the examples
link `ring` and `blake3`'s SIMD backends, both compiled by
cosmocc. What does not work is `-sys` crates that expect a prebuilt system
library, like openssl-sys, which will find none for this target.

### Known broken, with no fix yet

What follows is only what has been hit so far, not a complete inventory.

The epoll behind mio is not the real thing off Linux, and two of the gaps are
inherent to building it on `poll()`. `EPOLLET` is accepted and ignored, so
every registration behaves as level-triggered: mio asks for edge triggering
and gets told about a ready descriptor more often than Linux would tell it.
That costs wakeups, it does not lose them, which is the direction an emulation
should err in. The other one costs latency. On Windows cosmo sorts a
`poll()` call into an NT wait and a `WSAPoll`, each holding 64, and answers
`EINVAL` rather than truncating when one of them overflows. Its own path for
oversized arrays splits the call up but gets the arithmetic wrong (4.0.2), so
`shim/epoll.c` hands cosmo 32 at a time once it has seen the refusal, which
means a set that big can no longer block in the kernel and scans on a 10ms
tick instead. A tokio program holding 120 children's stdin and stdout, 240
pipes in all, gets through in about a second and a half, which is what
`tokio_pipe_stress.rs` measures. Sets that stay small never enter that mode
and keep blocking as before. async-io stays on `poll()` rather than epoll,
because polling's epoll backend wants timerfd and cosmo has none, so
`shim/poll.c` carries the same retry and the ceiling isn't there either.

`UnixDatagram` doesn't work on Windows. NT's AF_UNIX is stream-only and
answers `socket(AF_UNIX, SOCK_DGRAM)` with WSAEAFNOSUPPORT, which is a gap in
the OS rather than in cosmo. `UnixStream` and `UnixListener` do work, paths
included.

macOS has an AF_UNIX gap of its own, and it lands somewhere you would not
guess. std's `Command::spawn` takes a fork path instead of `posix_spawn`
whenever there is a pre_exec closure, a uid, a chroot, or a bare program name
with the environment touched, and on a linux target it builds the channel the
child reports a failed exec on out of a `SOCK_SEQPACKET` pair. XNU's AF_UNIX
has no such type. `shim/socket.c` retries those as a stream pair, which
carries the same one-write-then-EOF exchange and gives up message boundaries
in return. `spawn_preexec.rs` covers it, and it is the only example that
reaches that path at all.

A host API can call back into your code, but only on a thread cosmo made. On
one of its own, everything works: `examples/host_api.rs` runs a Win32 callback
that allocates and prints. On a thread the host created, which is every
`CALLBACK_FUNCTION` audio callback, window procedure and IO completion
routine, cosmo's thread block was never installed and the first libc call is
an access violation. Not `malloc`, not `write`. Win32 itself is fine there,
because cosmo's thunks only shuffle registers, so the way through is to have
the callback ring an event and let a thread you own do the work. Audio and GUI
APIs take `CALLBACK_EVENT` and `CALLBACK_WINDOW` for exactly this. The example
shows that shape. Adopting the foreign thread outright is possible, by
allocating a block with `_mktls` and installing it with `__set_tls` plus the
real thread id in `tib_ptid`, but that is internal cosmo territory, and it
still leaves `pthread_self()` null, thread-local destructors unrun and a few
KiB leaked per thread.

TUI support is partial. Some of the escape sequences TUIs rely on work,
others don't; one known case is that setting the cursor position does nothing
in Windows PowerShell.

### Checking whether a project will port

To size up an existing project (or a design you're about to start), scan its
dependency tree (`cargo tree`) against this table before investing time:

| If it involves | Verdict |
| --- | --- |
| tokio, or anything else on mio | :white_check_mark: mio picks epoll on this target and `shim/epoll.c` provides it. Its waker is still forced onto a pipe, since cosmo has no eventfd |
| `-sys` crates that link a prebuilt system library | :x: no such library exists for this target |
| rustls on its default aws-lc-rs backend | :x: aws-lc-sys guards a whole `.S` file on `__linux__`, which cosmocc undefines, and the object comes out with no symbol table. Switch to the ring backend, whose asm is guarded on `__ELF__` |
| zstd, anywhere it reaches zstd-sys | :warning: same `__linux__` guard, on `huf_decompress_amd64.S`. Its `no_asm` feature takes the C path and links fine |
| anything that reads the system certificate store (rustls-platform-verifier, rustls-native-certs, native-tls) | :warning: it asks the OS the compile-time way, so it reads `/etc/ssl` on every host and comes back empty on Windows, sometimes without saying so. Bundled roots, `SSL_CERT_FILE`, or crypt32 at runtime; [all three written out above](#problems-you-can-work-around) |
| a pseudo-terminal (portable-pty, and terminal multiplexers on top of it) | :warning: works on Unix, where cosmo has openpty and the shim answers the session ioctls the child needs after fork. On Windows there is no pty at all: cosmo imports `CreatePseudoConsole` but never wires it to openpty, so openpty is ENOSYS and nothing routes around it |
| C/C++/asm vendored in the crate, built via `cc` | :warning: works if the code sticks to APIs cosmo has (e.g. `ring` and `blake3` do, OpenSSL's `dladdr()` use doesn't) |
| calling a host API directly (Win32, or a shared library through `cosmo_dlopen`) | :warning: works, chosen at runtime with `ape::is_windows()` and friends, never by `cfg`. Almost all of what cosmo imports is a plain `extern "C"` away, though A/W pairs are exported unsuffixed and wide (`MessageBox`, not `MessageBoxW`). Beyond that it's `GetProcAddress` and an `extern "win64"` pointer, which is x86_64-only and so needs an arch gate. `windows-sys` builds and links; the `windows` crate does not. See `examples/host_api.rs` |
| a replacement `#[global_allocator]` | :warning: jemalloc works, and ripgrep (which picks it up on exactly this target) builds and runs unmodified. On Windows it writes a few `Error in munmap(): Operation not supported` lines to stderr per run, because NT can't release part of a mapping; stdout stays clean and no `MALLOC_CONF` setting quiets it. mimalloc is the one to avoid, it segfaults before `main` on every host |
| raw `libc` usage in the domains the shim covers (errno, file/socket/signal flags, IPv6 families, nonblocking writes) | :white_check_mark: translated at the libc boundary |
| terminal control (`tcgetattr`/`struct termios`, crossterm, ratatui TUIs) | :warning: repacked and translated by the shim; TUIs run on Linux terminals and Windows consoles, crossterm's default mio event source included. Known gaps on the cosmo side: NT never answers the DSR query (`cursor::position()` times out), OPOST/CSIZE report host semantics, arbitrary baud rates unmapped |
| smol, async-io, rustls, and pure-Rust crates in general | :white_check_mark: works, some via `patches/` |

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

## TODO

* Benchmark against a natively built binary
* Maybe a separate repository demonstrating how to build various existing
  projects as APE binaries

## Thanks

* [jart/cosmopolitan](https://github.com/jart/cosmopolitan): the foundation
  this project is built on.
* [ahgamut/rust-ape-example](https://github.com/ahgamut/rust-ape-example): an
  early demonstration of building Rust code with cosmocc, and where the idea
  of this project came from.
* [crisidev/ape-rs](https://github.com/crisidev/ape-rs), and the
  [blog series](https://blog.crisidev.org/tags/series-one-bin-to-rule-them-all/)
  that goes with it. Reading someone else work through the same problems is
  what got me interested in making Rust fit APE properly again.
