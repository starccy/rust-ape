//! The /proc and /sys surface a monitoring program relies on, as a matrix:
//! every case reports on its own and the process fails at the end if any
//! did. The files are read directly first, in every spelling procfs-style
//! crates use, then the sysinfo crate is asked what a monitor asks.
//!
//! Linux has the real thing; Windows and Apple Silicon macOS run the
//! emulation in shim/procfs/, so all three must pass every case. The BSDs
//! have no /proc, real or emulated, and skip the matrix.

use std::ffi::CString;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::path::Path;

use sysinfo::{Networks, Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use rust_ape_examples::Report;

fn e<T>(what: &str, r: Result<T, std::io::Error>) -> Result<T, String> {
    r.map_err(|err| format!("{what}: {err}"))
}

fn read(path: &str) -> Result<String, String> {
    e(&format!("read {path}"), fs::read_to_string(path))
}

// A file that must exist and whose text must contain `needle`.
fn has(path: &str, needle: &str) -> Result<String, String> {
    let s = read(path)?;
    if !s.contains(needle) {
        return Err(format!("{path} lacks {needle:?}: {:?}", s.lines().next()));
    }
    Ok(format!("{path} has {needle}"))
}

// "Key: value" files. Returns the first number after `key`.
fn field(path: &str, key: &str) -> Result<u64, String> {
    let s = read(path)?;
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix(key) {
            let rest = rest.trim_start_matches(':').trim();
            let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            return num.parse().map_err(|_| format!("{path}: {key} is {rest:?}"));
        }
    }
    Err(format!("{path}: no {key}"))
}

fn positive(path: &str, key: &str) -> Result<String, String> {
    let v = field(path, key)?;
    if v == 0 {
        return Err(format!("{path}: {key} is 0"));
    }
    Ok(format!("{key} = {v}"))
}

fn stat_fields(pid: &str) -> Result<Vec<String>, String> {
    let s = read(&format!("/proc/{pid}/stat"))?;
    // comm sits in parentheses and may hold spaces; split after it
    let close = s.rfind(')').ok_or("stat: no comm")?;
    let mut v = vec![s[..s.find('(').unwrap_or(0)].trim().to_string()];
    v.push(s[s.find('(').map(|i| i + 1).unwrap_or(0)..close].to_string());
    v.extend(s[close + 1..].split_whitespace().map(str::to_string));
    if v.len() < 44 {
        return Err(format!("stat has {} fields", v.len()));
    }
    Ok(v)
}

fn direct(rep: &mut Report) {
    let pid = std::process::id();
    let me = pid.to_string();

    // --- top level, what `top`, `uptime` and `free` read
    rep.check("stat-cpu-lines", (|| {
        let s = read("/proc/stat")?;
        let cpus = s.lines().filter(|l| l.starts_with("cpu") && !l.starts_with("cpu ")).count();
        if !s.starts_with("cpu ") || cpus == 0 {
            return Err(format!("no cpu lines: {:?}", s.lines().next()));
        }
        has("/proc/stat", "btime ")?;
        Ok(format!("{cpus} cpus"))
    })());
    rep.check("meminfo", (|| {
        positive("/proc/meminfo", "MemTotal")?;
        positive("/proc/meminfo", "MemAvailable")?;
        let total = field("/proc/meminfo", "MemTotal")?;
        let avail = field("/proc/meminfo", "MemAvailable")?;
        if avail > total {
            return Err(format!("MemAvailable {avail} > MemTotal {total}"));
        }
        Ok(format!("{} MiB total", total / 1024))
    })());
    rep.check("uptime-loadavg", (|| {
        let up = read("/proc/uptime")?;
        let secs: f64 = up.split_whitespace().next().unwrap_or("").parse().map_err(|_| format!("uptime {up:?}"))?;
        let la = read("/proc/loadavg")?;
        if la.split_whitespace().count() < 5 {
            return Err(format!("loadavg {la:?}"));
        }
        Ok(format!("up {secs:.0}s, loadavg {}", la.trim()))
    })());
    rep.check("cpuinfo", (|| {
        let s = read("/proc/cpuinfo")?;
        let n = s.lines().filter(|l| l.starts_with("processor")).count();
        if n == 0 {
            return Err("no processor entries".into());
        }
        Ok(format!("{n} processors"))
    })());
    rep.check("version-mounts", (|| {
        let v = read("/proc/version")?;
        let ostype = read("/proc/sys/kernel/ostype")?;
        let rel = read("/proc/sys/kernel/osrelease")?;
        if !v.starts_with(ostype.trim()) {
            return Err(format!("version {v:?} vs ostype {ostype:?}"));
        }
        has("/proc/mounts", " / ")?;
        Ok(format!("{} {}", ostype.trim(), rel.trim()))
    })());
    rep.check("net", (|| {
        let dev = read("/proc/net/dev")?;
        let ifs = dev.lines().skip(2).filter(|l| l.contains(':')).count();
        if ifs == 0 {
            return Err("net/dev lists nothing".into());
        }
        has("/proc/net/tcp", "local_address")?;
        has("/proc/net/udp", "local_address")?;
        Ok(format!("{ifs} interfaces"))
    })());

    // --- the process's own directory, both spellings
    rep.check("self-is-pid", (|| {
        let link = e("readlink /proc/self", fs::read_link("/proc/self"))?;
        if link != Path::new(&me) {
            return Err(format!("/proc/self -> {} but pid is {me}", link.display()));
        }
        let a = read("/proc/self/stat")?;
        let b = read(&format!("/proc/{me}/stat"))?;
        if a.split(')').next() != b.split(')').next() {
            return Err("self and pid disagree on comm".into());
        }
        Ok(format!("/proc/self -> {me}"))
    })());
    rep.check("self-stat", (|| {
        let f = stat_fields(&me)?;
        if f[0] != me {
            return Err(format!("pid column is {}", f[0]));
        }
        let state = &f[2];
        if !matches!(state.as_str(), "R" | "S" | "D") {
            return Err(format!("state {state:?}"));
        }
        let threads: u64 = f[19].parse().map_err(|_| format!("num_threads {:?}", f[19]))?;
        if threads == 0 {
            return Err("num_threads is 0".into());
        }
        let rss: u64 = f[23].parse().map_err(|_| format!("rss {:?}", f[23]))?;
        if rss == 0 {
            return Err("rss is 0".into());
        }
        Ok(format!("comm {:?} state {state} threads {threads} rss {rss} pages", f[1]))
    })());
    rep.check("self-status", (|| {
        let s = read("/proc/self/status")?;
        for key in ["Name:", "State:", "Pid:", "PPid:", "Uid:", "VmRSS:", "Threads:"] {
            if !s.lines().any(|l| l.starts_with(key)) {
                return Err(format!("status lacks {key}"));
            }
        }
        let p = field("/proc/self/status", "Pid")?;
        if p != pid as u64 {
            return Err(format!("Pid: {p}"));
        }
        Ok("Name/State/Pid/PPid/Uid/VmRSS/Threads present".into())
    })());
    rep.check("self-cmdline", (|| {
        let raw = e("read cmdline", fs::read("/proc/self/cmdline"))?;
        let args: Vec<_> = raw.split(|&b| b == 0).filter(|a| !a.is_empty()).collect();
        // under the APE loader on Linux the binary is argv[1], not argv[0]
        let argv0 = std::env::args().next().unwrap_or_default();
        let stem = |s: &str| Path::new(s).file_stem().map(|x| x.to_string_lossy().to_lowercase());
        let seen: Vec<String> = args.iter().map(|a| String::from_utf8_lossy(a).into_owned()).collect();
        if !seen.iter().any(|a| stem(a) == stem(&argv0)) {
            return Err(format!("cmdline {seen:?} never names argv[0] {argv0:?}"));
        }
        Ok(format!("{} args, [0] = {}", args.len(), seen[0]))
    })());
    rep.check("self-links", (|| {
        let exe = e("readlink exe", fs::read_link("/proc/self/exe"))?;
        e("stat exe target", fs::metadata(&exe))?;
        if !exe.is_absolute() {
            return Err(format!("exe {} is not absolute", exe.display()));
        }
        // under the ape loader the kernel's answer is the loader; the shim
        // must hand back the program instead
        let leaf = exe.file_name().unwrap_or_default().to_string_lossy();
        if leaf.starts_with(".ape-") || exe == Path::new("/usr/bin/ape") {
            return Err(format!("exe {} is the ape loader", exe.display()));
        }
        if fs::canonicalize(std::env::current_exe().map_err(|e| e.to_string())?).ok() != fs::canonicalize(&exe).ok() {
            return Err(format!("current_exe disagrees with exe link {}", exe.display()));
        }
        let cwd = e("readlink cwd", fs::read_link("/proc/self/cwd"))?;
        let real = e("current_dir", std::env::current_dir())?;
        let canon = |p: &Path| fs::canonicalize(p).map(|x| x.to_string_lossy().to_lowercase());
        if canon(&cwd).ok() != canon(&real).ok() {
            return Err(format!("cwd link {} vs {}", cwd.display(), real.display()));
        }
        let meta = e("lstat exe", fs::symlink_metadata("/proc/self/exe"))?;
        if !meta.file_type().is_symlink() {
            return Err("lstat exe is not a symlink".into());
        }
        Ok(format!("exe {} cwd {}", exe.display(), cwd.display()))
    })());
    rep.check("self-fd", (|| {
        let f = e("open", fs::File::open("/proc/self/stat"))?;
        let fd = f.as_raw_fd();
        let names: Vec<String> = e("read_dir fd", fs::read_dir("/proc/self/fd"))?
            .flatten()
            .map(|d| d.file_name().to_string_lossy().into_owned())
            .collect();
        if !names.iter().any(|n| n == &fd.to_string()) {
            return Err(format!("fd {fd} not listed among {names:?}"));
        }
        let target = e("readlink fd", fs::read_link(format!("/proc/self/fd/{fd}")))?;
        drop(f);
        Ok(format!("{} entries, fd {fd} -> {}", names.len(), target.display()))
    })());
    rep.check("self-maps-environ", (|| {
        let maps = read("/proc/self/maps")?;
        let n = maps.lines().count();
        if n == 0 {
            return Err("maps is empty".into());
        }
        let first = maps.lines().next().unwrap();
        if first.split_whitespace().count() < 5 {
            return Err(format!("maps line {first:?}"));
        }
        let env = e("read environ", fs::read("/proc/self/environ"))?;
        if !env.split(|&b| b == 0).any(|kv| kv.starts_with(b"PATH=")) {
            return Err("environ lacks PATH".into());
        }
        Ok(format!("{n} mappings, environ has PATH"))
    })());
    rep.check("self-task", (|| {
        let tids: Vec<String> = e("read_dir task", fs::read_dir("/proc/self/task"))?
            .flatten()
            .map(|d| d.file_name().to_string_lossy().into_owned())
            .collect();
        if tids.is_empty() {
            return Err("task/ is empty".into());
        }
        let t = &tids[0];
        has(&format!("/proc/self/task/{t}/status"), "Pid:")?;
        Ok(format!("{} threads, task/{t}/status readable", tids.len()))
    })());

    // --- listing, and the dirfd-relative spelling procfs-style crates use
    rep.check("proc-listing", (|| {
        let names: Vec<String> = e("read_dir /proc", fs::read_dir("/proc"))?
            .flatten()
            .map(|d| d.file_name().to_string_lossy().into_owned())
            .collect();
        let pids = names.iter().filter(|n| n.chars().all(|c| c.is_ascii_digit())).count();
        if !names.iter().any(|n| n == &me) {
            return Err(format!("own pid {me} missing from {pids} pid entries"));
        }
        for want in ["self", "stat", "meminfo", "net", "sys"] {
            if !names.iter().any(|n| n == want) {
                return Err(format!("{want} missing from listing"));
            }
        }
        let ty = e("metadata", fs::metadata(format!("/proc/{me}")))?;
        if !ty.is_dir() {
            return Err("pid entry is not a directory".into());
        }
        Ok(format!("{pids} pids, own pid and the fixed names listed"))
    })());
    rep.check("openat-relative", (|| {
        let dir = e("open pid dir", fs::File::open(format!("/proc/{me}")))?;
        let mut got = Vec::new();
        for name in ["stat", "status", "cmdline"] {
            let c = CString::new(name).unwrap();
            let fd = unsafe { libc::openat(dir.as_raw_fd(), c.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
            if fd < 0 {
                return Err(format!("openat({name}): {}", std::io::Error::last_os_error()));
            }
            let mut f = unsafe { fs::File::from_raw_fd(fd) };
            let mut s = String::new();
            e(&format!("read {name}"), f.read_to_string(&mut s))?;
            if s.is_empty() {
                return Err(format!("{name} via openat is empty"));
            }
            got.push(s.len());
        }
        let c = CString::new("stat").unwrap();
        let fd = unsafe { libc::openat(dir.as_raw_fd(), c.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        let r = unsafe { libc::fstat(fd, &mut st) };
        unsafe { libc::close(fd) };
        if r != 0 {
            return Err(format!("fstat via openat: {}", std::io::Error::last_os_error()));
        }
        let c = CString::new("nope").unwrap();
        let fd = unsafe { libc::openat(dir.as_raw_fd(), c.as_ptr(), libc::O_RDONLY) };
        if fd >= 0 {
            unsafe { libc::close(fd) };
            return Err("openat of a nonexistent entry succeeded".into());
        }
        Ok(format!("stat/status/cmdline sizes {got:?}, nonexistent entry refused"))
    })());
    rep.check("seek-reread", (|| {
        // a reader that keeps the file open and rewinds must see fresh text
        let mut f = e("open stat", fs::File::open("/proc/self/stat"))?;
        let mut a = String::new();
        e("read", f.read_to_string(&mut a))?;
        let before = stat_fields(&me)?;
        let spin_until = std::time::Instant::now() + std::time::Duration::from_millis(300);
        let mut x = 0u64;
        while std::time::Instant::now() < spin_until {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
        }
        std::hint::black_box(x);
        e("seek", f.seek(SeekFrom::Start(0)))?;
        let mut b = String::new();
        e("reread", f.read_to_string(&mut b))?;
        if b.is_empty() || b.split(')').next() != a.split(')').next() {
            return Err(format!("reread {b:?}"));
        }
        let utime = |s: &str| -> u64 {
            s.rsplit(')').next().unwrap_or("").split_whitespace().nth(11).and_then(|v| v.parse().ok()).unwrap_or(0)
        };
        if utime(&b) < utime(&a) {
            return Err(format!("utime went backwards {} -> {}", utime(&a), utime(&b)));
        }
        let _ = before;
        Ok(format!("utime {} -> {} ticks after a 300ms spin", utime(&a), utime(&b)))
    })());
    rep.check("other-pid", (|| {
        // any other live process must have a readable stat and status
        let mut seen = 0;
        for d in e("read_dir", fs::read_dir("/proc"))?.flatten() {
            let n = d.file_name().to_string_lossy().into_owned();
            if n == me || !n.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            if let Ok(f) = stat_fields(&n) {
                if f[0] == n && fs::read_to_string(format!("/proc/{n}/status")).is_ok() {
                    seen += 1;
                }
            }
            if seen >= 3 {
                break;
            }
        }
        if seen == 0 {
            return Err("no other process readable".into());
        }
        Ok(format!("{seen} other processes readable"))
    })());

    // --- the /sys slices sysinfo reads instead of /proc
    rep.check("sysfs-cpu-net", (|| {
        has("/sys/devices/system/cpu/online", "0")?;
        let ifs: Vec<String> = e("read_dir class/net", fs::read_dir("/sys/class/net"))?
            .flatten()
            .map(|d| d.file_name().to_string_lossy().into_owned())
            .collect();
        if ifs.is_empty() {
            return Err("/sys/class/net is empty".into());
        }
        let i = &ifs[0];
        field(&format!("/sys/class/net/{i}/statistics/rx_bytes"), "")?;
        field(&format!("/sys/class/net/{i}/mtu"), "")?;
        Ok(format!("cpu online, {} interfaces, {i} has statistics", ifs.len()))
    })());
}

fn with_sysinfo(rep: &mut Report) {
    let mut sys = System::new_all();
    let me = Pid::from_u32(std::process::id());

    rep.check("sysinfo-cpus", (|| {
        let n = sys.cpus().len();
        if n == 0 {
            return Err("no cpus".into());
        }
        let brand = sys.cpus()[0].brand().to_string();
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        sys.refresh_cpu_usage();
        let usage = sys.global_cpu_usage();
        if !(0.0..=100.0).contains(&usage) {
            return Err(format!("usage {usage}"));
        }
        Ok(format!("{n} cpus, {brand:?}, {usage:.0}% busy, physical cores {:?}", System::physical_core_count()))
    })());
    rep.check("sysinfo-memory", (|| {
        let total = sys.total_memory();
        let used = sys.used_memory();
        if total == 0 || used == 0 || used > total {
            return Err(format!("total {total} used {used}"));
        }
        Ok(format!("{} MiB used of {} MiB", used >> 20, total >> 20))
    })());
    rep.check("sysinfo-host", (|| {
        // name() comes from /etc/os-release, which is not /proc's business
        let name = System::name().unwrap_or_else(|| "(no /etc/os-release)".into());
        let kernel = System::kernel_version().ok_or("no kernel version")?;
        let host = System::host_name().ok_or("no host name")?;
        let up = System::uptime();
        if up == 0 {
            return Err("uptime 0".into());
        }
        Ok(format!("{name} {kernel} on {host}, up {up}s, boot {}", System::boot_time()))
    })());
    rep.check("sysinfo-self", (|| {
        sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[me]), true, ProcessRefreshKind::everything());
        let p = sys.process(me).ok_or("own process missing")?;
        let name = p.name().to_string_lossy().into_owned();
        let argv0 = std::env::args().next().unwrap_or_default();
        let stem = Path::new(&argv0).file_stem().unwrap_or_default().to_string_lossy().to_lowercase();
        // comm is truncated to 15 bytes; compare on the shorter of the two
        let n = stem.len().min(15).min(name.len());
        if name.to_lowercase()[..n] != stem[..n] {
            return Err(format!("name {name:?} vs argv[0] {argv0:?}"));
        }
        if p.memory() == 0 {
            return Err("memory 0".into());
        }
        let exe = p.exe().ok_or("no exe")?;
        let cwd = p.cwd().ok_or("no cwd")?;
        if p.cmd().is_empty() {
            return Err("empty cmd".into());
        }
        if p.start_time() > System::boot_time() + System::uptime() + 1 {
            return Err(format!("start_time {} in the future", p.start_time()));
        }
        Ok(format!("{name}, {} KiB, exe {}, cwd {}, {} args, run_time {}s", p.memory() >> 10, exe.display(), cwd.display(), p.cmd().len(), p.run_time()))
    })());
    rep.check("sysinfo-processes", (|| {
        sys.refresh_processes(ProcessesToUpdate::All, true);
        let n = sys.processes().len();
        if n < 2 {
            return Err(format!("{n} processes"));
        }
        let parent = sys.process(me).and_then(|p| p.parent()).ok_or("no parent pid")?;
        let pp = sys.process(parent).ok_or(format!("parent {parent} not in the table"))?;
        Ok(format!("{n} processes, parent {parent} is {:?}", pp.name()))
    })());
    rep.check("sysinfo-networks", (|| {
        let nets = Networks::new_with_refreshed_list();
        if nets.is_empty() {
            return Err("no interfaces".into());
        }
        let (name, data) = nets.iter().next().unwrap();
        Ok(format!("{} interfaces, {name}: {} B in, mtu {}", nets.len(), data.total_received(), data.mtu()))
    })());
}

fn main() {
    let mut rep = Report::new();

    if ape::is_bsd() {
        println!("SKIP /proc matrix: this host has no /proc");
    } else {
        direct(&mut rep);
        with_sysinfo(&mut rep);
    }

    rep.finish("proc sysinfo ok");
}
