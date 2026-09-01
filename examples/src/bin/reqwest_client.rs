//! reqwest on tokio, with most of its feature surface turned on.
//!
//! The build matters as much as the code. reqwest defaults to aws-lc-rs, whose
//! assembly is guarded on `__linux__` and comes out of cosmocc with no symbol
//! table, so the TLS stack is assembled by hand: ring as the provider,
//! webpki-roots for trust, ALPN set explicitly. That last one is the sneaky
//! part, since `tls_backend_preconfigured` skips the path where reqwest would
//! have filled it in, and the only symptom of getting it wrong is that
//! everything quietly falls back to HTTP/1.1.
//!
//! `zstd` reaches the same `__linux__` trap through zstd-sys, which is why
//! Cargo.toml names zstd-sys only to turn its `no_asm` feature on.
//!
//! Three parts: real sites over HTTPS, typed JSON off a real API, and a local
//! tokio server everything else is measured and checked against.

use std::io::Write;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

const SITES: [&str; 4] = [
    "https://example.com/",
    "https://www.google.com/",
    "https://github.com/",
    "https://www.rust-lang.org/",
];

const NAMES: [&str; 4] = ["example.com", "rust-lang.org", "github.com", "cloudflare.com"];

const CLIENTS: usize = 64;
const ROUNDS: usize = 20;
// Kept under 256KB deliberately. Bodies at or above that size occasionally
// wedge on Windows: server and client both freeze with a couple of hundred KB
// in flight, and the request only finishes when its own timeout timer happens
// to re-poll it. See the note in README's known-broken section. Anything at
// or below 128KB has never reproduced it.
const BIG: usize = 128 * 1024;
const COMPRESSIBLE: &str = "the same sentence over and over compresses nicely. ";

fn main() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("install ring as the crypto provider");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("build runtime");

    rt.block_on(async {
        real_sites().await;
        typed_json().await;
        hickory_resolver().await;
        local().await;
    });

    println!("\nreqwest ok");
}

/// One TLS config for every client here. reqwest builds its TLS stack in
/// `build()` rather than on first use, so a client that only ever speaks
/// http:// still needs this or it panics looking for a system trust store.
fn tls_config() -> rustls::ClientConfig {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let mut cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    // reqwest only fills this in for a config it built itself.
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    cfg
}

fn builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        // Not Some(tls); the function wraps it.
        .tls_backend_preconfigured(tls_config())
        .timeout(Duration::from_secs(30))
        .user_agent("rust-ape-examples")
}

/// Four sites at once over one client, so the pool and several TLS handshakes
/// overlap.
async fn real_sites() {
    let http = builder().pool_max_idle_per_host(8).build().expect("build client");
    let started = Instant::now();

    let tasks: Vec<_> = SITES
        .iter()
        .map(|&url| {
            let http = http.clone();
            tokio::spawn(async move {
                let resp = http.get(url).send().await.expect("request failed");
                let (status, version) = (resp.status(), resp.version());
                let body = resp.bytes().await.expect("read body");
                (url, status, version, body.len())
            })
        })
        .collect();

    let (mut total, mut over_h2) = (0usize, 0usize);
    for t in tasks {
        let (url, status, version, len) = t.await.expect("task");
        println!("{status} {version:?} {len:>7} bytes  {url}");
        assert!(status.is_success(), "{url} answered {status}");
        assert!(len > 0, "{url} returned an empty body");
        if version == reqwest::Version::HTTP_2 {
            over_h2 += 1;
        }
        total += len;
    }

    // All four serve h2. None negotiating it means ALPN never reached the
    // config, which costs performance and breaks nothing, so no other
    // assertion here would notice.
    assert!(over_h2 > 0, "nothing came back over HTTP/2, check alpn_protocols");
    println!("{} sites, {total} bytes, {over_h2} over h2, in {:?}", SITES.len(), started.elapsed());
}

// Cloudflare's DNS-over-HTTPS JSON API, picked because the shape of what comes
// back is worth asserting on rather than just counting bytes.
#[derive(Deserialize)]
struct DnsReply {
    #[serde(rename = "Status")]
    status: i32,
    #[serde(rename = "Question")]
    question: Vec<DnsQuestion>,
    #[serde(rename = "Answer")]
    answer: Option<Vec<DnsAnswer>>,
}

#[derive(Deserialize)]
struct DnsQuestion {
    name: String,
}

#[derive(Deserialize)]
struct DnsAnswer {
    #[serde(rename = "type")]
    kind: u16,
    data: String,
}

async fn typed_json() {
    let http = builder().build().expect("build client");

    let tasks: Vec<_> = NAMES
        .iter()
        .map(|&name| {
            let http = http.clone();
            tokio::spawn(async move {
                let reply: DnsReply = http
                    .get("https://cloudflare-dns.com/dns-query")
                    .query(&[("name", name), ("type", "A")])
                    .header("accept", "application/dns-json")
                    .send()
                    .await
                    .expect("dns request failed")
                    .error_for_status()
                    .expect("dns request status")
                    .json()
                    .await
                    .expect("decode json");
                (name, reply)
            })
        })
        .collect();

    for t in tasks {
        let (name, reply) = t.await.expect("task");
        assert_eq!(reply.status, 0, "{name} came back with Status {}", reply.status);

        let asked = reply.question.first().expect("no Question in the reply");
        assert_eq!(
            asked.name.trim_end_matches('.').to_ascii_lowercase(),
            name,
            "reply answered a different question"
        );

        // Type 1 is an A record; CNAMEs can ride along, so filter.
        let addrs: Vec<Ipv4Addr> = reply
            .answer
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|a| a.kind == 1)
            .filter_map(|a| a.data.parse().ok())
            .collect();
        assert!(!addrs.is_empty(), "{name} resolved to no usable A record");
        println!("{name:<16} -> {}", addrs[0]);
    }

    println!("{} json replies decoded into typed structs", NAMES.len());
}

/// hickory-dns replaces getaddrinfo with a pure-Rust resolver that speaks DNS
/// over its own UDP and TCP sockets, so this is a different path to the same
/// place.
async fn hickory_resolver() {
    let http = builder().hickory_dns(true).build().expect("build client");
    let resp = http.get("https://example.com/").send().await.expect("request through hickory");
    assert!(resp.status().is_success(), "hickory client got {}", resp.status());
    let len = resp.bytes().await.expect("body").len();
    assert!(len > 0);
    println!("hickory-dns resolver: example.com fetched, {len} bytes");
}

// ---------------------------------------------------------------------------
// A minimal keep-alive HTTP/1.1 server, and everything measured against it.

struct Request {
    path: String,
    accept_encoding: String,
    cookie: String,
    body: Vec<u8>,
}

async fn read_request<R>(r: &mut BufReader<R>) -> Option<Request>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut line = String::new();
    if r.read_line(&mut line).await.ok()? == 0 {
        return None;
    }
    let path = line.split_whitespace().nth(1)?.to_string();

    let (mut len, mut chunked) = (0usize, false);
    let (mut accept_encoding, mut cookie) = (String::new(), String::new());
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).await.ok()? == 0 {
            return None;
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        let (k, v) = h.split_once(':')?;
        let v = v.trim();
        match k.to_ascii_lowercase().as_str() {
            "content-length" => len = v.parse().unwrap_or(0),
            "transfer-encoding" => chunked = v.to_ascii_lowercase().contains("chunked"),
            "accept-encoding" => accept_encoding = v.to_string(),
            "cookie" => cookie = v.to_string(),
            _ => {}
        }
    }

    // Multipart bodies arrive chunked, since their length isn't known up front.
    let mut body = Vec::new();
    if chunked {
        loop {
            let mut size = String::new();
            if r.read_line(&mut size).await.ok()? == 0 {
                return None;
            }
            let n = usize::from_str_radix(size.trim().split(';').next()?, 16).ok()?;
            let mut chunk = vec![0u8; n];
            r.read_exact(&mut chunk).await.ok()?;
            let mut crlf = String::new();
            r.read_line(&mut crlf).await.ok()?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk);
        }
    } else if len > 0 {
        body.resize(len, 0);
        r.read_exact(&mut body).await.ok()?;
    }

    Some(Request { path, accept_encoding, cookie, body })
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(data).expect("gzip");
    e.finish().expect("gzip finish")
}

fn zlib(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(data).expect("deflate");
    e.finish().expect("deflate finish")
}

/// Header and body go out in one write and Nagle is off. Splitting them costs
/// a delayed ACK per request, measured at 40ms each, which buries whatever the
/// test was supposed to show.
async fn serve(sock: TcpStream) {
    sock.set_nodelay(true).expect("nodelay");
    let (r, mut w) = sock.into_split();
    let mut r = BufReader::new(r);

    while let Some(req) = read_request(&mut r).await {
        let (mut encoding, body): (Option<&str>, Vec<u8>) = match req.path.as_str() {
            "/big" => (None, vec![b'x'; BIG]),
            "/gzip" => (Some("gzip"), gzip(COMPRESSIBLE.repeat(200).as_bytes())),
            "/deflate" => (Some("deflate"), zlib(COMPRESSIBLE.repeat(200).as_bytes())),
            "/accept-encoding" => (None, req.accept_encoding.into_bytes()),
            "/set-cookie" => (None, b"set".to_vec()),
            "/read-cookie" => (None, req.cookie.into_bytes()),
            "/echo" => (None, req.body.clone()),
            "/echo-len" => (None, req.body.len().to_string().into_bytes()),
            p => (None, p.trim_start_matches('/').as_bytes().to_vec()),
        };
        if req.path == "/set-cookie" {
            encoding = None;
        }

        let mut head = format!(
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\ncontent-type: text/plain\r\n",
            body.len()
        );
        if let Some(enc) = encoding {
            head += &format!("content-encoding: {enc}\r\n");
        }
        if req.path == "/set-cookie" {
            head += "set-cookie: ape=chocolate; Path=/\r\n";
        }
        head += "\r\n";

        let mut out = head.into_bytes();
        out.extend_from_slice(&body);
        if w.write_all(&out).await.is_err() {
            return;
        }
    }
}

async fn spawn_server() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            tokio::spawn(serve(sock));
        }
    });
    addr
}

async fn local() {
    let addr = spawn_server().await;
    let base = format!("http://{addr}");
    println!("local server on {addr}");

    // One client across the phases that don't need their own. Building a
    // fresh one per phase means tearing 64 pooled connections down while the
    // next phase is opening more, and that churn is what the Windows stall
    // described in the README likes best.
    let http = Arc::new(
        builder()
            .pool_max_idle_per_host(CLIENTS)
            .build()
            .expect("build client"),
    );

    // This used to be held back on Windows: mio's poll backend dropped a
    // wakeup about one run in ten at this much traffic. mio is on epoll now,
    // answered by shim/epoll.c, so the load test runs everywhere.
    load(&http, &base).await;
    compression(&http, &base).await;
    bodies(&http, &base).await;
    streaming(&http, &base).await;
    cookies(&base).await;
    through_socks(&base).await;
    blocking(&base).await;
}

/// The stress half: one shared client, so what's under load is the connection
/// pool rather than repeated handshakes.
async fn load(http: &Arc<reqwest::Client>, base: &str) {
    let started = Instant::now();
    let tasks: Vec<_> = (0..CLIENTS)
        .map(|id| {
            let (http, base) = (http.clone(), base.to_string());
            tokio::spawn(async move {
                let mut latencies = Vec::with_capacity(ROUNDS);
                for round in 0..ROUNDS {
                    let token = format!("c{id}r{round}");
                    let t = Instant::now();
                    let body = tokio::time::timeout(Duration::from_secs(10), async {
                        http.get(format!("{base}/{token}"))
                            .send()
                            .await
                            .expect("send")
                            .text()
                            .await
                            .expect("body")
                    })
                    .await
                    .unwrap_or_else(|_| panic!("{token} stalled past 10s"));
                    latencies.push(t.elapsed());
                    // A pooled connection handing back someone else's response
                    // would show up right here.
                    assert_eq!(body, token, "client {id} got {body:?}");
                }
                latencies
            })
        })
        .collect();

    let mut latencies = Vec::with_capacity(CLIENTS * ROUNDS);
    for t in tasks {
        latencies.extend(t.await.expect("client task"));
    }
    let elapsed = started.elapsed();

    latencies.sort_unstable();
    let at = |q: f64| latencies[((latencies.len() as f64 * q) as usize).min(latencies.len() - 1)];
    let n = latencies.len();
    assert_eq!(n, CLIENTS * ROUNDS);
    println!(
        "{n} requests over {CLIENTS} connections in {elapsed:?}  \
         ({:.0} req/s, p50 {:?}, p99 {:?}, max {:?})",
        n as f64 / elapsed.as_secs_f64(),
        at(0.50),
        at(0.99),
        latencies[n - 1]
    );
}

async fn compression(http: &reqwest::Client, base: &str) {
    let want = COMPRESSIBLE.repeat(200);

    for route in ["gzip", "deflate"] {
        let resp = http.get(format!("{base}/{route}")).send().await.expect("send");
        let body = resp.text().await.expect("body");
        assert_eq!(body, want, "{route} body came back wrong ({} bytes)", body.len());
        println!("{route}: {} bytes decompressed transparently", body.len());
    }

    // brotli and zstd have no encoder here, so what gets checked is that
    // reqwest advertised them; a feature that failed to wire up would be
    // missing from this header and nothing else would notice.
    let offered = http
        .get(format!("{base}/accept-encoding"))
        .send()
        .await
        .expect("send")
        .text()
        .await
        .expect("body");
    for enc in ["gzip", "deflate", "br", "zstd"] {
        assert!(offered.contains(enc), "accept-encoding was {offered:?}, missing {enc}");
    }
    println!("accept-encoding offered: {offered}");
}

async fn cookies(base: &str) {
    let http = builder().cookie_store(true).build().expect("build client");

    let set = http.get(format!("{base}/set-cookie")).send().await.expect("send");
    assert_eq!(set.text().await.expect("body"), "set");

    let sent = http
        .get(format!("{base}/read-cookie"))
        .send()
        .await
        .expect("send")
        .text()
        .await
        .expect("body");
    assert!(sent.contains("ape=chocolate"), "the jar sent back {sent:?}");

    // A client without the jar must not carry it.
    let bare = builder().build().expect("build client");
    let none = bare
        .get(format!("{base}/read-cookie"))
        .send()
        .await
        .expect("send")
        .text()
        .await
        .expect("body");
    assert!(none.is_empty(), "a client with no jar still sent {none:?}");
    println!("cookies: stored, replayed, and absent without a jar");
}

async fn bodies(http: &reqwest::Client, base: &str) {
    // A urlencoded form, echoed back as it arrived.
    let echoed = http
        .post(format!("{base}/echo"))
        .form(&[("crate", "rust-ape"), ("runs on", "six operating systems")])
        .send()
        .await
        .expect("send")
        .text()
        .await
        .expect("body");
    assert_eq!(
        echoed, "crate=rust-ape&runs+on=six+operating+systems",
        "form body arrived as {echoed:?}"
    );

    // Multipart arrives chunked, since its length isn't known up front.
    let form = reqwest::multipart::Form::new()
        .text("field", "value")
        .part(
            "file",
            reqwest::multipart::Part::bytes(vec![b'z'; 4096]).file_name("blob.bin"),
        );
    let len: usize = http
        .post(format!("{base}/echo-len"))
        .multipart(form)
        .send()
        .await
        .expect("send")
        .text()
        .await
        .expect("body")
        .parse()
        .expect("parse length");
    assert!(len > 4096, "multipart body was only {len} bytes");
    println!("bodies: form echoed exactly, multipart carried {len} bytes over chunked");
}

/// bytes_stream() pulls the body incrementally instead of buffering it whole.
async fn streaming(http: &reqwest::Client, base: &str) {
    let resp = http.get(format!("{base}/big")).send().await.expect("send");

    let mut stream = resp.bytes_stream();
    let (mut total, mut chunks) = (0usize, 0usize);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("chunk");
        assert!(chunk.iter().all(|&c| c == b'x'), "a chunk came back corrupted");
        total += chunk.len();
        chunks += 1;
    }
    assert_eq!(total, BIG, "stream delivered {total} bytes");
    println!("stream: {BIG} bytes in {chunks} chunks, all accounted for");
}

/// A SOCKS5 proxy small enough to be obviously correct: greet, accept
/// no-auth, take one CONNECT, then get out of the way.
async fn socks_proxy() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind socks");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        while let Ok((mut client, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut head = [0u8; 2];
                if client.read_exact(&mut head).await.is_err() {
                    return;
                }
                let mut methods = vec![0u8; head[1] as usize];
                if client.read_exact(&mut methods).await.is_err() {
                    return;
                }
                // Version 5, no authentication.
                if client.write_all(&[5, 0]).await.is_err() {
                    return;
                }

                let mut req = [0u8; 4];
                if client.read_exact(&mut req).await.is_err() {
                    return;
                }
                let dest = match req[3] {
                    1 => {
                        let mut v4 = [0u8; 4];
                        if client.read_exact(&mut v4).await.is_err() {
                            return;
                        }
                        std::net::IpAddr::from(v4).to_string()
                    }
                    3 => {
                        let mut n = [0u8; 1];
                        if client.read_exact(&mut n).await.is_err() {
                            return;
                        }
                        let mut host = vec![0u8; n[0] as usize];
                        if client.read_exact(&mut host).await.is_err() {
                            return;
                        }
                        String::from_utf8_lossy(&host).into_owned()
                    }
                    _ => return,
                };
                let mut port = [0u8; 2];
                if client.read_exact(&mut port).await.is_err() {
                    return;
                }
                let port = u16::from_be_bytes(port);

                let mut upstream = match TcpStream::connect((dest.as_str(), port)).await {
                    Ok(s) => s,
                    Err(_) => return,
                };
                // Success, bound address 0.0.0.0:0 (the client ignores it).
                if client.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await.is_err() {
                    return;
                }
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
        }
    });
    addr
}

async fn through_socks(base: &str) {
    let proxy_addr = socks_proxy().await;
    let http = builder()
        .proxy(reqwest::Proxy::all(format!("socks5://{proxy_addr}")).expect("socks proxy"))
        .build()
        .expect("build client");

    let body = http
        .get(format!("{base}/through-socks"))
        .send()
        .await
        .expect("send through socks")
        .text()
        .await
        .expect("body");
    assert_eq!(body, "through-socks", "socks path returned {body:?}");
    println!("socks5: request relayed through a local proxy on {proxy_addr}");
}

/// The blocking client runs its own runtime on a background thread, so it has
/// to be driven from somewhere that is allowed to block.
async fn blocking(base: &str) {
    let base = base.to_string();
    let body = tokio::task::spawn_blocking(move || {
        let http = reqwest::blocking::Client::builder()
            .tls_backend_preconfigured(tls_config())
            .timeout(Duration::from_secs(30))
            .build()
            .expect("build blocking client");
        http.get(format!("{base}/blocking"))
            .send()
            .expect("blocking send")
            .text()
            .expect("blocking body")
    })
    .await
    .expect("blocking task");

    assert_eq!(body, "blocking", "blocking client returned {body:?}");
    println!("blocking client: its own runtime, same server");
}
