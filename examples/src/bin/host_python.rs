//! Embedding the host's CPython, which is about as much of a host libc as a
//! library can use: it loads its own extension modules with dlopen, starts
//! threads, installs signal handlers, forks and execs, and reads the
//! environment. Everything it does goes through the host libc, so this is
//! a check that the two libcs really coexist.
//!
//! What the script inside covers:
//!
//! - extension modules (_json, _sqlite3, _ssl, _hashlib, _ctypes), loaded by
//!   the host loader from inside a library we loaded.
//! - ctypes finding functions this program exports, and calling them from
//!   Python threads, with doubles mixed in.
//! - a SIGALRM handler installed by Python and delivered while it sleeps.
//! - subprocess, which forks and execs from inside the host libc.
//! - files and directories inside our zip store, on Linux, where the host
//!   libc's file functions can be pointed at it.
//! - the environment, set on one side and read on the other.
//!
//! Needs a CPython shared library on the host: libpython3.x on Linux,
//! python3x.dll on Windows (found on PATH or where python.org's installer
//! puts it), the Python framework on macOS.

use std::ffi::{CStr, c_char, c_double, c_int};
use std::sync::atomic::{AtomicUsize, Ordering};

#[ape::host_library(static PY = global, linux ["libpython3.so", "libpython3.14.so.1.0", "libpython3.13.so.1.0",
                    "libpython3.12.so.1.0", "libpython3.11.so.1.0", "libpython3.10.so.1.0"],
                    macos ["/Library/Frameworks/Python.framework/Versions/Current/Python",
                    "/opt/homebrew/Frameworks/Python.framework/Versions/Current/Python",
                    "/Library/Developer/CommandLineTools/Library/Frameworks/Python3.framework/Versions/Current/Python3"],
                    windows ["python314.dll", "python313.dll", "python312.dll", "python311.dll", "python3.dll"],
                    bsd ["libpython3.11.so", "libpython3.so"])]
unsafe extern "C" {
    fn Py_InitializeEx(initsigs: c_int);
    fn Py_IsInitialized() -> c_int;
    fn Py_FinalizeEx() -> c_int;
    fn Py_GetVersion() -> *const c_char;
    fn PyRun_SimpleString(command: *const c_char) -> c_int;
}

static CALLS: AtomicUsize = AtomicUsize::new(0);
static LOGS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static TOUCHED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

ape::host_exports! {
    static API = "libape-py.so", windows "ape-py.dll";
    fn ape_mul(a: c_int, b: c_int) -> c_int {
        CALLS.fetch_add(1, Ordering::Relaxed);
        TOUCHED.with(|t| t.set(t.get() + 1));
        a * b
    }
    fn ape_scale(x: c_double, n: c_int, y: c_double) -> c_double {
        CALLS.fetch_add(1, Ordering::Relaxed);
        x * n as f64 + y
    }
    fn ape_log(s: *const c_char) {
        LOGS.fetch_add(1, Ordering::Relaxed);
        let s = unsafe { CStr::from_ptr(s) }.to_string_lossy();
        let v: Vec<u8> = s.bytes().collect();
        println!("  [rust] {} ({} bytes, thread {:?})", s, v.len(), std::thread::current().id());
    }
}

const SCRIPT: &str = r#"
import sys, os, threading, json, sqlite3, hashlib, ctypes, signal, time
sys.stdout.reconfigure(line_buffering=True)
print("  python", sys.version.split()[0], "on", sys.platform)

lib = ctypes.CDLL(None) if sys.platform != "win32" else ctypes.CDLL("ape-py.dll")
lib.ape_mul.restype = ctypes.c_int
lib.ape_mul.argtypes = [ctypes.c_int, ctypes.c_int]
lib.ape_scale.restype = ctypes.c_double
lib.ape_scale.argtypes = [ctypes.c_double, ctypes.c_int, ctypes.c_double]
lib.ape_log.argtypes = [ctypes.c_char_p]
assert lib.ape_mul(6, 7) == 42
assert lib.ape_scale(1.5, 4, 0.25) == 6.25
lib.ape_log(b"hello from the main thread")

# python threads call into rust
res = []
def work(i):
    for _ in range(20):
        res.append(lib.ape_mul(i, 2))
    if i == 0:
        lib.ape_log(b"hello from a python thread")
ts = [threading.Thread(target=work, args=(i,)) for i in range(32)]
for t in ts: t.start()
for t in ts: t.join()
assert sorted(res) == sorted(2 * i for i in range(32) for _ in range(20)), res[:10]
print("  32 python threads made", len(res), "calls into rust")

# extension modules and a real workload
db = sqlite3.connect(":memory:")
db.execute("create table t(x)")
db.executemany("insert into t values (?)", [(i,) for i in range(1000)])
(total,) = db.execute("select sum(x) from t").fetchone()
assert total == 499500
h = hashlib.sha256(json.dumps(list(range(10000))).encode()).hexdigest()
assert h == "7d99a727ec84a6794eb1905bb9aa18ce6bfcef7a3b7cc203a68b281fb7f46e61", h
try:
    import ssl
    ctx = ssl.create_default_context()
    print("  sqlite3", sqlite3.sqlite_version, "sha256 ok, ssl", ssl.OPENSSL_VERSION.split()[1])
except ImportError as e:
    print("  sqlite3", sqlite3.sqlite_version, "sha256 ok, no ssl:", e)

# a signal handler installed by python, delivered during sleep
if sys.platform != "win32":
    got = []
    signal.signal(signal.SIGALRM, lambda *a: got.append(a[0]))
    signal.setitimer(signal.ITIMER_REAL, 0.05)
    t0 = time.time()
    while not got and time.time() - t0 < 2:
        time.sleep(0.01)
    assert got == [signal.SIGALRM], got
    print("  SIGALRM handler ran after", round(time.time() - t0, 3), "s")

# fork and exec inside the host libc. an embedded interpreter's
# sys.executable is whatever started the process, so run a shell
import subprocess
cmd = ["cmd", "/c", "echo 42"] if sys.platform == "win32" else ["/bin/sh", "-c", "echo $((6*7))"]
out = subprocess.run(cmd, capture_output=True, timeout=30)
assert out.stdout.strip() == b"42", out
print("  subprocess ran a shell:", out.stdout.strip().decode())

# our zip store, through open(), os.stat(), os.listdir() and glob, on
# linux, where the host libc's file functions are routed to it
p = "/zip/.bundle/usr/share/rust-ape-remap-test/hello.txt"
if sys.platform == "linux":
    import glob
    d = os.path.dirname(p)
    names = sorted(os.listdir(d))
    assert names == ["hello.txt", "sub"], names
    assert len(glob.glob(d + "/*.txt")) == 1
    walked = sum(len(f) for _, _, f in os.walk(d))
    assert walked == 2, walked
    # and through the path bundle.toml maps there
    assert sorted(os.listdir("/usr/share/rust-ape-remap-test")) == names
    print("  listed", d, names, "and walked", walked, "files")
    data = open(p, "rb").read()
    st = os.stat(p)
    assert len(data) == st.st_size and data, (len(data), st.st_size)
    print("  read", len(data), "bytes from", p)

# the environment, both ways
if True:
    assert os.environ.get("APE_FROM_RUST") == "set before Py_Initialize", os.environ.get("APE_FROM_RUST")
    os.environ["APE_FROM_PYTHON"] = "set by os.environ"
    print("  environment seen from python: APE_FROM_RUST =", os.environ["APE_FROM_RUST"])
"#;

// python.org's installer doesn't put its directory on PATH, so a bare
// python313.dll isn't found. Loading it by its full path first makes the
// bare name find the module already loaded.
fn preload_windows_python() {
    let mut roots = Vec::new();
    for var in ["LOCALAPPDATA", "PROGRAMFILES"] {
        if let Ok(v) = std::env::var(var) {
            roots.push(std::path::PathBuf::from(&v).join("Programs/Python"));
            roots.push(std::path::PathBuf::from(v));
        }
    }
    for root in roots {
        let Ok(dirs) = std::fs::read_dir(&root) else {
            continue;
        };
        for dir in dirs.flatten() {
            let name = dir.file_name().to_string_lossy().to_lowercase();
            if !name.starts_with("python3") {
                continue;
            }
            let dll = dir.path().join(format!("{name}.dll"));
            if dll.exists() {
                if let Ok(lib) = ape::host::Library::open(dll.to_str().unwrap()) {
                    std::mem::forget(lib);
                    return;
                }
            }
        }
    }
}

fn main() {
    if ape::is_windows() {
        preload_windows_python();
    }
    if let Err(e) = PY.load() {
        println!("no python on this host, skipping: {e}");
        return;
    }
    let version = unsafe { CStr::from_ptr(Py_GetVersion()) }
        .to_string_lossy()
        .into_owned();
    println!("host python: {}", version.lines().next().unwrap_or(""));

    API.export().unwrap_or_else(|e| panic!("export: {e}"));
    unsafe { std::env::set_var("APE_FROM_RUST", "set before Py_Initialize") };

    unsafe { Py_InitializeEx(1) };
    assert_eq!(unsafe { Py_IsInitialized() }, 1);
    let script = std::ffi::CString::new(SCRIPT).unwrap();
    let rc = unsafe { PyRun_SimpleString(script.as_ptr()) };
    assert_eq!(rc, 0, "the python script failed");

    {
        let from_python = std::env::var("APE_FROM_PYTHON");
        println!("environment seen from rust: APE_FROM_PYTHON = {from_python:?}");
        assert_eq!(from_python.as_deref(), Ok("set by os.environ"));
    }

    let rc = unsafe { Py_FinalizeEx() };
    assert_eq!(rc, 0, "Py_FinalizeEx failed");
    println!(
        "python finalized; rust was called {} times, logged {} times",
        CALLS.load(Ordering::Relaxed),
        LOGS.load(Ordering::Relaxed)
    );
    assert_eq!(CALLS.load(Ordering::Relaxed), 2 + 32 * 20);
    assert_eq!(LOGS.load(Ordering::Relaxed), 2);

    // our own threads still work after all that
    let hs: Vec<_> = (0..8).map(|i| std::thread::spawn(move || i * i)).collect();
    let sum: i32 = hs.into_iter().map(|h| h.join().unwrap()).sum();
    assert_eq!(sum, 140);
    println!("\nhost python ok");
}
