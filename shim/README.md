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
| `socket.c` | socket types, sockopts, `MSG_*` flags, and the `AF_*` family value inside every sockaddr, both directions; plus three NT workarounds — `sun_path` through cosmo's path translation, `SOCK_NONBLOCK` peeled off `socketpair`, and a socketpair built from a real listener so poll reports it writable; and one XNU workaround, an AF_UNIX `SOCK_SEQPACKET` pair retried as a stream pair, which is what std's `Command` asks for on the fork path |
| `signal.c` | signal numbers and `SA_*`/`SS_*` flags; `struct sigaction` repacked field by field, handlers wrapped in a trampoline so they see Linux-coded signums; also `__libc_current_sigrt{min,max}`, which musl has as functions and cosmo as variables; and `sigwait`, which on NT has to hold a no-op handler over each SIG_DFL signal in the set for the duration of the wait — there is no real signal mask there, so the default action would otherwise run instead (SIGALRM kills the process, SIGCHLD is discarded and the waiter never hears that its child exited) |
| `poll.c` | the `POLL*` event bits, in and out; plus a chunked retry for arrays NT refuses outright, which is what keeps async-io off the same ceiling `epoll.c` works around; and the blocking wait itself, shared with `epoll.c`, because cosmo's NT poll cannot be woken by a pipe — a pipe handle is not signalled when the other end writes, so readability is only noticed on one of the loop's own timed rounds, ~11ms later. Anything built out of small pipe round-trips pays that per round trip: drawing fish's default prompt is 35 of them. Windows waits therefore poll first — 500us naps for 10ms, then 1ms out to 250ms (`RUST_APE_POLL_MS`) — and only then hand off to cosmo, so an idle wait still parks and costs nothing. The naps are `nanosleep`, which honors ~500us; an empty-set `ppoll` sleeps on the system tick and would turn every nap into ~15.6ms |
| `fork-nt.c` | replaces cosmo's `libc.a(fork-nt.o)`: a reservation made by `mmap.c` (tagged `MAP_APE_RESERVE`, `mmap.h`) is rebuilt in the child as a reservation — one `VirtualAllocEx(MEM_RESERVE)` for the span, then a `VirtualQuery` walk of the parent commits, copies and re-protects only the runs that are really committed. Upstream commits every private map in full and copies the whole span, which for a 4 GiB reservation is the same `ERROR_COMMITMENT_LIMIT` the mmap fix removed, so fork would fail for any process holding one. Cost is proportional to committed bytes; adjacent pieces committed one at a time are one copy |
| `proc.c` | cosmo 4.0.2's `libc.a(proc.o)`, with one change in `__proc_harvest`: a native win32 child's exit code is decoded as an exit code. cosmo's NT convention packs a whole unix wait status into the 32-bit exit code (`exit(N)` really exits `N<<8`, a signal death exits with the raw signal number), so a native program's literal `exit 1` read back as "terminated by SIGHUP" — every nonzero exit of every native program became a signal. The child's image is sniffed through the process handle at harvest time (`K32GetProcessImageFileNameW` — the Query variant fails on dead processes — plus `\\?\GLOBALROOT` for the device path); an APE (leading `MZqFpD`) keeps the upstream decode, any other PE gets unix semantics: NTSTATUS crash codes become the matching signal, anything else is `(code&0xFF)<<8` |
| `fchdir-nt.c` | cosmo 4.0.2's `libc.a(fchdir-nt.o)`, minus the `\\?\` prefix on the resulting cwd. Upstream resolves the descriptor with `GetFinalPathNameByHandle`, which always answers in the `\\?\` namespace, and stores that verbatim as the process cwd (the prefix disables win32 normalization). Every child spawned afterwards inherits the mangled cwd: cmd.exe refuses it and falls back to `C:\Windows`, and a cosmopolitan-master child segfaults during startup. A race-free cd is exactly `open` + `fchdir`, which is how fish changes directory, so after every interactive `cd` all spawned commands broke. The path is downgraded to its classic form (`C:\x`, `\\srv\share\x`) whenever it fits win32's 260-char limit |
| `epoll.c` | epoll, which cosmo deleted in 2024: the raw syscalls on Linux, an interest list over `poll.c`'s wait everywhere else, chunked once a set grows past what NT's `poll()` will accept. `EPOLLET`/`EPOLLONESHOT` are honored through per-direction arming (report once, re-arm on `epoll_ctl` MOD, on EAGAIN from the I/O shims, or on a write to a recorded pipe peer), which is what keeps tokio's reactor from busy-looping on an idle socket's `POLLOUT` or mio's never-drained waker pipe. Read its header for the details |
| `inotify.c` | inotify, which cosmo numbers but never wrapped: the raw syscalls on Linux, a scan-and-diff over the watched paths everywhere else, delivered through a pipe. Read its header for what a poller can't see |
| `winsock.c` | holds a Winsock reference for the process, so cosmo's atexit `WSACleanup` doesn't pull it out from under threads that are still running |
| `mmap.c` | the `MAP_*` bits; plus reserve/commit on NT: a `PROT_NONE` anonymous private mapping is only reserved (`VirtualAlloc(MEM_RESERVE)`, registered in cosmo's map table so `munmap` releases it), and `mprotect` commits the still-reserved pages before cosmo's `VirtualProtect` sees them. cosmo's NT mmap always commits, and commit is charged against the pagefile up front, so edit's 4 GiB-per-document gap buffer (and any wasm runtime or arena that reserves address space and grows into it) failed with `ERROR_COMMITMENT_LIMIT` (1455 → ENOMEM) on the second document. The reservation is `MAP_NOFORK`: cosmo's NT fork would otherwise try to commit the whole span in the child and every `Command::spawn` on the fork path would ENOMEM |
| `clock.c` | `CLOCK_*` ids, plus `pthread_condattr_setclock` |
| `futex.c` | reroutes std's `SYS_futex` calls onto cosmo's cross-platform futex, which is what makes Mutex/Condvar actually sleep instead of spin |
| `io.c` | a poll gate on `write`/`writev` for nonblocking fds, working around cosmo's NT send path ignoring O_NONBLOCK; also feeds `epoll.c`'s edge-triggered arming (EAGAIN re-arms the write side, a successful write to a recorded pipe end re-arms the paired reader) |
| `read.c` | replaces cosmo's `libc.a(read.o)` and `libc.a(readv.o)`: upstream forwarding plus the EAGAIN hook `epoll.c`'s edge-triggered arming needs — pipes and inotify are read through plain `read()`, which the libc crate doesn't route through a `__ape_shim_*` name |
| `pipe.c` | `tee` and `vmsplice`, which cosmo numbers but never wrapped (it does have `splice` and `copy_file_range`): the raw syscalls on Linux and `ENOSYS` elsewhere, which callers already treat as "no fast path, use a read/write loop" |
| `dladdr.c` | `dladdr`, which cosmo's dlfcn lacks: answered from `/proc/self/maps` on Linux (`dli_fname` and `dli_fbase` only), "no information" elsewhere |
| `ifname.c` | the `if_nametoindex`/`if_indextoname`/`if_nameindex` family, which cosmo lacks entirely: `SIOCGIFINDEX`/`SIOCGIFNAME` on Linux over an enumeration of `/sys/class/net`, `GetAdaptersAddresses`' `IfIndex` on NT (where those ioctls are `WSAEOPNOTSUPP`), and an enumeration-order index elsewhere. All three calls share one enumeration, so name → index → name round-trips on every host |
| `commandv.c` | replaces cosmo's `libc.a(commandv.o)`. The `$PATH` resolver behind `posix_spawnp`/`execvp`, i.e. Rust's `Command::spawn`: on NT an extensionless name that misses retries with `.exe` then `.com` (what CreateProcess and the Cygwin/MSYS exec layers do) |
| `mkntpath.c` | replaces cosmo's `libc.a(mkntpath.o)`: a win32-absolute segment (`X:\…` or `\\server\share`) embedded mid-path re-roots the path, because unix `Path` semantics treat those as relative and glue the cwd in front |
| `mkntpathat.c` | replaces cosmo's `libc.a(mkntpathat.o)`: dirfd-relative NT paths on a network share came back as the relative path `UNC\srv\share\...` (the `\\?\` strip only knew drive letters), so every `*at` call there was ENOENT — which std's `remove_dir_all` swallows until the final rmdir's ENOTEMPTY |
| `readlinkat-nt.c` | replaces cosmo's `libc.a(readlinkat-nt.o)`. `readlink` of an existing non-symlink now fails with EINVAL instead of stale errno, which is the signal musl-realpath's walk relies on — without it `realpath()` failed for every path on NT |
| `realpath.c` | replaces cosmo's `libc.a(realpath.o)`: on NT a leading `//` survives (UNC share, musl's POSIX reading) and the unopenable `//server`/`//server/share` prefixes skip `readlink`, making `canonicalize()` work on shares |
| `rlimit.c` | the `RLIMIT_*` resource numbers, plus a `prlimit` cosmo doesn't have |
| `xattr.c` | the extended-attribute family, as a raw syscall on Linux and `ENOTSUP` elsewhere |
| `mknod.c` | `mknodat`, which cosmo lacks along with the rest of the family (`mknod`, `mkfifo`, `mkfifoat`) — rustix builds `mkfifoat` on it: the raw syscall on Linux, `ENOSYS` elsewhere, since NT has no device nodes and its named pipes are a different namespace |
| `timer.c` | POSIX interval timers (`timer_create`/`settime`/`gettime`/`delete`), which cosmo has no wrapper for: the raw syscalls on Linux, `ITIMER_REAL` everywhere else (one timer, SIGALRM only — the same fallback uucore itself uses on Apple/OpenBSD). `sigevent` and `itimerspec` pass through as `void *` — the libc crate's Linux layouts already are the kernel's |
| `umask.c` | the process umask, which cosmo starts at 0777 on NT (nothing to inherit): reset to 0022 at startup, because a Linux-compiled caller reads it back and computes `mode & ~umask`, which 0777 turns into a mode with no write bit |
| `getgroups.c` | `getgroups`, which fails on NT — one identity per process, no list to enumerate. The host answer is passed through wherever it succeeds; where it cannot, the process belongs to exactly its own group, which is what code that resolves a file's owner/group/other bits by hand needs to get an answer at all instead of an error |
| `hostid.c` | `gethostid`, read from `/etc/hostid` on any host and 0 when absent (glibc's first step, musl's answer as the fallback), and `sethostname`, a raw syscall on Linux and `ENOSYS` elsewhere |
| `ftruncate-nt.c` | replaces cosmo's `libc.a(ftruncate-nt.o)`, the NT backend both `ftruncate` and `truncate` funnel through: a file extended past its current end is marked sparse first (`FSCTL_SET_SPARSE`, best effort — FAT/exFAT/some SMB servers refuse and keep their eager fill). POSIX reads the extended area as zeros and every unix backs that with holes; an NTFS file is not sparse by default, so the first write past the valid-data mark zero-fills everything before it, synchronously — `set_len(5 GiB)` + one small write at 4 GiB cost 32s and 4 GiB of disk on a CI runner, vs 15ms on Linux. A shrink or same-size call leaves the attributes alone |
| `rename.c` | `rename`/`renameat` on NT, where cosmo's `MoveFileEx(..., MOVEFILE_REPLACE_EXISTING)` cannot replace a destination anyone holds open — including the calling process, and despite cosmo opening everything with `kNtFileShareDelete`. Write-temp-then-rename is how files get updated atomically, so this is common (fish printed a rename error for every command typed). An EACCES from upstream is retried through `FileRenameInfoEx`'s POSIX semantics, the same call Rust's own `std::fs::rename` makes on Windows; anything else keeps its errno. **The class number is spelled out, not taken from `libc/nt/enum/fileinfobyhandleclass.h`** — that header's second block is off by one (its two halves collide: `kNtFileStreamInfo` and `kNtFileEndOfFileInfo` are both 7), and `kNtFileRenameInfo` lands on `FileDispositionInfo`, which reports success and deletes the source |
| `wait.c` | `wait`/`waitpid`/`wait4`: the W* option bits, and the signal number packed into a wait status. cosmo's NT wait rejects any option outside `WNOHANG|WUNTRACED` outright (`libc/proc/wait4-nt.c`: "no support for WCONTINUED yet"), so a job-control shell asking for `WCONTINUED` gets -1 from every reap rather than merely missing the resume edge — `WCONTINUED` is dropped there instead. The status word itself is Linux-shaped on both sides and passes through, but the signal it carries is cosmo-coded on NT, so the terminating- and stop-signal fields are translated back and the core-dump bit preserved; on NT a SIGCHLD is raised after a reap whenever a real handler is installed, because cosmo's process tracker generates one only when nobody is in wait4 at the moment the child exits, so the common Popen-then-wait shape never ran the handler; and `waitid`, which cosmo never wrapped (only the syscall number, on 4.0.2 and master alike): the raw syscall on Linux, and elsewhere a `wait4` in disguise that fills the `si_code`/`si_status`/`si_pid` a caller reads, minus `WNOWAIT`, which is EINVAL there because a status cannot be looked at without reaping it |
| `scanf.c` | the `__isoc99_scanf`/`fscanf`/`sscanf` aliases glibc's headers redirect to and cosmo has no name for. The libc crate applies that redirect to every `target_os = "linux"`, so `libc::sscanf` will not link without them; cosmo's plain `scanf` is already the C99 one the alias stands for, so each forwards through the matching `v*` form |
| `environ.c` | drops the environment entries NT keeps its per-drive working directories in (`=C:` and friends). POSIX reads a name as everything before the first `=`, so those have none. Reading the environment tolerates that; re-encoding it does not, so code that builds a child's environment out of its own is left choosing between dropping entries it cannot name and refusing the spawn |
| `syscall.h` | raw Linux syscalls, for the few APIs cosmo numbers but doesn't wrap |
| `termios.c` | terminal control, with `struct termios` repacked, flag bits, `c_cc` indices and baud codes translated, and tty ioctls rerouted, `FIONBIO` among them as the `fcntl(F_SETFL)` cosmo recommends in its place, which is what let std, async-io and async-process keep their `FIONBIO` paths unpatched; the session and job-control requests (`TIOCSCTTY`, `TIOCNOTTY`, `TIOCGPGRP`, `TIOCSPGRP`, `TIOCGSID`) are answered too, which is what a pty child needs between fork and exec; `tcsetattr` carries cosmo's `kTtyXtMouse` bit across the call, since the NT implementation rebuilds `__ttyconf.magic` from termios flags alone and would silently turn xterm mouse reporting off |
| `layouts.c` | no code, only compile-time asserts for the structs passed through unrepacked |
| `stack_chk.c` | a weak `__stack_chk_guard` so -sys crates that force `-fstack-protector` back on still link on aarch64 |
| `tables.h` | the generated Linux-to-cosmo value tables everything above reads |
