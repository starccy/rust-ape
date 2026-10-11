//! Making functions and variables of this program findable by name, for
//! plugins and other libraries that look their host's API up.

use std::ffi::{CString, c_char, c_int, c_void};
use std::sync::OnceLock;

use super::callback::Callback;
use super::cosmo_dlexport;
use super::library::{Library, dlerror};

// struct CosmoDlexport in libc/dlopen/dlfcn.h
#[repr(C)]
pub(super) struct Export {
    pub(super) name: *const c_char,
    addr: *const c_void,
    pub(super) size: usize,
    kind: c_int,
    pub(super) va: *const c_void,
}

const EXPORT_FUNC: c_int = 0;
const EXPORT_DATA: c_int = 1;
const EXPORT_VARARG: c_int = 2;

/// One thing [`export_items`] makes visible to host libraries, which
/// [`host_exports!`](crate::host_exports) builds for you.
#[derive(Clone, Copy, Debug)]
pub enum ExportItem {
    /// A function, as [`host_callback`](crate::host_callback) makes it.
    Function(&'static str, Callback),
    /// A variable at that address, that many bytes of it, which the
    /// library binds to and shares. A static of this program, not heap
    /// memory.
    Variable(&'static str, *mut c_void, usize),
    /// A variadic function: how many of its parameters are fixed, the
    /// function itself (an `extern "C"` function taking `...`, which
    /// Linux calls directly), and the same function taking a
    /// `core::ffi::VaList` after the fixed parameters, which the other
    /// hosts get through a trampoline that lays their arguments out as
    /// one. At most 16 fixed parameters on Windows and 8 on macOS.
    Variadic(&'static str, usize, *const c_void, *const c_void),
}

unsafe impl Send for ExportItem {}
unsafe impl Sync for ExportItem {}

/// A variable of this program that host libraries share, which
/// [`host_exports!`](crate::host_exports) declares for a `static` among
/// its items. Reads and writes go through the pointer since the library
/// writes to it as well.
#[repr(transparent)]
pub struct Shared<T>(std::cell::UnsafeCell<T>);

unsafe impl<T> Sync for Shared<T> {}
unsafe impl<T: Send> Send for Shared<T> {}

impl<T> Shared<T> {
    pub const fn new(v: T) -> Shared<T> {
        Shared(std::cell::UnsafeCell::new(v))
    }

    /// Where it is, which is what the library sees under the name.
    pub const fn as_ptr(&self) -> *mut T {
        self.0.get()
    }

    /// # Safety
    /// Nothing is writing it at the same time.
    pub unsafe fn get(&self) -> T
    where
        T: Copy,
    {
        unsafe { self.0.get().read() }
    }

    /// # Safety
    /// Nothing is reading or writing it at the same time.
    pub unsafe fn set(&self, v: T) {
        unsafe { self.0.get().write(v) }
    }
}

impl<T: Copy + std::fmt::Debug> std::fmt::Debug for Shared<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        unsafe { self.get() }.fmt(f)
    }
}

/// Makes functions of this program visible to host libraries by name.
///
/// This builds a small host library called `name` that exports each of
/// `functions` and loads it, before you load a library that looks those
/// names up: a Lua C module wants `lua_*` from its host, a plugin wants its
/// host's API. On Linux and macOS the names become visible to everything
/// loaded afterwards, and `name` also satisfies a dependency on a library
/// by that name. On Windows `name` has to be exactly the DLL name the
/// library imports from, such as `"lua54.dll"`.
///
/// The host calls these with its own convention, so pass the `ptr()` of a
/// [`host_callback`](crate::host_callback) for each, not a plain `extern
/// "C" fn`.
pub fn export(name: &str, functions: &[(&str, Callback)]) -> Result<Library, String> {
    let items: Vec<ExportItem> = functions
        .iter()
        .map(|&(n, f)| ExportItem::Function(Box::leak(n.to_owned().into_boxed_str()), f))
        .collect();
    export_items(name, &items)
}

/// [`export`] with variables and variadic functions among the items.
pub fn export_items(name: &str, items: &[ExportItem]) -> Result<Library, String> {
    let cname = CString::new(name).map_err(|e| e.to_string())?;
    let names = items
        .iter()
        .map(|i| {
            let n = match i {
                ExportItem::Function(n, _)
                | ExportItem::Variable(n, _, _)
                | ExportItem::Variadic(n, _, _, _) => n,
            };
            CString::new(*n).map_err(|e| e.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let table: Vec<Export> = names
        .iter()
        .zip(items)
        .map(|(n, i)| match *i {
            ExportItem::Function(_, f) => Export {
                name: n.as_ptr(),
                addr: f.0,
                size: 0,
                kind: EXPORT_FUNC,
                va: std::ptr::null(),
            },
            ExportItem::Variable(_, p, size) => Export {
                name: n.as_ptr(),
                addr: p,
                size,
                kind: EXPORT_DATA,
                va: std::ptr::null(),
            },
            ExportItem::Variadic(_, fixed, f, va) => Export {
                name: n.as_ptr(),
                addr: f,
                size: fixed,
                kind: EXPORT_VARARG,
                va,
            },
        })
        .collect();
    let handle = unsafe { cosmo_dlexport(cname.as_ptr(), table.as_ptr(), table.len()) };
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

/// A set of functions exported to host libraries by name, made by
/// [`host_exports!`](crate::host_exports).
pub struct Exports {
    unix: &'static str,
    pub(super) windows: &'static str,
    list: fn() -> Vec<ExportItem>,
    pub(super) lib: OnceLock<Result<Library, String>>,
}

impl Exports {
    #[doc(hidden)]
    pub const fn new(
        unix: &'static str,
        windows: &'static str,
        list: fn() -> Vec<ExportItem>,
    ) -> Exports {
        Exports {
            unix,
            windows,
            list,
            lib: OnceLock::new(),
        }
    }

    /// Builds and loads the library that exports the functions, once. Do
    /// this before loading anything that looks them up.
    pub fn export(&self) -> Result<&Library, &str> {
        self.lib
            .get_or_init(|| {
                let name = if crate::is_windows() {
                    self.windows
                } else {
                    self.unix
                };
                export_items(name, &(self.list)())
            })
            .as_ref()
            .map_err(|e| e.as_str())
    }
}
