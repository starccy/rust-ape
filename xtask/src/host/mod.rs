//! Calling C libraries that only the host has: the stubs the linker takes
//! for them (link), the rustc wrapper that rewrites a crate's declarations
//! (rustcwrap), and the generator that turns bindgen output into
//! `#[ape::host_library]` declarations (bind) with the per-host data model
//! worked out (layout).

pub mod bind;
pub mod layout;
pub mod link;
pub mod rustcwrap;
