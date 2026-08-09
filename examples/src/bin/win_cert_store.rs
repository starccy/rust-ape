//! Reading the Windows certificate store, the third option in the README's
//! TLS section.
//!
//! Every crate that reads the system store picks its backend at compile time,
//! and this target says unix, so on Windows they all look in /etc/ssl and
//! come back empty. The store itself is fine; it sits behind crypt32, which
//! cosmo imports nothing from. So the way in is `LoadLibraryA` plus
//! `GetProcAddress`, and the pointers that come back want Microsoft's x64
//! convention, which is what `extern "win64"` is and why the enumeration is
//! x86_64 only.
//!
//! What the enumeration yields is plain DER, exactly what rustls wants in a
//! `RootCertStore`, and feeding it through `add()` is also the parse check.
//! This example stops at the filled store; wiring it into a client from
//! there is the same rustls code as on any other host.

use std::ffi::c_void;

fn main() {
    if !ape::is_windows() {
        println!("no crypt32 on this host; its roots are in /etc/ssl, where the unix path of every certs crate already looks");
        println!("\nwin cert store ok");
        return;
    }

    #[cfg(target_arch = "x86_64")]
    enumerate();
    #[cfg(not(target_arch = "x86_64"))]
    println!("the crypt32 calls need the win64 ABI, which is x86_64 only");

    println!("\nwin cert store ok");
}

#[cfg(target_arch = "x86_64")]
unsafe extern "C" {
    fn LoadLibraryA(name: *const u8) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}

/// The prefix of CERT_CONTEXT, which is all the enumeration touches.
#[cfg(target_arch = "x86_64")]
#[repr(C)]
struct CertContext {
    dw_cert_encoding_type: u32,
    pb_cert_encoded: *const u8,
    cb_cert_encoded: u32,
    p_cert_info: *mut c_void,
    h_cert_store: *mut c_void,
}

#[cfg(target_arch = "x86_64")]
fn enumerate() {
    use rustls::pki_types::CertificateDer;

    type Open = unsafe extern "win64" fn(*mut c_void, *const u8) -> *mut c_void;
    type Next = unsafe extern "win64" fn(*mut c_void, *const CertContext) -> *const CertContext;
    type Close = unsafe extern "win64" fn(*mut c_void, u32) -> i32;

    let (open, next, close) = unsafe {
        let lib = LoadLibraryA(c"crypt32.dll".as_ptr() as *const u8);
        assert!(!lib.is_null(), "LoadLibraryA(crypt32.dll) failed");
        let sym = |name: &std::ffi::CStr| {
            let p = GetProcAddress(lib, name.as_ptr() as *const u8);
            assert!(!p.is_null(), "GetProcAddress({name:?}) came back null");
            p
        };
        (
            std::mem::transmute::<*mut c_void, Open>(sym(c"CertOpenSystemStoreA")),
            std::mem::transmute::<*mut c_void, Next>(sym(c"CertEnumCertificatesInStore")),
            std::mem::transmute::<*mut c_void, Close>(sym(c"CertCloseStore")),
        )
    };

    let mut roots = rustls::RootCertStore::empty();
    for store_name in [c"ROOT", c"CA"] {
        let mut seen = 0usize;
        let before = roots.len();
        unsafe {
            let store = open(std::ptr::null_mut(), store_name.as_ptr() as *const u8);
            assert!(!store.is_null(), "CertOpenSystemStoreA({store_name:?}) failed");
            let mut ctx: *const CertContext = std::ptr::null();
            loop {
                ctx = next(store, ctx);
                if ctx.is_null() {
                    break;
                }
                seen += 1;
                let der = std::slice::from_raw_parts(
                    (*ctx).pb_cert_encoded,
                    (*ctx).cb_cert_encoded as usize,
                );
                roots.add(CertificateDer::from(der.to_vec())).ok();
            }
            close(store, 0);
        }
        println!(
            "{}: {seen} certificates, {} accepted by rustls",
            store_name.to_str().unwrap(),
            roots.len() - before
        );
    }

    assert!(!roots.is_empty(), "a Windows machine with an empty ROOT store");
    println!("RootCertStore holds {} trust anchors from the host", roots.len());
}
