//! URL-safety bridge: joey-browser cannot depend on joey-tools (DAG:
//! joey-tools depends on joey-browser), so the check is injected via a
//! function pointer set at wiring time by the higher crate (FR-020 —
//! reuses the SAME url_safety::is_safe_url the web tools use).

use std::sync::RwLock;

type CheckFn = fn(&str) -> Result<(), String>;

static INSTALLED: RwLock<Option<CheckFn>> = RwLock::new(None);

/// Install the real checker (called once during wiring from joey-tools or
/// joey-cli). Falls back to a conservative default until installed.
pub fn install_url_safety_check(f: CheckFn) {
    let mut guard = INSTALLED.write().expect("url-safety lock");
    *guard = Some(f);
}

/// Normalize a browser-accepted non-canonical IPv4 host to a u32:
/// a whole-host decimal integer (`2130706433` = 127.0.0.1), a whole-host
/// 0x-hex integer (`0x7f000001`), or four dotted segments where at least
/// one is 0x-hex or octal-looking (`0x7f.0.0.1`, `0177.0.0.1`). Plain
/// dotted decimal is intentionally NOT handled here — it falls through
/// to the caller's dotted-quad check unchanged.
fn to_ipv4_u32(host: &str) -> Option<u32> {
    if !host.contains('.') {
        if let Some(hex) = host.strip_prefix("0x").or_else(|| host.strip_prefix("0X")) {
            return u32::from_str_radix(hex, 16).ok();
        }
        if !host.is_empty() && host.chars().all(|c| c.is_ascii_digit()) {
            return host.parse::<u32>().ok();
        }
        return None;
    }
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    // Only claim the host when at least one segment is non-plain-decimal.
    let looks_non_decimal = |p: &str| {
        p.starts_with("0x")
            || p.starts_with("0X")
            || (p.len() > 1 && p.starts_with('0') && p[1..].chars().all(|c| c.is_ascii_digit()))
    };
    if !parts.iter().any(|p| looks_non_decimal(p)) {
        return None;
    }
    let mut ip: u32 = 0;
    for p in parts {
        let v = if let Some(hex) = p.strip_prefix("0x").or_else(|| p.strip_prefix("0X")) {
            u32::from_str_radix(hex, 16).ok()?
        } else if p.len() > 1 && p.starts_with('0') {
            // Octal-looking segment (leading 0 + digits); invalid octal
            // digits (e.g. `08`) fail and fall back to the dotted check.
            u32::from_str_radix(&p[1..], 8).ok()?
        } else {
            p.parse::<u32>().ok()?
        };
        if v > 0xff {
            return None;
        }
        ip = (ip << 8) | v;
    }
    Some(ip)
}

fn default_check(url: &str) -> Result<(), String> {
    // Conservative default: block loopback/private ranges via std only.
    // Mirrors joey-tools url_safety policy until the real checker is wired.
    let parsed = match url::Url::parse(url) {
        Ok(u) => u,
        Err(_) => return Err(format!("invalid URL: {url}")),
    };
    let host = parsed.host_str().unwrap_or("");
    let blocked_octets = |a: u32, b: u32| {
        a == 10
            || a == 127
            || a == 0
            || (a == 172 && (16..=31).contains(&b))
            || (a == 192 && b == 168)
            || (a == 169 && b == 254)
    };
    let is_private_ipv4 = |h: &str| {
        // Non-dotted IPv4 forms bypass a dotted-quad-only check (browsers
        // accept http://2130706433/ = 127.0.0.1, hex 0x7f.0.0.1, octal
        // 0177.0.0.1): normalize those to a u32 first and check the
        // resulting range; plain dotted quads fall through unchanged.
        if let Some(ip) = to_ipv4_u32(h) {
            let a = (ip >> 24) & 0xff;
            let b = (ip >> 16) & 0xff;
            return blocked_octets(a, b);
        }
        let quad: Vec<u32> = h
            .split('.')
            .filter_map(|o| o.parse::<u32>().ok())
            .collect();
        if quad.len() == 4 {
            blocked_octets(quad[0], quad[1])
        } else {
            false
        }
    };
    let blocked = matches!(host, "localhost" | "::1" | "[::1]" | "" | "metadata.google.internal")
        || is_private_ipv4(host);
    if blocked {
        Err(format!("local/private network target refused: {host}"))
    } else {
        Ok(())
    }
}

/// Run the active URL-safety check (installed one wins; else default).
pub fn url_safety_check(url: &str) -> Result<(), String> {
    let guard = INSTALLED.read().expect("url-safety lock");
    match *guard {
        Some(f) => f(url),
        None => default_check(url),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The bridge's `INSTALLED` is process-global state; tests that touch it
    /// (or rely on the default) must not interleave. Serializes
    /// `default_blocks_local_and_private` against `injected_check_is_used_then_restored`.
    static BRIDGE_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn default_blocks_local_and_private() {
        let _guard = BRIDGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        assert!(url_safety_check("http://127.0.0.1:8080/").is_err());
        assert!(url_safety_check("http://localhost/x").is_err());
        assert!(url_safety_check("http://192.168.1.1/").is_err());
        assert!(url_safety_check("http://10.0.0.5/").is_err());
        assert!(url_safety_check("http://172.31.0.1/").is_err());
        assert!(url_safety_check("http://[::1]/").is_err());
        assert!(url_safety_check("not a url").is_err());
    }

    #[test]
    fn default_blocks_non_dotted_ipv4_forms() {
        let _guard = BRIDGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Whole-host integer forms of loopback/private addresses.
        assert!(url_safety_check("http://2130706433/").is_err()); // decimal 127.0.0.1
        assert!(url_safety_check("http://0x7f000001/").is_err()); // hex 127.0.0.1
        assert!(url_safety_check("http://3232235777/").is_err()); // decimal 192.168.1.1
        // Hex / octal dotted segments.
        assert!(url_safety_check("http://0x7f.0.0.1/").is_err());
        assert!(url_safety_check("http://0177.0.0.1/").is_err());
        // Plain dotted forms still gated (regression).
        assert!(url_safety_check("http://192.168.1.1/").is_err());
        // Public addresses stay allowed, dotted and non-dotted.
        assert!(url_safety_check("http://8.8.8.8/").is_ok());
        assert!(url_safety_check("http://134744072/").is_ok()); // decimal 8.8.8.8
    }

    #[test]
    fn default_allows_public() {
        let _guard = BRIDGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        assert!(url_safety_check("https://example.com/").is_ok());
        assert!(url_safety_check("https://portal.pega.com/").is_ok());
        // 172.32+ is public space — must NOT be blocked by the 172.16/31 rule.
        assert!(url_safety_check("http://172.32.0.1/").is_ok());
    }

    #[test]
    fn injected_check_is_used_then_restored() {
        use std::sync::atomic::Ordering;

        let _guard = BRIDGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        static CALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        fn mark_called(_u: &str) -> Result<(), String> {
            CALLED.store(true, Ordering::SeqCst);
            Ok(())
        }
        install_url_safety_check(mark_called);
        assert!(url_safety_check("https://x.test/").is_ok());
        assert!(CALLED.load(Ordering::SeqCst));
        // Restore no-check state for other tests.
        INSTALLED.write().unwrap().take();
    }
}
