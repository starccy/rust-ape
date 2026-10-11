//! A host library and its functions, loaded with the host's own loader.

use std::ffi::{CStr, CString, c_int, c_void};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::callback::{HostFn, thunk};
use super::{
    cosmo_dlclose, cosmo_dlerror, cosmo_dlhostcall, cosmo_dlopen, cosmo_dlsym, cosmo_dltramp,
    cosmo_hostlink,
};

const RTLD_NOW: c_int = 2;
pub(super) const RTLD_GLOBAL: c_int = 256;

pub(super) fn dlerror() -> String {
    let p = unsafe { cosmo_dlerror() };
    if p.is_null() {
        "unknown error".into()
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

/// A loaded host library.
#[derive(Debug)]
pub struct Library {
    pub(super) handle: *mut c_void,
    pub(super) name: String,
    // cosmo_dltramp and cosmo_dlhostcall generate code they never free,
    // so each symbol (and argument count) gets one
    pub(super) tramps: std::sync::Mutex<std::collections::HashMap<(String, c_int), usize>>,
}

unsafe impl Send for Library {}
unsafe impl Sync for Library {}

impl Library {
    /// Loads `name` the way the host would, so a bare name goes through the
    /// host's search path. A name ending in `.so` is tried as `.dll` on
    /// Windows and `.dylib` on macOS.
    pub fn open(name: &str) -> Result<Library, String> {
        Self::open_mode(name, RTLD_NOW)
    }

    /// Like [`open`](Self::open), but makes the library's symbols available
    /// to libraries loaded after it (`RTLD_GLOBAL`), which Windows ignores.
    pub fn open_global(name: &str) -> Result<Library, String> {
        Self::open_mode(name, RTLD_NOW | RTLD_GLOBAL)
    }

    fn open_mode(name: &str, mode: c_int) -> Result<Library, String> {
        let c = CString::new(name).map_err(|e| e.to_string())?;
        let handle = unsafe { cosmo_dlopen(c.as_ptr(), mode) };
        if handle.is_null() {
            Err(format!("{name}: {}", dlerror()))
        } else {
            Ok(Library {
                handle,
                name: name.to_owned(),
                tramps: Default::default(),
            })
        }
    }

    /// Loads the first of `names` that loads, which is how a library whose
    /// name carries a version (`libavcodec.so.61`, `libavcodec.so.60`) is
    /// found.
    pub fn open_any(names: &[&str]) -> Result<Library, String> {
        Self::open_any_mode(names, RTLD_NOW)
    }

    fn open_any_mode(names: &[&str], mode: c_int) -> Result<Library, String> {
        let mut errors = Vec::new();
        for name in names {
            match Self::open_mode(name, mode) {
                Ok(lib) => return Ok(lib),
                Err(e) => errors.push(e),
            }
        }
        if errors.is_empty() {
            Err("no library name given for this host".into())
        } else {
            Err(errors.join("; "))
        }
    }

    /// Loads the library the program links with `-l<name>`, when the
    /// build could only find it as a shared object on the build machine.
    ///
    /// `cargo xtask build` links a library like that through stubs, which
    /// load the host's copy the first time one of its functions is called
    /// and end the program if there isn't one, the way the host's loader
    /// would have at startup. Asking here first lets the program tell the
    /// user what to install instead. `name` is spelled as in `-l`, such as
    /// `"webkit2gtk-4.1"` or `"sqlite3"`.
    pub fn linked(name: &str) -> Result<Library, String> {
        let c = CString::new(name).map_err(|e| e.to_string())?;
        let handle = unsafe { cosmo_hostlink(c.as_ptr()) };
        if handle.is_null() {
            Err(format!("-l{name}: {}", dlerror()))
        } else {
            Ok(Library {
                handle,
                name: name.to_owned(),
                tramps: Default::default(),
            })
        }
    }

    /// The name it was loaded by.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The address of `symbol` as the host sees it. On Windows that's a
    /// function taking Microsoft's convention; call it through
    /// [`func`](Self::func) unless you know what you're doing.
    pub fn sym(&self, symbol: &str) -> Option<*mut c_void> {
        let c = CString::new(symbol).ok()?;
        let p = unsafe { cosmo_dlsym(self.handle, c.as_ptr()) };
        (!p.is_null()).then_some(p)
    }

    /// The address of function `symbol`, ready to be called: as an
    /// `extern "win64"` function on Windows, and as an `extern "C"` one
    /// everywhere else. On Windows that's a stub for up to 20 arguments,
    /// see [`direct`](super::direct); [`func_n`](Self::func_n) makes one for the exact
    /// count.
    pub fn func(&self, symbol: &str) -> Option<*mut c_void> {
        self.func_impl(symbol, -1)
    }

    /// [`func`](Self::func) for a function of `nargs` arguments; more
    /// than 20 of them, or a variadic function given `usize::MAX`, is
    /// called directly on Windows.
    pub fn func_n(&self, symbol: &str, nargs: usize) -> Option<*mut c_void> {
        self.func_impl(symbol, nargs.min(c_int::MAX as usize) as c_int)
    }

    fn func_impl(&self, symbol: &str, nargs: c_int) -> Option<*mut c_void> {
        let key = (symbol.to_owned(), nargs);
        let mut tramps = self.tramps.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(&t) = tramps.get(&key) {
            return Some(t as *mut c_void);
        }
        let p = self.sym(symbol)?;
        let t = if crate::is_windows() {
            unsafe { cosmo_dlhostcall(p, nargs) }
        } else {
            unsafe { cosmo_dltramp(p) }
        };
        if t.is_null() {
            return None;
        }
        tramps.insert(key, t as usize);
        Some(t)
    }

    /// The function `symbol`, callable as `F` on every host; see
    /// [`thunk`].
    pub fn typed<F: HostFn>(&self, symbol: &str) -> Option<F> {
        unsafe { thunk(self.sym(symbol)?) }
    }

    /// Unloads the library. Anything still pointing into it dangles.
    pub unsafe fn close(self) -> Result<(), String> {
        if unsafe { cosmo_dlclose(self.handle) } == 0 {
            Ok(())
        } else {
            Err(dlerror())
        }
    }
}

/// A host library that is loaded the first time something needs it, with a
/// list of names to try on each host. Made by
/// [`host_library`](crate::host_library).
pub struct LazyLibrary {
    linux: &'static [&'static str],
    macos: &'static [&'static str],
    pub(super) windows: &'static [&'static str],
    bsd: &'static [&'static str],
    global: bool,
    pub(super) lib: OnceLock<Result<Library, String>>,
}

impl LazyLibrary {
    /// A library with no names yet; `linux()`, `macos()`, `windows()` and
    /// `bsd()` add the names each host tries, in order. A host left out
    /// fails to load with a message saying so.
    pub const fn new() -> LazyLibrary {
        LazyLibrary {
            linux: &[],
            macos: &[],
            windows: &[],
            bsd: &[],
            global: false,
            lib: OnceLock::new(),
        }
    }

    pub const fn linux(mut self, names: &'static [&'static str]) -> LazyLibrary {
        assert!(self.linux.is_empty(), "linux names given twice");
        self.linux = names;
        self
    }

    pub const fn macos(mut self, names: &'static [&'static str]) -> LazyLibrary {
        assert!(self.macos.is_empty(), "macos names given twice");
        self.macos = names;
        self
    }

    pub const fn windows(mut self, names: &'static [&'static str]) -> LazyLibrary {
        assert!(self.windows.is_empty(), "windows names given twice");
        self.windows = names;
        self
    }

    pub const fn bsd(mut self, names: &'static [&'static str]) -> LazyLibrary {
        assert!(self.bsd.is_empty(), "bsd names given twice");
        self.bsd = names;
        self
    }

    /// Loads with `RTLD_GLOBAL`, so that libraries loaded afterwards can
    /// resolve their undefined symbols against this one. An interpreter's
    /// extension modules need that of the interpreter.
    pub const fn global(mut self) -> LazyLibrary {
        self.global = true;
        self
    }

    /// The names this host tries, in order.
    pub fn names(&self) -> &'static [&'static str] {
        match crate::current_os() {
            crate::Os::Windows => self.windows,
            crate::Os::Xnu => self.macos,
            crate::Os::Freebsd | crate::Os::Netbsd | crate::Os::Openbsd => self.bsd,
            _ => self.linux,
        }
    }

    /// Loads the library if that hasn't happened yet.
    pub fn load(&self) -> Result<&Library, &str> {
        self.lib
            .get_or_init(|| {
                let mode = if self.global {
                    RTLD_NOW | RTLD_GLOBAL
                } else {
                    RTLD_NOW
                };
                Library::open_any_mode(self.names(), mode)
            })
            .as_ref()
            .map_err(|e| e.as_str())
    }

    #[doc(hidden)]
    pub fn resolve(&self, cache: &AtomicUsize, symbol: &str) -> usize {
        self.resolve_n(cache, symbol, None)
    }

    #[doc(hidden)]
    pub fn resolve_n(&self, cache: &AtomicUsize, symbol: &str, nargs: Option<usize>) -> usize {
        let p = cache.load(Ordering::Acquire);
        if p != 0 {
            return p;
        }
        let lib = match self.load() {
            Ok(lib) => lib,
            Err(e) => panic!("host library for {symbol}() didn't load: {e}"),
        };
        let found = match nargs {
            Some(n) => lib.func_n(symbol, n),
            None => lib.func(symbol),
        };
        let p = match found {
            Some(p) => p as usize,
            None => panic!("{} has no {symbol}(): {}", lib.name(), dlerror()),
        };
        cache.store(p, Ordering::Release);
        p
    }
}
