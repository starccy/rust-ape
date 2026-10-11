//! cosmo's side: the value each constant has there, from the headers
//! and consts.sh.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::{Cosmo, Syscon};
use crate::util;

const C_HEADERS: &[&str] = &[
    "errno.h",
    "fcntl.h",
    "unistd.h",
    "signal.h",
    "spawn.h",
    "sched.h",
    "pthread.h",
    "poll.h",
    "termios.h",
    "time.h",
    "dirent.h",
    "dlfcn.h",
    "limits.h",
    "locale.h",
    "langinfo.h",
    "netdb.h",
    "syslog.h",
    "semaphore.h",
    "fnmatch.h",
    "glob.h",
    "regex.h",
    "grp.h",
    "pwd.h",
    "stdio.h",
    "stdlib.h",
    "ifaddrs.h",
    "elf.h",
    "utime.h",
    "wordexp.h",
    "nl_types.h",
    "iconv.h",
    "sys/file.h",
    "sys/stat.h",
    "sys/socket.h",
    "sys/un.h",
    "sys/mman.h",
    "sys/wait.h",
    "sys/resource.h",
    "sys/ioctl.h",
    "sys/uio.h",
    "sys/time.h",
    "sys/times.h",
    "sys/utsname.h",
    "sys/statvfs.h",
    "sys/statfs.h",
    "sys/select.h",
    "sys/sysinfo.h",
    "sys/xattr.h",
    "sys/random.h",
    "sys/auxv.h",
    "sys/ptrace.h",
    "sys/prctl.h",
    "sys/ipc.h",
    "sys/shm.h",
    "sys/sem.h",
    "sys/msg.h",
    "sys/mount.h",
    "sys/timerfd.h",
    "sys/eventfd.h",
    "sys/inotify.h",
    "sys/epoll.h",
    "sys/reboot.h",
    "sys/syscall.h",
    "sys/vfs.h",
    "sys/param.h",
    "sys/sysmacros.h",
    "sys/personality.h",
    "netinet/in.h",
    "netinet/tcp.h",
    "netinet/udp.h",
    "netinet/ip.h",
    "netinet/ip_icmp.h",
    "arpa/inet.h",
    "net/if.h",
    "net/if_arp.h",
    "net/ethernet.h",
];

/// libc crate name -> cosmo symbol name, where the two worlds disagree.
fn cosmo_name(libc_name: &str) -> &str {
    match libc_name {
        "IPV6_ADD_MEMBERSHIP" => "IPV6_JOIN_GROUP",
        "IPV6_DROP_MEMBERSHIP" => "IPV6_LEAVE_GROUP",
        other => other,
    }
}

/// Compile `const __int128 AUDIT_<name> = (__int128)(<name>)` for every name
/// to assembly and read the values back. What gcc refuses tells the rest:
/// undeclared, a runtime constant, or not an integer.
pub(super) fn cosmo_values(
    root: &Path,
    arch: &str,
    names: &[&str],
) -> Result<HashMap<String, (String, Cosmo)>> {
    let mut out: HashMap<String, (String, Cosmo)> = HashMap::new();
    let mut spelled: Vec<(String, String)> = names
        .iter()
        .map(|n| (n.to_string(), cosmo_name(n).to_string()))
        .collect();
    // second pass: what was undeclared gets one more try as _NAME
    for pass in 0..2 {
        let got = cosmo_probe(root, arch, &spelled)?;
        let mut retry = Vec::new();
        for (name, cname) in &spelled {
            let c = got[cname.as_str()];
            if c == Cosmo::Absent && pass == 0 {
                retry.push((name.clone(), format!("_{cname}")));
            }
            if pass == 0 || c != Cosmo::Absent {
                out.insert(name.clone(), (cname.clone(), c));
            }
        }
        spelled = retry;
        if spelled.is_empty() {
            break;
        }
    }
    Ok(out)
}

fn cosmo_probe(
    root: &Path,
    arch: &str,
    spelled: &[(String, String)],
) -> Result<HashMap<String, Cosmo>> {
    let cc = root.join(format!("vendor/cosmocc/bin/{arch}-unknown-cosmo-cc"));
    let inc = root.join("vendor/cosmocc/include");
    let mut header = String::from("#define _COSMO_SOURCE\n#define _GNU_SOURCE\n");
    for h in C_HEADERS {
        let _ = writeln!(header, "#if __has_include(<{h}>)\n#include <{h}>\n#endif");
    }
    let mut consts_headers = Vec::new();
    util::walk(&inc.join("libc/sysv/consts"), "h", &mut consts_headers)?;
    consts_headers.sort();
    for h in consts_headers {
        let rel = h.strip_prefix(&inc).unwrap().display().to_string();
        // assembler-only
        if rel.ends_with("syscon.internal.h") {
            continue;
        }
        let _ = writeln!(header, "#include <{rel}>");
    }
    let header_lines = header.lines().count();

    let generated = root.join("generated");
    fs::create_dir_all(&generated)?;
    let src = generated.join(format!("audit-probe-{arch}.c"));
    let asm = generated.join(format!("audit-probe-{arch}.s"));
    let mut result: HashMap<String, Cosmo> = HashMap::new();
    let mut live: Vec<&str> = spelled.iter().map(|(_, c)| c.as_str()).collect();
    live.sort();
    live.dedup();
    for _round in 0..8 {
        let mut text = header.clone();
        for c in &live {
            let _ = writeln!(text, "const __int128 AUDIT_{c} = (__int128)({c});");
        }
        fs::write(&src, text)?;
        let out = Command::new(&cc)
            .args(["-S", "-O0", "-w", "-fmax-errors=0", "-o"])
            .arg(&asm)
            .arg(&src)
            .output()
            .context("running cosmocc for the constant probe")?;
        if out.status.success() {
            for (name, v) in parse_asm(&fs::read_to_string(&asm)?)? {
                result.insert(name, Cosmo::Fixed(v));
            }
            let _ = fs::remove_file(&src);
            let _ = fs::remove_file(&asm);
            return Ok(result);
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        let prefix = format!("{}:", src.display());
        let mut bad: HashMap<usize, Cosmo> = HashMap::new();
        for line in stderr.lines() {
            let Some(rest) = line.strip_prefix(&prefix) else {
                continue;
            };
            let Some((lineno, rest)) = rest.split_once(':') else {
                continue;
            };
            let Ok(lineno) = lineno.parse::<usize>() else {
                continue;
            };
            if !rest.contains(" error: ") {
                continue;
            }
            let why = if rest.contains("undeclared") {
                Cosmo::Absent
            } else if rest.contains("substitute constant") || rest.contains("is not constant") {
                Cosmo::Runtime
            } else {
                Cosmo::Other
            };
            // an undeclared name also trips follow-up errors; keep the first
            bad.entry(lineno).or_insert(why);
        }
        if bad.is_empty() {
            bail!("cosmocc failed without naming a probe line:\n{stderr}");
        }
        let mut next = Vec::new();
        for (i, c) in live.iter().enumerate() {
            match bad.get(&(i + 1 + header_lines)) {
                Some(why) => {
                    result.insert(c.to_string(), *why);
                }
                None => next.push(*c),
            }
        }
        live = next;
    }
    bail!("the cosmocc probe for {arch} still doesn't compile after 8 rounds of pruning")
}

/// `AUDIT_X:` followed by two 64-bit words (low, high) or `.zero 16`.
fn parse_asm(asm: &str) -> Result<Vec<(String, i128)>> {
    let mut out = Vec::new();
    let mut lines = asm.lines().peekable();
    while let Some(line) = lines.next() {
        let Some(name) = line
            .strip_prefix("AUDIT_")
            .and_then(|l| l.strip_suffix(':'))
        else {
            continue;
        };
        let mut words: Vec<i128> = Vec::new();
        while words.len() < 2 {
            let Some(l) = lines.next() else { break };
            let mut it = l.split_whitespace();
            match (it.next(), it.next()) {
                (Some(".quad" | ".xword"), Some(v)) => words.push(v.parse::<i128>()?),
                (Some(".zero"), Some(n)) => {
                    for _ in 0..n.parse::<usize>()? / 8 {
                        words.push(0);
                    }
                }
                _ => bail!("unexpected directive after AUDIT_{name}: {l}"),
            }
        }
        if words.len() < 2 {
            bail!("short initializer for AUDIT_{name}");
        }
        let low = words[0] as u64 as u128;
        let high = words[1] as u64 as u128;
        out.push((name.to_string(), ((high << 64) | low) as i128));
    }
    Ok(out)
}

pub(super) fn parse_consts_sh(path: &Path) -> Result<HashMap<String, Syscon>> {
    let text = fs::read_to_string(path)?;
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("");
        let mut it = line.split_whitespace();
        if it.next() != Some("syscon") {
            continue;
        }
        let (Some(group), Some(name)) = (it.next(), it.next()) else {
            continue;
        };
        let cols: Vec<Option<i128>> = it.take(8).map(parse_c_int).collect();
        out.insert(
            name.to_string(),
            Syscon {
                group: group.to_string(),
                cols,
            },
        );
    }
    Ok(out)
}

fn parse_c_int(s: &str) -> Option<i128> {
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let s = s.trim_end_matches(['u', 'U', 'l', 'L']);
    let v = if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        i128::from_str_radix(h, 16).ok()?
    } else if let Some(b) = s.strip_prefix("0b") {
        i128::from_str_radix(b, 2).ok()?
    } else if s.len() > 1 && s.starts_with('0') {
        i128::from_str_radix(&s[1..], 8).ok()?
    } else {
        s.parse().ok()?
    };
    Some(if neg { -v } else { v })
}

// ---------------------------------------------------------------------- shim
