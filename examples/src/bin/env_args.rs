//! Test that argv and the environment are passed through correctly, and that the
//! working directory and executable path are correct.

use std::env;

const VAR: &str = "RUST_APE_EXAMPLE_VAR";

fn main() {
    // argv[0] plus whatever was passed. Under APE, argv[0] is the path the
    // program was invoked by, not the loader.
    let args: Vec<String> = env::args().collect();
    assert!(!args.is_empty(), "argv is empty");
    println!("argv[0]: {}", args[0]);
    if args.len() > 1 {
        println!("extra args: {:?}", &args[1..]);
    }

    // Reading the environment.
    let count = env::vars().count();
    assert!(count > 0, "the environment is empty");
    println!("{count} environment variables");
    for key in ["PATH", "HOME", "USERPROFILE"] {
        if let Some(v) = env::var_os(key) {
            let v = v.to_string_lossy();
            let shown = if v.len() > 60 { format!("{}...", &v[..60]) } else { v.into_owned() };
            println!("  {key}={shown}");
        }
    }

    // Writing it, and reading it back.
    assert!(env::var_os(VAR).is_none(), "{VAR} was already set");
    unsafe { env::set_var(VAR, "hello") };
    assert_eq!(env::var(VAR).as_deref(), Ok("hello"), "set_var did not take");
    unsafe { env::remove_var(VAR) };
    assert!(env::var_os(VAR).is_none(), "remove_var did not take");
    println!("set and removed {VAR}");

    // Working directory.
    let cwd = env::current_dir().expect("current_dir");
    assert!(cwd.is_absolute(), "current_dir returned a relative path: {}", cwd.display());
    assert!(cwd.is_dir(), "current_dir is not a directory");
    println!("cwd: {}", cwd.display());

    // Here we must use cosmo's API to get the program's own executable path,
    // because [`std::env::current_exe`] will return the loader under APE, not the program itself.
    // And what's worse is that on Windows, an error will be reported directly :(
    let exe = ape::program_executable_name().expect("program_executable_name");
    let md = std::fs::metadata(&exe).expect("stat our own executable");
    assert!(md.is_file(), "our executable is not a regular file");
    assert!(md.len() > 0, "our executable is empty");
    println!("executable: {exe} ({} bytes)", md.len());

    println!("\nenv args ok");
}
