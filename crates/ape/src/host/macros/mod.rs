//! The macro_rules macros, one file each. `#[macro_export]` puts them at
//! the crate root, so this module is only where the source lives. The
//! attribute macros live in the ape-macros crate.

mod exports;
mod variadic;
