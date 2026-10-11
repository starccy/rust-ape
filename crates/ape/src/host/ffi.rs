//! Calls through libffi, for what a function pointer's Rust type can't
//! express: a `long double` by value on Linux, which x86-64 passes on the
//! x87 stack, and a structure by value that each host lays out
//! differently. libffi is part of libcosmo and follows the convention of
//! the host we're on, Apple's variant of arm64 included.

use std::ffi::c_void;

use super::Callback;
use super::layout::{LongDouble, WStr16};

#[repr(C)]
pub struct FfiType {
    size: usize,
    alignment: u16,
    type_: u16,
    elements: *mut *mut FfiType,
}

unsafe impl Send for FfiType {}
unsafe impl Sync for FfiType {}

unsafe extern "C" {
    static mut ffi_type_void: FfiType;
    static mut ffi_type_uint8: FfiType;
    static mut ffi_type_sint8: FfiType;
    static mut ffi_type_uint16: FfiType;
    static mut ffi_type_sint16: FfiType;
    static mut ffi_type_uint32: FfiType;
    static mut ffi_type_sint32: FfiType;
    static mut ffi_type_uint64: FfiType;
    static mut ffi_type_sint64: FfiType;
    static mut ffi_type_float: FfiType;
    static mut ffi_type_double: FfiType;
    static mut ffi_type_longdouble: FfiType;
    static mut ffi_type_pointer: FfiType;
    fn ffi_prep_cif(
        cif: *mut u8,
        abi: i32,
        nargs: u32,
        rtype: *mut FfiType,
        atypes: *mut *mut FfiType,
    ) -> i32;
    fn ffi_call(cif: *mut u8, f: *const c_void, rvalue: *mut c_void, avalue: *mut *mut c_void);
}

const FFI_TYPE_STRUCT: u16 = 13;

/// A C type as libffi classifies it. A structure lists its members in
/// order, an array as that many of its element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Void,
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    F32,
    F64,
    LongDouble,
    Ptr,
    Struct(&'static [Kind]),
}

/// The [`Kind`] of a type in a `#[host(ffi, ..)]` declaration; the
/// generator writes the impl for a structure.
pub trait FfiKind {
    fn kind() -> Kind;
}

macro_rules! kinds {
    ($($t:ty => $k:ident),*) => {$(
        impl FfiKind for $t {
            fn kind() -> Kind {
                Kind::$k
            }
        }
    )*};
}

kinds!(() => Void, i8 => I8, u8 => U8, i16 => I16, u16 => U16, i32 => I32, u32 => U32,
       i64 => I64, u64 => U64, isize => I64, usize => U64, f32 => F32, f64 => F64,
       LongDouble => LongDouble, Callback => Ptr, WStr16 => Ptr);

impl FfiKind for bool {
    fn kind() -> Kind {
        Kind::U8
    }
}

impl<T> FfiKind for *const T {
    fn kind() -> Kind {
        Kind::Ptr
    }
}

impl<T> FfiKind for *mut T {
    fn kind() -> Kind {
        Kind::Ptr
    }
}

// the convention of the host we're on, in libffi's numbering for this
// architecture
fn abi() -> i32 {
    #[cfg(target_arch = "x86_64")]
    {
        if crate::is_windows() { 3 } else { 2 }
    }
    #[cfg(target_arch = "aarch64")]
    {
        if crate::is_xnu() { 1 } else { 3 }
    }
}

/// A prepared call interface for one signature, made once per function.
pub struct Cif {
    raw: Box<[u64; 8]>,
    // element lists point into these, so each stays where it is
    #[allow(clippy::vec_box)]
    _types: Vec<Box<FfiType>>,
    _lists: Vec<Box<[*mut FfiType]>>,
}

unsafe impl Send for Cif {}
unsafe impl Sync for Cif {}

impl Cif {
    pub fn new(args: &[Kind], ret: Kind) -> Cif {
        let mut cif = Cif {
            raw: Box::new([0; 8]),
            _types: Vec::new(),
            _lists: Vec::new(),
        };
        let rtype = cif.type_of(ret);
        let mut atypes: Vec<*mut FfiType> = args.iter().map(|&k| cif.type_of(k)).collect();
        let n = atypes.len() as u32;
        let st = unsafe {
            ffi_prep_cif(
                cif.raw.as_mut_ptr() as *mut u8,
                abi(),
                n,
                rtype,
                atypes.as_mut_ptr(),
            )
        };
        assert_eq!(st, 0, "ffi_prep_cif failed for {args:?} -> {ret:?}");
        cif._lists.push(atypes.into_boxed_slice());
        cif
    }

    fn type_of(&mut self, k: Kind) -> *mut FfiType {
        match k {
            Kind::Void => &raw mut ffi_type_void,
            Kind::I8 => &raw mut ffi_type_sint8,
            Kind::U8 => &raw mut ffi_type_uint8,
            Kind::I16 => &raw mut ffi_type_sint16,
            Kind::U16 => &raw mut ffi_type_uint16,
            Kind::I32 => &raw mut ffi_type_sint32,
            Kind::U32 => &raw mut ffi_type_uint32,
            Kind::I64 => &raw mut ffi_type_sint64,
            Kind::U64 => &raw mut ffi_type_uint64,
            Kind::F32 => &raw mut ffi_type_float,
            Kind::F64 => &raw mut ffi_type_double,
            Kind::LongDouble => &raw mut ffi_type_longdouble,
            Kind::Ptr => &raw mut ffi_type_pointer,
            Kind::Struct(members) => {
                let mut list: Vec<*mut FfiType> =
                    members.iter().map(|&m| self.type_of(m)).collect();
                list.push(std::ptr::null_mut());
                let mut list = list.into_boxed_slice();
                // size and alignment are filled in by ffi_prep_cif
                let t = Box::new(FfiType {
                    size: 0,
                    alignment: 0,
                    type_: FFI_TYPE_STRUCT,
                    elements: list.as_mut_ptr(),
                });
                self._lists.push(list);
                self._types.push(t);
                let p: *const FfiType = &**self._types.last().unwrap();
                p as *mut FfiType
            }
        }
    }

    /// Calls `f` with `args`, each a pointer to an argument of the kind
    /// given to [`new`](Self::new), in order. `R` is the return type, its
    /// kind's Rust form.
    ///
    /// # Safety
    ///
    /// `f` has to be a function of that signature, in the convention of
    /// the host we're on, and the pointers valid.
    pub unsafe fn call<R: Copy>(&self, f: usize, args: &[*mut c_void]) -> R {
        // an integer return smaller than a register comes back as a
        // whole one, so the buffer is at least that big
        let words = size_of::<R>().max(8).div_ceil(16);
        let mut small = [0u128; 4];
        let mut big = Vec::new();
        let buf = if words <= small.len() {
            small.as_mut_ptr()
        } else {
            big.resize(words, 0u128);
            big.as_mut_ptr()
        };
        unsafe {
            ffi_call(
                self.raw.as_ptr() as *mut u8,
                f as *const c_void,
                buf as *mut c_void,
                args.as_ptr() as *mut *mut c_void,
            );
            std::ptr::read_unaligned(buf as *const R)
        }
    }
}

/// What libffi calls for a closure: the cif, where to write the return
/// value, the argument pointers in order, and the user data.
pub type Handler = unsafe extern "C" fn(*mut u8, *mut c_void, *mut *mut c_void, *mut c_void);

unsafe extern "C" {
    fn ffi_get_closure_size() -> usize;
    fn ffi_closure_alloc(size: usize, code: *mut *mut c_void) -> *mut c_void;
    fn ffi_closure_free(p: *mut c_void);
    fn ffi_prep_closure_loc(
        closure: *mut c_void,
        cif: *mut u8,
        f: Handler,
        user: *mut c_void,
        code: *mut c_void,
    ) -> i32;
}

/// A function the host can call in its own convention, which lands in a
/// [`Handler`] with the arguments laid out by libffi. For a callback whose
/// signature no Rust function can have, as a `long double` by value on
/// Linux; `#[host_callback(ffi, ..)]` makes one per callback.
pub struct Closure {
    _cif: Cif,
    mem: *mut c_void,
    code: *mut c_void,
}

unsafe impl Send for Closure {}
unsafe impl Sync for Closure {}

impl Closure {
    pub fn new(args: &[Kind], ret: Kind, f: Handler, user: *mut c_void) -> Closure {
        let cif = Cif::new(args, ret);
        let mut code: *mut c_void = std::ptr::null_mut();
        let mem = unsafe { ffi_closure_alloc(ffi_get_closure_size(), &mut code) };
        assert!(!mem.is_null(), "ffi_closure_alloc failed");
        let st = unsafe { ffi_prep_closure_loc(mem, cif.raw.as_ptr() as *mut u8, f, user, code) };
        assert_eq!(st, 0, "ffi_prep_closure_loc failed for {args:?} -> {ret:?}");
        Closure {
            _cif: cif,
            mem,
            code,
        }
    }

    /// The entry point to give the host.
    pub fn code(&self) -> *const c_void {
        self.code
    }
}

impl Drop for Closure {
    fn drop(&mut self) {
        unsafe { ffi_closure_free(self.mem) };
    }
}

/// Writes `v` where a [`Handler`] returns it. An integer narrower than a
/// register has to be written as a whole one, which libffi reads.
///
/// # Safety
///
/// `ret` is the handler's return pointer, for a kind `R` matches.
pub unsafe fn closure_ret<R: Copy>(ret: *mut c_void, v: R) {
    if size_of::<R>() == 0 {
        return;
    }
    unsafe {
        if size_of::<R>() < 8 {
            std::ptr::write_bytes(ret as *mut u8, 0, 8);
        }
        std::ptr::write_unaligned(ret as *mut R, v);
    }
}
