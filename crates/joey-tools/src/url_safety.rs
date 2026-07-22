//! URL safety checks — port of `tools/url_safety.py` (`is_safe_url` and the
//! sensitive-query-parameter detector).
//!
//! Blocks SSRF targets: cloud-metadata hostnames/IPs (always), and
//! private/loopback/link-local/reserved/multicast/unspecified/CGNAT addresses
//! unless `security.allow_private_urls` (or `JOEY_ALLOW_PRIVATE_URLS`) is
//! enabled. DNS is resolved and EVERY answer checked; resolution failure fails
//! closed.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use joey_core::Config;
use url::Url;

/// Hostnames that are always blocked regardless of IP resolution or toggles.
const BLOCKED_HOSTNAMES: &[&str] = &["metadata.google.internal", "metadata.goog"];

/// Exact HTTPS hostnames allowed to resolve to private/benchmark-space IPs.
const TRUSTED_PRIVATE_IP_HOSTS: &[&str] = &["multimedia.nt.qq.com.cn"];

fn always_blocked_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    // 169.254.0.0/16 — entire link-local range (includes 169.254.169.254,
    // 169.254.170.2, 169.254.169.253).
    if o[0] == 169 && o[1] == 254 {
        return true;
    }
    // Alibaba Cloud metadata.
    ip == Ipv4Addr::new(100, 100, 100, 200)
}

fn always_blocked(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => always_blocked_v4(*v4),
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return always_blocked_v4(mapped);
            }
            // fd00:ec2::254 — AWS metadata (IPv6).
            *v6 == "fd00:ec2::254".parse::<Ipv6Addr>().unwrap()
        }
    }
}

fn in_cgnat(v4: Ipv4Addr) -> bool {
    // 100.64.0.0/10 (RFC 6598) — not covered by is_private.
    let o = v4.octets();
    o[0] == 100 && (64..128).contains(&o[1])
}

/// Python `ipaddress` blocked-class union for IPv4:
/// is_private | is_loopback | is_link_local | is_reserved | is_multicast |
/// is_unspecified | CGNAT.
fn blocked_class_v4(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    v4.is_private()                       // 10/8, 172.16/12, 192.168/16
        || v4.is_loopback()               // 127/8
        || v4.is_link_local()             // 169.254/16
        || v4.is_multicast()              // 224/4
        || v4.is_broadcast()              // 255.255.255.255
        || v4.is_documentation()          // 192.0.2/24, 198.51.100/24, 203.0.113/24
        || o[0] == 0                      // 0.0.0.0/8 (Python is_private)
        || o[0] >= 240                    // 240/4 (Python is_reserved)
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)     // 192.0.0.0/24 (IETF protocol)
        || (o[0] == 198 && (o[1] == 18 || o[1] == 19)) // 198.18.0.0/15 (benchmarking)
        || in_cgnat(v4)
}

fn blocked_class_v6(v6: Ipv6Addr) -> bool {
    if let Some(mapped) = v6.to_ipv4_mapped() {
        return blocked_class_v4(mapped);
    }
    let seg = v6.segments();
    v6.is_loopback()                      // ::1
        || v6.is_unspecified()            // ::
        || v6.is_multicast()              // ff00::/8
        || (seg[0] & 0xffc0) == 0xfe80    // fe80::/10 link-local
        || (seg[0] & 0xfe00) == 0xfc00    // fc00::/7 unique-local (Python is_private)
        || (seg[0] == 0x100 && seg[1] == 0 && seg[2] == 0 && seg[3] == 0) // 100::/64 discard
        || (seg[0] == 0x2001 && seg[1] == 0xdb8) // 2001:db8::/32 documentation
}

/// Port of `_is_blocked_ip`.
pub fn is_blocked_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => blocked_class_v4(*v4),
        IpAddr::V6(v6) => blocked_class_v6(*v6),
    }
}

/// A DNS resolver hook so tests can inject fixed answers.
pub type Resolver = dyn Fn(&str) -> std::io::Result<Vec<IpAddr>> + Send + Sync;

fn system_resolve(hostname: &str) -> std::io::Result<Vec<IpAddr>> {
    use std::net::ToSocketAddrs;
    let addrs = (hostname, 0u16).to_socket_addrs()?;
    Ok(addrs.map(|a| a.ip()).collect())
}

fn global_allow_private_urls(config: &Config) -> bool {
    // 1. Env var override (highest priority).
    if let Ok(v) = std::env::var("JOEY_ALLOW_PRIVATE_URLS") {
        match v.trim().to_lowercase().as_str() {
            "true" | "1" | "yes" => return true,
            "false" | "0" | "no" => return false,
            _ => {}
        }
    }
    // 2. Config file: security.allow_private_urls, legacy browser.allow_private_urls.
    config.get_bool("security.allow_private_urls", false)
        || config.get_bool("browser.allow_private_urls", false)
}

/// Port of `is_safe_url` — returns `true` when the URL target is not a
/// private/internal address. Fails closed on DNS errors and parse errors.
pub fn is_safe_url(url: &str, config: &Config) -> bool {
    is_safe_url_with_resolver(url, config, &system_resolve)
}

/// Testable variant taking an explicit resolver.
pub fn is_safe_url_with_resolver(
    url: &str,
    config: &Config,
    resolve: &(impl Fn(&str) -> std::io::Result<Vec<IpAddr>> + ?Sized),
) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        tracing::warn!("Blocked request — URL safety check error for {}", url);
        return false;
    };
    let scheme = parsed.scheme().to_lowercase();
    if scheme != "http" && scheme != "https" {
        tracing::warn!("Blocked request — unsupported URL scheme: {}", scheme);
        return false;
    }
    let hostname = match parsed.host_str() {
        Some(h) => h.trim().to_lowercase().trim_end_matches('.').to_string(),
        None => return false,
    };
    if hostname.is_empty() {
        return false;
    }

    // Block known internal hostnames — ALWAYS, even with the toggle on.
    if BLOCKED_HOSTNAMES.contains(&hostname.as_str()) {
        tracing::warn!("Blocked request to internal hostname: {}", hostname);
        return false;
    }

    let allow_all_private = global_allow_private_urls(config);
    let allow_private_ip = scheme == "https" && TRUSTED_PRIVATE_IP_HOSTS.contains(&hostname.as_str());

    // Literal IPs (url crate brackets v6 hosts).
    let bare = hostname.trim_start_matches('[').trim_end_matches(']');
    let resolved_ips: Vec<IpAddr> = if let Ok(ip) = bare.parse::<IpAddr>() {
        vec![ip]
    } else {
        match resolve(&hostname) {
            Ok(ips) => ips,
            Err(_) => {
                // DNS resolution failed — fail closed.
                tracing::warn!("Blocked request — DNS resolution failed for: {}", hostname);
                return false;
            }
        }
    };

    for ip in resolved_ips {
        // Always block cloud metadata IPs and link-local, even with toggle on.
        if always_blocked(&ip) {
            tracing::warn!("Blocked request to cloud metadata address: {} -> {}", hostname, ip);
            return false;
        }
        if !allow_all_private && !allow_private_ip && is_blocked_ip(&ip) {
            tracing::warn!(
                "Blocked request to private/internal address: {} -> {}",
                hostname,
                ip
            );
            return false;
        }
    }

    true
}

/// Query parameter names that are unambiguously credential-bearing
/// (`_SENSITIVE_QUERY_PARAM_NAMES`).
const SENSITIVE_QUERY_PARAM_NAMES: &[&str] = &[
    "access_token",
    "api_key",
    "apikey",
    "auth_token",
    "authorization",
    "awsaccesskeyid",
    "client_secret",
    "credential",
    "credentials",
    "jwt",
    "password",
    "passwd",
    "secret",
    "session_id",
    "signature",
    "token",
    "x_amz_security_token",
    "x_amz_signature",
    "x-amz-security-token",
    "x-amz-signature",
];

/// Port of `sensitive_query_param_name` — the first credential-named query
/// parameter with a non-empty value, if any.
pub fn sensitive_query_param_name(url: &str) -> Option<String> {
    if !url.contains('?') {
        return None;
    }
    let parsed = Url::parse(url.trim()).ok()?;
    let scheme = parsed.scheme().to_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    for (key, value) in parsed.query_pairs() {
        if !value.is_empty() && SENSITIVE_QUERY_PARAM_NAMES.contains(&key.to_lowercase().as_str()) {
            return Some(key.into_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config::defaults()
    }

    fn no_dns(_: &str) -> std::io::Result<Vec<IpAddr>> {
        Err(std::io::Error::new(std::io::ErrorKind::Other, "no dns in tests"))
    }

    #[test]
    fn scheme_gate() {
        assert!(!is_safe_url_with_resolver("ftp://example.com/x", &cfg(), &no_dns));
        assert!(!is_safe_url_with_resolver("file:///etc/passwd", &cfg(), &no_dns));
    }

    #[test]
    fn metadata_ips_blocked() {
        let _guard = crate::test_env_lock();
        std::env::remove_var("JOEY_ALLOW_PRIVATE_URLS");
        for u in [
            "http://169.254.169.254/latest/meta-data/",
            "http://169.254.170.2/",
            "http://169.254.169.253/",
            "http://100.100.100.200/",
            "http://[fd00:ec2::254]/",
            "http://[::ffff:169.254.169.254]/",
            "http://metadata.google.internal/computeMetadata/v1/",
            "http://metadata.goog/",
        ] {
            assert!(!is_safe_url_with_resolver(u, &cfg(), &no_dns), "{} must be blocked", u);
        }
    }

    #[test]
    fn private_classes_blocked() {
        let _guard = crate::test_env_lock();
        std::env::remove_var("JOEY_ALLOW_PRIVATE_URLS");
        for u in [
            "http://127.0.0.1/",
            "http://10.0.0.5/",
            "http://172.16.1.1/",
            "http://192.168.1.1/",
            "http://100.64.0.1/",       // CGNAT
            "http://0.0.0.0/",
            "http://224.0.0.1/",        // multicast
            "http://240.0.0.1/",        // reserved
            "http://[::1]/",
            "http://[fe80::1]/",
            "http://[fc00::1]/",
            "http://[::ffff:10.0.0.1]/", // mapped private
        ] {
            assert!(!is_safe_url_with_resolver(u, &cfg(), &no_dns), "{} must be blocked", u);
        }
    }

    #[test]
    fn dns_fail_closed_and_answers_checked() {
        let _guard = crate::test_env_lock();
        std::env::remove_var("JOEY_ALLOW_PRIVATE_URLS");
        // DNS failure → blocked.
        assert!(!is_safe_url_with_resolver("https://example.com/", &cfg(), &no_dns));
        // One private answer among public ones → blocked.
        let mixed = |_: &str| -> std::io::Result<Vec<IpAddr>> {
            Ok(vec!["93.184.216.34".parse().unwrap(), "10.0.0.1".parse().unwrap()])
        };
        assert!(!is_safe_url_with_resolver("https://example.com/", &cfg(), &mixed));
        // All-public answers → allowed.
        let public = |_: &str| -> std::io::Result<Vec<IpAddr>> {
            Ok(vec!["93.184.216.34".parse().unwrap()])
        };
        assert!(is_safe_url_with_resolver("https://example.com/", &cfg(), &public));
        // Metadata answer → blocked even though hostname looks public.
        let evil = |_: &str| -> std::io::Result<Vec<IpAddr>> {
            Ok(vec!["169.254.169.254".parse().unwrap()])
        };
        assert!(!is_safe_url_with_resolver("https://example.com/", &cfg(), &evil));
    }

    #[test]
    fn allow_private_toggle_keeps_metadata_blocked() {
        let _guard = crate::test_env_lock();
        std::env::set_var("JOEY_ALLOW_PRIVATE_URLS", "true");
        let private = |_: &str| -> std::io::Result<Vec<IpAddr>> {
            Ok(vec!["192.168.1.10".parse().unwrap()])
        };
        assert!(is_safe_url_with_resolver("http://router.local/", &cfg(), &private));
        // Metadata endpoints stay blocked regardless.
        assert!(!is_safe_url_with_resolver("http://169.254.169.254/", &cfg(), &private));
        assert!(!is_safe_url_with_resolver("http://metadata.google.internal/", &cfg(), &private));
        std::env::remove_var("JOEY_ALLOW_PRIVATE_URLS");
    }

    #[test]
    fn sensitive_query_params() {
        assert_eq!(
            sensitive_query_param_name("https://x.test/cb?token=abc").as_deref(),
            Some("token")
        );
        assert_eq!(
            sensitive_query_param_name("https://x.test/cb?API_KEY=abc").as_deref(),
            Some("API_KEY")
        );
        assert_eq!(sensitive_query_param_name("https://x.test/cb?token="), None);
        assert_eq!(sensitive_query_param_name("https://x.test/page?code=SAVE20"), None);
        assert_eq!(sensitive_query_param_name("https://x.test/plain"), None);
    }
}
