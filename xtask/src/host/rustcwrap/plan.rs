//! What the rewrite has decided to do to a crate: the edits, the
//! libraries it needs, and the features the helper module then takes.

use std::collections::BTreeMap;

use crate::host::link;

pub(super) struct Plan {
    /// (file, start, end, replacement), applied back to front
    pub(super) edits: Vec<(usize, usize, usize, String)>,
    /// per library, the symbol of its HostLinkLib
    pub(super) libs: BTreeMap<String, String>,
    /// libraries with no copy on the build machine, a DLL a `link!` names
    /// or a framework: the helper module defines their HostLinkLib
    pub(super) explicit: BTreeMap<String, Explicit>,
    /// whether a variadic function became a callable static, which takes
    /// two unstable features
    pub(super) variadic: bool,
    /// whether a wrapper is exported weakly, which takes another
    pub(super) linkage: bool,
    /// whether the OsStr conversions of std::os::windows were rewritten,
    /// which puts their replacements, which need std, in the helper
    pub(super) os_ext: bool,
    /// whether a struct field became a HostCallback, which the helper
    /// then defines
    pub(super) callbacks: bool,
}

// a module of the crate: a file, the inline modules down to it, and its
// path from the crate root
pub(super) struct Scope {
    pub(super) fi: usize,
    pub(super) inline: Vec<usize>,
    pub(super) modpath: Vec<String>,
}

pub(super) struct Explicit {
    pub(super) sym: String,
    /// file names to try on linux, macos, windows
    pub(super) names: [Vec<String>; 3],
}

impl Explicit {
    pub(super) fn dll(dll: &str) -> Explicit {
        let id: String = dll
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        // tried on every host, so a call on the wrong one names the DLL
        let names = vec![dll.to_string()];
        Explicit {
            sym: format!("__dll_{id}"),
            names: [names.clone(), names.clone(), names],
        }
    }

    pub(super) fn framework(name: &str) -> Explicit {
        let id: String = name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let path = format!("/System/Library/Frameworks/{name}.framework/{name}");
        let mut names = [vec![path.clone()], vec![path.clone()], vec![path]];
        // the project's metadata may say where else to look
        if let Ok(config) = std::env::var("RUST_APE_HOST_LIBS")
            && let Ok(config) = serde_json::from_str::<serde_json::Value>(&config)
        {
            for (i, os) in ["linux", "macos", "windows"].iter().enumerate() {
                if let Some(list) = link::host_names(&config, name, os) {
                    names[i] = list;
                }
            }
        }
        Explicit {
            sym: format!("__framework_{id}"),
            names,
        }
    }
}
