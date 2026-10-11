//! `host_variadic!`, see its documentation.

/// Calls a variadic host function declared in
/// [`host_library`](crate::host_library), with the convention of the host
/// we're on: `unsafe { host_variadic!(printf(c"%d\n".as_ptr(), 42)) }`.
/// It takes `unsafe`, as calling the function would.
///
/// Pass the variadic arguments already promoted the way C does it, so `f64`
/// rather than `f32` and `c_int` rather than a `u8`; an integer literal is
/// a `c_int`. On Apple Silicon these go on the stack rather than in
/// registers, which is handled here.
#[macro_export]
macro_rules! host_variadic {
    ($name:ident($($arg:expr),* $(,)?)) => {{
        #[cfg(target_arch = "x86_64")]
        let r = if $crate::is_windows() {
            $name::win64()($($arg),*)
        } else {
            $name::sysv()($($arg),*)
        };
        #[cfg(target_arch = "aarch64")]
        let r = if $crate::is_xnu() {
            $name::xnu(&[$($crate::host::VarArg::classify($arg)),*])
        } else {
            $name::sysv()($($arg),*)
        };
        $crate::host::after_host_call();
        r
    }};
}
