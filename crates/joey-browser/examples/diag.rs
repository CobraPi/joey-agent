use joey_browser::config::{BrowserConfig, HeadlessPolicy};
use joey_browser::session::BrowserManager;

#[tokio::main]
async fn main() {
    let cfg = BrowserConfig { headless: HeadlessPolicy::Always, allow_local_urls: true, ..BrowserConfig::default() };
    let m = BrowserManager::connect(cfg).await.expect("connect");
    // Inject a DOM into about:blank, then scan.
    let _ = m.evaluate("document.body.innerHTML = '<button id=\"b1\">Alpha</button><a href=\"#\">Beta link</a><input value=\"Gamma\">'").await;
    let c1 = m.eval_string("String(document.querySelectorAll('button').length)").await;
    println!("plain button count eval: {c1:?}");
    // SCAN_JS raw result type check
    let raw = m.evaluate(joey_browser::extract::SCAN_JS).await;
    println!("scan evaluate type: {}", match &raw { Ok(v) => format!("{:?}", v), Err(e) => format!("ERR {e}") }.chars().take(400).collect::<String>());
    let mut reg = m.scan_to_registry().await;
    if let Ok(r) = reg.as_mut() {
        println!("registry: {}", r.elements.len());
        for e in r.elements.iter().take(5) { println!("  elem: {:?}", e); }
    } else {
        println!("registry ERR: {reg:?}");
    }
    m.disconnect_arc().await.unwrap();
}
