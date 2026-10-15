//! The system trust store, as the certificate crates see it on each host.
//!
//! rustls-native-certs picks its backend at compile time and this target
//! says linux, so on every host it reads /etc/ssl. Linux has that, macOS
//! has cert.pem there, and on Windows the fork serves ca-certificates.crt
//! from the system ROOT store. examples/bundle.toml also packs the build
//! machine's ca-certificates.crt into this binary, which cosmo serves at
//! the Linux path wherever the host has nothing there (a host that has
//! the file, the fork's virtual one included, wins). So the count has to
//! be a real one everywhere, and a request through ureq's platform
//! verifier (which is that same crate underneath) has to complete.

use std::time::Duration;
use ureq::config::Config;
use ureq::tls::{RootCerts, TlsConfig};

fn main() {
    let result = rustls_native_certs::load_native_certs();
    for e in &result.errors {
        println!("error: {e}");
    }
    println!("{} roots from the system store", result.certs.len());
    assert!(result.errors.is_empty(), "the store loaded with errors");
    // Linux and macOS bundles carry well over a hundred roots; a Windows
    // machine's ROOT store can be as small as a couple of dozen
    assert!(
        result.certs.len() > 10,
        "{} roots is not a real store; is the bundle in this binary (unzip -l)?",
        result.certs.len()
    );

    let config = Config::builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .user_agent("rust-ape-examples")
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .build();
    let agent = config.new_agent();
    let url = "https://example.com/";
    let mut resp = agent
        .get(url)
        .call()
        .expect("request through the platform verifier failed");
    let status = resp.status().as_u16();
    let len = resp
        .body_mut()
        .read_to_string()
        .expect("could not read body")
        .len();
    println!("{status} {len} bytes  {url}");
    assert_eq!(status, 200);

    println!("\nnative certs ok");
}
