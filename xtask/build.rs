use std::fs;
use std::path::{Path, PathBuf};

// Hashes of the code whose output cargo and the stub cache can't see change:
// REWRITE_HASH for what rustcwrap writes, LINKGEN_HASH for the link stubs.
fn main() {
    let rewrite = hash(&["src/host/rustcwrap", "src/host/link"]);
    let linkgen = hash(&["src/host/link"]);
    println!("cargo:rustc-env=REWRITE_HASH={rewrite:016x}");
    println!("cargo:rustc-env=LINKGEN_HASH={linkgen:016x}");
}

fn hash(dirs: &[&str]) -> u64 {
    let mut files = Vec::new();
    for d in dirs {
        println!("cargo:rerun-if-changed={d}");
        collect(Path::new(d), &mut files);
    }
    files.sort();
    let mut h: u64 = 0xcbf29ce484222325;
    for f in files {
        let data = fs::read(&f).unwrap();
        for b in f.to_string_lossy().bytes().chain([0]).chain(data) {
            h = (h ^ b as u64).wrapping_mul(0x100000001b3);
        }
    }
    h
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            collect(&p, out);
        } else {
            out.push(p);
        }
    }
}
