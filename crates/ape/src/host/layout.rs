//! What bindings made by `cargo xtask host-bindings` use to follow the
//! host's C data model and calling convention.
//!
//! The bindings' API uses the types of the Linux build throughout: a
//! `c_long` is an `i64`, a `wchar_t` an `i32`, a `long double` a
//! [`LongDouble`]. Windows keeps `long` at 32 bits and `wchar_t` at 16,
//! and both Microsoft and Apple's arm64 make `long double` a plain
//! `double`. A structure holding one of those is laid out differently on
//! each side, so the generated bindings keep such a structure in the
//! layout of the host we run on, told apart by [`host`], and reach its
//! fields through methods. Function parameters and return values are
//! converted on the way in and out by [`HostArg`] and [`HostRet`], as the
//! `#[host(..)]` attribute on a declaration says.
//!
//! Apple's arm64 convention also has the caller widen an integer argument
//! smaller than 32 bits, where the standard one has the callee do it. The
//! generator declares such a parameter as `i32` or `u32` on every host but
//! Windows, which the same conversions handle.

use std::ffi::c_int;

/// Which layout structures take here: 0 on Linux and the BSDs, 1 on
/// Windows (x86-64 only), 2 on macOS (arm64 only). What the generated
/// accessors switch on.
#[inline(always)]
pub fn host() -> usize {
    #[cfg(target_arch = "x86_64")]
    if crate::is_windows() {
        return 1;
    }
    #[cfg(target_arch = "aarch64")]
    if crate::is_xnu() {
        return 2;
    }
    0
}

/// Whether structures take Windows' layout here.
#[inline(always)]
pub fn win() -> bool {
    host() == 1
}

/// Converts an argument from the type the bindings declare to the one the
/// host function takes. `len` is how many elements a pointer points at,
/// when the bindings were told (`--len`), and one otherwise. `Keep` is
/// whatever has to live until the call returns, and `back` copies results
/// out of it.
pub trait HostArg<W>: Sized {
    type Keep;
    fn to_host(self, len: Option<usize>) -> (W, Self::Keep);
    fn back(_keep: Self::Keep) {}
}

/// Converts what a host function returned, or what a host passed a
/// callback, to the type the bindings declare.
pub trait HostRet<W> {
    fn from_host(w: W) -> Self;
}

impl<T> HostArg<T> for T {
    type Keep = ();
    #[inline(always)]
    fn to_host(self, _: Option<usize>) -> (T, ()) {
        (self, ())
    }
}

impl<T> HostRet<T> for T {
    #[inline(always)]
    fn from_host(w: T) -> T {
        w
    }
}

// a value the host reads at another width: a `long` on Windows read as
// its low half, or a small integer widened the way Apple's arm64 wants
macro_rules! value {
    ($($from:ty => $to:ty),*) => {$(
        impl HostArg<$to> for $from {
            type Keep = ();
            #[inline]
            fn to_host(self, _: Option<usize>) -> ($to, ()) {
                (self as $to, ())
            }
        }
        impl HostRet<$to> for $from {
            #[inline]
            fn from_host(w: $to) -> $from {
                w as $from
            }
        }
    )*};
}

value!(i64 => i32, u64 => u32, i8 => i32, u8 => u32, i16 => i32, u16 => u32);

// a `char` is a `u8` to Rust on arm64 and signed to Apple, which wants it
// sign extended
impl HostArg<i32> for u8 {
    type Keep = ();
    #[inline]
    fn to_host(self, _: Option<usize>) -> (i32, ()) {
        (self as i8 as i32, ())
    }
}

impl HostRet<i32> for u8 {
    #[inline]
    fn from_host(w: i32) -> u8 {
        w as u8
    }
}

impl HostArg<u32> for bool {
    type Keep = ();
    #[inline]
    fn to_host(self, _: Option<usize>) -> (u32, ()) {
        (self as u32, ())
    }
}

impl HostRet<u32> for bool {
    #[inline]
    fn from_host(w: u32) -> bool {
        w != 0
    }
}

// a pointer to `long`s: the DLL reads and writes a copy of the width it
// knows, which goes back where the caller pointed
macro_rules! narrow_ptr {
    ($($wide:ty => $narrow:ty),*) => {$(
        impl HostArg<*mut $narrow> for *mut $wide {
            type Keep = Option<(*mut $wide, Vec<$narrow>)>;
            #[allow(clippy::not_unsafe_ptr_arg_deref)]
            fn to_host(self, len: Option<usize>) -> (*mut $narrow, Self::Keep) {
                if self.is_null() {
                    return (std::ptr::null_mut(), None);
                }
                let n = len.unwrap_or(1);
                let mut v: Vec<$narrow> =
                    (0..n).map(|i| unsafe { self.add(i).read_unaligned() } as $narrow).collect();
                (v.as_mut_ptr(), Some((self, v)))
            }
            fn back(keep: Self::Keep) {
                if let Some((p, v)) = keep {
                    for (i, x) in v.into_iter().enumerate() {
                        unsafe { p.add(i).write_unaligned(x as $wide) }
                    }
                }
            }
        }
        impl HostArg<*const $narrow> for *const $wide {
            type Keep = Option<Vec<$narrow>>;
            #[allow(clippy::not_unsafe_ptr_arg_deref)]
            fn to_host(self, len: Option<usize>) -> (*const $narrow, Self::Keep) {
                if self.is_null() {
                    return (std::ptr::null(), None);
                }
                let n = len.unwrap_or(1);
                let v: Vec<$narrow> =
                    (0..n).map(|i| unsafe { self.add(i).read_unaligned() } as $narrow).collect();
                (v.as_ptr(), Some(v))
            }
        }
    )*};
}

narrow_ptr!(i64 => i32, u64 => u32);

/// A C `long double`, in the form the host we're built for keeps it: x87
/// extended precision on x86-64, IEEE quadruple on arm64, both padded to
/// 16 bytes. Where the host makes it a plain `double` (Windows, macOS on
/// arm64) the bindings convert on the way through.
#[repr(C, align(16))]
#[derive(Clone, Copy, Default)]
pub struct LongDouble(pub [u8; 16]);

impl LongDouble {
    pub fn from_f64(v: f64) -> LongDouble {
        #[cfg(target_arch = "x86_64")]
        let bits = f64_to_f80(v);
        #[cfg(not(target_arch = "x86_64"))]
        let bits = f64_to_f128(v);
        LongDouble(bits.to_le_bytes())
    }

    /// The value rounded to the nearest `f64`.
    pub fn to_f64(self) -> f64 {
        let bits = u128::from_le_bytes(self.0);
        #[cfg(target_arch = "x86_64")]
        return f80_to_f64(bits);
        #[cfg(not(target_arch = "x86_64"))]
        return f128_to_f64(bits);
    }
}

impl From<f64> for LongDouble {
    fn from(v: f64) -> LongDouble {
        LongDouble::from_f64(v)
    }
}

impl From<LongDouble> for f64 {
    fn from(v: LongDouble) -> f64 {
        v.to_f64()
    }
}

impl std::fmt::Debug for LongDouble {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_f64())
    }
}

/// A `long double` by value, where the host makes it a plain `double`.
impl HostArg<f64> for LongDouble {
    type Keep = ();
    #[inline]
    fn to_host(self, _: Option<usize>) -> (f64, ()) {
        (self.to_f64(), ())
    }
}

impl HostRet<f64> for LongDouble {
    #[inline]
    fn from_host(w: f64) -> LongDouble {
        LongDouble::from_f64(w)
    }
}

/// A `wchar_t` by value on Windows.
impl HostArg<u16> for i32 {
    type Keep = ();
    #[inline]
    fn to_host(self, _: Option<usize>) -> (u16, ()) {
        (self as u16, ())
    }
}

impl HostRet<u16> for i32 {
    #[inline]
    fn from_host(w: u16) -> i32 {
        w as i32
    }
}

/// A `const wchar_t *` string for a Windows DLL: the caller's UTF-32,
/// null terminated, converted to UTF-16 for the duration of the call.
#[repr(transparent)]
#[derive(Clone, Copy, Debug)]
pub struct WStr16(pub *const u16);

impl HostArg<WStr16> for *const c_int {
    type Keep = Option<Vec<u16>>;
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    fn to_host(self, _: Option<usize>) -> (WStr16, Self::Keep) {
        if self.is_null() {
            return (WStr16(std::ptr::null()), None);
        }
        let mut v = Vec::new();
        let mut i = 0;
        loop {
            let c = unsafe { self.add(i).read_unaligned() } as u32;
            if c == 0 {
                break;
            }
            let mut buf = [0u16; 2];
            match char::from_u32(c) {
                Some(ch) => v.extend_from_slice(ch.encode_utf16(&mut buf)),
                None => v.push(c as u16),
            }
            i += 1;
        }
        v.push(0);
        (WStr16(v.as_ptr()), Some(v))
    }
}

/// A `wchar_t` buffer a Windows DLL fills, `len` units long: it gets a
/// UTF-16 buffer that size, whose contents up to the first null come back
/// as UTF-32, null terminated if there's room. Surrogate pairs become one
/// character, so the result is never longer than what the DLL wrote.
impl HostArg<*mut u16> for *mut c_int {
    type Keep = Option<(*mut c_int, Vec<u16>)>;
    fn to_host(self, len: Option<usize>) -> (*mut u16, Self::Keep) {
        if self.is_null() {
            return (std::ptr::null_mut(), None);
        }
        let mut v = vec![0u16; len.unwrap_or(1)];
        (v.as_mut_ptr(), Some((self, v)))
    }
    fn back(keep: Self::Keep) {
        let Some((p, v)) = keep else { return };
        let end = v.iter().position(|&c| c == 0).unwrap_or(v.len());
        let mut n = 0;
        for c in char::decode_utf16(v[..end].iter().copied()) {
            let c = c.map(|c| c as c_int).unwrap_or(0xfffd);
            unsafe { p.add(n).write_unaligned(c) };
            n += 1;
        }
        if n < v.len() {
            unsafe { p.add(n).write_unaligned(0) };
        }
    }
}

/// Reads `width` bits at bit `offset` of a bitfield unit at `p`, bit 0
/// being the lowest of the first byte, as both compilers number them on
/// little endian machines.
///
/// # Safety
///
/// The bytes covering the field have to be readable.
#[inline]
pub unsafe fn get_bits(p: *const u8, offset: usize, width: u8) -> u64 {
    let mut v = 0u64;
    for i in 0..width as usize {
        let b = offset + i;
        if unsafe { p.add(b / 8).read() } >> (b % 8) & 1 != 0 {
            v |= 1 << i;
        }
    }
    v
}

/// Writes the low `width` bits of `v` to a bitfield, see [`get_bits`].
///
/// # Safety
///
/// The bytes covering the field have to be writable.
#[inline]
pub unsafe fn set_bits(p: *mut u8, offset: usize, width: u8, v: u64) {
    for i in 0..width as usize {
        let b = offset + i;
        let q = unsafe { p.add(b / 8) };
        let m = 1u8 << (b % 8);
        unsafe {
            q.write(if v >> i & 1 != 0 {
                q.read() | m
            } else {
                q.read() & !m
            })
        };
    }
}

/// The x87 extended precision bits of a `long double`, rounded to the
/// nearest `f64`.
pub fn f80_to_f64(bits: u128) -> f64 {
    let mant = bits as u64;
    let exp = (bits >> 64) as u32 & 0x7fff;
    let neg = (bits >> 79) & 1 != 0;
    let sign = if neg { 1u64 << 63 } else { 0 };
    if exp == 0x7fff {
        let frac = mant << 1;
        return f64::from_bits(if frac == 0 {
            sign | 0x7ff0_0000_0000_0000
        } else {
            sign | 0x7ff8_0000_0000_0000 | (frac >> 12)
        });
    }
    if mant == 0 {
        return f64::from_bits(sign);
    }
    // the value is mant * 2^(exp - 16383 - 63), done in f64 arithmetic
    // on a mantissa rounded to 53 bits, scaled in two steps so neither
    // overflows early
    let lead = mant.leading_zeros();
    let m = mant << lead;
    let e = exp as i32 - 16383 - lead as i32;
    let top = m >> 11;
    let rest = m & 0x7ff;
    let mut q = top;
    if rest > 0x400 || (rest == 0x400 && top & 1 != 0) {
        q += 1;
    }
    let mut v = q as f64 * 2f64.powi(-52);
    let mut e = e;
    while e > 1000 {
        v *= 2f64.powi(1000);
        e -= 1000;
    }
    while e < -1000 {
        v *= 2f64.powi(-1000);
        e += 1000;
    }
    v *= 2f64.powi(e);
    if neg { -v } else { v }
}

/// An `f64` as the x87 extended precision bits of a `long double`.
pub fn f64_to_f80(v: f64) -> u128 {
    let b = v.to_bits();
    let sign = ((b >> 63) as u128) << 79;
    let exp = (b >> 52) as u32 & 0x7ff;
    let frac = b & 0x000f_ffff_ffff_ffff;
    if exp == 0x7ff {
        let m = 1u64 << 63 | frac << 11;
        return sign | 0x7fffu128 << 64 | m as u128;
    }
    if exp == 0 {
        if frac == 0 {
            return sign;
        }
        // subnormal, frac * 2^-1074: normalized, extended precision has
        // the range for it
        let lz = frac.leading_zeros();
        let m = frac << lz;
        let e = 16383 + 63 - 1074 - lz as i32;
        return sign | ((e as u128) << 64) | m as u128;
    }
    let e = exp as i32 - 1023 + 16383;
    let m = 1u64 << 63 | frac << 11;
    sign | ((e as u128) << 64) | m as u128
}

/// The IEEE quadruple precision bits of an arm64 `long double`, rounded
/// to the nearest `f64`.
pub fn f128_to_f64(bits: u128) -> f64 {
    let mant = bits & ((1u128 << 112) - 1);
    let exp = (bits >> 112) as u32 & 0x7fff;
    let sign = if bits >> 127 != 0 { 1u64 << 63 } else { 0 };
    if exp == 0x7fff {
        return f64::from_bits(if mant == 0 {
            sign | 0x7ff0_0000_0000_0000
        } else {
            sign | 0x7ff8_0000_0000_0000 | ((mant >> 60) as u64 & 0x0007_ffff_ffff_ffff)
        });
    }
    if exp == 0 {
        // a quad subnormal is far below what an f64 can hold
        return f64::from_bits(sign);
    }
    let e = exp as i32 - 16383;
    if e > 1023 {
        return f64::from_bits(sign | 0x7ff0_0000_0000_0000);
    }
    // round the 112 bit mantissa to 52 bits, or fewer when the result is
    // an f64 subnormal, to nearest even
    let full = mant | 1u128 << 112;
    let shift = if e >= -1022 { 60 } else { 60 + (-1022 - e) };
    if shift > 113 {
        return f64::from_bits(sign);
    }
    let q = full >> shift;
    let rest = full & ((1u128 << shift) - 1);
    let half = 1u128 << (shift - 1);
    let q = if rest > half || (rest == half && q & 1 != 0) {
        q + 1
    } else {
        q
    };
    let q = q as u64;
    if e >= -1022 {
        // a carry out of the mantissa steps the exponent by landing in it
        let biased = (e + 1023) as u64;
        let raw = (biased << 52) + q - (1u64 << 52);
        if raw >> 52 >= 0x7ff {
            return f64::from_bits(sign | 0x7ff0_0000_0000_0000);
        }
        return f64::from_bits(sign | raw);
    }
    f64::from_bits(sign | q)
}

/// An `f64` as the IEEE quadruple precision bits of an arm64 `long double`.
pub fn f64_to_f128(v: f64) -> u128 {
    let b = v.to_bits();
    let sign = ((b >> 63) as u128) << 127;
    let exp = (b >> 52) as u32 & 0x7ff;
    let frac = b & 0x000f_ffff_ffff_ffff;
    if exp == 0x7ff {
        return sign | 0x7fffu128 << 112 | (frac as u128) << 60;
    }
    if exp == 0 {
        if frac == 0 {
            return sign;
        }
        // subnormal, frac * 2^-1074: normalized, with the leading one
        // dropped
        let lz = frac.leading_zeros();
        let m = ((frac as u128) << (lz + 1)) & (u64::MAX as u128);
        let e = 63 - lz as i32 - 1074 + 16383;
        return sign | ((e as u128) << 112) | m << 48;
    }
    let e = exp as i32 - 1023 + 16383;
    sign | ((e as u128) << 112) | (frac as u128) << 60
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLES: [f64; 10] = [
        0.0,
        -0.0,
        1.0,
        -2.5,
        1e-310,
        5e-324,
        3.141592653589793,
        1e308,
        f64::MIN_POSITIVE,
        f64::MAX,
    ];

    #[test]
    fn f80_round_trip() {
        for v in SAMPLES {
            assert_eq!(f80_to_f64(f64_to_f80(v)).to_bits(), v.to_bits(), "{v}");
        }
        assert!(f80_to_f64(f64_to_f80(f64::NAN)).is_nan());
        assert_eq!(f80_to_f64(f64_to_f80(f64::INFINITY)), f64::INFINITY);
        // 1.0 in x87 bits
        assert_eq!(f64_to_f80(1.0), 0x3fff_8000_0000_0000_0000);
    }

    #[test]
    fn f128_round_trip() {
        for v in SAMPLES {
            assert_eq!(f128_to_f64(f64_to_f128(v)).to_bits(), v.to_bits(), "{v}");
        }
        assert!(f128_to_f64(f64_to_f128(f64::NAN)).is_nan());
        assert_eq!(f128_to_f64(f64_to_f128(-f64::INFINITY)), -f64::INFINITY);
        // 1.0 and -2.0 in quad bits
        assert_eq!(f64_to_f128(1.0), 0x3fff_0000_0000_0000_0000_0000_0000_0000);
        assert_eq!(f64_to_f128(-2.0), 0xc000_0000_0000_0000_0000_0000_0000_0000);
        // more precision than an f64 has: 1 + 2^-60 rounds back to 1,
        // 1 + 2^-53 + 2^-60 rounds up to 1 + 2^-52
        let one = 0x3fffu128 << 112;
        assert_eq!(f128_to_f64(one | 1 << 52), 1.0);
        assert_eq!(f128_to_f64(one | 1 << 59 | 1 << 52), 1.0 + 2f64.powi(-52));
        // beyond the f64 range
        assert_eq!(f128_to_f64(0x43ffu128 << 112), f64::INFINITY);
        assert_eq!(f128_to_f64(0x3bffu128 << 112), f64::from_bits(1 << 50));
        assert_eq!(f128_to_f64(0x2fffu128 << 112), 0.0);
    }

    #[test]
    fn long_double() {
        for v in SAMPLES {
            assert_eq!(
                LongDouble::from_f64(v).to_f64().to_bits(),
                v.to_bits(),
                "{v}"
            );
        }
        assert_eq!(LongDouble::default().to_f64(), 0.0);
    }

    #[test]
    fn bits() {
        let mut b = [0u8; 2];
        unsafe {
            set_bits(b.as_mut_ptr(), 3, 5, 0b10110);
            assert_eq!(get_bits(b.as_ptr(), 3, 5), 0b10110);
            set_bits(b.as_mut_ptr(), 6, 4, 0b1111);
            assert_eq!(get_bits(b.as_ptr(), 3, 3), 0b110);
        }
    }

    #[test]
    fn widening() {
        assert_eq!(<i8 as HostArg<i32>>::to_host(-1, None).0, -1);
        assert_eq!(<u8 as HostArg<i32>>::to_host(0xff, None).0, -1);
        assert_eq!(<u8 as HostArg<u32>>::to_host(0xff, None).0, 255);
        assert_eq!(<i16 as HostRet<i32>>::from_host(0x1_2345), 0x2345);
        assert!(<bool as HostRet<u32>>::from_host(2));
    }
}
