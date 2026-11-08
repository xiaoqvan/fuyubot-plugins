// In-process `NetProvider` implementation.
//
// blitz-dom routes every external resource (fonts, images, stylesheets,
// `<img>` sources, `@font-face` URLs) through a `NetProvider`. The default
// `DummyNetProvider` is a no-op, which silently drops every embedded asset.
//
// Upstream PR #3 adds `blitz-net` (reqwest + tokio) for this. That stack is
// pinned to blitz 0.2 / blitz-net 0.2 and cannot be used with the blitz
// 0.3.0-alpha.4 from PR #1, so this file provides the equivalent for our
// vendored stack:
//
// * `data:` URIs are decoded in-process (unchanged from PR #1);
// * `http(s)` images and fonts are fetched with a small blocking client;
// * everything else (including `file://`, remote CSS, private IPs) is ignored.

use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};
use std::sync::OnceLock;
use std::time::Duration;

use blitz_traits::net::{Bytes, NetHandler, NetProvider, Request};

/// Hard cap for a single remote resource (8 MiB).
const MAX_REMOTE_BYTES: usize = 8 * 1024 * 1024;

/// Per-request connect / read timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// How many redirects we follow (each hop is re-validated).
const MAX_REDIRECTS: usize = 3;

/// A `NetProvider` that resolves `data:` URIs and public `http(s)` images/fonts.
///
/// Remote fetching is deliberately conservative because the caller is often a
/// bot rendering untrusted HTML:
///
/// * only `http` / `https` on ports 80/443;
/// * loopback / private / link-local / CGNAT / multicast destinations are
///   rejected (basic SSRF protection, including DNS results);
/// * redirects are followed manually, re-validating every hop;
/// * only `image/*` and font content types are accepted;
/// * 5 s connect / 10 s read timeout and a 8 MiB size cap.
///
/// `file://` and remote stylesheets are intentionally not supported.
pub struct RemoteNetProvider;

impl NetProvider for RemoteNetProvider {
    fn fetch(&self, _doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        let url = request.url;
        let url_str = url.as_str().to_owned();

        if url_str.starts_with("data:") {
            let Ok(data_url) = data_url::DataUrl::process(&url_str) else {
                return;
            };
            let Ok((bytes, _fragment)) = data_url.decode_to_vec() else {
                return;
            };
            if bytes.len() > MAX_REMOTE_BYTES {
                return;
            }
            handler.bytes(url_str, Bytes::from(bytes));
            return;
        }

        if url.scheme() != "http" && url.scheme() != "https" {
            return;
        }

        let Some((resolved, bytes)) = fetch_remote(&url) else {
            return;
        };
        handler.bytes(resolved, Bytes::from(bytes));
    }
}

fn shared_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        let mut builder = ureq::AgentBuilder::new()
            .timeout_connect(CONNECT_TIMEOUT)
            .timeout_read(READ_TIMEOUT)
            .timeout_write(READ_TIMEOUT)
            // Redirects are followed manually so each hop can be validated.
            .redirects(0)
            .user_agent(concat!("fuyubot-html2img/", env!("CARGO_PKG_VERSION")));
        // Respect the same proxy environment variables as reqwest/curl.
        if let Some(proxy) = proxy_from_env() {
            builder = builder.proxy(proxy);
        }
        builder.build()
    })
}

fn proxy_from_env() -> Option<ureq::Proxy> {
    for key in [
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        if let Ok(value) = std::env::var(key) {
            if value.is_empty() {
                continue;
            }
            if let Ok(proxy) = ureq::Proxy::new(&value) {
                return Some(proxy);
            }
        }
    }
    None
}

fn fetch_remote(url: &blitz_traits::net::Url) -> Option<(String, Vec<u8>)> {
    let mut current = url.clone();
    for _ in 0..=MAX_REDIRECTS {
        if !is_fetchable(&current) {
            return None;
        }
        let response = match shared_agent().get(current.as_str()).call() {
            Ok(response) => response,
            Err(ureq::Error::Status(_, response)) => response,
            Err(_) => return None,
        };

        let status = response.status();
        if (300..400).contains(&status) {
            let location = response.header("location")?;
            current = current.join(location).ok()?;
            continue;
        }
        if !(200..300).contains(&status) {
            return None;
        }

        let content_type = response.header("content-type").unwrap_or_default();
        if !allowed_content_type(content_type, current.path()) {
            return None;
        }
        if let Some(length) = response.header("content-length") {
            if let Ok(length) = length.parse::<usize>() {
                if length > MAX_REMOTE_BYTES {
                    return None;
                }
            }
        }

        let mut bytes = Vec::new();
        response
            .into_reader()
            .take((MAX_REMOTE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() > MAX_REMOTE_BYTES {
            return None;
        }
        return Some((current.to_string(), bytes));
    }
    None
}

/// Basic SSRF guard: only public `http(s)` URLs on 80/443 are fetchable.
fn is_fetchable(url: &blitz_traits::net::Url) -> bool {
    if url.scheme() != "http" && url.scheme() != "https" {
        return false;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let Some(port) = url.port_or_known_default() else {
        return false;
    };
    if port != 80 && port != 443 {
        return false;
    }

    let lower = host.to_ascii_lowercase();
    if lower == "localhost" || lower.ends_with(".localhost") || lower.ends_with(".local") {
        return false;
    }

    if let Ok(ip) = host.parse::<IpAddr>() {
        return is_public_ip(ip);
    }

    // Resolve first and reject if *any* address is private. (There is still a
    // small DNS-rebinding window because ureq resolves again; an allowlist is
    // the only complete fix, which is out of scope for this provider.)
    let Ok(addresses) = (host, port).to_socket_addrs() else {
        return false;
    };
    let mut resolved = false;
    for address in addresses {
        resolved = true;
        if !is_public_ip(address.ip()) {
            return false;
        }
    }
    resolved
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_v4(ip),
        IpAddr::V6(ip) => is_public_v6(ip),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || ip.is_multicast()
        || octets[0] == 0
        // Carrier-grade NAT (100.64.0.0/10).
        || (octets[0] == 100 && (64..=127).contains(&octets[1])))
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_unique_local()
        || ip.is_unicast_link_local()
        || ip.is_multicast()
        || ip
            .to_ipv4_mapped()
            .is_some_and(|mapped| !is_public_v4(mapped)))
}

fn allowed_content_type(content_type: &str, path: &str) -> bool {
    let content_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if content_type.is_empty() {
        return has_font_extension(path);
    }
    content_type.starts_with("image/")
        || content_type.starts_with("font/")
        || content_type.starts_with("application/font")
        || content_type == "application/vnd.ms-fontobject"
        || (content_type == "application/octet-stream" && has_font_extension(path))
}

fn has_font_extension(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    [".woff2", ".woff", ".ttf", ".otf", ".ttc"]
        .iter()
        .any(|extension| path.ends_with(extension))
}

#[cfg(test)]
mod tests {
    use super::*;
    use blitz_traits::net::Url;
    use std::sync::{Arc, Mutex};

    /// Test double for `NetHandler` that records the single `bytes(...)`
    /// invocation it receives, so tests can assert what the provider passed
    /// downstream (or that nothing was passed at all).
    struct CapturingHandler {
        captured: Arc<Mutex<Option<(String, Bytes)>>>,
    }

    impl NetHandler for CapturingHandler {
        fn bytes(self: Box<Self>, resolved_url: String, bytes: Bytes) {
            *self.captured.lock().unwrap() = Some((resolved_url, bytes));
        }
    }

    fn request_for(url: &str) -> Request {
        Request::get(Url::parse(url).expect("test URL must be valid"))
    }

    fn run(provider: &RemoteNetProvider, url: &str) -> Option<(String, Bytes)> {
        let captured: Arc<Mutex<Option<(String, Bytes)>>> = Arc::new(Mutex::new(None));
        let handler = Box::new(CapturingHandler {
            captured: Arc::clone(&captured),
        });
        provider.fetch(0, request_for(url), handler);
        let result = captured.lock().unwrap().take();
        result
    }

    #[test]
    fn resolves_base64_data_url() {
        // base64 of "Hello" → SGVsbG8=
        let result = run(&RemoteNetProvider, "data:text/plain;base64,SGVsbG8=");
        let (resolved_url, bytes) = result.expect("handler must be invoked for a valid data URL");
        assert!(resolved_url.starts_with("data:"));
        assert_eq!(bytes.as_ref(), b"Hello");
    }

    #[test]
    fn resolves_percent_encoded_data_url() {
        let result = run(&RemoteNetProvider, "data:text/plain,Hello%20World");
        let (_, bytes) = result.expect("handler must be invoked for a valid data URL");
        assert_eq!(bytes.as_ref(), b"Hello World");
    }

    #[test]
    fn refuses_private_and_local_urls_without_fetching() {
        for url in [
            "http://127.0.0.1/a.png",
            "http://localhost/a.png",
            "http://10.0.0.1/a.png",
            "http://169.254.169.254/latest/meta-data/",
            "http://[::1]/a.png",
            "http://100.64.0.1/a.png",
            "https://192.168.1.1/a.png",
            "file:///etc/passwd",
        ] {
            let parsed = Url::parse(url).unwrap();
            assert!(!is_fetchable(&parsed), "{url} must be rejected");
        }
    }

    #[test]
    fn refuses_nonstandard_ports_and_userinfo() {
        let parsed = Url::parse("http://example.com:8080/a.png").unwrap();
        assert!(!is_fetchable(&parsed));
        let parsed = Url::parse("https://user:pass@example.com/a.png").unwrap();
        assert!(!is_fetchable(&parsed));
    }

    #[test]
    fn content_type_filter() {
        assert!(allowed_content_type("image/png", "/a"));
        assert!(allowed_content_type("image/jpeg; charset=binary", "/a"));
        assert!(allowed_content_type("font/woff2", "/a"));
        assert!(allowed_content_type("", "/font.woff2"));
        assert!(!allowed_content_type("text/html", "/a"));
        assert!(!allowed_content_type("application/json", "/a"));
        assert!(!allowed_content_type("", "/a"));
    }

    #[test]
    fn public_ip_detection() {
        assert!(is_public_ip("1.1.1.1".parse().unwrap()));
        assert!(is_public_ip("2606:4700:4700::1111".parse().unwrap()));
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.0.1",
            "169.254.1.1",
            "0.0.0.0",
            "100.64.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
        ] {
            assert!(!is_public_ip(ip.parse().unwrap()), "{ip} must be private");
        }
    }
}
