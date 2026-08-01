//! An HTTPS client: ureq over rustls, several requests in flight at once.
//!
//! This one tests the build as much as the code. ureq pulls in rustls and
//! ring, and ring is C plus hand-written assembly compiled through the `cc`
//! crate. So it only links if cosmocc can act as the host compiler on both
//! architectures. Trust roots come from webpki-roots and are compiled in,
//! so there is no need to look for a system certificate store.
//!
//! ureq itself blocks, so each request has to go to smol's thread pool.
//! Chose the synchronous `ureq` because I couldn't find a crate in the `smol` ecosystem 
//! that was both active and offered a feature set comparable to `reqwest` :(

use std::time::{Duration, Instant};

const URLS: [&str; 4] = [
    "https://example.com/",
    "https://www.cloudflare.com/",
    "https://github.com/",
    "https://www.rust-lang.org/",
];

fn main() {
    smol::block_on(async {
        let started = Instant::now();

        let tasks: Vec<_> = URLS
            .iter()
            .map(|&url| smol::unblock(move || (url, fetch(url))))
            .collect();

        let mut total = 0usize;
        for t in tasks {
            let (url, (status, len)) = t.await;
            println!("{status} {len:>7} bytes  {url}");
            assert_eq!(status, 200, "{url} answered {status}");
            assert!(len > 0, "{url} returned an empty body");
            total += len;
        }

        println!(
            "\nhttps request ok: {} sites, {total} bytes in {:?}",
            URLS.len(),
            started.elapsed()
        );
    });
}

/// One blocking GET, returning the status and how much body came back.
fn fetch(url: &str) -> (u16, usize) {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        // api.github.com rejects requests that don't identify themselves.
        .user_agent("rust-ape-examples")
        .build()
        .new_agent();

    let mut resp = agent.get(url).call().expect("request failed");
    let status = resp.status().as_u16();
    let body = resp.body_mut().read_to_string().expect("could not read body");
    (status, body.len())
}
