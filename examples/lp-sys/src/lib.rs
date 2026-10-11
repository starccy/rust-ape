//! The test library host_layout loads, declared the way a -sys crate
//! declares its library, so calls go through the rewrite `cargo xtask
//! build` makes of such crates (host_layout's other bindings are made with
//! `ape::host_library!`). The names come from data/layout-test/lp.h.

use std::ffi::{c_char, c_int, c_long, c_schar, c_short, c_uchar, c_ulong};

pub type wchar_t = c_int;
pub type lp_lfn = Option<unsafe extern "C" fn(a: c_long, b: c_ulong, c: wchar_t) -> c_long>;
#[repr(C)]
pub struct lp_hooks {
    pub f: lp_lfn,
}

#[link(name = "lp")]
unsafe extern "C" {
    pub fn lp_apply_long(f: lp_lfn) -> c_long;
    pub fn lp_call_hooks(h: *const lp_hooks) -> c_long;
    pub fn lp_wchar_ret(c: c_int) -> wchar_t;
    pub fn lp_widen(c: c_uchar, s: c_schar, h: c_short, b: bool, k: c_char) -> c_int;
    /// `double lp_mix(int, float, long long, double, int, float, int,
    /// double)`, declared without its parameters and called through a
    /// pointer with them, the way objc2 calls `objc_msgSend`
    pub fn lp_mix();
}
