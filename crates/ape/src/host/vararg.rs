//! Variadic calls in both directions: reading a host's `va_list`, and
//! laying out the arguments of a call the way each host wants them, Apple's
//! arm64 stack convention included.

use std::ffi::c_void;

/// An argument to a variadic host function, as `host_variadic!` sees it on
/// Apple Silicon. Where the ABI splits by kind, `classify` says whether the
/// value goes in a floating point register, and gives its bits.
#[doc(hidden)]
pub trait VarArg {
    fn classify(self) -> (bool, u64);
}

macro_rules! vararg_int {
    ($($t:ty),*) => {$(
        impl VarArg for $t {
            fn classify(self) -> (bool, u64) {
                (false, self as i64 as u64)
            }
        }
    )*};
}

vararg_int!(
    i8, i16, i32, i64, isize, u8, u16, u32, u64, usize, bool, char
);

impl<T> VarArg for *const T {
    fn classify(self) -> (bool, u64) {
        (false, self as usize as u64)
    }
}

impl<T> VarArg for *mut T {
    fn classify(self) -> (bool, u64) {
        (false, self as usize as u64)
    }
}

impl VarArg for f64 {
    fn classify(self) -> (bool, u64) {
        (true, self.to_bits())
    }
}

impl VarArg for f32 {
    fn classify(self) -> (bool, u64) {
        (true, self.to_bits() as u64)
    }
}

/// A `va_list` a host function handed one of our callbacks, read with the
/// convention of the host we're on.
///
/// `av_log_set_callback`'s callback gets one, and so does anything else a
/// C library lets you plug in behind a `printf`-like function. It is a
/// pointer-sized argument on every host, so the callback declares it as
/// `*mut c_void` (bindgen's `*mut __va_list_tag` is the same thing) and
/// wraps it here. Each `arg` takes the next value; ask for what C would
/// have promoted the argument to, `c_int` for a `char` or `short`, `f64`
/// for a `float`, and a pointer for a string. The list stays the host's,
/// so it can also be passed on to `vsnprintf` or `av_log_format_line2`
/// of the same library untouched.
///
/// The layouts: Windows x64 and Apple Silicon keep a pointer to 8 byte
/// stack slots; SysV x86-64 has the register save area with `gp_offset`
/// and `fp_offset` and then the overflow area; AAPCS64 Linux has the
/// general and vector register save areas with negative offsets and the
/// stack after them.
pub struct VaList {
    // where SysV and AAPCS64 keep their structure, and on Windows and
    // Apple the next slot itself, since there the argument is the pointer
    pub(super) p: *mut c_void,
}

impl VaList {
    /// Wraps the `va_list` argument a host function passed.
    pub unsafe fn from_host(p: *mut c_void) -> Self {
        Self { p }
    }

    /// The list as the host knows it, for passing on to a host function,
    /// which continues from where this one has read to.
    pub fn as_ptr(&self) -> *mut c_void {
        self.p
    }

    /// The next argument.
    pub unsafe fn arg<T: VaArg>(&mut self) -> T {
        unsafe { T::from_slot(self.next(T::FLOAT, size_of::<T>())) }
    }

    // where the next argument of the given kind and size sits; the list
    // moves past it
    pub(super) unsafe fn next(&mut self, float: bool, size: usize) -> *const u8 {
        unsafe {
            #[cfg(target_arch = "x86_64")]
            if !crate::is_windows() {
                return self.next_sysv(float, size);
            }
            #[cfg(target_arch = "aarch64")]
            if !crate::is_xnu() {
                return self.next_aapcs(float, size);
            }
            // windows and apple: one 8 byte slot each, a larger struct
            // through a pointer to a copy
            let at = self.p as *const u8;
            self.p = at.add(8) as *mut c_void;
            if size > 8 {
                *(at as *const *const u8)
            } else {
                at
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    unsafe fn next_sysv(&mut self, float: bool, size: usize) -> *const u8 {
        #[repr(C)]
        struct Tag {
            gp_offset: u32,
            fp_offset: u32,
            overflow_arg_area: *const u8,
            reg_save_area: *const u8,
        }
        unsafe {
            let t = &mut *(self.p as *mut Tag);
            if size <= 8 && !float && t.gp_offset < 48 {
                let at = t.reg_save_area.add(t.gp_offset as usize);
                t.gp_offset += 8;
                return at;
            }
            if size <= 8 && float && t.fp_offset < 176 {
                let at = t.reg_save_area.add(t.fp_offset as usize);
                t.fp_offset += 16;
                return at;
            }
            let at = t.overflow_arg_area;
            t.overflow_arg_area = at.add(size.div_ceil(8) * 8);
            at
        }
    }

    #[cfg(target_arch = "aarch64")]
    unsafe fn next_aapcs(&mut self, float: bool, size: usize) -> *const u8 {
        #[repr(C)]
        struct List {
            stack: *const u8,
            gr_top: *const u8,
            vr_top: *const u8,
            gr_offs: i32,
            vr_offs: i32,
        }
        unsafe {
            let l = &mut *(self.p as *mut List);
            if size <= 8 && !float && l.gr_offs < 0 {
                let at = l.gr_top.offset(l.gr_offs as isize);
                l.gr_offs += 8;
                return at;
            }
            if size <= 8 && float && l.vr_offs < 0 {
                let at = l.vr_top.offset(l.vr_offs as isize);
                l.vr_offs += 16;
                return at;
            }
            // a struct over 16 bytes is passed as a pointer to a copy
            let at = l.stack;
            let n = if size > 16 { 8 } else { size.div_ceil(8) * 8 };
            l.stack = at.add(n);
            if size > 16 {
                *(at as *const *const u8)
            } else {
                at
            }
        }
    }
}

/// A value read from a [`VaList`].
pub trait VaArg: Sized {
    #[doc(hidden)]
    const FLOAT: bool;
    #[doc(hidden)]
    unsafe fn from_slot(p: *const u8) -> Self;
}

macro_rules! vaarg_plain {
    ($float:expr; $($t:ty),*) => {$(
        impl VaArg for $t {
            const FLOAT: bool = $float;
            unsafe fn from_slot(p: *const u8) -> Self {
                unsafe { (p as *const $t).read_unaligned() }
            }
        }
    )*};
}

vaarg_plain!(false; i32, u32, i64, u64, isize, usize);
vaarg_plain!(true; f64);

impl<T> VaArg for *const T {
    const FLOAT: bool = false;
    unsafe fn from_slot(p: *const u8) -> Self {
        unsafe { (p as *const *const T).read_unaligned() }
    }
}

impl<T> VaArg for *mut T {
    const FLOAT: bool = false;
    unsafe fn from_slot(p: *const u8) -> Self {
        unsafe { (p as *const *mut T).read_unaligned() }
    }
}

/// What a variadic host function returned, read from the integer and
/// floating point return registers.
#[doc(hidden)]
pub trait VarRet {
    fn from_regs(x0: u64, d0: u64) -> Self;
}

macro_rules! hostret_int {
    ($($t:ty),*) => {$(
        impl VarRet for $t {
            fn from_regs(x0: u64, _: u64) -> Self {
                x0 as $t
            }
        }
    )*};
}

hostret_int!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize);

impl VarRet for () {
    fn from_regs(_: u64, _: u64) {}
}

impl VarRet for bool {
    fn from_regs(x0: u64, _: u64) -> bool {
        x0 as u8 != 0
    }
}

impl<T> VarRet for *const T {
    fn from_regs(x0: u64, _: u64) -> Self {
        x0 as usize as *const T
    }
}

impl<T> VarRet for *mut T {
    fn from_regs(x0: u64, _: u64) -> Self {
        x0 as usize as *mut T
    }
}

impl VarRet for f64 {
    fn from_regs(_: u64, d0: u64) -> f64 {
        f64::from_bits(d0)
    }
}

impl VarRet for f32 {
    fn from_regs(_: u64, d0: u64) -> f32 {
        f32::from_bits(d0 as u32)
    }
}

/// Calls a variadic function with Apple's arm64 convention, where the fixed
/// arguments take registers as usual and every variadic argument goes on
/// the stack in an 8 byte slot. Our code is built for the standard AAPCS64,
/// which passes them in registers, so this call has to be spelled out.
///
/// `args` holds every argument, the first `nfixed` of them being the fixed
/// ones. Returns the x0 and d0 registers.
#[cfg(target_arch = "aarch64")]
#[doc(hidden)]
pub unsafe fn xnu_vcall(f: usize, args: &[(bool, u64)], nfixed: usize) -> (u64, u64) {
    let mut x = [0u64; 8];
    let mut d = [0u64; 8];
    let (mut nx, mut nd) = (0, 0);
    let mut stack: Vec<u64> = Vec::new();
    for (i, &(float, bits)) in args.iter().enumerate() {
        if i >= nfixed {
            stack.push(bits);
        } else if float && nd < 8 {
            d[nd] = bits;
            nd += 1;
        } else if !float && nx < 8 {
            x[nx] = bits;
            nx += 1;
        } else {
            stack.push(bits);
        }
    }
    unsafe { xnu_call(f, &x, &d, &stack) }
}

/// Calls a function with more than eight arguments with Apple's arm64
/// convention, where the arguments past the registers are packed on the
/// stack at their natural alignment, not in 8 byte slots as the standard
/// AAPCS64 our code is built for has them.
///
/// Each argument is whether it goes in a floating point register, its
/// bits, and its size and alignment in the host's declaration. Returns
/// the x0 and d0 registers.
#[cfg(target_arch = "aarch64")]
#[doc(hidden)]
pub unsafe fn xnu_pcall(f: usize, args: &[(bool, u64, usize, usize)]) -> (u64, u64) {
    let mut x = [0u64; 8];
    let mut d = [0u64; 8];
    let (mut nx, mut nd) = (0, 0);
    let mut stack: Vec<u64> = vec![0; args.len()];
    let mut used = 0;
    for &(float, bits, size, align) in args {
        if float && nd < 8 {
            d[nd] = bits;
            nd += 1;
        } else if !float && nx < 8 {
            x[nx] = bits;
            nx += 1;
        } else {
            let align = align.max(1);
            let at = (used + align - 1) / align * align;
            let size = size.min(8);
            unsafe {
                core::ptr::copy_nonoverlapping(
                    bits.to_le_bytes().as_ptr(),
                    (stack.as_mut_ptr() as *mut u8).add(at),
                    size,
                );
            }
            used = at + size;
        }
    }
    stack.truncate((used + 7) / 8);
    unsafe { xnu_call(f, &x, &d, &stack) }
}

/// Whether a call laid out the standard way puts any of these arguments
/// somewhere else than Apple's arm64 convention does: each is whether it
/// goes in a floating point register, and its size and alignment.
#[cfg(target_arch = "aarch64")]
#[doc(hidden)]
pub const fn xnu_packs(args: &[(bool, usize, usize)]) -> bool {
    let (mut nx, mut nd, mut standard, mut packed) = (0, 0, 0, 0);
    let mut i = 0;
    while i < args.len() {
        let (float, size, align) = args[i];
        i += 1;
        if float && nd < 8 {
            nd += 1;
        } else if !float && nx < 8 {
            nx += 1;
        } else {
            let align = if align < 1 { 1 } else { align };
            packed = (packed + align - 1) / align * align;
            if packed != standard {
                return true;
            }
            packed += size;
            standard += 8;
        }
    }
    false
}

/// The smaller of two sizes.
#[doc(hidden)]
pub const fn xnu_min(a: usize, b: usize) -> usize {
    if a < b { a } else { b }
}

/// The arguments a host passed one of our functions with Apple's arm64
/// convention, taken out one at a time in order: from the registers while
/// they last, then from the stack where Apple packs them at their own
/// sizes.
#[cfg(target_arch = "aarch64")]
#[doc(hidden)]
pub struct XnuArgs {
    x: [u64; 8],
    d: [u64; 8],
    pub(super) stack: *const u8,
    nx: usize,
    nd: usize,
    off: usize,
}

#[cfg(target_arch = "aarch64")]
impl XnuArgs {
    pub fn new(x: [u64; 8], d: [f64; 8], stack: *const u8) -> XnuArgs {
        XnuArgs {
            x,
            d: d.map(f64::to_bits),
            stack,
            nx: 0,
            nd: 0,
            off: 0,
        }
    }

    /// The next argument, declared as `T` and passed as `H`: the host's
    /// width is the narrower of the two, as a small integer travels at
    /// its own size on the stack and a `long double` as a double.
    ///
    /// # Safety
    /// The arguments were passed in this order, and `stack` is where the
    /// ones past the registers begin.
    pub unsafe fn next<T: Copy + super::layout::HostRet<H>, H: Copy>(&mut self, float: bool) -> T {
        let size = xnu_min(size_of::<T>(), size_of::<H>());
        let align = xnu_min(align_of::<T>(), align_of::<H>()).max(1);
        let mut bytes = [0u8; 8];
        if float && self.nd < 8 {
            bytes = self.d[self.nd].to_le_bytes();
            self.nd += 1;
        } else if !float && self.nx < 8 {
            bytes = self.x[self.nx].to_le_bytes();
            self.nx += 1;
        } else {
            let at = self.off.div_ceil(align) * align;
            unsafe { core::ptr::copy_nonoverlapping(self.stack.add(at), bytes.as_mut_ptr(), size) };
            self.off = at + size;
        }
        unsafe {
            if size_of::<T>() <= 8 {
                core::ptr::read_unaligned(bytes.as_ptr() as *const T)
            } else {
                T::from_host(core::ptr::read_unaligned(bytes.as_ptr() as *const H))
            }
        }
    }
}

/// The bits of a value as a register holds them: its bytes, up to eight,
/// in the low end.
#[cfg(target_arch = "aarch64")]
#[doc(hidden)]
pub fn xnu_bits<T>(v: &T) -> u64 {
    let mut bytes = [0u8; 8];
    let n = core::mem::size_of::<T>().min(8);
    unsafe { core::ptr::copy_nonoverlapping(v as *const T as *const u8, bytes.as_mut_ptr(), n) };
    u64::from_le_bytes(bytes)
}

/// `Kind::<T>::FLOAT` says at compile time whether `T` is passed in a
/// floating point register: an inherent constant for `f32` and `f64`,
/// and [`XnuKind`]'s for every other type.
#[doc(hidden)]
pub struct Kind<T: ?Sized>(core::marker::PhantomData<T>);

impl Kind<f64> {
    pub const FLOAT: bool = true;
}

impl Kind<f32> {
    pub const FLOAT: bool = true;
}

#[doc(hidden)]
pub trait XnuKind {
    const FLOAT: bool;
}

impl<T: ?Sized> XnuKind for Kind<T> {
    const FLOAT: bool = false;
}

#[cfg(target_arch = "aarch64")]
unsafe fn xnu_call(f: usize, x: &[u64; 8], d: &[u64; 8], stack: &[u64]) -> (u64, u64) {
    let n = stack.len();
    let size = (n * 8 + 15) & !15;
    let x0: u64;
    let d0: f64;
    unsafe {
        core::arch::asm!(
            "mov x20, sp",
            "sub sp, sp, x13",
            "mov x14, sp",
            "cbz x10, 2f",
            "1:",
            "ldr x15, [x9], #8",
            "str x15, [x14], #8",
            "subs x10, x10, #1",
            "b.ne 1b",
            "2:",
            "ldp x0, x1, [x11]",
            "ldp x2, x3, [x11, #16]",
            "ldp x4, x5, [x11, #32]",
            "ldp x6, x7, [x11, #48]",
            "ldp d0, d1, [x12]",
            "ldp d2, d3, [x12, #16]",
            "ldp d4, d5, [x12, #32]",
            "ldp d6, d7, [x12, #48]",
            "blr x16",
            "mov sp, x20",
            in("x9") stack.as_ptr(),
            in("x10") n,
            in("x11") x.as_ptr(),
            in("x12") d.as_ptr(),
            in("x13") size,
            in("x16") f,
            out("x20") _,
            lateout("x0") x0,
            lateout("d0") d0,
            clobber_abi("C"),
        );
    }
    (x0, d0.to_bits())
}
