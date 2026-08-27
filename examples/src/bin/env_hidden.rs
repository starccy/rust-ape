//! The environment, as a Linux-coded program is entitled to read it.
//!
//! NT stores its per-drive working directories in the environment block under
//! names like "=C:". POSIX splits an entry at the first '=', so those have no
//! name at all, and code that re-encodes the environment it was handed --
//! anything building a child process env, most of all -- treats them as
//! corruption rather than as a platform quirk. shim/environ.c drops them at
//! startup; this checks that they are gone and that ordinary variables, the
//! ones that pay for that filtering, still survive both directions.

use std::collections::HashMap;
use std::process::Command;

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--child") {
        println!("{}", std::env::var("APE_SHIM_CHILD").expect("APE_SHIM_CHILD"));
        return;
    }

    let hidden: Vec<String> = std::env::vars_os()
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .filter(|k| k.is_empty() || k.contains('='))
        .collect();
    assert!(hidden.is_empty(), "environment exposes unnameable entries: {hidden:?}");
    println!("no unnameable entries ok");

    let path = std::env::var_os("PATH").expect("PATH");
    assert!(!path.is_empty(), "PATH is empty");
    println!("PATH survived ok");

    unsafe { std::env::set_var("APE_SHIM_ENV_TEST", "exist") };
    assert_eq!(std::env::var("APE_SHIM_ENV_TEST").as_deref(), Ok("exist"));
    assert!(
        std::env::vars().any(|(k, v)| k == "APE_SHIM_ENV_TEST" && v == "exist"),
        "a variable set through setenv is missing from the environ walk"
    );
    println!("setenv visible both ways ok");

    // Hand the environment to a child. A nameless entry is harmless until it
    // has to be re-encoded for the handoff, so this is where it would break.
    let mut env: HashMap<String, String> = std::env::vars().collect();
    env.insert("APE_SHIM_CHILD".into(), "inherited".into());
    let exe = std::env::args_os().next().expect("argv[0]");
    let out = Command::new(&exe)
        .arg("--child")
        .env_clear()
        .envs(&env)
        .output()
        .expect("spawn a child with the inherited environment");
    assert!(out.status.success(), "child failed: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "inherited");
    println!("round-trip through a child ok");

    println!("all environment checks ok");
}
