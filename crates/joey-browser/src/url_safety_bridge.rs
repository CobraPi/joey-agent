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

fn default_check(url: &str) -> Result<(), String> {
    // Conservative default: block loopback/private ranges via std only.
    // Mirrors joey-tools url_safety policy until the real checker is wired.
    let parsed = match url::Url::parse(url) {
        Ok(u) => u,
        Err(_) => return Err(format!("invalid URL: {url}")),
    };
    let host = parsed.host_str().unwrap_or("");
    let is_private_ipv4 = |h: &str| {
        let quad: Vec<u32> = h
            .split('.')
            .filter_map(|o| o.parse::<u32>().ok())
            .collect();
        if quad.len() == 4 {
            let [a, b, _, _] = [quad[0], quad[1], quad[2], quad[3]];
            a == 10
                || a == 127
                || a == 0
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 168)
                || (a == 169 && b == 254)
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

    #[test]
    fn default_blocks_local_and_private() {
        assert!(url_safety_check("http://127.0.0.1:8080/").is_err());
        assert!(url_safety_check("http://localhost/x").is_err());
        assert!(url_safety_check("http://192.168.1.1/").is_err());
        assert!(url_safety_check("http://10.0.0.5/").is_err());
        assert!(url_safety_check("http://172.31.0.1/").is_err());
        assert!(url_safety_check("http://[::1]/").is_err());
        assert!(url_safety_check("not a url").is_err());
    }

    #[test]
    fn default_allows_public() {
        assert!(url_safety_check("https://example.com/").is_ok());
        assert!(url_safety_check("https://portal.pega.com/").is_ok());
        // 172.32+ is public space — must NOT be blocked by the 172.16/31 rule.
        assert!(url_safety_check("http://172.32.0.1/").is_ok());
    }

    #[test]
    fn injected_check_is_used_then_restored() {
        use std::sync::atomic::Ordering;

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
