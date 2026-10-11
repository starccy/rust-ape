//! Functions of this program handed to a host library, in the convention
//! the host calls with, and host function pointers made callable from here.

use std::ffi::{c_int, c_void};

use super::vararg::VarArg;
use super::{__hostlink_callback, __hostlink_thunk, cosmo_dlhostcall};

/// A function pointer that a host library can call back, as produced by
/// [`host_callback`](crate::host_callback). Declare callback parameters and
/// struct fields with this type rather than `extern "C" fn`, since on Windows
/// the library calls back with a different convention. It is a plain pointer
/// to the library, and can sit in a `static`.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Callback(pub *const c_void);

unsafe impl Send for Callback {}
unsafe impl Sync for Callback {}

impl Callback {
    /// No callback, for a library that takes that to mean "none".
    pub const NULL: Callback = Callback(std::ptr::null());

    pub fn is_null(self) -> bool {
        self.0.is_null()
    }
}

impl Default for Callback {
    fn default() -> Callback {
        Callback::NULL
    }
}

impl VarArg for Callback {
    fn classify(self) -> (bool, u64) {
        (false, self.0 as usize as u64)
    }
}

/// Makes a function pointer a host library handed back callable as `F`,
/// an `extern "C"` function pointer type, on every host.
///
/// What a library returns at runtime, like `vkGetInstanceProcAddr`,
/// `wglGetProcAddress` or a table of functions such as ONNX Runtime's
/// `OrtApi`, takes Microsoft's convention on Windows, which
/// [`host_library`](crate::host_library) can't see. The thunk lays the
/// arguments out for it, and for that it needs the signature: `F` says how
/// many there are and which are floating point. A pointer to a function
/// of this program, and everything anywhere but Windows, comes back as it
/// is, and null comes back as `None`.
///
/// The thunk is made once per function and signature and never freed. A
/// callback the host calls back is the other direction, see
/// [`host_callback`](crate::host_callback).
///
/// # Safety
/// `p` is null or a function of signature `F` with C's rules for `F`'s
/// types, and the library it came from stays loaded.
pub unsafe fn thunk<F: HostFn>(p: *mut c_void) -> Option<F> {
    if p.is_null() {
        return None;
    }
    let t = unsafe { __hostlink_thunk(p, F::desc()) };
    Some(unsafe { std::mem::transmute_copy::<*mut c_void, F>(&t) })
}

/// What to call `p`, a function pointer a host library handed back, with
/// the host's own convention, as an `extern "win64"` function on x86-64
/// Windows.
///
/// On Windows that's a stub outside this program that makes the call,
/// for `nargs` arguments or up to 20 when `None`. A host function that
/// tail-calls a thunk it was handed (what [`thunk`], [`Library::typed`](super::Library::typed)
/// and C's `dlsym` make) leaves it the return address of whoever called
/// that function, and a return address in this program reads as a caller
/// in our convention; made from the stub, the call leaves one outside
/// it. [`Library::func`](super::Library::func) and [`host_library`](crate::host_library) go
/// through this. Everything else comes back as it is: a function of this
/// program, one of more than 20 arguments, and anything on other hosts.
///
/// # Safety
/// `p` is null or a function the library it came from keeps loaded.
pub unsafe fn direct(p: *mut c_void, nargs: Option<usize>) -> *mut c_void {
    let n = nargs.map_or(-1, |n| n.min(c_int::MAX as usize) as c_int);
    unsafe { cosmo_dlhostcall(p, n) }
}

/// [`thunk`] with the signature given as the fork takes it: the argument
/// count in the low 6 bits, and bit `8 + i` set for a floating point
/// i-th argument. 64 means unknown, which is taken as integers only.
///
/// # Safety
/// As [`thunk`], with `desc` describing the function.
pub unsafe fn thunk_raw(p: *mut c_void, desc: u64) -> *mut c_void {
    unsafe { __hostlink_thunk(p, desc) }
}

/// Makes `f`, a function of this program, callable by host code on every
/// host: the other direction of [`thunk`], for a function that goes into
/// a table the library calls through, such as ICU's `UTextFuncs` or
/// Vulkan's `VkAllocationCallbacks`. On Windows the result is a thunk
/// taking Microsoft's convention, which is what
/// [`host_callback`](crate::host_callback) makes at compile time;
/// elsewhere it is `f`. A function that isn't this program's, such as one
/// the host handed out, comes back as it is.
pub fn callback<F: HostFn>(f: F) -> F {
    unsafe {
        let p = std::mem::transmute_copy::<F, *mut c_void>(&f);
        let t = __hostlink_callback(p, F::desc());
        std::mem::transmute_copy::<*mut c_void, F>(&t)
    }
}

/// [`callback`] with the signature given as [`thunk_raw`] takes it.
///
/// # Safety
/// `p` is a function of this program, and `desc` describes it.
pub unsafe fn callback_raw(p: *mut c_void, desc: u64) -> *mut c_void {
    unsafe { __hostlink_callback(p, desc) }
}

/// A C function pointer type, `extern "C" fn(..) -> ..` with or without
/// `unsafe`, `Option` of one included, as [`thunk`] and [`callback`] take
/// it. The signature is read off the type's name, so references and
/// lifetimes in it are fine.
pub trait HostFn: Copy {
    #[doc(hidden)]
    fn desc() -> u64 {
        let name = std::any::type_name::<Self>();
        match desc_of(name) {
            Some(d) if size_of::<Self>() == size_of::<usize>() => d,
            _ => panic!("{name} isn't a C function pointer type"),
        }
    }
}

impl<F: Copy> HostFn for F {}

// the argument count and float bits of a C function pointer type's name
fn desc_of(name: &str) -> Option<u64> {
    let name = name.strip_prefix("core::option::Option<").unwrap_or(name);
    let name = match name.find("> ") {
        Some(i) if name.starts_with("for<") => &name[i + 2..],
        _ => name,
    };
    let name = name.strip_prefix("unsafe ").unwrap_or(name);
    let rest = [
        "extern \"C\" fn(",
        "extern \"C-unwind\" fn(",
        "extern \"system\" fn(",
        "extern \"system-unwind\" fn(",
    ]
    .iter()
    .find_map(|p| name.strip_prefix(p))?;
    let (mut depth, mut end, mut prev) = (0i32, None, ' ');
    for (i, c) in rest.char_indices() {
        match c {
            '(' | '<' | '[' => depth += 1,
            '>' if prev == '-' => {}
            ')' if depth == 0 => {
                end = Some(i);
                break;
            }
            ')' | '>' | ']' => depth -= 1,
            _ => {}
        }
        prev = c;
    }
    let params = &rest[..end?];
    let (mut n, mut floats, mut depth, mut start, mut prev) = (0u64, 0u64, 0i32, 0usize, ' ');
    let mut one = |t: &str, n: &mut u64| {
        let t = t.trim();
        if t == "f64" || t == "f32" {
            floats |= 1 << *n;
        }
        *n += 1;
    };
    for (i, c) in params.char_indices() {
        match c {
            '(' | '<' | '[' => depth += 1,
            '>' if prev == '-' => {}
            ')' | '>' | ']' => depth -= 1,
            ',' if depth == 0 => {
                one(&params[start..i], &mut n);
                start = i + 1;
            }
            _ => {}
        }
        prev = c;
    }
    if !params[start..].trim().is_empty() {
        one(&params[start..], &mut n);
    }
    (n <= 32).then_some(n | floats << 8)
}

#[cfg(test)]
mod desc_tests {
    use super::desc_of;

    #[test]
    fn signatures() {
        assert_eq!(desc_of("unsafe extern \"C\" fn()"), Some(0));
        assert_eq!(
            desc_of("extern \"C\" fn(i32, f32, i64, f64) -> f64"),
            Some(4 | 0b1010 << 8)
        );
        assert_eq!(
            desc_of("for<'a> unsafe extern \"C\" fn(&'a mut Ut, i64, bool) -> bool"),
            Some(3)
        );
        assert_eq!(
            desc_of(
                "core::option::Option<unsafe extern \"C\" fn(*mut c_void, Option<fn(f32)>) -> ()>"
            ),
            Some(2)
        );
        assert_eq!(desc_of("fn(i32)"), None);
        assert_eq!(desc_of("i32"), None);
    }
}
