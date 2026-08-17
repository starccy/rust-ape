//! regenerate shim/tables.h and the compile-time
//! cross-check example from the vendored sources.

use crate::util;
use anyhow::{Context, Result, bail};
use clap::Args;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct GenShimArgs {
    /// Print the tables instead of writing them
    #[arg(long)]
    pub dry_run: bool,
}

const ARCHES: &[&str] = &["x86_64", "aarch64"];

/// Search order inside the libc crate for one arch. most specific first.
fn libc_search_paths(root: &Path, arch: &str) -> Vec<PathBuf> {
    let src = root.join("vendor/patches/libc/src");
    [
        format!("unix/linux_like/linux/musl/b64/{arch}/mod.rs"),
        "unix/linux_like/linux/musl/b64/mod.rs".into(),
        "unix/linux_like/linux/musl/mod.rs".into(),
        "unix/linux_like/linux/arch/generic/mod.rs".into(),
        "unix/linux_like/linux/mod.rs".into(),
        "unix/linux_like/linux_l4re_shared.rs".into(),
        "unix/linux_like/mod.rs".into(),
        "unix/mod.rs".into(),
        // the libc crate's "new structure": FUTEX_* live here
        "new/linux_uapi/linux/futex.rs".into(),
    ]
    .into_iter()
    .map(|p| src.join(p))
    .collect()
}

/// One table in the manifest. `cosmo_header` is where the runtime symbol has
/// to be declared; names listed in `fixed_ok` may instead be #defines there
/// (same value on every platform, no translation needed, still asserted).
struct Domain {
    /// Name of the emitted X-macro, e.g. "SHIM_ERRNO_TABLE".
    macro_name: &'static str,
    cosmo_header: &'static str,
    /// (name, droppable) — droppable only matters for flag-style tables.
    names: &'static [(&'static str, bool)],
    with_droppable_column: bool,
    /// C type of the cosmo-side constant, for SHIM_FIX_* storage.
    ctype: &'static str,
}

const D: bool = true; // droppable / hint
const R: bool = false; // required

const ERRNOS: &[(&str, bool)] = &[
    ("EPERM", R), ("ENOENT", R), ("ESRCH", R), ("EINTR", R), ("EIO", R),
    ("ENXIO", R), ("E2BIG", R), ("ENOEXEC", R), ("EBADF", R), ("ECHILD", R),
    ("EAGAIN", R), ("ENOMEM", R), ("EACCES", R), ("EFAULT", R), ("ENOTBLK", R),
    ("EBUSY", R), ("EEXIST", R), ("EXDEV", R), ("ENODEV", R), ("ENOTDIR", R),
    ("EISDIR", R), ("EINVAL", R), ("ENFILE", R), ("EMFILE", R), ("ENOTTY", R),
    ("ETXTBSY", R), ("EFBIG", R), ("ENOSPC", R), ("ESPIPE", R), ("EROFS", R),
    ("EMLINK", R), ("EPIPE", R), ("EDOM", R), ("ERANGE", R), ("EDEADLK", R),
    ("ENAMETOOLONG", R), ("ENOLCK", R), ("ENOSYS", R), ("ENOTEMPTY", R),
    ("ELOOP", R), ("ENOMSG", R), ("EIDRM", R), ("ECHRNG", R), ("EL2NSYNC", R),
    ("EL3HLT", R), ("EL3RST", R), ("ELNRNG", R), ("EUNATCH", R), ("ENOCSI", R),
    ("EL2HLT", R), ("EBADE", R), ("EBADR", R), ("EXFULL", R), ("ENOANO", R),
    // ETIME deliberately sits AFTER ETIMEDOUT (below): the reverse lookup is
    // first-match, and on NT both resolve to the same host value — a timeout
    // must read back as ETIMEDOUT (what everyone compares against), not the
    // STREAMS relic. Moved out of its natural cluster here.
    //
    // ENOATTR carries ENODATA's Linux value (see libc_name): it is the
    // BSD/XNU spelling of the same condition and Linux has no name of its
    // own for it. It sits AFTER ENODATA so the forward lookup keeps mapping
    // Linux 61 onto the host's ENODATA; the reverse direction is what this
    // entry is for, and there ENOATTR's host value is unique everywhere.
    ("EBADRQC", R), ("EBADSLT", R), ("ENOSTR", R), ("ENODATA", R), ("ENOATTR", R),
    ("ENOSR", R), ("ENONET", R), ("ENOPKG", R), ("EREMOTE", R), ("ENOLINK", R),
    ("EADV", R), ("ESRMNT", R), ("ECOMM", R), ("EPROTO", R), ("EMULTIHOP", R),
    ("EDOTDOT", R), ("EBADMSG", R), ("EOVERFLOW", R), ("ENOTUNIQ", R),
    ("EBADFD", R), ("EREMCHG", R), ("ELIBACC", R), ("ELIBBAD", R),
    ("ELIBSCN", R), ("ELIBMAX", R), ("ELIBEXEC", R), ("EILSEQ", R),
    ("ERESTART", R), ("ESTRPIPE", R), ("EUSERS", R), ("ENOTSOCK", R),
    ("EDESTADDRREQ", R), ("EMSGSIZE", R), ("EPROTOTYPE", R), ("ENOPROTOOPT", R),
    ("EPROTONOSUPPORT", R), ("ESOCKTNOSUPPORT", R), ("EOPNOTSUPP", R),
    ("ENOTSUP", R), ("EPFNOSUPPORT", R), ("EAFNOSUPPORT", R), ("EADDRINUSE", R),
    ("EADDRNOTAVAIL", R), ("ENETDOWN", R), ("ENETUNREACH", R), ("ENETRESET", R),
    ("ECONNABORTED", R), ("ECONNRESET", R), ("ENOBUFS", R), ("EISCONN", R),
    ("ENOTCONN", R), ("ESHUTDOWN", R), ("ETOOMANYREFS", R), ("ETIMEDOUT", R),
    ("ETIME", R),
    ("ECONNREFUSED", R), ("EHOSTDOWN", R), ("EHOSTUNREACH", R), ("EALREADY", R),
    ("EINPROGRESS", R), ("ESTALE", R), ("EUCLEAN", R), ("ENOTNAM", R),
    ("ENAVAIL", R), ("EISNAM", R), ("EREMOTEIO", R), ("EDQUOT", R),
    ("ENOMEDIUM", R), ("EMEDIUMTYPE", R), ("ECANCELED", R), ("ENOKEY", R),
    ("EKEYEXPIRED", R), ("EKEYREVOKED", R), ("EKEYREJECTED", R),
    ("EOWNERDEAD", R), ("ENOTRECOVERABLE", R), ("ERFKILL", R), ("EHWPOISON", R),
];

const OFLAGS: &[(&str, bool)] = &[
    ("O_CREAT", R), ("O_EXCL", R), ("O_TRUNC", R), ("O_APPEND", R),
    ("O_NONBLOCK", R), ("O_CLOEXEC", R), ("O_DIRECTORY", R), ("O_NOFOLLOW", R),
    ("O_PATH", R), ("O_DSYNC", R),
    ("O_NOCTTY", D), ("O_ASYNC", D), ("O_DIRECT", D), ("O_NOATIME", D),
];

const FCNTL_CMDS: &[(&str, bool)] = &[
    ("F_GETLK", R), ("F_SETLK", R), ("F_SETLKW", R),
    ("F_SETOWN", R), ("F_GETOWN", R), ("F_DUPFD_CLOEXEC", R),
];

/// struct flock l_type values. The struct layouts agree (layouts.c pins
/// them), but these three are runtime constants on the cosmo side.
const LOCK_TYPES: &[(&str, bool)] = &[
    ("F_RDLCK", R), ("F_WRLCK", R), ("F_UNLCK", R),
];

/// termios flag bits, one domain per c_*flag field. The C side translates
/// bit by bit and silently drops what the host lacks — kernels ignore
/// unknown termios bits, so strictness here would only break raw-mode setup.
const TIFLAGS: &[(&str, bool)] = &[
    ("IGNBRK", R), ("BRKINT", R), ("IGNPAR", R), ("PARMRK", R), ("INPCK", R),
    ("ISTRIP", R), ("INLCR", R), ("IGNCR", R), ("ICRNL", R), ("IUCLC", R),
    ("IXON", R), ("IXANY", R), ("IXOFF", R), ("IMAXBEL", R), ("IUTF8", R),
];

const TOFLAGS: &[(&str, bool)] = &[
    ("OPOST", R), ("OLCUC", R), ("ONLCR", R), ("OCRNL", R), ("ONOCR", R),
    ("ONLRET", R), ("OFILL", R), ("OFDEL", R),
];

/// Output delay fields: multi-bit masks plus their nonzero values (every *0
/// value is 0 on both sides). Translated as enumerated fields, not as bits.
const TODLYS: &[(&str, bool)] = &[
    ("NLDLY", R), ("NL1", R),
    ("CRDLY", R), ("CR1", R), ("CR2", R), ("CR3", R),
    ("TABDLY", R), ("TAB1", R), ("TAB2", R), ("TAB3", R), ("XTABS", R),
    ("BSDLY", R), ("BS1", R),
    ("VTDLY", R), ("VT1", R),
    ("FFDLY", R), ("FF1", R),
];

const TCFLAGS: &[(&str, bool)] = &[
    ("CSIZE", R), ("CS6", R), ("CS7", R), ("CS8", R),
    ("CSTOPB", R), ("CREAD", R), ("PARENB", R), ("PARODD", R),
    ("HUPCL", R), ("CLOCAL", R), ("CRTSCTS", R), ("CMSPAR", R),
];

const TLFLAGS: &[(&str, bool)] = &[
    ("ISIG", R), ("ICANON", R), ("XCASE", R), ("ECHO", R), ("ECHOE", R),
    ("ECHOK", R), ("ECHONL", R), ("NOFLSH", R), ("TOSTOP", R), ("ECHOCTL", R),
    ("ECHOPRT", R), ("ECHOKE", R), ("FLUSHO", R), ("PENDIN", R), ("IEXTEN", R),
    ("EXTPROC", R),
];

/// c_cc indices, not bits: V* says where in c_cc a control character lives
/// (musl NCCS 32, cosmo NCCS 20); the repack moves bytes between indexings.
const TCCS: &[(&str, bool)] = &[
    ("VINTR", R), ("VQUIT", R), ("VERASE", R), ("VKILL", R), ("VEOF", R),
    ("VTIME", R), ("VMIN", R), ("VSWTC", R), ("VSTART", R), ("VSTOP", R),
    ("VSUSP", R), ("VEOL", R), ("VREPRINT", R), ("VDISCARD", R),
    ("VWERASE", R), ("VLNEXT", R), ("VEOL2", R),
];

/// Baud codes: enumerated values living in musl's CBAUD field of c_cflag and
/// in cosmo's _c_ispeed/_c_ospeed. B460800/B921600 exist in musl but not in
/// cosmo, so they are simply not mapped (unmatched rates pass unset).
const BAUDS: &[(&str, bool)] = &[
    ("B0", R), ("B50", R), ("B75", R), ("B110", R), ("B134", R), ("B150", R),
    ("B200", R), ("B300", R), ("B600", R), ("B1200", R), ("B1800", R),
    ("B2400", R), ("B4800", R), ("B9600", R), ("B19200", R), ("B38400", R),
    ("B57600", R), ("B115200", R), ("B230400", R), ("B500000", R),
    ("B576000", R), ("B1000000", R), ("B1152000", R), ("B1500000", R),
    ("B2000000", R), ("B2500000", R), ("B3000000", R), ("B3500000", R),
    ("B4000000", R),
];

const TCFLUSHS: &[(&str, bool)] = &[
    ("TCIFLUSH", R), ("TCOFLUSH", R), ("TCIOFLUSH", R),
];

const TCFLOWS: &[(&str, bool)] = &[
    ("TCOOFF", R), ("TCOON", R), ("TCIOFF", R), ("TCION", R),
];

/// ioctl request codes forwarded to cosmo's ioctl (struct winsize has the
/// same shape on both sides). The tty-config requests (TCGETS family) are
/// deliberately NOT here: the ioctl shim reroutes them through
/// tcgetattr/tcsetattr, so their Linux values live in SINGLES instead.
const TIOCS: &[(&str, bool)] = &[
    ("TIOCGWINSZ", R), ("TIOCSWINSZ", R),
    // Session control, which is what a pty child does between fork and exec.
    ("TIOCSCTTY", R), ("TIOCNOTTY", R),
];

const FIOS: &[(&str, bool)] = &[
    ("FIONREAD", R),
];

const POLLS: &[(&str, bool)] = &[
    ("POLLIN", R), ("POLLPRI", R), ("POLLOUT", R), ("POLLERR", R),
    ("POLLHUP", R), ("POLLNVAL", R), ("POLLRDNORM", R), ("POLLRDBAND", R),
    ("POLLWRNORM", R), ("POLLWRBAND", R), ("POLLRDHUP", R),
];

const MSGS: &[(&str, bool)] = &[
    ("MSG_DONTWAIT", R), ("MSG_WAITALL", R), ("MSG_NOSIGNAL", R),
    ("MSG_TRUNC", R), ("MSG_CTRUNC", R),
];

const SOS: &[(&str, bool)] = &[
    ("SO_REUSEADDR", R), ("SO_TYPE", R), ("SO_ERROR", R), ("SO_DONTROUTE", R),
    ("SO_BROADCAST", R), ("SO_SNDBUF", R), ("SO_RCVBUF", R), ("SO_KEEPALIVE", R),
    ("SO_OOBINLINE", R), ("SO_LINGER", R), ("SO_REUSEPORT", R),
    ("SO_RCVLOWAT", R), ("SO_SNDLOWAT", R), ("SO_RCVTIMEO", R),
    ("SO_SNDTIMEO", R), ("SO_ACCEPTCONN", R),
];

const TCPS: &[(&str, bool)] = &[
    ("TCP_NODELAY", R), ("TCP_MAXSEG", R), ("TCP_CORK", R), ("TCP_KEEPIDLE", R),
    ("TCP_KEEPINTVL", R), ("TCP_KEEPCNT", R), ("TCP_SYNCNT", R),
    ("TCP_DEFER_ACCEPT", R), ("TCP_FASTOPEN", R), ("TCP_QUICKACK", R),
];

const IPS: &[(&str, bool)] = &[
    ("IP_TOS", R), ("IP_TTL", R), ("IP_HDRINCL", R), ("IP_OPTIONS", R),
    ("IP_RECVTOS", R), ("IP_RECVTTL", R), ("IP_MULTICAST_IF", R),
    ("IP_MULTICAST_TTL", R), ("IP_MULTICAST_LOOP", R), ("IP_ADD_MEMBERSHIP", R),
    ("IP_DROP_MEMBERSHIP", R), ("IP_PKTINFO", R),
];

const IPV6S: &[(&str, bool)] = &[
    ("IPV6_V6ONLY", R), ("IPV6_UNICAST_HOPS", R), ("IPV6_MULTICAST_IF", R),
    ("IPV6_MULTICAST_HOPS", R), ("IPV6_MULTICAST_LOOP", R),
    ("IPV6_ADD_MEMBERSHIP", R), ("IPV6_DROP_MEMBERSHIP", R), ("IPV6_TCLASS", R),
    ("IPV6_RECVTCLASS", R), ("IPV6_PKTINFO", R),
];

/// The scraper reads every `pub const` without judging `cfg_if!` branches;
/// for the few names whose branches collide (the 32-bit time64 variants of
/// the SO_ timeouts), the 64-bit value is stated here explicitly. Wrong
/// entries cannot survive: the generated const asserts have rustc check
/// every value against the real cfg-resolved libc.
const CFG_OVERRIDES: &[(&str, i64, i64)] = &[
    // (name, x86_64, aarch64)
    ("SO_RCVTIMEO", 20, 20),
    ("SO_SNDTIMEO", 21, 21),
];

/// Names that newer cosmocc versions have removed outright because their
/// values became Linux-coded internally (no macro, no runtime symbol — the
/// shim's pass-through IS the correct translation). Older toolchains still
/// declare them as runtime constants. When the vendored cosmocc doesn't
/// declare one of these, it is skipped instead of failing the run.
const OPTIONAL_IF_ABSENT: &[&str] = &[
    "SIGIO", "SIGPOLL", "SIGPWR", "SIGSTKFLT",
    // dropped from newer cosmocc entirely (no support on any host there);
    // with no table entry the shim passes the raw cmd through and cosmo
    // answers EINVAL, which is the accurate "not supported" behavior
    "F_SETOWN", "F_GETOWN",
];

/// Headers that moved between cosmocc versions: (canonical, fallback).
const HEADER_FALLBACKS: &[(&str, &str)] = &[
    // rlimit consts became #defines living next to the struct
    ("sysv/consts/rlimit.h", "calls/struct/rlimit.h"),
];

/// libc crate name -> cosmo symbol name, where the two worlds disagree.
/// The C table carries the cosmo name (it takes the symbol's address); the
/// Rust assert uses the libc name.
fn cosmo_name(libc_name: &str) -> &str {
    match libc_name {
        "IPV6_ADD_MEMBERSHIP" => "IPV6_JOIN_GROUP",
        "IPV6_DROP_MEMBERSHIP" => "IPV6_LEAVE_GROUP",
        other => other,
    }
}

/// Table name -> the libc crate name whose value it carries, for the entries
/// cosmo publishes under a spelling Linux never had. Without this the value
/// lookup and the generated assert would both go looking for a `libc::` const
/// that does not exist.
fn libc_name(name: &str) -> &str {
    match name {
        // "attribute not found" from the xattr syscalls: BSD and XNU say
        // ENOATTR, Linux says ENODATA, and that is what the xattr crates
        // compare against. Untranslated, XNU's 93 reads back as
        // EPROTONOSUPPORT on the Rust side.
        "ENOATTR" => "ENODATA",
        other => other,
    }
}

const CLOCKS: &[(&str, bool)] = &[
    ("CLOCK_REALTIME", R), ("CLOCK_MONOTONIC", R),
    ("CLOCK_PROCESS_CPUTIME_ID", R), ("CLOCK_THREAD_CPUTIME_ID", R),
    ("CLOCK_MONOTONIC_RAW", R), ("CLOCK_REALTIME_COARSE", R),
    ("CLOCK_MONOTONIC_COARSE", R), ("CLOCK_BOOTTIME", R),
];

const MADVS: &[(&str, bool)] = &[
    ("MADV_NORMAL", R), ("MADV_RANDOM", R), ("MADV_SEQUENTIAL", R),
    ("MADV_WILLNEED", R), ("MADV_DONTNEED", R), ("MADV_FREE", R),
    ("MADV_DONTFORK", R), ("MADV_DOFORK", R), ("MADV_MERGEABLE", R),
    ("MADV_UNMERGEABLE", R), ("MADV_HUGEPAGE", R), ("MADV_NOHUGEPAGE", R),
    ("MADV_DONTDUMP", R), ("MADV_DODUMP", R), ("MADV_HWPOISON", R),
];

const RLIMITS: &[(&str, bool)] = &[
    ("RLIMIT_CPU", R), ("RLIMIT_FSIZE", R), ("RLIMIT_DATA", R),
    ("RLIMIT_STACK", R), ("RLIMIT_CORE", R), ("RLIMIT_RSS", R),
    ("RLIMIT_NPROC", R), ("RLIMIT_NOFILE", R), ("RLIMIT_MEMLOCK", R),
    ("RLIMIT_AS", R), ("RLIMIT_LOCKS", R), ("RLIMIT_SIGPENDING", R),
    ("RLIMIT_MSGQUEUE", R), ("RLIMIT_NICE", R), ("RLIMIT_RTPRIO", R),
];

const SIGS: &[(&str, bool)] = &[
    ("SIGBUS", R), ("SIGUSR1", R), ("SIGUSR2", R), ("SIGCHLD", R),
    ("SIGCONT", R), ("SIGSTOP", R), ("SIGTSTP", R), ("SIGURG", R),
    ("SIGIO", R), ("SIGPOLL", R), ("SIGPWR", R), ("SIGSYS", R),
    ("SIGSTKFLT", R),
];

const MAPS: &[(&str, bool)] = &[
    ("MAP_ANONYMOUS", R), ("MAP_FIXED_NOREPLACE", R), ("MAP_HUGETLB", R),
    ("MAP_NORESERVE", D), ("MAP_POPULATE", D), ("MAP_LOCKED", D),
];

/// sa_flags bits for sigaction. cosmo stores these as uint64_t.
const SAS: &[(&str, bool)] = &[
    ("SA_NOCLDSTOP", R), ("SA_NOCLDWAIT", R), ("SA_SIGINFO", R),
    ("SA_ONSTACK", R), ("SA_RESTART", R), ("SA_NODEFER", R),
    ("SA_RESETHAND", R),
];

/// sigaltstack ss_flags. SS_ONSTACK is 1 on both sides; SS_DISABLE varies.
const SSES: &[(&str, bool)] = &[
    ("SS_ONSTACK", R), ("SS_DISABLE", R),
];

/// getauxval() keys. cosmo synthesizes the auxv on non-Linux hosts and its
/// AT_* keys are runtime constants, so a Linux-coded key has to be mapped
/// before the lookup. AT_MINSIGSTKSZ is the one std actually reads (it sizes
/// sigaltstack with it); the HWCAP pair rides along for feature detection.
const AUXVS: &[(&str, bool)] = &[
    ("AT_MINSIGSTKSZ", R), ("AT_HWCAP", R), ("AT_HWCAP2", R),
];

/// Address families. UNSPEC/UNIX/INET are universal (cosmo #defines them);
/// AF_INET6 is the runtime one, and it also lives inside sockaddr_in6's
/// family field, which the socket shim rewrites in both directions.
const AFS: &[(&str, bool)] = &[
    ("AF_UNSPEC", R), ("AF_UNIX", R), ("AF_INET", R), ("AF_INET6", R),
];

/// Constants emitted as bare SHIM_LIN_* defines (no cosmo table needed, the
/// shim handles each one specially), still asserted on the Rust side.
const SINGLES: &[&str] = &[
    "O_LARGEFILE", "O_SYNC", "O_DIRECTORY", "O_TMPFILE",
    "AT_FDCWD", "AT_SYMLINK_NOFOLLOW", "AT_REMOVEDIR", "AT_SYMLINK_FOLLOW",
    "AT_EMPTY_PATH", "AT_EACCESS", "AT_NO_AUTOMOUNT",
    // access/faccessat amode bits. POSIX numbers these 4/2/1 and every Unix
    // agrees, but cosmo publishes NT's access mask for them on Windows, so
    // they need translating like any other constant. F_OK is 0 everywhere,
    // which an empty translated mask reproduces on its own.
    "R_OK", "W_OK", "X_OK",
    "UTIME_NOW", "UTIME_OMIT",
    "SOCK_CLOEXEC", "SOCK_NONBLOCK", "MAP_STACK",
    "SIG_BLOCK", "SIG_UNBLOCK", "SIG_SETMASK", "SO_ERROR",
    "SOL_SOCKET", "IPPROTO_IP", "IPPROTO_TCP", "IPPROTO_IPV6", "IPPROTO_UDP",
    "F_GETLK", "F_SETLK", "F_SETLKW", "F_SETOWN", "F_GETOWN",
    "F_DUPFD_CLOEXEC",
    // tty-config ioctl requests the shim reroutes to tcgetattr/tcsetattr
    // (kernel ABI numbers; on musl Ioctl is c_int so the *2 family is
    // negative), plus the musl-side baud fields of c_cflag.
    "TCGETS", "TCSETS", "TCSETSW", "TCSETSF",
    "TCGETS2", "TCSETS2", "TCSETSW2", "TCSETSF2",
    "BOTHER", "CBAUD", "CBAUDEX",
    // Job-control ioctl requests the shim reroutes to tcgetpgrp/tcsetpgrp/
    // tcgetsid; cosmo publishes no constant for the first and last of them.
    "TIOCGPGRP", "TIOCSPGRP", "TIOCGSID",
    // raw syscall rerouting: futex (std's parker, parking_lot & friends)
    // and getrandom (the getrandom crate's syscall path)
    "SYS_futex", "FUTEX_WAIT", "FUTEX_WAKE", "FUTEX_PRIVATE_FLAG",
    "FUTEX_CLOCK_REALTIME", "FUTEX_WAIT_BITSET", "FUTEX_WAKE_BITSET",
    "FUTEX_BITSET_MATCH_ANY", "SYS_getrandom",
];

fn domains() -> Vec<Domain> {
    vec![
        Domain { macro_name: "SHIM_ERRNO_TABLE", cosmo_header: "errno.h", names: ERRNOS, with_droppable_column: false, ctype: "int" },
        Domain { macro_name: "SHIM_OFLAG_TABLE", cosmo_header: "sysv/consts/o.h", names: OFLAGS, with_droppable_column: true, ctype: "unsigned" },
        Domain { macro_name: "SHIM_FCNTL_CMD_TABLE", cosmo_header: "sysv/consts/f.h", names: FCNTL_CMDS, with_droppable_column: false, ctype: "int" },
        Domain { macro_name: "SHIM_LOCKTYPE_TABLE", cosmo_header: "sysv/consts/f.h", names: LOCK_TYPES, with_droppable_column: false, ctype: "int" },
        Domain { macro_name: "SHIM_POLL_TABLE", cosmo_header: "sysv/consts/poll.h", names: POLLS, with_droppable_column: false, ctype: "int16_t" },
        Domain { macro_name: "SHIM_MSG_TABLE", cosmo_header: "sysv/consts/msg.h", names: MSGS, with_droppable_column: false, ctype: "int" },
        Domain { macro_name: "SHIM_SO_TABLE", cosmo_header: "sysv/consts/so.h", names: SOS, with_droppable_column: false, ctype: "int" },
        Domain { macro_name: "SHIM_TCP_TABLE", cosmo_header: "sysv/consts/tcp.h", names: TCPS, with_droppable_column: false, ctype: "int" },
        Domain { macro_name: "SHIM_IP_TABLE", cosmo_header: "sysv/consts/ip.h", names: IPS, with_droppable_column: false, ctype: "int" },
        Domain { macro_name: "SHIM_IPV6_TABLE", cosmo_header: "sysv/consts/ipv6.h", names: IPV6S, with_droppable_column: false, ctype: "int" },
        Domain { macro_name: "SHIM_MAP_TABLE", cosmo_header: "sysv/consts/map.h", names: MAPS, with_droppable_column: true, ctype: "int" },
        Domain { macro_name: "SHIM_CLOCK_TABLE", cosmo_header: "sysv/consts/clock.h", names: CLOCKS, with_droppable_column: false, ctype: "int" },
        Domain { macro_name: "SHIM_MADV_TABLE", cosmo_header: "sysv/consts/madv.h", names: MADVS, with_droppable_column: false, ctype: "unsigned" },
        Domain { macro_name: "SHIM_RLIMIT_TABLE", cosmo_header: "sysv/consts/rlimit.h", names: RLIMITS, with_droppable_column: false, ctype: "unsigned" },
        Domain { macro_name: "SHIM_SIG_TABLE", cosmo_header: "sysv/consts/sig.h", names: SIGS, with_droppable_column: false, ctype: "int" },
        Domain { macro_name: "SHIM_SA_TABLE", cosmo_header: "sysv/consts/sa.h", names: SAS, with_droppable_column: false, ctype: "uint64_t" },
        Domain { macro_name: "SHIM_SS_TABLE", cosmo_header: "sysv/consts/ss.h", names: SSES, with_droppable_column: false, ctype: "int" },
        Domain { macro_name: "SHIM_AUXV_TABLE", cosmo_header: "sysv/consts/auxv.h", names: AUXVS, with_droppable_column: false, ctype: "unsigned long" },
        Domain { macro_name: "SHIM_AF_TABLE", cosmo_header: "sysv/consts/af.h", names: AFS, with_droppable_column: false, ctype: "int" },
        Domain { macro_name: "SHIM_TIFLAG_TABLE", cosmo_header: "sysv/consts/termios.h", names: TIFLAGS, with_droppable_column: false, ctype: "uint32_t" },
        Domain { macro_name: "SHIM_TOFLAG_TABLE", cosmo_header: "sysv/consts/termios.h", names: TOFLAGS, with_droppable_column: false, ctype: "uint32_t" },
        Domain { macro_name: "SHIM_TODLY_TABLE", cosmo_header: "sysv/consts/termios.h", names: TODLYS, with_droppable_column: false, ctype: "uint32_t" },
        Domain { macro_name: "SHIM_TCFLAG_TABLE", cosmo_header: "sysv/consts/termios.h", names: TCFLAGS, with_droppable_column: false, ctype: "uint32_t" },
        Domain { macro_name: "SHIM_TLFLAG_TABLE", cosmo_header: "sysv/consts/termios.h", names: TLFLAGS, with_droppable_column: false, ctype: "uint32_t" },
        Domain { macro_name: "SHIM_TCC_TABLE", cosmo_header: "sysv/consts/termios.h", names: TCCS, with_droppable_column: false, ctype: "uint8_t" },
        Domain { macro_name: "SHIM_BAUD_TABLE", cosmo_header: "sysv/consts/baud.internal.h", names: BAUDS, with_droppable_column: false, ctype: "uint32_t" },
        Domain { macro_name: "SHIM_TCFLUSH_TABLE", cosmo_header: "sysv/consts/termios.h", names: TCFLUSHS, with_droppable_column: false, ctype: "int" },
        Domain { macro_name: "SHIM_TCFLOW_TABLE", cosmo_header: "sysv/consts/termios.h", names: TCFLOWS, with_droppable_column: false, ctype: "int" },
        Domain { macro_name: "SHIM_TIOC_TABLE", cosmo_header: "sysv/consts/termios.h", names: TIOCS, with_droppable_column: false, ctype: "uint64_t" },
        Domain { macro_name: "SHIM_FIO_TABLE", cosmo_header: "sysv/consts/fio.h", names: FIOS, with_droppable_column: false, ctype: "uint32_t" },
    ]
}

/// Ask cosmocc's preprocessor what each cosmo-side name expands to. Names
/// that resolve to a plain integer are fixed portable values (`#define
/// TCP_NODELAY 1`): the table cannot take their address, so tables.h gives
/// them local storage instead. Names that stay identifiers are the extern
/// runtime constants.
fn classify_cosmo(root: &Path, names: &[(&str, &str)]) -> Result<HashMap<String, Option<i64>>> {
    let mut probe = String::from(
        "#include <errno.h>\n#include <fcntl.h>\n#include <poll.h>\n\
         #include <sys/socket.h>\n#include <sys/mman.h>\n\
         #include <libc/sysv/consts/at.h>\n#include <libc/sysv/consts/utime.h>\n\
         #include <libc/sysv/consts/sock.h>\n#include <libc/sysv/consts/so.h>\n\
         #include <libc/sysv/consts/sol.h>\n#include <libc/sysv/consts/tcp.h>\n\
         #include <libc/sysv/consts/ip.h>\n#include <libc/sysv/consts/ipv6.h>\n\
         #include <libc/sysv/consts/msg.h>\n#include <libc/sysv/consts/map.h>\n\
         #include <libc/sysv/consts/clock.h>\n#include <libc/sysv/consts/madv.h>\n\
         #if __has_include(<libc/sysv/consts/rlimit.h>)\n\
         #include <libc/sysv/consts/rlimit.h>\n\
         #else\n#include <libc/calls/struct/rlimit.h>\n#endif\n\
         #include <libc/sysv/consts/sig.h>\n\
         #include <libc/sysv/consts/sa.h>\n#include <libc/sysv/consts/ss.h>\n\
         #include <libc/sysv/consts/auxv.h>\n\
         #include <libc/sysv/consts/af.h>\n\
         #include <libc/sysv/consts/termios.h>\n\
         #include <libc/sysv/consts/baud.internal.h>\n\
         #include <libc/sysv/consts/fio.h>\n",
    );
    for (_, cname) in names {
        // A string literal survives expansion; the bare name after it is the
        // probe. cpp linemarkers shred line structure, so parsing joins
        // everything and splits on the quotes again.
        let _ = writeln!(probe, "\"{cname}\" {cname}");
    }
    // /dev/shm: plain tmp directories can hand the compiler ciphertext here
    let probe_path = Path::new("/dev/shm/rust-ape-genshim-probe.c");
    fs::write(probe_path, probe)?;
    let out = util::capture(
        std::process::Command::new(root.join("vendor/cosmocc/bin/x86_64-unknown-cosmo-cc"))
            .arg("-E")
            .arg(probe_path),
    )?;
    let _ = fs::remove_file(probe_path);
    let joined: String = out
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join(" ");
    let mut map = HashMap::new();
    let mut parts = joined.split('"');
    let _leading = parts.next();
    while let (Some(name), Some(expansion)) = (parts.next(), parts.next()) {
        let cleaned: String = expansion
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '(' && *c != ')')
            .collect();
        map.insert(name.to_string(), eval(&cleaned, &HashMap::new(), 7).ok());
    }
    for (_, cname) in names {
        if !map.contains_key(*cname) {
            bail!("preprocessor probe lost track of {cname}");
        }
    }
    Ok(map)
}

/// All `pub const NAME: ty = expr;` in one file, expression text unevaluated.
fn scrape_consts(path: &Path, out: &mut HashMap<String, String>) {
    let Ok(text) = fs::read_to_string(path) else { return };
    for line in text.lines() {
        let t = line.trim();
        let Some(rest) = t.strip_prefix("pub const ") else { continue };
        let Some((name, rest)) = rest.split_once(':') else { continue };
        let Some((_ty, expr)) = rest.split_once('=') else { continue };
        let Some(expr) = expr.trim().strip_suffix(';') else { continue };
        // first definition wins: files are scanned most-specific first
        out.entry(name.trim().to_string()).or_insert_with(|| expr.trim().to_string());
    }
}

/// Evaluate an extracted expression: integer literals (0x/0o/decimal, with _
/// separators and a possible `as` cast), identifiers (looked up recursively)
/// and `|` combinations thereof.
fn eval(expr: &str, consts: &HashMap<String, String>, depth: u32) -> Result<i64> {
    if depth > 8 {
        bail!("expression recurses too deep: {expr}");
    }
    let expr = expr.trim();
    if let Some((l, r)) = expr.split_once('|') {
        return Ok(eval(l, consts, depth + 1)? | eval(r, consts, depth + 1)?);
    }
    // libc wraps would-be-negative bit patterns: SA_RESETHAND = u32_cast_int(0x80000000).
    // Evaluate the inside, then reproduce the u32 -> i32 reinterpretation.
    if let Some(inner) = expr.strip_prefix("u32_cast_int(").and_then(|r| r.strip_suffix(')')) {
        return Ok(eval(inner, consts, depth + 1)? as u32 as i32 as i64);
    }
    // Same wrapper, ioctl flavor: musl's Ioctl is c_int, so TCGETS2 & co are
    // negative there.
    if let Some(inner) = expr.strip_prefix("u32_cast_ioctl(").and_then(|r| r.strip_suffix(')')) {
        return Ok(eval(inner, consts, depth + 1)? as u32 as i32 as i64);
    }
    let expr = expr.split(" as ").next().unwrap().trim();
    let expr = expr.trim_start_matches("crate::");
    let lit = expr.replace('_', "");
    let parsed = if let Some(h) = lit.strip_prefix("0x") {
        i64::from_str_radix(h, 16).ok()
    } else if let Some(o) = lit.strip_prefix("0o") {
        i64::from_str_radix(o, 8).ok()
    } else if lit.starts_with('0') && lit.len() > 1 && lit.chars().all(|c| c.is_ascii_digit()) {
        i64::from_str_radix(&lit[1..], 8).ok() // C-style octal shows up too
    } else {
        lit.parse::<i64>().ok()
    };
    if let Some(v) = parsed {
        return Ok(v);
    }
    let target = consts
        .get(expr)
        .with_context(|| format!("cannot resolve identifier {expr:?}"))?;
    eval(target, consts, depth + 1)
}

/// Verify a name is declared in the given cosmo header, as an extern runtime
/// constant or as a fixed #define (both are linkable/usable from the shim).
fn cosmo_declares(header_text: &str, name: &str) -> bool {
    header_text.lines().any(|l| {
        let t = l.trim();
        // last word, not a fixed position: the type may be multi-word
        // ("extern const unsigned long AT_MINSIGSTKSZ;")
        (t.starts_with("extern const") && t.split_whitespace().last().map(|w| w.trim_end_matches(';')) == Some(name))
            || t.starts_with(&format!("#define {name} "))
            || t.starts_with(&format!("#define {name}\t"))
    })
}

pub fn run(args: &GenShimArgs) -> Result<()> {
    let root = util::repo_root();

    // Scrape both arches' constant space once.
    let mut per_arch: HashMap<&str, HashMap<String, String>> = HashMap::new();
    for &arch in ARCHES {
        let mut consts = HashMap::new();
        for p in libc_search_paths(&root, arch) {
            scrape_consts(&p, &mut consts);
        }
        if consts.is_empty() {
            bail!("no constants scraped for {arch}; is vendor/patches/libc populated?");
        }
        per_arch.insert(arch, consts);
    }
    let value = |arch: &str, name: &str| -> Result<i64> {
        let name = libc_name(name);
        if let Some(&(_, x, a)) = CFG_OVERRIDES.iter().find(|(n, _, _)| *n == name) {
            return Ok(if arch == "x86_64" { x } else { a });
        }
        let consts = &per_arch[arch];
        let expr = consts
            .get(name)
            .with_context(|| format!("{name} not found in the libc crate for {arch}"))?;
        eval(expr, consts, 0).with_context(|| format!("evaluating {name} for {arch}"))
    };

    let mut h = String::new();
    let mut rs = String::new();
    let _ = writeln!(h, "/* Generated by `cargo xtask gen-shim`. DO NOT EDIT.");
    let _ = writeln!(h, " *");
    let _ = writeln!(h, " * Left column: the value musl bakes into the Rust world at compile");
    let _ = writeln!(h, " * time. Right side (taken by the shim as &NAME): cosmo's runtime");
    let _ = writeln!(h, " * constant for the same name. Cross-checked at build time by");
    let _ = writeln!(h, " * examples/src/bin/shim_tables_check.rs. */");
    let _ = writeln!(h, "#ifndef RUST_APE_SHIM_TABLES_H_");
    let _ = writeln!(h, "#define RUST_APE_SHIM_TABLES_H_");
    let _ = writeln!(rs, "//! Generated by `cargo xtask gen-shim`. DO NOT EDIT.");
    let _ = writeln!(rs, "//!");
    let _ = writeln!(rs, "//! One const assert per value in shim/tables.h: if the extraction ever");
    let _ = writeln!(rs, "//! disagrees with what rustc resolves libc's constants to, the build of");
    let _ = writeln!(rs, "//! either target fails right here instead of misbehaving at runtime.");
    let _ = writeln!(rs, "#![allow(overflowing_literals)]");
    let _ = writeln!(rs);

    let assert_line = |rs: &mut String, arch: &str, name: &str, v: i64| {
        let name = libc_name(name);
        let _ = writeln!(
            rs,
            "#[cfg(target_arch = \"{arch}\")] const _: () = assert!(libc::{name} as i64 == {v});"
        );
    };

    // SHIM_LIN_* singles first, then the tables.
    let _ = writeln!(h, "\n/* one-off Linux values the shim logic handles specially */");
    for &name in SINGLES {
        let x = value("x86_64", name)?;
        let a = value("aarch64", name)?;
        if x == a {
            let _ = writeln!(h, "#define SHIM_LIN_{name} {x}");
        } else {
            let _ = writeln!(h, "#if defined(__x86_64__)");
            let _ = writeln!(h, "#define SHIM_LIN_{name} {x}");
            let _ = writeln!(h, "#elif defined(__aarch64__)");
            let _ = writeln!(h, "#define SHIM_LIN_{name} {a}");
            let _ = writeln!(h, "#endif");
        }
        assert_line(&mut rs, "x86_64", name, x);
        assert_line(&mut rs, "aarch64", name, a);
    }

    // Resolve every name's cosmo-side spelling first: the canonical name, a
    // `_`-prefixed variant (newer cosmocc renamed O_PATH -> _O_PATH), or
    // absent (skippable for the errno domain and OPTIONAL_IF_ABSENT names —
    // those became Linux-coded internally, so pass-through is correct). The
    // preprocessor probe must ask about the RESOLVED spelling, otherwise a
    // renamed macro classifies as a runtime symbol and the table would take
    // the address of something that no longer exists.
    let read_domain_header = |cosmo_header: &str| -> Result<String> {
        let mut header_path = root.join("vendor/cosmocc/include/libc").join(cosmo_header);
        if !header_path.is_file() {
            if let Some(&(_, fb)) = HEADER_FALLBACKS.iter().find(|&&(c, _)| c == cosmo_header) {
                header_path = root.join("vendor/cosmocc/include/libc").join(fb);
            }
        }
        fs::read_to_string(&header_path)
            .with_context(|| format!("could not read {}", header_path.display()))
    };
    let mut resolved: HashMap<(&str, &str), Option<String>> = HashMap::new();
    for d in domains() {
        let header = read_domain_header(d.cosmo_header)?;
        for &(name, droppable) in d.names {
            let base = cosmo_name(name);
            let underscored = format!("_{base}");
            let cname = if cosmo_declares(&header, base) {
                Some(base.to_string())
            } else if cosmo_declares(&header, &underscored) {
                println!("note: {base} is spelled {underscored} in this cosmocc");
                Some(underscored)
            } else if OPTIONAL_IF_ABSENT.contains(&base)
                || d.macro_name == "SHIM_ERRNO_TABLE"
                || d.macro_name == "SHIM_MADV_TABLE" // advice is a hint; absent = unsupported
                // culled rlimits are Linux-only resources: pass-through is
                // identity on Linux and accurately EINVALs elsewhere
                || d.macro_name == "SHIM_RLIMIT_TABLE"
                || droppable
            {
                // droppable names are hints by definition; a cosmocc that
                // removed one simply doesn't support it anywhere.
                println!("note: {base} is gone from this cosmocc; pass-through/drop");
                None
            } else {
                bail!("{base} is not declared in cosmo's libc/{}", d.cosmo_header);
            };
            resolved.insert((d.macro_name, name), cname);
        }
    }
    let probe_names: Vec<(&str, String)> = domains()
        .iter()
        .flat_map(|d| {
            d.names.iter().filter_map(|&(n, _)| {
                resolved[&(d.macro_name, n)].clone().map(|c| (n, c))
            })
        })
        .collect();
    let all_names: Vec<(&str, &str)> =
        probe_names.iter().map(|(n, c)| (*n, c.as_str())).collect();
    let fixedness = classify_cosmo(&root, &all_names)?;

    for d in domains() {
        let _ = writeln!(h, "\n/* {} <- libc crate; cosmo side declared in libc/{} */", d.macro_name, d.cosmo_header);
        // Arch-differing names become SHIM_LIN_<name> defines above the macro.
        let mut lines = Vec::new();
        for &(name, droppable) in d.names {
            let Some(cname) = resolved[&(d.macro_name, name)].as_deref() else {
                continue;
            };
            let x = value("x86_64", name)?;
            let a = value("aarch64", name)?;
            if x == a {
                let _ = writeln!(h, "#define SHIM_LIN_{name} {x}");
            } else {
                let _ = writeln!(h, "#if defined(__x86_64__)");
                let _ = writeln!(h, "#define SHIM_LIN_{name} {x}");
                let _ = writeln!(h, "#elif defined(__aarch64__)");
                let _ = writeln!(h, "#define SHIM_LIN_{name} {a}");
                let _ = writeln!(h, "#endif");
            }
            let cell = format!("SHIM_LIN_{name}");
            // Fixed portable values get addressable storage; the X user
            // always does &<first-arg>, so hand it the right identifier.
            let sym = match fixedness[cname] {
                Some(v) => {
                    let _ = writeln!(
                        h,
                        "static const {} SHIM_FIX_{cname} = {v}; /* cosmo fixes this per-platform-invariant */",
                        d.ctype
                    );
                    format!("SHIM_FIX_{cname}")
                }
                None => cname.to_string(),
            };
            if d.with_droppable_column {
                lines.push(format!("  X({sym}, {cell}, {}) \\", droppable as u8));
            } else {
                lines.push(format!("  X({sym}, {cell}) \\"));
            }
            assert_line(&mut rs, "x86_64", name, x);
            assert_line(&mut rs, "aarch64", name, a);
        }
        let _ = writeln!(h, "#define {}(X) \\", d.macro_name);
        for l in &lines {
            let _ = writeln!(h, "{l}");
        }
        let _ = writeln!(h, "  /* end {} */", d.macro_name);
    }
    let _ = writeln!(h, "\n#endif /* RUST_APE_SHIM_TABLES_H_ */");

    let _ = writeln!(rs, "\nfn main() {{");
    let _ = writeln!(rs, "    println!(\"shim tables check ok (it already passed: the checks are at compile time)\");");
    let _ = writeln!(rs, "}}");

    if args.dry_run {
        println!("{h}");
        return Ok(());
    }
    let h_path = root.join("shim/tables.h");
    let rs_path = root.join("examples/src/bin/shim_tables_check.rs");
    fs::write(&h_path, &h)?;
    fs::write(&rs_path, &rs)?;
    println!("==> wrote {} ({} lines)", h_path.display(), h.lines().count());
    println!("==> wrote {} ({} lines)", rs_path.display(), rs.lines().count());
    Ok(())
}
