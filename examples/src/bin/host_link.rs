//! A `-sys` crate linking a system library, unmodified: libsqlite3-sys
//! without its bundled SQLite asks for `-lsqlite3`. There's no static
//! libsqlite3 to link, so `cargo xtask build` links stubs in its place that
//! load the host's library the first time a function is called:
//! libsqlite3.so.0 on Linux, libsqlite3.dylib on macOS, and on Windows the
//! winsqlite3.dll that examples/Cargo.toml names under
//! `[package.metadata.rust-ape.host-libs]`.
//!
//! What it declares is compiled from a rewritten copy of the crate, in which
//! each function calls the host's with the host's convention, so floating
//! point arguments reach a Windows DLL where it looks for them, and so do
//! callbacks, which get made callable the DLL's way. A variadic function
//! becomes a value that can be called like one and sees what types each
//! call passes, which is what Windows and Apple need to lay them out. A
//! callback in a struct field is typed `HostCallback` in the copy, so the
//! virtual table module below has to say `HostCallback::new(f)` for each
//! of its functions, which is what makes them callable by the DLL, and
//! the VFS the library hands back is called through `get()`.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::ptr::null_mut;

use libsqlite3_sys as ffi;

// a virtual table of the squares 1..=5: the library calls each of these
// through the module struct
#[repr(C)]
struct SeqCursor {
    base: ffi::sqlite3_vtab_cursor,
    i: i64,
}

unsafe extern "C" fn seq_connect(
    db: *mut ffi::sqlite3,
    _aux: *mut c_void,
    _argc: c_int,
    _argv: *const *const c_char,
    out: *mut *mut ffi::sqlite3_vtab,
    _err: *mut *mut c_char,
) -> c_int {
    let rc = unsafe { ffi::sqlite3_declare_vtab(db, c"create table x(value integer)".as_ptr()) };
    if rc != ffi::SQLITE_OK {
        return rc;
    }
    unsafe { *out = Box::into_raw(Box::new(std::mem::zeroed::<ffi::sqlite3_vtab>())) };
    ffi::SQLITE_OK
}

unsafe extern "C" fn seq_disconnect(vt: *mut ffi::sqlite3_vtab) -> c_int {
    drop(unsafe { Box::from_raw(vt) });
    ffi::SQLITE_OK
}

unsafe extern "C" fn seq_best_index(
    _: *mut ffi::sqlite3_vtab,
    info: *mut ffi::sqlite3_index_info,
) -> c_int {
    unsafe { (*info).estimatedCost = 1.0 };
    ffi::SQLITE_OK
}

unsafe extern "C" fn seq_open(
    _: *mut ffi::sqlite3_vtab,
    out: *mut *mut ffi::sqlite3_vtab_cursor,
) -> c_int {
    let c = Box::new(SeqCursor {
        base: unsafe { std::mem::zeroed() },
        i: 0,
    });
    unsafe { *out = Box::into_raw(c) as *mut ffi::sqlite3_vtab_cursor };
    ffi::SQLITE_OK
}

unsafe extern "C" fn seq_close(c: *mut ffi::sqlite3_vtab_cursor) -> c_int {
    drop(unsafe { Box::from_raw(c as *mut SeqCursor) });
    ffi::SQLITE_OK
}

unsafe extern "C" fn seq_filter(
    c: *mut ffi::sqlite3_vtab_cursor,
    _: c_int,
    _: *const c_char,
    _: c_int,
    _: *mut *mut ffi::sqlite3_value,
) -> c_int {
    unsafe { (*(c as *mut SeqCursor)).i = 1 };
    ffi::SQLITE_OK
}

unsafe extern "C" fn seq_next(c: *mut ffi::sqlite3_vtab_cursor) -> c_int {
    unsafe { (*(c as *mut SeqCursor)).i += 1 };
    ffi::SQLITE_OK
}

unsafe extern "C" fn seq_eof(c: *mut ffi::sqlite3_vtab_cursor) -> c_int {
    (unsafe { (*(c as *mut SeqCursor)).i } > 5) as c_int
}

unsafe extern "C" fn seq_column(
    c: *mut ffi::sqlite3_vtab_cursor,
    ctx: *mut ffi::sqlite3_context,
    _: c_int,
) -> c_int {
    let i = unsafe { (*(c as *mut SeqCursor)).i };
    unsafe { ffi::sqlite3_result_int64(ctx, i * i) };
    ffi::SQLITE_OK
}

unsafe extern "C" fn seq_rowid(
    c: *mut ffi::sqlite3_vtab_cursor,
    out: *mut ffi::sqlite3_int64,
) -> c_int {
    unsafe { *out = (*(c as *mut SeqCursor)).i };
    ffi::SQLITE_OK
}

fn check(db: *mut ffi::sqlite3, rc: c_int, what: &str) {
    if rc != ffi::SQLITE_OK {
        let msg = unsafe { CStr::from_ptr(ffi::sqlite3_errmsg(db)) };
        panic!("{what}: {rc} {}", msg.to_string_lossy());
    }
}

unsafe extern "C" fn each_row(
    rows: *mut c_void,
    n: c_int,
    values: *mut *mut c_char,
    _: *mut *mut c_char,
) -> c_int {
    let rows = unsafe { &mut *(rows as *mut Vec<String>) };
    let cols: Vec<String> = (0..n as usize)
        .map(|i| {
            unsafe { CStr::from_ptr(*values.add(i)) }
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    rows.push(cols.join("|"));
    0
}

unsafe extern "C" fn rust_hypot(
    ctx: *mut ffi::sqlite3_context,
    argc: c_int,
    argv: *mut *mut ffi::sqlite3_value,
) {
    assert_eq!(argc, 2);
    let (x, y) = unsafe {
        (
            ffi::sqlite3_value_double(*argv),
            ffi::sqlite3_value_double(*argv.add(1)),
        )
    };
    unsafe { ffi::sqlite3_result_double(ctx, x.hypot(y)) };
}

fn main() {
    if ape::is_windows() && cfg!(target_arch = "aarch64") {
        println!("skipped: no host libraries on arm64 Windows yet");
        return;
    }
    // a variable of the library, set before the library is even loaded
    static DIR: &CStr = c"/rust-ape/set-before-load";
    unsafe { ffi::sqlite3_temp_directory = DIR.as_ptr() as *mut c_char };

    // the stubs would end the program at the first call if it were missing
    if let Err(e) = ape::host::Library::linked("sqlite3") {
        eprintln!("the host has no SQLite: {e}");
        std::process::exit(1);
    }
    assert!(ape::host::Library::linked("nosuchlib").is_err());

    let version = unsafe { CStr::from_ptr(ffi::sqlite3_libversion()) };
    println!("host SQLite {}", version.to_string_lossy());
    // declared as an empty array, the string is the library's
    let exported = unsafe { CStr::from_ptr((&raw const ffi::sqlite3_version).cast()) };
    assert_eq!(exported, version, "sqlite3_version");
    assert!(unsafe { ffi::sqlite3_libversion_number() } >= 3_000_000);

    let mut db = null_mut();
    let name = CString::new(":memory:").unwrap();
    let rc = unsafe { ffi::sqlite3_open(name.as_ptr(), &mut db) };
    check(db, rc, "open");
    let ddl = c"create table t(n integer, s text); insert into t values (1, 'one'), (2, 'two'), (40, 'forty')";
    check(
        db,
        unsafe { ffi::sqlite3_exec(db, ddl.as_ptr(), None, null_mut(), null_mut()) },
        "exec",
    );

    let mut st = null_mut();
    let sql = c"select sum(n), group_concat(s, '+') from t where n > ?1";
    check(
        db,
        unsafe { ffi::sqlite3_prepare_v2(db, sql.as_ptr(), -1, &mut st, null_mut()) },
        "prepare",
    );
    check(db, unsafe { ffi::sqlite3_bind_int64(st, 1, 1) }, "bind");
    assert_eq!(unsafe { ffi::sqlite3_step(st) }, ffi::SQLITE_ROW);
    let sum = unsafe { ffi::sqlite3_column_int64(st, 0) };
    let names = unsafe { CStr::from_ptr(ffi::sqlite3_column_text(st, 1) as *const c_char) }
        .to_string_lossy()
        .into_owned();
    unsafe { ffi::sqlite3_finalize(st) };
    println!("sum {sum}, {names}");
    assert_eq!((sum, names.as_str()), (42, "two+forty"));

    {
        let mut st = null_mut();
        let sql = c"select ?1 * 2.5";
        check(
            db,
            unsafe { ffi::sqlite3_prepare_v2(db, sql.as_ptr(), -1, &mut st, null_mut()) },
            "prepare",
        );
        check(db, unsafe { ffi::sqlite3_bind_double(st, 1, 4.0) }, "bind");
        assert_eq!(unsafe { ffi::sqlite3_step(st) }, ffi::SQLITE_ROW);
        let d = unsafe { ffi::sqlite3_column_double(st, 0) };
        unsafe { ffi::sqlite3_finalize(st) };
        assert_eq!(d, 10.0);

        // SQLite calls a Rust function, which calls back into SQLite
        let fname = c"rust_hypot";
        let rc = unsafe {
            ffi::sqlite3_create_function_v2(
                db,
                fname.as_ptr(),
                2,
                ffi::SQLITE_UTF8 | ffi::SQLITE_DETERMINISTIC,
                null_mut(),
                Some(rust_hypot),
                None,
                None,
                None,
            )
        };
        check(db, rc, "create_function");
        let mut rows: Vec<String> = Vec::new();
        let q = c"select n, rust_hypot(n, 9) from t order by n";
        let rc = unsafe {
            ffi::sqlite3_exec(
                db,
                q.as_ptr(),
                Some(each_row),
                &mut rows as *mut _ as *mut c_void,
                null_mut(),
            )
        };
        check(db, rc, "exec");
        println!("rows {rows:?}");
        assert_eq!(rows[2], "40|41.0");
    }

    // functions in a struct the library calls through
    {
        use ffi::HostCallback;
        let module = Box::leak(Box::new(ffi::sqlite3_module {
            iVersion: 0,
            xConnect: Some(HostCallback::new(seq_connect)),
            xDisconnect: Some(HostCallback::new(seq_disconnect)),
            xBestIndex: Some(HostCallback::new(seq_best_index)),
            xOpen: Some(HostCallback::new(seq_open)),
            xClose: Some(HostCallback::new(seq_close)),
            xFilter: Some(HostCallback::new(seq_filter)),
            xNext: Some(HostCallback::new(seq_next)),
            xEof: Some(HostCallback::new(seq_eof)),
            xColumn: Some(HostCallback::new(seq_column)),
            xRowid: Some(HostCallback::new(seq_rowid)),
            ..unsafe { std::mem::zeroed() }
        }));
        let rc = unsafe {
            ffi::sqlite3_create_module_v2(db, c"rustseq".as_ptr(), module, null_mut(), None)
        };
        check(db, rc, "create_module");
        let mut rows: Vec<String> = Vec::new();
        let rc = unsafe {
            ffi::sqlite3_exec(
                db,
                c"select sum(value), count(*) from rustseq".as_ptr(),
                Some(each_row),
                &mut rows as *mut _ as *mut c_void,
                null_mut(),
            )
        };
        check(db, rc, "exec");
        println!("virtual table {rows:?}");
        assert_eq!(rows, ["55|5"]);
    }

    // and functions in a struct the library filled in, called from here
    {
        let vfs = unsafe { ffi::sqlite3_vfs_find(std::ptr::null()) };
        assert!(!vfs.is_null());
        let name = unsafe { CStr::from_ptr((*vfs).zName) }
            .to_string_lossy()
            .into_owned();
        let mut buf = [0u8; 16];
        let n = unsafe {
            ((*vfs).xRandomness.unwrap().get())(vfs, 16, buf.as_mut_ptr() as *mut c_char)
        };
        let mut day = 0f64;
        let rc = unsafe { ((*vfs).xCurrentTime.unwrap().get())(vfs, &mut day) };
        println!("vfs {name}: {n} random bytes, julian day {day:.3}");
        assert_eq!((n, rc), (16, ffi::SQLITE_OK));
        assert!(buf.iter().any(|&b| b != 0));
        assert!(day > 2_460_000.0, "{day}");
    }

    // the library saw what we set before it loaded
    let pragma = |db, sql: &CStr| -> Option<String> {
        let mut rows: Vec<String> = Vec::new();
        let rc = unsafe {
            ffi::sqlite3_exec(
                db,
                sql.as_ptr(),
                Some(each_row),
                &mut rows as *mut _ as *mut c_void,
                null_mut(),
            )
        };
        check(db, rc, "pragma");
        rows.pop()
    };
    match pragma(db, c"pragma temp_store_directory") {
        Some(dir) => {
            println!("temp_store_directory {dir}");
            assert_eq!(dir, DIR.to_str().unwrap());
            // changes after it loaded go both ways too: on Linux there's one
            // variable, elsewhere the two copies are synced around each call
            {
                let ours = unsafe {
                    ffi::sqlite3_mprintf(c"%s".as_ptr(), c"/rust-ape/set-after-load".as_ptr())
                };
                unsafe { ffi::sqlite3_temp_directory = ours };
                assert_eq!(
                    pragma(db, c"pragma temp_store_directory").as_deref(),
                    Some("/rust-ape/set-after-load")
                );
                let tmp = std::env::temp_dir().to_string_lossy().into_owned();
                let set = CString::new(format!("pragma temp_store_directory = '{tmp}'")).unwrap();
                pragma(db, &set);
                let now = unsafe { CStr::from_ptr(ffi::sqlite3_temp_directory) };
                assert_eq!(now.to_str().unwrap(), tmp);
                println!("shared both ways");
            }
        }
        None => println!("temp_store_directory: this SQLite has no such pragma"),
    }

    // a variadic function, ints, doubles and a pointer mixed
    let p = unsafe {
        ffi::sqlite3_mprintf(
            c"%d|%.3f|%s|%lld|%g".as_ptr(),
            7,
            2.5f64,
            c"mixed".as_ptr(),
            1i64 << 40,
            0.125f64,
        )
    };
    let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
    unsafe { ffi::sqlite3_free(p as *mut c_void) };
    println!("mprintf {s}");
    assert_eq!(s, "7|2.500|mixed|1099511627776|0.125");

    unsafe { ffi::sqlite3_close(db) };
    println!("ok");
}
