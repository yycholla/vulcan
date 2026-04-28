//! YYC-246: SSRF protection for `web_fetch` / `web_search`.
//!
//! The LLM controls the URL passed into `web_fetch`. Without validation
//! the agent will happily reach AWS IMDS (`169.254.169.254`), localhost
//! ports (`http://localhost:6379`), or anything inside an RFC1918 LAN —
//! a textbook SSRF surface that prompt injection can exploit.
//!
//! [`validate`] parses the URL, refuses non-HTTP(S) schemes, then either
//! classifies the literal IP host or resolves the hostname and rejects
//! the request if any resolved address falls into a private/loopback/
//! link-local/multicast/etc. class. Defence in depth: even if one
//! resolved address is public, a *single* private one is enough to
//! refuse — DNS rebinding can flip them between the check and the
//! actual fetch otherwise.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use thiserror::Error;
use url::Url;

#[derive(Debug, Error)]
pub enum SsrfError {
    #[error("invalid URL `{url}`: {reason}")]
    Parse { url: String, reason: String },
    #[error("blocked URL scheme `{scheme}` (only http and https are allowed)")]
    BadScheme { scheme: String },
    #[error("URL has no host")]
    MissingHost,
    #[error("blocked URL `{url}`: host resolves to {addr} which is in the {class} address class")]
    BlockedAddress {
        url: String,
        addr: IpAddr,
        class: &'static str,
    },
    #[error("DNS resolution failed for `{host}`: {source}")]
    Resolve {
        host: String,
        #[source]
        source: std::io::Error,
    },
}

/// Parse + validate a URL. Resolves the hostname (if present) and
/// rejects any URL whose target IP sits in a non-public address class.
/// Returns the parsed [`Url`] on success.
pub async fn validate(raw: &str) -> Result<Url, SsrfError> {
    let parsed = Url::parse(raw).map_err(|e| SsrfError::Parse {
        url: raw.to_string(),
        reason: e.to_string(),
    })?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(SsrfError::BadScheme {
            scheme: scheme.to_string(),
        });
    }
    let host = parsed.host_str().ok_or(SsrfError::MissingHost)?.to_string();
    if let Ok(ip) = host.parse::<IpAddr>() {
        check_ip(raw, ip)?;
        return Ok(parsed);
    }
    let port = parsed.port_or_known_default().unwrap_or(80);
    let target = format!("{host}:{port}");
    let addrs = tokio::net::lookup_host(target.as_str())
        .await
        .map_err(|source| SsrfError::Resolve {
            host: host.clone(),
            source,
        })?;
    let mut any = false;
    for addr in addrs {
        any = true;
        check_ip(raw, addr.ip())?;
    }
    if !any {
        return Err(SsrfError::Resolve {
            host: host.clone(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "no addresses returned"),
        });
    }
    Ok(parsed)
}

fn check_ip(url: &str, addr: IpAddr) -> Result<(), SsrfError> {
    if let Some(class) = classify_blocked(addr) {
        return Err(SsrfError::BlockedAddress {
            url: url.to_string(),
            addr,
            class,
        });
    }
    Ok(())
}

fn classify_blocked(addr: IpAddr) -> Option<&'static str> {
    match addr {
        IpAddr::V4(v4) => classify_v4(v4),
        IpAddr::V6(v6) => classify_v6(v6),
    }
}

fn classify_v4(addr: Ipv4Addr) -> Option<&'static str> {
    let o = addr.octets();
    if addr.is_loopback() {
        return Some("loopback (127/8)");
    }
    if addr.is_unspecified() {
        return Some("unspecified (0.0.0.0)");
    }
    if addr.is_broadcast() {
        return Some("broadcast (255.255.255.255)");
    }
    if addr.is_multicast() {
        return Some("multicast (224/4)");
    }
    if o[0] == 10 {
        return Some("RFC1918 private (10/8)");
    }
    if o[0] == 172 && (16..=31).contains(&o[1]) {
        return Some("RFC1918 private (172.16/12)");
    }
    if o[0] == 192 && o[1] == 168 {
        return Some("RFC1918 private (192.168/16)");
    }
    if o[0] == 169 && o[1] == 254 {
        return Some("link-local / IMDS (169.254/16)");
    }
    if o[0] == 100 && (64..=127).contains(&o[1]) {
        return Some("CGNAT (100.64/10)");
    }
    if o[0] == 192 && o[1] == 0 && o[2] == 0 {
        return Some("IETF protocol assignments (192.0.0/24)");
    }
    if o[0] == 192 && o[1] == 0 && o[2] == 2 {
        return Some("documentation (192.0.2/24)");
    }
    if o[0] == 198 && (o[1] == 18 || o[1] == 19) {
        return Some("benchmark (198.18/15)");
    }
    if o[0] == 198 && o[1] == 51 && o[2] == 100 {
        return Some("documentation (198.51.100/24)");
    }
    if o[0] == 203 && o[1] == 0 && o[2] == 113 {
        return Some("documentation (203.0.113/24)");
    }
    if o[0] >= 240 {
        return Some("reserved (240/4)");
    }
    None
}

fn classify_v6(addr: Ipv6Addr) -> Option<&'static str> {
    if addr.is_loopback() {
        return Some("IPv6 loopback (::1)");
    }
    if addr.is_unspecified() {
        return Some("IPv6 unspecified (::)");
    }
    if addr.is_multicast() {
        return Some("IPv6 multicast (ff00::/8)");
    }
    let segs = addr.segments();
    let first = segs[0];
    if (first & 0xfe00) == 0xfc00 {
        return Some("IPv6 ULA (fc00::/7)");
    }
    if (first & 0xffc0) == 0xfe80 {
        return Some("IPv6 link-local (fe80::/10)");
    }
    if let Some(v4) = addr.to_ipv4_mapped() {
        if let Some(_class) = classify_v4(v4) {
            return Some("IPv4-mapped private/loopback");
        }
    }
    if (first & 0xff00) == 0x0100 {
        return Some("IPv6 discard (100::/64)");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_imds_endpoint() {
        let err = validate("http://169.254.169.254/latest/meta-data")
            .await
            .unwrap_err();
        match err {
            SsrfError::BlockedAddress { class, .. } => assert!(class.contains("169.254")),
            other => panic!("expected BlockedAddress, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_ipv4_loopback() {
        let err = validate("http://127.0.0.1:6379/").await.unwrap_err();
        assert!(matches!(err, SsrfError::BlockedAddress { .. }));
    }

    #[tokio::test]
    async fn rejects_ipv6_loopback() {
        let err = validate("http://[::1]/").await.unwrap_err();
        assert!(matches!(err, SsrfError::BlockedAddress { .. }));
    }

    #[tokio::test]
    async fn rejects_rfc1918_private_ranges() {
        for raw in [
            "http://10.0.0.1/",
            "http://172.16.0.1/",
            "http://172.31.255.254/",
            "http://192.168.1.1/",
        ] {
            let err = validate(raw).await.unwrap_err();
            assert!(
                matches!(err, SsrfError::BlockedAddress { .. }),
                "expected block for {raw}"
            );
        }
    }

    #[tokio::test]
    async fn rejects_link_local_and_cgnat() {
        for raw in ["http://169.254.5.6/", "http://100.64.0.1/"] {
            let err = validate(raw).await.unwrap_err();
            assert!(matches!(err, SsrfError::BlockedAddress { .. }));
        }
    }

    #[tokio::test]
    async fn rejects_unspecified_and_broadcast() {
        for raw in ["http://0.0.0.0/", "http://255.255.255.255/"] {
            let err = validate(raw).await.unwrap_err();
            assert!(matches!(err, SsrfError::BlockedAddress { .. }));
        }
    }

    #[tokio::test]
    async fn rejects_multicast_and_reserved() {
        for raw in ["http://224.0.0.1/", "http://240.0.0.1/"] {
            let err = validate(raw).await.unwrap_err();
            assert!(matches!(err, SsrfError::BlockedAddress { .. }));
        }
    }

    #[tokio::test]
    async fn rejects_ipv6_ula_and_link_local() {
        for raw in [
            "http://[fc00::1]/",
            "http://[fd12::1]/",
            "http://[fe80::1]/",
        ] {
            let err = validate(raw).await.unwrap_err();
            assert!(matches!(err, SsrfError::BlockedAddress { .. }));
        }
    }

    #[tokio::test]
    async fn rejects_ipv4_mapped_loopback_via_ipv6() {
        let err = validate("http://[::ffff:127.0.0.1]/").await.unwrap_err();
        assert!(matches!(err, SsrfError::BlockedAddress { .. }));
    }

    #[tokio::test]
    async fn rejects_non_http_schemes() {
        for raw in [
            "file:///etc/passwd",
            "ftp://example.com/",
            "gopher://example.com/",
            "data:text/plain,hello",
        ] {
            let err = validate(raw).await.unwrap_err();
            assert!(
                matches!(err, SsrfError::BadScheme { .. }),
                "expected BadScheme for {raw}, got {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn rejects_unparseable_url() {
        let err = validate("not a url").await.unwrap_err();
        assert!(matches!(err, SsrfError::Parse { .. }));
    }

    #[tokio::test]
    async fn allows_public_ipv4_literal() {
        // Cloudflare DNS — public anycast, will not match any private class.
        let ok = validate("https://1.1.1.1/").await.unwrap();
        assert_eq!(ok.host_str(), Some("1.1.1.1"));
    }

    #[tokio::test]
    async fn rejects_localhost_via_dns_resolution() {
        // `localhost` is configured in /etc/hosts to resolve to 127.0.0.1
        // (and possibly ::1). Confirms the resolver-based path catches
        // hostnames, not just IP literals.
        let err = validate("http://localhost/").await.unwrap_err();
        assert!(
            matches!(err, SsrfError::BlockedAddress { .. }),
            "expected BlockedAddress for localhost, got {err:?}"
        );
    }

    #[test]
    fn classify_v4_covers_documented_classes() {
        assert!(classify_v4(Ipv4Addr::new(127, 0, 0, 1)).is_some());
        assert!(classify_v4(Ipv4Addr::new(10, 0, 0, 1)).is_some());
        assert!(classify_v4(Ipv4Addr::new(172, 16, 0, 0)).is_some());
        assert!(classify_v4(Ipv4Addr::new(172, 31, 255, 254)).is_some());
        // Boundary check: 172.32.x is NOT private.
        assert!(classify_v4(Ipv4Addr::new(172, 32, 0, 0)).is_none());
        assert!(classify_v4(Ipv4Addr::new(192, 168, 0, 1)).is_some());
        assert!(classify_v4(Ipv4Addr::new(169, 254, 169, 254)).is_some());
        assert!(classify_v4(Ipv4Addr::new(8, 8, 8, 8)).is_none());
    }
}
