//! The `__ape_hostlink` module appended to a rewritten crate, which the
//! generated code calls into.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::plan::Explicit;

pub(super) fn helper(
    libs: &BTreeMap<String, String>,
    explicit: &BTreeMap<String, Explicit>,
    os_ext: bool,
    callbacks: bool,
    wide: bool,
    reexport: bool,
) -> String {
    let mut decls = String::new();
    for sym in libs.values() {
        let _ = writeln!(decls, "        pub static {sym}: u8;");
    }
    // a callback field's type, see Types
    let callback_ty = if callbacks {
        r#"
    /// A function in a struct the host library calls through, or fills in.
    ///
    /// `HostCallback::new(f)` takes a function of this program and makes
    /// it one the library can call on every host (on Windows, a copy
    /// taking Microsoft's convention). `get()` gives one this program can
    /// call, whichever side put it there. `M` marks the parameters that
    /// are a C `long`, `unsigned long` or `wchar_t`, which Windows passes
    /// narrower, so the copy widens them on the way in and the thunk
    /// widens such a return value on the way out. See README.md, C
    /// libraries on the host.
    #[repr(transparent)]
    pub struct HostCallback<F: Copy, const M: u64 = 0>(F);

    impl<F: Copy, const M: u64> Clone for HostCallback<F, M> {
        fn clone(&self) -> Self {
            *self
        }
    }

    impl<F: Copy, const M: u64> Copy for HostCallback<F, M> {}

    impl<F: Copy, const M: u64> HostCallback<F, M> {
        /// `f`, a function of this program, as the library calls it.
        pub fn new(f: F) -> HostCallback<F, M> {
            HostCallback(host_arg_m::<F, M>(f))
        }

        /// A function the library can call, as [`ape::host::Callback`]
        /// holds one, or a function of the library's.
        pub fn from_raw(p: *const c_void) -> HostCallback<F, M> {
            assert!(self::__core::mem::size_of::<F>() == self::__core::mem::size_of::<usize>());
            HostCallback(unsafe { self::__core::mem::transmute_copy(&p) })
        }

        /// The function as this program calls it.
        pub fn get(self) -> F {
            let desc = callback_desc(self::__core::any::type_name::<F>()).unwrap_or(64) | M;
            unsafe {
                let p: *mut c_void = self::__core::mem::transmute_copy(&self.0);
                let q = __hostlink_thunk(p, desc);
                self::__core::mem::transmute_copy(&q)
            }
        }

        /// The function as the library sees it.
        pub fn as_raw(self) -> *const c_void {
            unsafe { self::__core::mem::transmute_copy(&self.0) }
        }
    }

    impl<F: Copy + self::__core::fmt::Debug, const M: u64> self::__core::fmt::Debug for HostCallback<F, M> {
        fn fmt(&self, f: &mut self::__core::fmt::Formatter<'_>) -> self::__core::fmt::Result {
            self.0.fmt(f)
        }
    }

    impl<F: Copy + PartialEq, const M: u64> PartialEq for HostCallback<F, M> {
        fn eq(&self, o: &Self) -> bool {
            self.0 == o.0
        }
    }

    impl<F: Copy + Eq, const M: u64> Eq for HostCallback<F, M> {}

    impl<F: Copy + PartialOrd, const M: u64> PartialOrd for HostCallback<F, M> {
        fn partial_cmp(&self, o: &Self) -> Option<self::__core::cmp::Ordering> {
            self.0.partial_cmp(&o.0)
        }
    }

    impl<F: Copy + Ord, const M: u64> Ord for HostCallback<F, M> {
        fn cmp(&self, o: &Self) -> self::__core::cmp::Ordering {
            self.0.cmp(&o.0)
        }
    }

    impl<F: Copy + self::__core::hash::Hash, const M: u64> self::__core::hash::Hash for HostCallback<F, M> {
        fn hash<H: self::__core::hash::Hasher>(&self, h: &mut H) {
            self.0.hash(h)
        }
    }
"#
    } else {
        ""
    };
    let reexport = if reexport {
        "\n#[doc(inline)]\npub use __ape_hostlink::HostCallback;\n"
    } else {
        ""
    };
    // an explicit library gets its HostLinkLib here, listed where
    // cosmo_hostlink() looks
    // std::os::windows::ffi for a Windows crate, through UTF-8
    let os_ext_fns = if os_ext {
        r#"
    pub fn from_wide(w: &[u16]) -> ::std::ffi::OsString {
        ::std::string::String::from_utf16_lossy(w).into()
    }

    pub fn encode_wide(s: &::std::ffi::OsStr) -> ::std::vec::IntoIter<u16> {
        s.to_string_lossy().encode_utf16().collect::<::std::vec::Vec<u16>>().into_iter()
    }
"#
    } else {
        ""
    };
    let wide_fns = if wide { WIDE } else { "" };
    let mut dll_defs = String::new();
    for (name, x) in explicit {
        let sym = &x.sym;
        let list = |v: &Vec<String>| -> String {
            v.iter().map(|n| format!("{n}\\0")).collect::<String>() + "\\0"
        };
        let (linux, macos, windows) = (list(&x.names[0]), list(&x.names[1]), list(&x.names[2]));
        let name = name.strip_prefix("framework:").unwrap_or(name);
        let _ = writeln!(
            dll_defs,
            r#"    pub static {sym}: Lib = Lib::new(b"{linux}", b"{macos}", b"{windows}", b"{name}\0");
    #[used]
    #[unsafe(link_section = ".sort.rodata.hostlink.2")]
    static {sym}_ENTRY: &Lib = &{sym};"#
        );
    }
    format!(
        r#"

#[doc(hidden)]
#[allow(warnings, clippy::all)]
pub mod __ape_hostlink {{
    pub extern crate core as __core;
    use self::__core::ffi::c_void;
    use self::__core::sync::atomic::{{AtomicPtr, Ordering}};

    unsafe extern "C" {{
        fn __hostlink_resolve(lib: *const u8, name: *const u8) -> *mut c_void;
        fn __hostlink_data(lib: *const u8, name: *const u8, addr: *mut u8, size: usize);
        fn __hostlink_callback(f: *mut c_void, desc: u64) -> *mut c_void;
        fn __hostlink_thunk(f: *mut c_void, desc: u64) -> *mut c_void;
        fn __hostlink_vcall(f: *mut c_void, args: *const u64, n: usize, nfixed: usize, floats: u64) -> Ret;
        fn cosmo_dltramp(f: *mut c_void) -> *mut c_void;
        fn cosmo_dlhostcall(f: *mut c_void, nargs: i32) -> *mut c_void;
        pub fn __hostlink_lazy();
        static __hostos: i32;
{decls}    }}

    // struct HostLinkLib in libc/dlopen/hostlink.c
    #[repr(C)]
    pub struct Lib {{
        handle: AtomicPtr<c_void>,
        linux_names: *const u8,
        xnu_names: *const u8,
        nt_names: *const u8,
        name: *const u8,
        sync: AtomicPtr<c_void>,
        flags: u64,
    }}

    unsafe impl self::__core::marker::Sync for Lib {{}}

    impl Lib {{
        pub const fn new(linux: &'static [u8], xnu: &'static [u8], nt: &'static [u8], name: &'static [u8]) -> Lib {{
            Lib {{
                handle: AtomicPtr::new(0 as *mut c_void),
                linux_names: linux.as_ptr(),
                xnu_names: xnu.as_ptr(),
                nt_names: nt.as_ptr(),
                name: name.as_ptr(),
                sync: AtomicPtr::new(0 as *mut c_void),
                flags: 1,
            }}
        }}

        #[inline]
        pub const fn as_ptr(&self) -> *const u8 {{
            self as *const Lib as *const u8
        }}
    }}

    // struct HostLinkSym in libc/dlopen/hostlink.c: what __hostlink_lazy
    // binds, the slot a jump goes through, its library and its name
    #[repr(C)]
    pub struct Sym {{
        slot: *const AtomicPtr<c_void>,
        lib: *const u8,
        name: *const u8,
    }}

    unsafe impl self::__core::marker::Sync for Sym {{}}

    impl Sym {{
        pub const fn new(slot: &'static AtomicPtr<c_void>, lib: *const u8, name: &'static [u8]) -> Sym {{
            Sym {{ slot, lib, name: name.as_ptr() }}
        }}
    }}

    /// an argument on its way to a host function: an integer narrower
    /// than 32 bits goes as a 32-bit one, which Apple's arm64 convention
    /// has the caller extend it to, anything else goes as it is; the
    /// inherent methods win over the trait's
    pub struct Wide<T>(pub T);

    pub trait WideAny {{
        type W;
        fn wide(self) -> Self::W;
    }}

    impl<T> WideAny for Wide<T> {{
        type W = T;
        #[inline(always)]
        fn wide(self) -> T {{
            self.0
        }}
    }}

    impl Wide<u8> {{
        #[inline(always)]
        pub fn wide(self) -> u32 {{
            self.0 as u32
        }}
    }}

    impl Wide<i8> {{
        #[inline(always)]
        pub fn wide(self) -> i32 {{
            self.0 as i32
        }}
    }}

    impl Wide<u16> {{
        #[inline(always)]
        pub fn wide(self) -> u32 {{
            self.0 as u32
        }}
    }}

    impl Wide<i16> {{
        #[inline(always)]
        pub fn wide(self) -> i32 {{
            self.0 as i32
        }}
    }}

    impl Wide<bool> {{
        #[inline(always)]
        pub fn wide(self) -> u32 {{
            self.0 as u32
        }}
    }}

{dll_defs}
    #[inline]
    pub fn is_windows() -> bool {{
        unsafe {{ __hostos & 4 != 0 }}
    }}

{os_ext_fns}{wide_fns}{callback_ty}
    /// where a declaration's function is, found the first time it's called
    pub struct Slot {{
        raw: AtomicPtr<c_void>,
        tramp: AtomicPtr<c_void>,
        direct: AtomicPtr<c_void>,
    }}

    impl Slot {{
        pub const fn new() -> Slot {{
            Slot {{
                raw: AtomicPtr::new(0 as *mut c_void),
                tramp: AtomicPtr::new(0 as *mut c_void),
                direct: AtomicPtr::new(0 as *mut c_void),
            }}
        }}

        /// the function as a stub outside the program that calls it in
        /// the host's convention, see cosmo_dlhostcall() in the fork
        #[inline]
        pub unsafe fn direct(&self, lib: *const u8, name: &[u8], nargs: i32) -> *mut c_void {{
            let d = self.direct.load(Ordering::Acquire);
            if !d.is_null() {{
                return d;
            }}
            let d = unsafe {{ cosmo_dlhostcall(self.raw(lib, name), nargs) }};
            self.direct.store(d, Ordering::Release);
            d
        }}

        /// the function as the host sees it
        #[inline]
        pub unsafe fn raw(&self, lib: *const u8, name: &[u8]) -> *mut c_void {{
            let p = self.raw.load(Ordering::Acquire);
            if !p.is_null() {{
                return p;
            }}
            let p = unsafe {{ __hostlink_resolve(lib, name.as_ptr()) }};
            self.raw.store(p, Ordering::Release);
            p
        }}

        /// the function behind cosmo's trampoline, callable as extern "C"
        #[inline]
        pub unsafe fn tramp(&self, lib: *const u8, name: &[u8]) -> *mut c_void {{
            let t = self.tramp.load(Ordering::Acquire);
            if !t.is_null() {{
                return t;
            }}
            let t = unsafe {{ cosmo_dltramp(self.raw(lib, name)) }};
            self.tramp.store(t, Ordering::Release);
            t
        }}
    }}

    // struct HostLinkSync in libc/dlopen/hostlink.c, the variables of a
    // library that has its own copies, which HostLinkLib's sync field
    // points at once it loads
    #[repr(C)]
    struct Var {{
        ours: *mut u8,
        theirs: *mut u8,
        last: *mut u8,
        size: usize,
    }}

    #[repr(C)]
    struct Sync {{
        n: usize,
        v: [Var; 0],
    }}

    #[inline(always)]
    unsafe fn vars<'a>(lib: *const u8) -> &'a [Var] {{
        unsafe {{
            let s = (*(lib.add(40) as *const AtomicPtr<Sync>)).load(Ordering::Acquire);
            if s.is_null() {{
                return &[];
            }}
            self::__core::slice::from_raw_parts((*s).v.as_ptr(), (*s).n)
        }}
    }}

    #[inline(always)]
    unsafe fn same(a: *const u8, b: *const u8, size: usize) -> bool {{
        unsafe {{
            if size == 8 {{
                (a as *const u64).read_unaligned() == (b as *const u64).read_unaligned()
            }} else if size == 4 {{
                (a as *const u32).read_unaligned() == (b as *const u32).read_unaligned()
            }} else {{
                self::__core::slice::from_raw_parts(a, size) == self::__core::slice::from_raw_parts(b, size)
            }}
        }}
    }}

    /// before a call: the library's copies of the variables we changed
    /// since the last call get our values
    #[inline(always)]
    pub unsafe fn sync_in(lib: *const u8) -> bool {{
        let vars = unsafe {{ vars(lib) }};
        for v in vars {{
            unsafe {{
                if !same(v.ours, v.last, v.size) {{
                    self::__core::ptr::copy_nonoverlapping(v.ours, v.theirs, v.size);
                    self::__core::ptr::copy_nonoverlapping(v.ours, v.last, v.size);
                }}
            }}
        }}
        !vars.is_empty()
    }}

    /// after it: ours get the values of the ones it changed
    #[inline(always)]
    pub unsafe fn sync_out(lib: *const u8) {{
        for v in unsafe {{ vars(lib) }} {{
            unsafe {{
                if !same(v.theirs, v.last, v.size) {{
                    self::__core::ptr::copy_nonoverlapping(v.theirs, v.ours, v.size);
                    self::__core::ptr::copy_nonoverlapping(v.theirs, v.last, v.size);
                }}
            }}
        }}
    }}

    pub unsafe fn data(lib: *const u8, name: &'static [u8], addr: *mut u8, size: usize) {{
        unsafe {{ __hostlink_data(lib, name.as_ptr(), addr, size) }}
    }}

    #[repr(C)]
    pub struct Ret {{
        x: u64,
        f: u64,
    }}

    // the signature of a C function pointer type, or an Option of one, as
    // __hostlink_callback() takes it: the argument count, which of them
    // are floating point, and 64 for none known (a type-erased callback)
    fn callback_desc(name: &str) -> Option<u64> {{
        let name = name.strip_prefix("core::option::Option<").unwrap_or(name);
        let name = match name.find("> ") {{
            Some(i) if name.starts_with("for<") => &name[i + 2..],
            _ => name,
        }};
        let name = name.strip_prefix("unsafe ").unwrap_or(name);
        let rest = ["extern \"C\" fn(", "extern \"C-unwind\" fn(", "extern \"system\" fn(", "extern \"system-unwind\" fn("]
            .iter()
            .find_map(|p| name.strip_prefix(p))?;
        let mut depth = 0i32;
        let mut end = None;
        let mut prev = ' ';
        for (i, c) in rest.char_indices() {{
            match c {{
                '(' | '<' | '[' => depth += 1,
                '>' if prev == '-' => {{}}
                ')' if depth == 0 => {{
                    end = Some(i);
                    break;
                }}
                ')' | '>' | ']' => depth -= 1,
                _ => {{}}
            }}
            prev = c;
        }}
        let params = &rest[..end?];
        if params.trim().is_empty() {{
            return Some(64);
        }}
        let (mut n, mut floats, mut depth, mut start, mut prev) = (0u64, 0u64, 0i32, 0usize, ' ');
        let mut one = |t: &str, n: &mut u64| {{
            let t = t.trim();
            if t == "f64" || t == "f32" {{
                floats |= 1 << *n;
            }}
            *n += 1;
        }};
        for (i, c) in params.char_indices() {{
            match c {{
                '(' | '<' | '[' => depth += 1,
                '>' if prev == '-' => {{}}
                ')' | '>' | ']' => depth -= 1,
                ',' if depth == 0 => {{
                    one(&params[start..i], &mut n);
                    start = i + 1;
                }}
                _ => {{}}
            }}
            prev = c;
        }}
        if !params[start..].trim().is_empty() {{
            one(&params[start..], &mut n);
        }}
        (n <= 32).then(|| n | floats << 8)
    }}

    /// an argument on its way to a Windows DLL: a callback of ours becomes
    /// something the DLL can call with its own convention
    #[inline]
    pub fn host_arg<T>(v: T) -> T {{
        host_arg_m::<T, 0>(v)
    }}

    /// host_arg for a callback whose parameters or return value are a
    /// C `long`, `unsigned long` or `wchar_t`: `M` says which, in the
    /// bits above the descriptor's floating point marks
    #[inline]
    pub fn host_arg_m<T, const M: u64>(v: T) -> T {{
        if self::__core::mem::size_of::<T>() != self::__core::mem::size_of::<usize>() {{
            return v;
        }}
        let Some(desc) = callback_desc(self::__core::any::type_name::<T>()) else {{ return v }};
        let desc = desc | M;
        unsafe {{
            let p: *mut c_void = self::__core::mem::transmute_copy(&v);
            let q = __hostlink_callback(p, desc);
            self::__core::mem::forget(v);
            self::__core::mem::transmute_copy(&q)
        }}
    }}

    /// an argument to a variadic function: whether it's floating point,
    /// and its bits, a variadic float promoted to double as C does
    #[inline]
    pub fn varg<T>(v: T, variadic: bool) -> (bool, u64) {{
        let name = self::__core::any::type_name::<T>();
        let size = self::__core::mem::size_of::<T>();
        if size > 8 {{
            panic!("{{}} can't be passed by value to a variadic host function", name);
        }}
        let v = if is_windows() {{ host_arg(v) }} else {{ v }};
        unsafe {{
            match name {{
                "f64" => (true, self::__core::mem::transmute_copy::<T, u64>(&v)),
                "f32" => {{
                    let x = self::__core::mem::transmute_copy::<T, f32>(&v);
                    (true, if variadic {{ (x as f64).to_bits() }} else {{ x.to_bits() as u64 }})
                }}
                "i8" => (false, self::__core::mem::transmute_copy::<T, i8>(&v) as i64 as u64),
                "i16" => (false, self::__core::mem::transmute_copy::<T, i16>(&v) as i64 as u64),
                "i32" => (false, self::__core::mem::transmute_copy::<T, i32>(&v) as i64 as u64),
                _ => {{
                    let mut b = [0u8; 8];
                    self::__core::ptr::copy_nonoverlapping(&v as *const T as *const u8, b.as_mut_ptr(), size);
                    self::__core::mem::forget(v);
                    (false, u64::from_le_bytes(b))
                }}
            }}
        }}
    }}

    /// calls a variadic host function, see __ape_hostlink_variadic
    pub unsafe fn vcall<R>(slot: &Slot, lib: *const u8, name: &[u8], nfixed: usize, args: &[(bool, u64)]) -> R {{
        let f = if is_windows() {{ unsafe {{ slot.raw(lib, name) }} }} else {{ unsafe {{ slot.tramp(lib, name) }} }};
        let mut bits = [0u64; 64];
        let mut floats = 0u64;
        for (i, (fl, b)) in args.iter().enumerate() {{
            bits[i] = *b;
            if *fl {{
                floats |= 1 << i;
            }}
        }}
        let s = unsafe {{ sync_in(lib) }};
        let r = unsafe {{ __hostlink_vcall(f, bits.as_ptr(), args.len(), nfixed, floats) }};
        if s {{
            unsafe {{ sync_out(lib) }};
        }}
        let name = self::__core::any::type_name::<R>();
        let size = self::__core::mem::size_of::<R>();
        unsafe {{
            if size == 0 {{
                self::__core::mem::zeroed()
            }} else if name == "f64" {{
                self::__core::mem::transmute_copy(&r.f)
            }} else if name == "f32" {{
                self::__core::mem::transmute_copy(&(r.f as u32))
            }} else if size <= 8 {{
                self::__core::mem::transmute_copy(&r.x)
            }} else {{
                panic!("{{}} can't be returned from a variadic host function", name)
            }}
        }}
    }}
}}

#[doc(hidden)]
#[macro_export]
macro_rules! __ape_hostlink_variadic {{
    ($ty:ident, $slot:path, $lib:expr, $sym:expr, [$($f:ident : $F:ty),*], $R:ty) => {{
        $crate::__ape_hostlink_variadic!(@one $ty, $slot, $lib, $sym, [$($f: $F),*], $R, []);
        $crate::__ape_hostlink_variadic!(@one $ty, $slot, $lib, $sym, [$($f: $F),*], $R, [v0 V0]);
        $crate::__ape_hostlink_variadic!(@one $ty, $slot, $lib, $sym, [$($f: $F),*], $R, [v0 V0, v1 V1]);
        $crate::__ape_hostlink_variadic!(@one $ty, $slot, $lib, $sym, [$($f: $F),*], $R, [v0 V0, v1 V1, v2 V2]);
        $crate::__ape_hostlink_variadic!(@one $ty, $slot, $lib, $sym, [$($f: $F),*], $R, [v0 V0, v1 V1, v2 V2, v3 V3]);
        $crate::__ape_hostlink_variadic!(@one $ty, $slot, $lib, $sym, [$($f: $F),*], $R, [v0 V0, v1 V1, v2 V2, v3 V3, v4 V4]);
        $crate::__ape_hostlink_variadic!(@one $ty, $slot, $lib, $sym, [$($f: $F),*], $R, [v0 V0, v1 V1, v2 V2, v3 V3, v4 V4, v5 V5]);
        $crate::__ape_hostlink_variadic!(@one $ty, $slot, $lib, $sym, [$($f: $F),*], $R, [v0 V0, v1 V1, v2 V2, v3 V3, v4 V4, v5 V5, v6 V6]);
        $crate::__ape_hostlink_variadic!(@one $ty, $slot, $lib, $sym, [$($f: $F),*], $R, [v0 V0, v1 V1, v2 V2, v3 V3, v4 V4, v5 V5, v6 V6, v7 V7]);
        $crate::__ape_hostlink_variadic!(@one $ty, $slot, $lib, $sym, [$($f: $F),*], $R, [v0 V0, v1 V1, v2 V2, v3 V3, v4 V4, v5 V5, v6 V6, v7 V7, v8 V8]);
        $crate::__ape_hostlink_variadic!(@one $ty, $slot, $lib, $sym, [$($f: $F),*], $R, [v0 V0, v1 V1, v2 V2, v3 V3, v4 V4, v5 V5, v6 V6, v7 V7, v8 V8, v9 V9]);
        $crate::__ape_hostlink_variadic!(@one $ty, $slot, $lib, $sym, [$($f: $F),*], $R, [v0 V0, v1 V1, v2 V2, v3 V3, v4 V4, v5 V5, v6 V6, v7 V7, v8 V8, v9 V9, v10 V10]);
        $crate::__ape_hostlink_variadic!(@one $ty, $slot, $lib, $sym, [$($f: $F),*], $R, [v0 V0, v1 V1, v2 V2, v3 V3, v4 V4, v5 V5, v6 V6, v7 V7, v8 V8, v9 V9, v10 V10, v11 V11]);
        $crate::__ape_hostlink_variadic!(@one $ty, $slot, $lib, $sym, [$($f: $F),*], $R, [v0 V0, v1 V1, v2 V2, v3 V3, v4 V4, v5 V5, v6 V6, v7 V7, v8 V8, v9 V9, v10 V10, v11 V11, v12 V12]);
        $crate::__ape_hostlink_variadic!(@one $ty, $slot, $lib, $sym, [$($f: $F),*], $R, [v0 V0, v1 V1, v2 V2, v3 V3, v4 V4, v5 V5, v6 V6, v7 V7, v8 V8, v9 V9, v10 V10, v11 V11, v12 V12, v13 V13]);
        $crate::__ape_hostlink_variadic!(@one $ty, $slot, $lib, $sym, [$($f: $F),*], $R, [v0 V0, v1 V1, v2 V2, v3 V3, v4 V4, v5 V5, v6 V6, v7 V7, v8 V8, v9 V9, v10 V10, v11 V11, v12 V12, v13 V13, v14 V14]);
        $crate::__ape_hostlink_variadic!(@one $ty, $slot, $lib, $sym, [$($f: $F),*], $R, [v0 V0, v1 V1, v2 V2, v3 V3, v4 V4, v5 V5, v6 V6, v7 V7, v8 V8, v9 V9, v10 V10, v11 V11, v12 V12, v13 V13, v14 V14, v15 V15]);
    }};
    (@one $ty:ident, $slot:path, $lib:expr, $sym:expr, [$($f:ident : $F:ty),*], $R:ty, [$($v:ident $V:ident),*]) => {{
        impl<$($V),*> $crate::__ape_hostlink::__core::ops::FnOnce<($($F,)* $($V,)*)> for $ty {{
            type Output = $R;
            extern "rust-call" fn call_once(self, a: ($($F,)* $($V,)*)) -> $R {{
                $crate::__ape_hostlink::__core::ops::Fn::call(&self, a)
            }}
        }}
        impl<$($V),*> $crate::__ape_hostlink::__core::ops::FnMut<($($F,)* $($V,)*)> for $ty {{
            extern "rust-call" fn call_mut(&mut self, a: ($($F,)* $($V,)*)) -> $R {{
                $crate::__ape_hostlink::__core::ops::Fn::call(&*self, a)
            }}
        }}
        impl<$($V),*> $crate::__ape_hostlink::__core::ops::Fn<($($F,)* $($V,)*)> for $ty {{
            #[allow(non_snake_case, unused_unsafe)]
            extern "rust-call" fn call(&self, a: ($($F,)* $($V,)*)) -> $R {{
                let ($($f,)* $($v,)*) = a;
                let nfixed = [$(stringify!($f)),*].len();
                unsafe {{
                    $crate::__ape_hostlink::vcall::<$R>(&$slot, $lib, $sym, nfixed, &[
                        $($crate::__ape_hostlink::varg($f, false),)*
                        $($crate::__ape_hostlink::varg($v, true),)*
                    ])
                }}
            }}
        }}
    }};
}}
{reexport}"#
    )
}

// The C library's wide character functions as Windows code means them,
// with a 16-bit wchar_t, for the stand-ins the rewrite puts in place of
// their declarations (see WIDE_FNS in transform.rs). The C library here
// has a 32-bit one. Case folding is the "C" locale's, ASCII only.
const WIDE: &str = r#"
    pub mod wide {
        use super::__core::ptr;

        unsafe extern "C" {
            fn malloc(n: usize) -> *mut u16;
        }

        fn fold(c: u16) -> u16 {
            if (0x41..=0x5a).contains(&c) { c + 0x20 } else { c }
        }

        pub unsafe fn wcslen(s: *const u16) -> usize {
            unsafe { wcsnlen(s, usize::MAX) }
        }

        pub unsafe fn wcsnlen(s: *const u16, max: usize) -> usize {
            let mut n = 0;
            while n < max && unsafe { *s.add(n) } != 0 {
                n += 1;
            }
            n
        }

        unsafe fn cmp(a: *const u16, b: *const u16, n: usize, f: fn(u16) -> u16, nul: bool) -> i32 {
            for i in 0..n {
                let (x, y) = unsafe { (f(*a.add(i)), f(*b.add(i))) };
                if x != y {
                    return x as i32 - y as i32;
                }
                if nul && x == 0 {
                    return 0;
                }
            }
            0
        }

        pub unsafe fn wcscmp(a: *const u16, b: *const u16) -> i32 {
            unsafe { cmp(a, b, usize::MAX, |c| c, true) }
        }

        pub unsafe fn wcsncmp(a: *const u16, b: *const u16, n: usize) -> i32 {
            unsafe { cmp(a, b, n, |c| c, true) }
        }

        pub unsafe fn _wcsicmp(a: *const u16, b: *const u16) -> i32 {
            unsafe { cmp(a, b, usize::MAX, fold, true) }
        }

        pub unsafe fn _wcsnicmp(a: *const u16, b: *const u16, n: usize) -> i32 {
            unsafe { cmp(a, b, n, fold, true) }
        }

        pub unsafe fn wmemcmp(a: *const u16, b: *const u16, n: usize) -> i32 {
            unsafe { cmp(a, b, n, |c| c, false) }
        }

        pub unsafe fn wcscpy(d: *mut u16, s: *const u16) -> *mut u16 {
            unsafe { ptr::copy(s, d, wcslen(s) + 1) };
            d
        }

        pub unsafe fn wcsncpy(d: *mut u16, s: *const u16, n: usize) -> *mut u16 {
            let k = unsafe { wcsnlen(s, n) };
            unsafe {
                ptr::copy(s, d, k);
                ptr::write_bytes(d.add(k), 0, n - k);
            }
            d
        }

        pub unsafe fn wcscat(d: *mut u16, s: *const u16) -> *mut u16 {
            unsafe { wcscpy(d.add(wcslen(d)), s) };
            d
        }

        pub unsafe fn wcsncat(d: *mut u16, s: *const u16, n: usize) -> *mut u16 {
            unsafe {
                let e = d.add(wcslen(d));
                let k = wcsnlen(s, n);
                ptr::copy(s, e, k);
                *e.add(k) = 0;
            }
            d
        }

        pub unsafe fn wcschr(s: *const u16, c: u16) -> *mut u16 {
            let mut p = s;
            loop {
                let x = unsafe { *p };
                if x == c {
                    return p as *mut u16;
                }
                if x == 0 {
                    return ptr::null_mut();
                }
                p = unsafe { p.add(1) };
            }
        }

        pub unsafe fn wcsrchr(s: *const u16, c: u16) -> *mut u16 {
            let n = unsafe { wcslen(s) };
            for i in (0..=n).rev() {
                if unsafe { *s.add(i) } == c {
                    return unsafe { s.add(i) } as *mut u16;
                }
            }
            ptr::null_mut()
        }

        pub unsafe fn wcsstr(h: *const u16, n: *const u16) -> *mut u16 {
            let (hl, nl) = unsafe { (wcslen(h), wcslen(n)) };
            for i in 0..=hl.saturating_sub(nl) {
                if nl <= hl && unsafe { cmp(h.add(i), n, nl, |c| c, false) } == 0 {
                    return unsafe { h.add(i) } as *mut u16;
                }
            }
            ptr::null_mut()
        }

        unsafe fn span(s: *const u16, set: *const u16, inside: bool) -> usize {
            let mut n = 0;
            loop {
                let c = unsafe { *s.add(n) };
                if c == 0 || unsafe { !wcschr(set, c).is_null() } != inside {
                    return n;
                }
                n += 1;
            }
        }

        pub unsafe fn wcsspn(s: *const u16, set: *const u16) -> usize {
            unsafe { span(s, set, true) }
        }

        pub unsafe fn wcscspn(s: *const u16, set: *const u16) -> usize {
            unsafe { span(s, set, false) }
        }

        pub unsafe fn wcspbrk(s: *const u16, set: *const u16) -> *mut u16 {
            let n = unsafe { span(s, set, false) };
            if unsafe { *s.add(n) } == 0 {
                ptr::null_mut()
            } else {
                unsafe { s.add(n) as *mut u16 }
            }
        }

        pub unsafe fn _wcsdup(s: *const u16) -> *mut u16 {
            let n = unsafe { wcslen(s) } + 1;
            let d = unsafe { malloc(n * 2) };
            if !d.is_null() {
                unsafe { ptr::copy_nonoverlapping(s, d, n) };
            }
            d
        }

        pub unsafe fn wmemcpy(d: *mut u16, s: *const u16, n: usize) -> *mut u16 {
            unsafe { ptr::copy_nonoverlapping(s, d, n) };
            d
        }

        pub unsafe fn wmemmove(d: *mut u16, s: *const u16, n: usize) -> *mut u16 {
            unsafe { ptr::copy(s, d, n) };
            d
        }

        pub unsafe fn wmemset(d: *mut u16, c: u16, n: usize) -> *mut u16 {
            for i in 0..n {
                unsafe { *d.add(i) = c };
            }
            d
        }

        pub unsafe fn wmemchr(s: *const u16, c: u16, n: usize) -> *mut u16 {
            for i in 0..n {
                if unsafe { *s.add(i) } == c {
                    return unsafe { s.add(i) } as *mut u16;
                }
            }
            ptr::null_mut()
        }
    }
"#;
