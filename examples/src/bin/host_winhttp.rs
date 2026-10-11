//! An asynchronous HTTPS request with WinHTTP, the HTTP stack Windows
//! ships. In asynchronous mode WinHTTP calls a status callback from its own
//! thread pool threads, at every step of the request, and the callback
//! drives the next step from there. So this is Rust code running on
//! threads a system library created, holding a Mutex, sending on a channel
//! and calling back into the library, a shape that Windows GUI and network
//! code has everywhere.
//!
//! Windows only; elsewhere the example says so and exits.

use std::ffi::{c_int, c_void};
use std::sync::{Mutex, mpsc};

use ape::host::Callback;

type HINTERNET = *mut c_void;

#[ape::host_library(static WINHTTP = windows ["winhttp.dll"])]
unsafe extern "C" {
    fn WinHttpOpen(
        agent: *const u16,
        access: u32,
        proxy: *const u16,
        bypass: *const u16,
        flags: u32,
    ) -> HINTERNET;
    fn WinHttpSetStatusCallback(
        h: HINTERNET,
        cb: Callback,
        flags: u32,
        reserved: usize,
    ) -> *mut c_void;
    fn WinHttpConnect(h: HINTERNET, server: *const u16, port: u16, reserved: u32) -> HINTERNET;
    fn WinHttpOpenRequest(
        h: HINTERNET,
        verb: *const u16,
        path: *const u16,
        version: *const u16,
        referrer: *const u16,
        accept: *const *const u16,
        flags: u32,
    ) -> HINTERNET;
    fn WinHttpSendRequest(
        h: HINTERNET,
        headers: *const u16,
        headers_len: u32,
        optional: *const c_void,
        optional_len: u32,
        total_len: u32,
        context: usize,
    ) -> c_int;
    fn WinHttpReceiveResponse(h: HINTERNET, reserved: *mut c_void) -> c_int;
    fn WinHttpQueryHeaders(
        h: HINTERNET,
        level: u32,
        name: *const u16,
        buf: *mut c_void,
        len: *mut u32,
        index: *mut u32,
    ) -> c_int;
    fn WinHttpReadData(h: HINTERNET, buf: *mut c_void, len: u32, read: *mut u32) -> c_int;
    fn WinHttpCloseHandle(h: HINTERNET) -> c_int;
}

#[ape::host_library(static KERNEL32 = windows ["kernel32.dll"])]
unsafe extern "C" {
    fn GetLastError() -> u32;
    fn GetCurrentThreadId() -> u32;
}

const WINHTTP_FLAG_ASYNC: u32 = 0x1000_0000;
const WINHTTP_FLAG_SECURE: u32 = 0x0080_0000;
const WINHTTP_CALLBACK_FLAG_ALL_NOTIFICATIONS: u32 = 0xffff_ffff;
const WINHTTP_CALLBACK_STATUS_SENDREQUEST_COMPLETE: u32 = 0x0040_0000;
const WINHTTP_CALLBACK_STATUS_HEADERS_AVAILABLE: u32 = 0x0002_0000;
const WINHTTP_CALLBACK_STATUS_READ_COMPLETE: u32 = 0x0008_0000;
const WINHTTP_CALLBACK_STATUS_REQUEST_ERROR: u32 = 0x0020_0000;
const WINHTTP_QUERY_STATUS_CODE: u32 = 19;
const WINHTTP_QUERY_FLAG_NUMBER: u32 = 0x2000_0000;

struct Request {
    body: Vec<u8>,
    buf: Box<[u8; 8192]>,
    status: u32,
    callbacks: usize,
    threads: Vec<u32>,
    events: Vec<u32>,
    done: mpsc::Sender<Result<(), String>>,
}

static REQ: Mutex<Option<Request>> = Mutex::new(None);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// runs on WinHTTP's worker threads, and sometimes nested inside a call
// made from here, so the lock is never held across a WinHTTP call
#[ape::host_callback]
fn on_status(h: HINTERNET, _ctx: usize, status: u32, info: *mut c_void, len: u32) {
    let tid = unsafe { GetCurrentThreadId() };
    let (buf, done) = {
        let mut guard = REQ.lock().unwrap();
        let Some(r) = guard.as_mut() else { return };
        r.callbacks += 1;
        r.events.push(status);
        if !r.threads.contains(&tid) {
            r.threads.push(tid);
        }
        if status == WINHTTP_CALLBACK_STATUS_READ_COMPLETE && len > 0 {
            let got = unsafe { std::slice::from_raw_parts(info as *const u8, len as usize) };
            r.body.extend_from_slice(got);
        }
        (r.buf.as_mut_ptr() as *mut c_void, r.done.clone())
    };
    let step = |ok: c_int, what: &str| {
        if ok == 0 {
            let _ = done.send(Err(format!("{what} failed: {}", unsafe { GetLastError() })));
        }
    };
    match status {
        WINHTTP_CALLBACK_STATUS_SENDREQUEST_COMPLETE => {
            step(
                unsafe { WinHttpReceiveResponse(h, std::ptr::null_mut()) },
                "WinHttpReceiveResponse",
            );
        }
        WINHTTP_CALLBACK_STATUS_HEADERS_AVAILABLE => {
            let mut code: u32 = 0;
            let mut n: u32 = 4;
            let ok = unsafe {
                WinHttpQueryHeaders(
                    h,
                    WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                    std::ptr::null(),
                    &mut code as *mut u32 as *mut c_void,
                    &mut n,
                    std::ptr::null_mut(),
                )
            };
            step(ok, "WinHttpQueryHeaders");
            if let Some(r) = REQ.lock().unwrap().as_mut() {
                r.status = code;
            }
            step(
                unsafe { WinHttpReadData(h, buf, 8192, std::ptr::null_mut()) },
                "WinHttpReadData",
            );
        }
        WINHTTP_CALLBACK_STATUS_READ_COMPLETE => {
            if len == 0 {
                let _ = done.send(Ok(()));
            } else {
                step(
                    unsafe { WinHttpReadData(h, buf, 8192, std::ptr::null_mut()) },
                    "WinHttpReadData",
                );
            }
        }
        WINHTTP_CALLBACK_STATUS_REQUEST_ERROR => {
            // info is a WINHTTP_ASYNC_RESULT { dwResult, dwError }
            let err = if info.is_null() {
                0
            } else {
                (unsafe { *(info as *const usize).add(1) }) as u32
            };
            let _ = done.send(Err(format!("request error {err}")));
        }
        _ => {}
    }
}

fn get(port: u16) {
    let session = unsafe {
        WinHttpOpen(
            wide("rust-ape/host_winhttp").as_ptr(),
            0,
            std::ptr::null(),
            std::ptr::null(),
            WINHTTP_FLAG_ASYNC,
        )
    };
    assert!(!session.is_null(), "WinHttpOpen: {}", unsafe {
        GetLastError()
    });
    let prev = unsafe {
        WinHttpSetStatusCallback(
            session,
            on_status::ptr(),
            WINHTTP_CALLBACK_FLAG_ALL_NOTIFICATIONS,
            0,
        )
    };
    assert_ne!(prev as isize, -1, "WinHttpSetStatusCallback: {}", unsafe {
        GetLastError()
    });
    let conn = unsafe { WinHttpConnect(session, wide("example.com").as_ptr(), port, 0) };
    assert!(!conn.is_null(), "WinHttpConnect: {}", unsafe {
        GetLastError()
    });
    let req = unsafe {
        WinHttpOpenRequest(
            conn,
            wide("GET").as_ptr(),
            wide("/").as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            if port == 443 { WINHTTP_FLAG_SECURE } else { 0 },
        )
    };
    assert!(!req.is_null(), "WinHttpOpenRequest: {}", unsafe {
        GetLastError()
    });

    let (tx, rx) = mpsc::channel();
    *REQ.lock().unwrap() = Some(Request {
        body: Vec::new(),
        buf: Box::new([0; 8192]),
        status: 0,
        callbacks: 0,
        threads: Vec::new(),
        events: Vec::new(),
        done: tx,
    });
    let ok = unsafe { WinHttpSendRequest(req, std::ptr::null(), 0, std::ptr::null(), 0, 0, 1) };
    assert_ne!(ok, 0, "WinHttpSendRequest: {}", unsafe { GetLastError() });

    let result = rx
        .recv_timeout(std::time::Duration::from_secs(60))
        .unwrap_or_else(|_| {
            let events = REQ
                .lock()
                .unwrap()
                .as_ref()
                .map(|r| r.events.clone())
                .unwrap_or_default();
            panic!("no completion within 60 s, status callbacks seen: {events:#x?}");
        });
    unsafe {
        WinHttpCloseHandle(req);
        WinHttpCloseHandle(conn);
        WinHttpCloseHandle(session);
    }
    let r = REQ.lock().unwrap().take().unwrap();
    let text = String::from_utf8_lossy(&r.body);
    println!(
        "port {port}: HTTP {}, {} bytes, {} status callbacks on {} WinHTTP threads (main is {})",
        r.status,
        r.body.len(),
        r.callbacks,
        r.threads.len(),
        unsafe { GetCurrentThreadId() }
    );
    println!("  statuses: {:x?}", r.events);
    match result {
        Ok(()) => {}
        Err(e) => {
            panic!("request failed, is there a network? {e}");
        }
    }
    assert_eq!(r.status, 200);
    assert!(text.contains("Example Domain"), "unexpected body: {text}");
    assert!(r.callbacks >= 4);
}

fn main() {
    if !ape::is_windows() {
        println!("windows only, skipping");
        return;
    }
    get(80);
    get(443);
    println!("\nhost winhttp ok");
}
