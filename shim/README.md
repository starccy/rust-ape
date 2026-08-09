# shim

These C files provide a small layer that translates Linux platform constants
into the host's. It is compiled by cosmocc and linked into every binary.

The Rust world is compiled for `*-unknown-linux-musl`, so every constant it
uses (errno, open flags, socket options, signal numbers and so on) is baked in
as the Linux value at compile time. Cosmopolitan resolves the same names at
runtime for whatever host it lands on. The patched std, libc and errno crates
redirect the affected functions to the `__ape_shim_*` entry points here, which
convert between the two codings in both directions. Nothing changes at the
call sites. Std and third-party crates keep passing `libc::` constants and
comparing `raw_os_error()` against `libc::E*`, and get correct behavior on
every host.

Why this is a layer of function wrappers rather than patched constants is
covered in [patches/README.md](../patches/README.md).

## How it gets built

There is no separate build step. `scripts/gcc-linker-wrapper.bash` compiles
`shim/*.c` at link time and caches the objects per arch under `generated/`.
Edit a file here and the next `cargo xtask build` picks it up.

## The value tables

`tables.h` is generated, not hand-written. `cargo xtask gen-shim` extracts the
Linux values from the vendored libc crate sources and pairs each with cosmo's
runtime constant of the same name. Two build-time cross-checks keep it honest:

* `examples/src/bin/shim_tables_check.rs` asserts every Linux value against
  the libc crate, so a wrong number fails the build instead of misbehaving at
  runtime.
* `layouts.c` and `examples/src/bin/struct_layout_check.rs` pin the structs
  the shim passes through untranslated. If a cosmocc or libc upgrade shifts a
  layout, the build breaks with the field's name in the error.

## Files

Each file starts with a comment explaining its corner in detail. The short
version:

| File | Covers |
| --- | --- |
| `errno.c` | keeps errno Linux-coded for the whole Rust world, via a translated thread-local copy |
| `open.c` | the `O_*`, `F_*` and `AT_*` flags through `open`/`openat`/`fcntl`/`pipe2` and the `*at` family |
| `socket.c` | socket types, sockopts, `MSG_*` flags, and the `AF_*` family value inside every sockaddr, both directions; plus three NT workarounds — `sun_path` through cosmo's path translation, `SOCK_NONBLOCK` peeled off `socketpair`, and a socketpair built from a real listener so poll reports it writable |
| `signal.c` | signal numbers and `SA_*`/`SS_*` flags; `struct sigaction` repacked field by field, handlers wrapped in a trampoline so they see Linux-coded signums; also `__libc_current_sigrt{min,max}`, which musl has as functions and cosmo as variables |
| `poll.c` | the `POLL*` event bits, in and out |
| `winsock.c` | holds a Winsock reference for the process, so cosmo's atexit `WSACleanup` doesn't pull it out from under threads that are still running |
| `mmap.c` | the `MAP_*` bits |
| `clock.c` | `CLOCK_*` ids, plus `pthread_condattr_setclock` |
| `futex.c` | reroutes std's `SYS_futex` calls onto cosmo's cross-platform futex, which is what makes Mutex/Condvar actually sleep instead of spin |
| `io.c` | a poll gate on `write`/`writev` for nonblocking fds, working around cosmo's NT send path ignoring O_NONBLOCK |
| `rlimit.c` | the `RLIMIT_*` resource numbers, plus a `prlimit` cosmo doesn't have |
| `xattr.c` | the extended-attribute family, as a raw syscall on Linux and `ENOTSUP` elsewhere |
| `syscall.h` | raw Linux syscalls, for the few APIs cosmo numbers but doesn't wrap |
| `termios.c` | terminal control, with `struct termios` repacked, flag bits, `c_cc` indices and baud codes translated, and tty ioctls rerouted; the session and job-control requests (`TIOCSCTTY`, `TIOCNOTTY`, `TIOCGPGRP`, `TIOCSPGRP`, `TIOCGSID`) are answered too, which is what a pty child needs between fork and exec |
| `layouts.c` | no code, only compile-time asserts for the structs passed through unrepacked |
| `stack_chk.c` | a weak `__stack_chk_guard` so -sys crates that force `-fstack-protector` back on still link on aarch64 |
| `tables.h` | the generated Linux-to-cosmo value tables everything above reads |
