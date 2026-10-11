//! `host_exports!`, see its documentation.

/// Declares functions and variables that host libraries find by name,
/// such as the API a plugin expects from its host.
///
/// ```ignore
/// ape::host_exports! {
///     pub static API = "libhostapi.so", windows "hostapi.dll";
///     fn host_add(a: c_int, b: c_int) -> c_int { a + b }
///     fn host_log(s: *const c_char) { /* ... */ }
///     fn host_logf(level: c_int, fmt: *const c_char, mut args: ...) {
///         let n = unsafe { args.next_arg::<c_int>() };
///         /* ... */
///     }
///     static host_verbose: c_int = 0;
/// }
/// API.export().unwrap();
/// let plugin = ape::host::Library::open("./plugin.so").unwrap();
/// if unsafe { host_verbose.get() } != 0 { /* the plugin set it */ }
/// ```
///
/// Each function is also an ordinary `extern "C" fn` in Rust, the
/// variadic one (its last parameter `mut name: ...`, a
/// [`core::ffi::VaList`] inside) with up to 16 fixed parameters on
/// Windows and 8 on macOS. A `static` becomes a
/// [`Shared`](crate::host::Shared), one variable the library and this
/// program read and write. On Windows the library name has to be the DLL
/// name the plugin imports from.
#[macro_export]
macro_rules! host_exports {
    (
        $(#[$lm:meta])*
        $lv:vis static $api:ident = $unix:literal, windows $windows:literal;
        $($items:tt)*
    ) => {
        $crate::host_exports!(@items [$(#[$lm])* $lv static $api = $unix, windows $windows] [] $($items)*);
    };

    (@items [$(#[$lm:meta])* $lv:vis static $api:ident = $unix:literal, windows $windows:literal] [$($list:expr),*]) => {
        $(#[$lm])*
        $lv static $api: $crate::host::Exports = $crate::host::Exports::new($unix, $windows, || {
            vec![$($list),*]
        });
    };

    (@items [$($head:tt)*] [$($list:expr),*]
        $(#[$m:meta])* fn $name:ident($($params:tt)*) $(-> $ret:ty)? $body:block
        $($rest:tt)*
    ) => {
        $crate::host_exports!(@fn [$($head)*] [$($list),*] [$(#[$m])*] $name [$($ret)?] $body { $($rest)* } [] $($params)*);
    };

    // the parameters, one at a time, until the variadic one or the end
    (@fn [$($head:tt)*] [$($list:expr),*] [$(#[$m:meta])*] $name:ident [$($ret:ty)?] $body:block { $($rest:tt)* }
        [$($acc:tt)*] mut $va:ident : ... $(,)?
    ) => {
        $crate::host_exports!(@variadic [$($head)*] [$($list),*] [$(#[$m])*] $name [$($ret)?] $body { $($rest)* } [$($acc)*] $va);
    };

    (@fn [$($head:tt)*] [$($list:expr),*] [$(#[$m:meta])*] $name:ident [$($ret:ty)?] $body:block { $($rest:tt)* }
        [$($acc:tt)*] $arg:ident : $ty:ty , $($params:tt)*
    ) => {
        $crate::host_exports!(@fn [$($head)*] [$($list),*] [$(#[$m])*] $name [$($ret)?] $body { $($rest)* } [$($acc)* $arg : $ty ,] $($params)*);
    };

    (@fn [$($head:tt)*] [$($list:expr),*] [$(#[$m:meta])*] $name:ident [$($ret:ty)?] $body:block { $($rest:tt)* }
        [$($acc:tt)*] $arg:ident : $ty:ty $(,)?
    ) => {
        $crate::host_exports!(@fixed [$($head)*] [$($list),*] [$(#[$m])*] $name [$($ret)?] $body { $($rest)* } [$($acc)* $arg : $ty ,]);
    };

    (@fn [$($head:tt)*] [$($list:expr),*] [$(#[$m:meta])*] $name:ident [$($ret:ty)?] $body:block { $($rest:tt)* }
        [$($acc:tt)*]
    ) => {
        $crate::host_exports!(@fixed [$($head)*] [$($list),*] [$(#[$m])*] $name [$($ret)?] $body { $($rest)* } [$($acc)*]);
    };

    (@fixed [$($head:tt)*] [$($list:expr),*] [$(#[$m:meta])*] $name:ident [$($ret:ty)?] $body:block { $($rest:tt)* }
        [$($arg:ident : $ty:ty ,)*]
    ) => {
        #[$crate::host_callback]
        $(#[$m])* pub fn $name($($arg: $ty),*) $(-> $ret)? $body
        $crate::host_exports!(@items [$($head)*] [$($list,)* $crate::host::ExportItem::Function(stringify!($name), $name::ptr())]
            $($rest)*);
    };

    (@variadic [$($head:tt)*] [$($list:expr),*] [$(#[$m:meta])*] $name:ident [$($ret:ty)?] $body:block { $($rest:tt)* }
        [$($arg:ident : $ty:ty ,)*] $va:ident
    ) => {
        $(#[$m])*
        #[allow(non_snake_case, clippy::missing_safety_doc)]
        pub unsafe extern "C" fn $name($($arg: $ty,)* mut $va: ...) $(-> $ret)? {
            $name::body($($arg,)* $va)
        }
        #[allow(non_snake_case)]
        pub mod $name {
            #[allow(unused_imports)]
            use super::*;
            pub const FIXED: usize = (&[$(stringify!($arg)),*] as &[&str]).len();
            pub unsafe extern "C" fn va($($arg: $ty,)* $va: ::core::ffi::VaList<'_>) $(-> $ret)? {
                body($($arg,)* $va)
            }
            #[allow(unused_mut)]
            pub(super) fn body($($arg: $ty,)* mut $va: ::core::ffi::VaList<'_>) $(-> $ret)? {
                $crate::host::after_host_call();
                $body
            }
        }
        $crate::host_exports!(@items [$($head)*] [$($list,)* $crate::host::ExportItem::Variadic(
            stringify!($name), $name::FIXED,
            $name as unsafe extern "C" fn($($ty,)* ...) $(-> $ret)? as *const ::core::ffi::c_void,
            $name::va as unsafe extern "C" fn($($ty,)* ::core::ffi::VaList<'_>) $(-> $ret)? as *const ::core::ffi::c_void)]
            $($rest)*);
    };

    (@items [$($head:tt)*] [$($list:expr),*]
        $(#[$m:meta])* static $name:ident : $ty:ty = $init:expr;
        $($rest:tt)*
    ) => {
        $(#[$m])*
        #[allow(non_upper_case_globals)]
        pub static $name: $crate::host::Shared<$ty> = $crate::host::Shared::new($init);
        $crate::host_exports!(@items [$($head)*] [$($list,)* $crate::host::ExportItem::Variable(
            stringify!($name), $name.as_ptr() as *mut ::core::ffi::c_void, ::core::mem::size_of::<$ty>())]
            $($rest)*);
    };
}
