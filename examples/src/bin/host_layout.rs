//! A host library whose structures and functions use `long`, `wchar_t`,
//! `long double`, bitfields and small integers, which Windows and macOS
//! lay out and pass differently. The bindings in src/hostlibs/layout.rs
//! were made by `cargo xtask host-bindings --windows-input --macos-input`,
//! so the same calls and the same field accessors work on every host;
//! data/layout-test has the library, which lp-sys/build.rs builds.

#![allow(non_camel_case_types, non_snake_case, dead_code)]

#[path = "../hostlibs/layout.rs"]
mod layout;

use std::ffi::{CStr, c_char, c_int, c_long, c_schar, c_short, c_uchar, c_ulong, c_void};
use std::hint::black_box;

use ape::host::layout::LongDouble;
use layout::*;

// what the generator said to put here for lp_fn
#[ape::host_callback(sysv = fn(c_long) -> c_long, xnu = fn(c_long) -> c_long, win64 = fn(i32) -> i32)]
fn twice(v: c_long) -> c_long {
    v * 2
}

// and for lp_cfn: a small integer comes back widened, as Apple's
// arm64 has the caller read the whole register
#[ape::host_callback(sysv = fn(i32) -> i32, xnu = fn(i32) -> i32, win64 = fn(c_short) -> i8)]
fn small(v: c_short) -> i8 {
    (v as i32 - 300) as i8
}

// lp_fn9: twelve arguments, more than the registers hold, the last
// ones narrow: Apple's arm64 packs those on the stack at their own
// sizes when it calls us, where our code expects 8 byte slots
#[ape::host_callback(sysv = fn(c_int, c_int, c_int, c_int, c_int, c_int, c_int, c_int, i32, i32, u32, f32) -> c_long,
                     xnu = fn(c_int, c_int, c_int, c_int, c_int, c_int, c_int, c_int, i32, i32, u32, f32) -> c_long,
                     win64 = fn(c_int, c_int, c_int, c_int, c_int, c_int, c_int, c_int, c_short, c_char, bool, f32) -> i32)]
fn nine(
    a: c_int,
    b: c_int,
    c: c_int,
    d: c_int,
    e: c_int,
    f: c_int,
    g: c_int,
    h: c_int,
    i: c_short,
    j: c_char,
    k: bool,
    x: f32,
) -> c_long {
    (a + 2 * b + 3 * c + 4 * d + 5 * e + 6 * f + 7 * g + 8 * h) as c_long
        + i as c_long * 100
        + j as c_long * 10_000
        + k as c_long * 1_000_000
        + (x * 2.0) as c_long * 100_000_000
}

// lp_ldfn: a long double by value, which no Rust function can take on
// Linux, so the host gets a libffi closure
#[ape::host_callback(ffi, sysv = fn(c_int, LongDouble) -> LongDouble, xnu = fn(c_int, f64) -> f64, win64 = fn(c_int, f64) -> f64)]
fn ld_scale(k: c_int, x: LongDouble) -> LongDouble {
    LongDouble::from_f64(x.to_f64() * k as f64)
}

// lp_lfn: a long, an unsigned long and a wchar_t, which Windows passes
// in 32, 32 and 16 bits with whatever above
#[ape::host_callback(sysv = fn(c_long, c_ulong, wchar_t) -> c_long, xnu = fn(c_long, c_ulong, wchar_t) -> c_long, win64 = fn(i32, u32, u16) -> i32)]
fn longs(a: c_long, b: c_ulong, c: wchar_t) -> c_long {
    a + b as c_long * 10 + c as c_long
}

// lp_vfn: the library's own variadic arguments come back as a va_list
// in the host's layout; `fmt` says what each one is, and the values
// are written to `ctx` as text
#[ape::host_callback]
fn vread(ctx: *mut c_void, fmt: *const c_char, ap: *mut c_void) {
    let out = unsafe { &mut *(ctx as *mut String) };
    let mut list = unsafe { ape::host::VaList::from_host(ap) };
    for k in unsafe { CStr::from_ptr(fmt) }.to_bytes() {
        let piece = match k {
            b'd' => unsafe { list.arg::<i32>() }.to_string(),
            b'l' => unsafe { list.arg::<i64>() }.to_string(),
            b'f' => unsafe { list.arg::<f64>() }.to_string(),
            b's' => unsafe { CStr::from_ptr(list.arg::<*const c_char>()) }
                .to_string_lossy()
                .into_owned(),
            _ => unreachable!(),
        };
        out.push_str(&piece);
        out.push(' ');
    }
}

type Widen = unsafe extern "C" fn(c_uchar, c_schar, c_short, bool, c_char) -> c_int;

// what lp-sys's lp_lfn takes, as a plain function
unsafe extern "C" fn longs_raw(a: c_long, b: c_ulong, c: lp_sys::wchar_t) -> c_long {
    a + b as c_long * 10 + c as c_long
}

// lp_widen(200, -3, -300, true, -5) with every argument register holding
// something above the argument's width
#[cfg(target_arch = "x86_64")]
unsafe fn dirty_widen(f: Widen) -> c_int {
    let r: u64;
    unsafe {
        std::arch::asm!(
            "call {f}",
            f = in(reg) f as usize,
            in("rdi") 0xdead_beef_dead_bec8u64,
            in("rsi") 0x0000_0000_0000_00fdu64,
            in("rdx") 0x0000_0000_0000_fed4u64,
            in("rcx") 0xffff_ffff_ffff_ff01u64,
            in("r8") 0x0000_0000_0000_00fbu64,
            out("rax") r,
            clobber_abi("C"),
        );
    }
    r as c_int
}

#[cfg(target_arch = "aarch64")]
unsafe fn dirty_widen(f: Widen) -> c_int {
    let r: u64;
    unsafe {
        std::arch::asm!(
            "blr {f}",
            f = in(reg) f as usize,
            inout("x0") 0xdead_beef_dead_bec8u64 => r,
            in("x1") 0x0000_0000_0000_00fdu64,
            in("x2") 0x0000_0000_0000_fed4u64,
            in("x3") 0xffff_ffff_ffff_ff01u64,
            in("x4") 0x0000_0000_0000_00fbu64,
            clobber_abi("C"),
        );
    }
    r as c_int
}

fn main() {
    // lp-sys/build.rs builds one per host with clang, and leaves out a host
    // it couldn't build for
    let lib = if ape::is_windows() {
        "lp.dll".to_string()
    } else if ape::is_xnu() {
        "liblp.dylib".to_string()
    } else {
        format!("liblp-{}.so", std::env::consts::ARCH)
    };
    if !std::path::Path::new("/usr/lib/rust-ape-layout-test")
        .join(&lib)
        .exists()
    {
        println!("no {lib} in the bundle, the build couldn't make one; skipping");
        return;
    }
    if let Err(e) = LP.load() {
        panic!("the test library didn't load: {e}");
    }

    assert_eq!(unsafe { lp_neg(123_456) }, -123_456);
    assert_eq!(unsafe { lp_neg(-7) }, 7);
    println!("long by value and returned: ok");

    // garbage in the upper half, which the library must not leave there
    let mut v: c_long = 0x7777_7777_7777_7777;
    unsafe { lp_out(&mut v) };
    assert_eq!(v, -123_456);
    println!("long written through a pointer: {v}");

    // arrays of them, sized by another parameter (--len)
    let arr: [std::ffi::c_ulong; 4] = [1, 20, 300, 4000];
    assert_eq!(unsafe { lp_sum(arr.as_ptr(), 4) }, 4321);
    let mut arr: [c_long; 3] = [1, -2, 3];
    unsafe { lp_scale(arr.as_mut_ptr(), 3, -10) };
    assert_eq!(arr, [-10, 20, -30]);
    println!("arrays of long, both ways: ok");

    // a callback the library calls with a long
    assert_eq!(unsafe { lp_apply(twice::ptr(), 21) }, 43);
    println!("callback taking and returning long: ok");

    // small integers cut from wider values, whose upper bits are whatever
    // was there: the library has to see 0x34, -128, 0x1234, true and 0xff,
    // the last a `char`, which is signed on x86-64 and to Apple
    let c = black_box(0x1234u32) as u8;
    let s = black_box(0x1_2380i32) as i8;
    let h = black_box(0x7_1234i32) as i16;
    let k = black_box(0x5_00ffi32) as u8 as std::ffi::c_char;
    let r = unsafe { lp_widen(c, s, h, true, k) };
    let k = if cfg!(target_arch = "x86_64") || ape::is_xnu() {
        -1
    } else {
        255
    };
    assert_eq!(r, 0x34 - 128 * 1000 + 0x1234 * 100000 + 7 + k * 1_000_000);
    println!("small integers widened for the callee: {r}");

    // and one a callback returns, read by the library as an int
    assert_eq!(
        unsafe { lp_capply(small::ptr(), 100) },
        (100 - 300i32) as i8 as i32
    );
    println!("small integer returned by a callback: ok");

    // a wchar_t buffer the library fills; 😀 is two UTF-16 units on
    // Windows, so the count it returns differs, the string doesn't
    let mut buf = [0i32; 16];
    let n = unsafe { lp_wname(buf.as_mut_ptr(), buf.len()) };
    let s: String = buf
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| char::from_u32(c as u32).unwrap())
        .collect();
    assert_eq!(s, "中😀!");
    assert_eq!(n, if ape::is_windows() { 4 } else { 3 });
    println!("wchar_t buffer filled by the library: {s}");

    let w: Vec<i32> = "中文 wide".chars().map(|c| c as i32).chain([0]).collect();
    assert_eq!(unsafe { lp_wlen(w.as_ptr()) }, 7);
    println!("wchar_t string: ok");

    // a long double by value: x87 on Linux x86-64, a quad on Linux arm64,
    // a double on Windows and macOS. The call goes through libffi
    let r = unsafe { lp_half(LongDouble::from_f64(5.0)) }.to_f64();
    assert_eq!(r, 2.5);
    println!("long double by value: 5 / 2 = {r}");

    // every field filled by the library, read here
    let mut o = outer::default();
    let size = unsafe { lp_fill(&mut o) };
    assert_eq!(size as usize, outer::host_size());
    assert_eq!(o.tag(), 7);
    // a nested structure that also follows the host, reached through At
    let inner = o.in_();
    assert_eq!((inner.a(), inner.b()), (-5, 9));
    assert_eq!((o.arr(0), o.arr(1), o.arr(2)), (100, -200, 300));
    assert_eq!(o.wc(), 0x4e2d);
    assert_eq!((o.flags(), o.mode()), (5, 17));
    assert_eq!(
        unsafe { CStr::from_ptr(o.name()) }.to_str().unwrap(),
        "outer"
    );
    assert_eq!(o.link(), o.in__ptr());
    assert_eq!(At(o.link()).b(), 9);
    println!("structure filled by the library, {size} bytes here: every field ok");

    // and set here, read by the library
    let mut o = outer::default();
    o.set_tag(1);
    o.in_().set_a(2);
    o.in_().set_b(3);
    o.set_arr(0, 4);
    o.set_arr(1, 5);
    o.set_arr(2, 6);
    o.set_wc(7);
    o.set_flags(3);
    o.set_mode(9);
    let want = 1 + 2 * 10 + 3 * 100 + 4 + 5 + 6 + 7 + 3 * 1000 + 9 * 10000;
    assert_eq!(unsafe { lp_check(&o) }, want);
    println!("structure set here, checked by the library: {want}");

    // a structure laid out differently, passed and returned by value,
    // which libffi classifies for the host
    let mut i = inner::default();
    i.set_a(21);
    i.set_b(4);
    let r = unsafe { lp_inner_val(i) };
    assert_eq!((r.a(), r.b()), (42, 8));
    assert_eq!(unsafe { lp_inner_sum(i, 5) }, 30);
    println!("structure by value through libffi: ok");

    // one with a long double, 8 bytes on macOS and 16 elsewhere
    let mut l = ld::default();
    let r = unsafe { lp_ld_fill(&mut l) }.to_f64();
    assert_eq!((l.i(), l.x().to_f64(), r), (3, 1.25, 2.5));
    l.set_x(LongDouble::from_f64(-0.5));
    assert_eq!(l.x().to_f64(), -0.5);
    println!(
        "structure holding a long double, {} bytes here: ok",
        ld::host_size()
    );

    // the same on every host, left a plain struct
    let mut p = plain {
        x: 0,
        y: 0.0,
        p: std::ptr::null_mut(),
    };
    assert_eq!(unsafe { lp_plain(&mut p) } as usize, size_of::<plain>());
    assert_eq!((p.x, p.y), (1, 2.5));
    let q = unsafe { lp_plain_val(p) };
    assert_eq!((q.x, q.y), (2, 5.0));
    println!("plain structure, by pointer and by value: ok");

    // a function pointer the library could have handed back at runtime,
    // typed here and called through ape::host::thunk: integers and floats
    // interleaved, more than the four register positions Windows has
    let lib = LP.load().unwrap();
    type Mix = unsafe extern "C" fn(i32, f32, i64, f64, i32, f32, i32, f64) -> f64;
    let mix: Mix = lib.typed("lp_mix").unwrap();
    let want = 1.0
        + 2.0 * 10.0
        + 3.0 * 100.0
        + 4.0 * 1000.0
        + 5.0 * 10000.0
        + 6.0 * 100000.0
        + 7.0 * 1000000.0
        + 8.0 * 10000000.0;
    assert_eq!(unsafe { mix(1, 2.0, 3, 4.0, 5, 6.0, 7, 8.0) }, want);
    assert_eq!(unsafe { lp_mix(1, 2.0, 3, 4.0, 5, 6.0, 7, 8.0) }, want);
    let mixf: unsafe extern "C" fn(f32, i32, f32) -> f32 = lib.typed("lp_mixf").unwrap();
    assert_eq!(unsafe { mixf(1.0, 2, 3.0) }, 123.0);
    println!("function pointers typed and called through thunks: {want}");

    // a callback with twelve arguments, called by the library
    let r = unsafe { lp_apply9(nine::ptr()) };
    assert_eq!(r, 204 - 900 + 100_000 + 1_000_000 + 500_000_000);
    println!("callback with twelve arguments, the last ones narrow: {r}");

    // a callback taking and returning a long double by value
    let r = unsafe { lp_apply_ld(ld_scale::ptr(), LongDouble::from_f64(1.5)) }.to_f64();
    assert_eq!(r, 5.5);
    println!("callback taking a long double by value, through libffi: {r}");

    // unions by value: one with a long, which libffi is told about as a
    // structure the convention places the same way, inside a structure
    // too and returned; and two the same everywhere, left as Rust unions
    let mut v = u::default();
    v.set_l(-42);
    assert_eq!(unsafe { lp_union_l(v) }, -42);
    v.set_d(1.5);
    assert_eq!(unsafe { lp_union_d(v, 4.0) }, 6.0);
    let mut h = hu::default();
    h.set_tag(3);
    h.v().set_l(21);
    assert_eq!(unsafe { lp_hu(h) }, 3021);
    assert_eq!(unsafe { lp_union_ret(21) }.l(), 42);
    assert_eq!(unsafe { lp_union_f(uf { b: [1.5, 2.0] }) }, 21.5);
    assert_eq!(unsafe { lp_union_m(um { i: 77 }) }, 77);
    println!("unions by value, through libffi and plain: ok");

    // a callback taking a long, an unsigned long and a wchar_t, which the
    // library calls with the registers dirty above those widths on
    // Windows; given straight and in a structure. And a wchar_t it
    // returns the same way
    assert_eq!(unsafe { lp_apply_long(longs::ptr()) }, 187);
    let hooks = lp_hooks { f: longs::ptr() };
    assert_eq!(unsafe { lp_call_hooks(&hooks) }, 187);
    assert_eq!(unsafe { lp_wchar_ret(0x7a) }, 0x7a);
    println!("long, unsigned long and wchar_t to a callback, registers dirty: ok");

    // eleven arguments, the last three narrow and past the registers:
    // Apple's arm64 packs them on the stack at their own sizes, where the
    // standard convention gives each an 8 byte slot
    let ten = unsafe { lp_ten(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, true) };
    assert_eq!(ten, 204 + 900 + 100_000 + 1_000_000);
    println!("eleven arguments, three narrow ones on the stack: {ten}");

    // the same library declared by lp-sys the way a -sys crate does, so
    // these calls go through the rewrite `cargo xtask build` makes of such
    // crates. The narrow arguments arrive with the registers dirty above
    // their width, as a caller in the standard convention may leave them,
    // and the wrapper has to clean them since on Apple's arm64 the callee
    // takes them as 32-bit values
    let widen = unsafe { dirty_widen(lp_sys::lp_widen) };
    // the library's `char` is unsigned on aarch64 Linux, signed elsewhere
    let k: i32 = if ape::is_xnu() {
        -5
    } else {
        0xfbu8 as c_char as i32
    };
    assert_eq!(widen, 200 - 3 * 1000 - 300 * 100_000 + 7 + k * 1_000_000);
    println!("narrow arguments through a -sys crate's declaration, registers dirty: {widen}");
    // the same long-taking callback through the -sys crate's declarations,
    // where the rewrite marks the parameters a DLL passes narrower so the
    // copy it hands the library widens them: as a parameter, and in a
    // struct field typed HostCallback
    assert_eq!(unsafe { lp_sys::lp_apply_long(Some(longs_raw)) }, 187);
    let hooks = lp_sys::lp_hooks {
        f: Some(lp_sys::HostCallback::new(longs_raw)),
    };
    assert_eq!(unsafe { lp_sys::lp_call_hooks(&hooks) }, 187);
    assert_eq!(unsafe { lp_sys::lp_wchar_ret(0x7a) }, 0x7a);
    println!("long, unsigned long and wchar_t through a -sys crate's callback: ok");

    // a declaration without parameters called through a pointer with its
    // real signature, registers and all, the first time through the lazy
    // binding; on Windows that pointer would have to be a win64 one
    if !ape::is_windows() {
        type Mix = unsafe extern "C" fn(c_int, f32, i64, f64, c_int, f32, c_int, f64) -> f64;
        let mix: Mix = unsafe { std::mem::transmute(lp_sys::lp_mix as unsafe extern "C" fn()) };
        assert_eq!(unsafe { mix(1, 2.0, 3, 4.0, 5, 6.0, 7, 8.0) }, want);
        assert_eq!(unsafe { mix(1, 2.0, 3, 4.0, 5, 6.0, 7, 8.0) }, want);
        println!("a parameterless declaration called through a typed pointer: {want}");
    }

    // a va_list the library built from its own variadic arguments, read
    // back with the host's layout: more integers and doubles than any
    // host passes in registers, so the overflow and stack parts are
    // read too, and a string among them
    let mut text = String::new();
    unsafe {
        ape::host_variadic!(lp_vcall(
            vread::ptr(),
            &mut text as *mut String as *mut c_void,
            c"dfdfdfdfdfdfdfdfdfdfsl".as_ptr(),
            1 as c_int,
            1.5f64,
            2 as c_int,
            2.5f64,
            3 as c_int,
            3.5f64,
            4 as c_int,
            4.5f64,
            5 as c_int,
            5.5f64,
            6 as c_int,
            6.5f64,
            7 as c_int,
            7.5f64,
            8 as c_int,
            8.5f64,
            9 as c_int,
            9.5f64,
            10 as c_int,
            10.5f64,
            c"ten".as_ptr(),
            1234567890123i64
        ))
    };
    assert_eq!(
        text,
        "1 1.5 2 2.5 3 3.5 4 4.5 5 5.5 6 6.5 7 7.5 8 8.5 9 9.5 10 10.5 ten 1234567890123 "
    );
    println!(
        "a va_list from the host read on this host: {}",
        text.trim_end()
    );

    println!("\nhost layout ok");
}
