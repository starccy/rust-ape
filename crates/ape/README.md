# ape

Safe wrappers around the runtime APIs Cosmopolitan offers beyond libc.

There isn't much code and it's all commented, see [src/lib.rs](src/lib.rs) for
the full list. The functions worth mentioning:

**`ape::current_os()`**

The standard library only knows the compile-time target, so
`std::env::consts::OS` is always `"linux"` no matter where the binary runs.
This function reads the host OS at runtime.

**`ape::program_executable_name()`**

Use this instead of `std::env::current_exe()`. On Unix an APE binary starts
through the APE loader, and `current_exe()` returns the loader's path instead
of your program's. On Windows there is no loader, but `current_exe()` fails
anyway because it reads `/proc/self/exe`, which doesn't exist there.

**`ape::tmpdir()`**

Replaces `std::env::temp_dir()`, which returns `/tmp` even on Windows. That
path still works, since cosmo maps it to the real Windows temp directory
behind the scenes. But if you want the actual native path, use this one.
