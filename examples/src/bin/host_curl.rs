//! Fetching over HTTPS with the host's libcurl, which brings the host's TLS
//! library and resolver threads with it. The interesting parts for us:
//!
//! - `curl_easy_setopt` and `curl_easy_getinfo` are variadic, so this is a
//!   real user of `host_variadic!`, on Apple Silicon included.
//! - the write callback runs on whichever thread called perform, and
//!   several of our threads fetch at once, each with its own handle.
//! - libcurl's threaded resolver starts a host thread per lookup.
//!
//! Needs a network. macOS ships libcurl, most Linux distributions have it,
//! Windows doesn't (curl.exe there is static), so the example skips where
//! the library is missing. The libcurl-x64.dll from curl.se works when put
//! next to the binary, but it's built on LibreSSL and libcurl, unlike the
//! curl tool, looks for no CA bundle on its own: without help every HTTPS
//! request fails with error 60. On Windows this example asks for the
//! system's certificate store (`CURLSSLOPT_NATIVE_CA`), which is fine on a
//! machine that has one; a bare VM's holds a couple of dozen roots and
//! fails the same way, and then `CURL_CA_BUNDLE=path\to\cacert.pem`
//! points it at a file, as it does for the curl tool. `CURL_VERBOSE=1`
//! shows the handshake.

use std::ffi::{CStr, c_char, c_int, c_long, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};

#[ape::host_library(static CURL = linux ["libcurl.so.4", "libcurl-gnutls.so.4", "libcurl.so"],
                    macos ["libcurl.4.dylib", "/usr/lib/libcurl.4.dylib"],
                    windows ["libcurl.dll", "libcurl-x64.dll"],
                    bsd ["libcurl.so.4", "libcurl.so"])]
unsafe extern "C" {
    fn curl_version() -> *const c_char;
    fn curl_global_init(flags: c_long) -> c_int;
    fn curl_global_cleanup();
    fn curl_easy_init() -> *mut c_void;
    fn curl_easy_perform(h: *mut c_void) -> c_int;
    fn curl_easy_cleanup(h: *mut c_void);
    fn curl_easy_strerror(code: c_int) -> *const c_char;
    fn curl_easy_setopt(h: *mut c_void, opt: c_int, ...) -> c_int;
    fn curl_easy_getinfo(h: *mut c_void, info: c_int, ...) -> c_int;
}

const CURL_GLOBAL_ALL: c_long = 3;
const CURLOPT_WRITEDATA: c_int = 10001;
const CURLOPT_URL: c_int = 10002;
const CURLOPT_USERAGENT: c_int = 10018;
const CURLOPT_TIMEOUT: c_int = 13;
const CURLOPT_FOLLOWLOCATION: c_int = 52;
const CURLOPT_WRITEFUNCTION: c_int = 20011;
const CURLOPT_SSL_OPTIONS: c_int = 216;
const CURLOPT_CAINFO: c_int = 10065;
const CURLOPT_VERBOSE: c_int = 41;
const CURLSSLOPT_NATIVE_CA: c_long = 1 << 2;
const CURLINFO_RESPONSE_CODE: c_int = 0x200002;

static CALLBACKS: AtomicUsize = AtomicUsize::new(0);

#[ape::host_callback]
fn on_write(data: *const u8, size: usize, nmemb: usize, user: *mut c_void) -> usize {
    CALLBACKS.fetch_add(1, Ordering::Relaxed);
    let body = unsafe { &mut *(user as *mut Vec<u8>) };
    let chunk = unsafe { std::slice::from_raw_parts(data, size * nmemb) };
    body.extend_from_slice(chunk);
    size * nmemb
}

fn fetch(url: &str) -> Result<(c_long, Vec<u8>), String> {
    let h = unsafe { curl_easy_init() };
    if h.is_null() {
        return Err("curl_easy_init failed".into());
    }
    let curl = std::ffi::CString::new(url).unwrap();
    let mut body: Vec<u8> = Vec::new();
    let rc = unsafe {
        let mut rc = ape::host_variadic!(curl_easy_setopt(h, CURLOPT_URL, curl.as_ptr()));
        rc |= ape::host_variadic!(curl_easy_setopt(h, CURLOPT_WRITEFUNCTION, on_write::ptr()));
        rc |= ape::host_variadic!(curl_easy_setopt(
            h,
            CURLOPT_WRITEDATA,
            &mut body as *mut Vec<u8>
        ));
        rc |= ape::host_variadic!(curl_easy_setopt(
            h,
            CURLOPT_USERAGENT,
            c"rust-ape/host_curl".as_ptr()
        ));
        rc |= ape::host_variadic!(curl_easy_setopt(h, CURLOPT_FOLLOWLOCATION, 1 as c_long));
        rc |= ape::host_variadic!(curl_easy_setopt(h, CURLOPT_TIMEOUT, 30 as c_long));
        // a libcurl built on OpenSSL or LibreSSL (the builds on curl.se
        // are) has no CA store on Windows unless told to use the system's
        if ape::is_windows() {
            rc |= ape::host_variadic!(curl_easy_setopt(
                h,
                CURLOPT_SSL_OPTIONS,
                CURLSSLOPT_NATIVE_CA
            ));
        }
        // the same knobs the curl tool has, for a host whose store is empty
        if let Ok(f) = std::env::var("CURL_CA_BUNDLE") {
            let f = std::ffi::CString::new(f).unwrap();
            rc |= ape::host_variadic!(curl_easy_setopt(h, CURLOPT_CAINFO, f.as_ptr()));
        }
        if std::env::var_os("CURL_VERBOSE").is_some() {
            rc |= ape::host_variadic!(curl_easy_setopt(h, CURLOPT_VERBOSE, 1 as c_long));
        }
        rc
    };
    assert_eq!(rc, 0, "curl_easy_setopt failed");
    let rc = unsafe { curl_easy_perform(h) };
    let result = if rc != 0 {
        let msg = unsafe { CStr::from_ptr(curl_easy_strerror(rc)) }
            .to_string_lossy()
            .into_owned();
        Err(format!("{url}: {msg} ({rc})"))
    } else {
        let mut code: c_long = 0;
        let rc = unsafe {
            ape::host_variadic!(curl_easy_getinfo(
                h,
                CURLINFO_RESPONSE_CODE,
                &mut code as *mut c_long
            ))
        };
        assert_eq!(rc, 0, "curl_easy_getinfo failed");
        Ok((code, body))
    };
    unsafe { curl_easy_cleanup(h) };
    result
}

fn main() {
    if let Err(e) = CURL.load() {
        println!("no libcurl on this host, skipping: {e}");
        return;
    }
    println!(
        "host curl: {}",
        unsafe { CStr::from_ptr(curl_version()) }.to_string_lossy()
    );
    assert_eq!(unsafe { curl_global_init(CURL_GLOBAL_ALL) }, 0);

    let url = "https://example.com/";
    match fetch(url) {
        Ok((code, body)) => {
            let text = String::from_utf8_lossy(&body);
            println!(
                "{url}: HTTP {code}, {} bytes, {} write callbacks",
                body.len(),
                CALLBACKS.load(Ordering::Relaxed)
            );
            assert_eq!(code, 200);
            assert!(text.contains("Example Domain"), "unexpected body: {text}");
        }
        Err(e) => {
            println!("no network, skipping: {e}");
            return;
        }
    }

    // several of our threads at once, each with its own handle and TLS session
    let before = CALLBACKS.load(Ordering::Relaxed);
    let handles: Vec<_> = (0..6)
        .map(|i| {
            std::thread::spawn(move || {
                let url = if i % 2 == 0 {
                    "https://example.com/"
                } else {
                    "https://www.example.org/"
                };
                let (code, body) = fetch(url).unwrap_or_else(|e| panic!("{e}"));
                (code, body.len())
            })
        })
        .collect();
    let mut total = 0;
    for h in handles {
        let (code, n) = h.join().unwrap();
        assert_eq!(code, 200);
        total += n;
    }
    println!(
        "6 threads fetched {total} bytes in parallel, {} write callbacks",
        CALLBACKS.load(Ordering::Relaxed) - before
    );

    unsafe { curl_global_cleanup() };
    println!("\nhost curl ok");
}
