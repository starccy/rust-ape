//! The host's zlib, through bindings that `cargo xtask host-bindings`
//! generated from bindgen output (src/hostlibs/zlib.rs). zlib calls back into
//! Rust for its memory, as a library with allocator hooks does.
//!
//! Linux, macOS and the BSDs ship zlib; Windows doesn't, and there a
//! zlib1.dll next to the program is used when there is one, otherwise
//! this only checks that the missing library is reported. zlib's `uLong`
//! is 32 bits there, so z_stream is laid out differently: the bindings
//! were made with `cargo xtask host-bindings --windows-input`, and the
//! stream's fields are reached through methods that follow the host.

#![allow(non_camel_case_types, non_snake_case, dead_code)]

#[path = "../hostlibs/zlib.rs"]
mod zlib;

use std::ffi::{CStr, c_int, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};

use zlib::*;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static FREES: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" {
    fn malloc(n: usize) -> *mut c_void;
    fn free(p: *mut c_void);
}

#[ape::host_callback]
fn zalloc(_opaque: *mut c_void, items: uInt, size: uInt) -> *mut c_void {
    ALLOCS.fetch_add(1, Ordering::Relaxed);
    unsafe { malloc(items as usize * size as usize) }
}

#[ape::host_callback]
fn zfree(_opaque: *mut c_void, p: *mut c_void) {
    FREES.fetch_add(1, Ordering::Relaxed);
    unsafe { free(p) }
}

const Z_OK: c_int = 0;
const Z_STREAM_END: c_int = 1;
const Z_FINISH: c_int = 4;

fn main() {
    if let Err(e) = ZLIB.load() {
        assert!(ape::is_windows(), "no zlib on this host: {e}");
        println!("no zlib1.dll on this Windows host: {e}");
        println!("\nhost zlib ok");
        return;
    }
    let version = unsafe { CStr::from_ptr(zlibVersion()) }.to_string_lossy();
    println!("host zlib {version}");

    let text = "It was the best of times, it was the worst of times. ".repeat(40);
    let mut packed = vec![0u8; unsafe { compressBound(text.len() as uLong) } as usize];
    let mut packed_len = packed.len() as uLongf;
    let rc = unsafe {
        compress2(
            packed.as_mut_ptr(),
            &mut packed_len,
            text.as_ptr(),
            text.len() as uLong,
            9,
        )
    };
    assert_eq!(rc, Z_OK);
    let mut back = vec![0u8; text.len()];
    let mut back_len = back.len() as uLongf;
    let rc = unsafe {
        uncompress(
            back.as_mut_ptr(),
            &mut back_len,
            packed.as_ptr(),
            packed_len,
        )
    };
    assert_eq!(rc, Z_OK);
    assert_eq!(&back[..back_len as usize], text.as_bytes());
    println!(
        "compress2/uncompress: {} -> {} -> {} bytes",
        text.len(),
        packed_len,
        back_len
    );

    let crc = unsafe { crc32(0, text.as_ptr(), text.len() as uInt) };
    println!("crc32 = {crc:#010x}");

    // a stream whose memory comes from our callbacks
    let mut strm = z_stream::default();
    strm.set_zalloc(zalloc::ptr());
    strm.set_zfree(zfree::ptr());
    let size = z_stream::host_size() as c_int;
    assert_eq!(
        unsafe { deflateInit_(&mut strm, 6, zlibVersion(), size) },
        Z_OK
    );
    let mut out = vec![0u8; 4096];
    strm.set_next_in(text.as_ptr() as *mut u8);
    strm.set_avail_in(text.len() as uInt);
    strm.set_next_out(out.as_mut_ptr());
    strm.set_avail_out(out.len() as uInt);
    assert_eq!(unsafe { deflate(&mut strm, Z_FINISH) }, Z_STREAM_END);
    let n = strm.total_out() as usize;
    assert_eq!(strm.total_in() as usize, text.len());
    unsafe { deflateEnd(&mut strm) };

    let mut strm = z_stream::default();
    strm.set_zalloc(zalloc::ptr());
    strm.set_zfree(zfree::ptr());
    assert_eq!(
        unsafe { inflateInit_(&mut strm, zlibVersion(), size) },
        Z_OK
    );
    let mut round = vec![0u8; text.len()];
    strm.set_next_in(out.as_mut_ptr());
    strm.set_avail_in(n as uInt);
    strm.set_next_out(round.as_mut_ptr());
    strm.set_avail_out(round.len() as uInt);
    assert_eq!(unsafe { inflate(&mut strm, Z_FINISH) }, Z_STREAM_END);
    unsafe { inflateEnd(&mut strm) };
    assert_eq!(round, text.as_bytes());
    let (a, f) = (
        ALLOCS.load(Ordering::Relaxed),
        FREES.load(Ordering::Relaxed),
    );
    println!("deflate/inflate stream through our zalloc: {a} allocations, {f} frees");
    assert!(a > 0);
    assert_eq!(a, f);

    println!("\nhost zlib ok");
}
