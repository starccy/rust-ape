//! The host's SQLite, through bindings `cargo xtask host-bindings` generated
//! from bindgen output (src/hostlibs/sqlite.rs): libsqlite3 on Linux and the
//! BSDs, /usr/lib/libsqlite3.dylib on macOS, winsqlite3.dll on Windows 10 and
//! later. SQLite calls back into Rust twice here, for each row of a query and
//! for a SQL function written in Rust, which itself calls back into SQLite.
//! Last, SQLite opens a database that ships inside the binary, by the path
//! `ape::host::host_path()` gives for it.

#![allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code
)]

#[path = "../hostlibs/sqlite.rs"]
mod sqlite;

use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr::null_mut;

use ape::host::Callback;

use sqlite::*;

#[ape::host_callback]
fn rust_hypot(ctx: *mut sqlite3_context, argc: c_int, argv: *mut *mut sqlite3_value) {
    assert_eq!(argc, 2);
    let (x, y) = unsafe {
        (
            sqlite3_value_double(*argv),
            sqlite3_value_double(*argv.add(1)),
        )
    };
    unsafe { sqlite3_result_double(ctx, x.hypot(y)) };
}

#[ape::host_callback]
fn each_row(
    rows: *mut c_void,
    n: c_int,
    values: *mut *mut c_char,
    _names: *mut *mut c_char,
) -> c_int {
    let rows = unsafe { &mut *(rows as *mut Vec<String>) };
    let cols: Vec<String> = (0..n as usize)
        .map(|i| {
            let v = unsafe { *values.add(i) };
            if v.is_null() {
                "NULL".into()
            } else {
                unsafe { CStr::from_ptr(v) }.to_string_lossy().into_owned()
            }
        })
        .collect();
    rows.push(cols.join("|"));
    0
}

fn check(db: *mut sqlite3, rc: c_int, what: &str) {
    if rc != SQLITE_OK as c_int {
        let msg = unsafe { CStr::from_ptr(sqlite3_errmsg(db)) }.to_string_lossy();
        panic!("{what}: {rc} {msg}");
    }
}

fn main() {
    if let Err(e) = SQLITE.load() {
        panic!("no host SQLite: {e}");
    }
    let version = unsafe { CStr::from_ptr(sqlite3_libversion()) }.to_string_lossy();
    println!("host SQLite {version} ({})", SQLITE.load().unwrap().name());

    let mut db: *mut sqlite3 = null_mut();
    check(
        db,
        unsafe { sqlite3_open(c":memory:".as_ptr(), &mut db) },
        "open",
    );
    let flags = (SQLITE_UTF8 | SQLITE_DETERMINISTIC) as c_int;
    check(
        db,
        unsafe {
            sqlite3_create_function_v2(
                db,
                c"rust_hypot".as_ptr(),
                2,
                flags,
                null_mut(),
                rust_hypot::ptr(),
                Callback::NULL,
                Callback::NULL,
                Callback::NULL,
            )
        },
        "create_function",
    );

    let sql = c"create table t(a integer, b text);
                insert into t values (3, 'three'), (5, 'five'), (8, 'eight');";
    let mut err: *mut c_char = null_mut();
    check(
        db,
        unsafe { sqlite3_exec(db, sql.as_ptr(), Callback::NULL, null_mut(), &mut err) },
        "create",
    );

    let mut rows: Vec<String> = Vec::new();
    let q = c"select a, b, round(rust_hypot(a, 4), 3) from t order by a";
    check(
        db,
        unsafe {
            sqlite3_exec(
                db,
                q.as_ptr(),
                each_row::ptr(),
                &mut rows as *mut _ as *mut c_void,
                &mut err,
            )
        },
        "select",
    );
    for r in &rows {
        println!("  {r}");
    }
    assert_eq!(rows, ["3|three|5.0", "5|five|6.403", "8|eight|8.944"]);

    let mut st: *mut sqlite3_stmt = null_mut();
    let q = c"select count(*), sum(a), avg(rust_hypot(a, 0)) from t where a > ?";
    check(
        db,
        unsafe { sqlite3_prepare_v2(db, q.as_ptr(), -1, &mut st, null_mut()) },
        "prepare",
    );
    unsafe { sqlite3_bind_int64(st, 1, 4) };
    assert_eq!(unsafe { sqlite3_step(st) }, SQLITE_ROW as c_int);
    let (count, sum, avg) = unsafe {
        (
            sqlite3_column_int64(st, 0),
            sqlite3_column_int64(st, 1),
            sqlite3_column_double(st, 2),
        )
    };
    assert_eq!(unsafe { sqlite3_step(st) }, SQLITE_DONE as c_int);
    unsafe { sqlite3_finalize(st) };
    println!("prepared: count {count}, sum {sum}, avg {avg}");
    assert_eq!((count, sum, avg), (2, 13, 6.5));

    check(db, unsafe { sqlite3_close(db) }, "close");

    open_bundled();
    println!("\nhost sqlite ok");
}

// SQLite opens this file itself, with the host's libc, so it gets the path
// of a copy rather than the /zip path, which only Linux would understand
fn open_bundled() {
    if !std::path::Path::new("/usr/share/rust-ape-host-data/cities.db").exists() {
        println!("no bundled database, build.rs had no sqlite3 or python3 to make it; skipping");
        return;
    }
    let path = ape::host::host_path("/usr/share/rust-ape-host-data/cities.db")
        .unwrap_or_else(|e| panic!("host_path: {e}"));
    println!(
        "bundled database, as the host sees it: {}",
        path.to_string_lossy()
    );
    let mut db: *mut sqlite3 = null_mut();
    check(
        db,
        unsafe { sqlite3_open(path.as_ptr(), &mut db) },
        "open bundled",
    );
    let mut rows: Vec<String> = Vec::new();
    let mut err: *mut c_char = null_mut();
    let q =
        c"select name, country from cities where population > 25000000 order by population desc";
    check(
        db,
        unsafe {
            sqlite3_exec(
                db,
                q.as_ptr(),
                each_row::ptr(),
                &mut rows as *mut _ as *mut c_void,
                &mut err,
            )
        },
        "select bundled",
    );
    println!("  {}", rows.join(", "));
    assert_eq!(rows, ["Tokyo|JP", "Delhi|IN", "Shanghai|CN"]);
    // the copy is read-only
    let rc = unsafe {
        sqlite3_exec(
            db,
            c"delete from cities".as_ptr(),
            Callback::NULL,
            null_mut(),
            &mut err,
        )
    };
    assert_ne!(rc, SQLITE_OK as c_int, "a bundled database took a write");
    check(db, unsafe { sqlite3_close(db) }, "close bundled");
}
