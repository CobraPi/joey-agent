use std::time::Duration;
use joey_browser::config::{BrowserConfig, HeadlessPolicy};
use joey_browser::session::BrowserManager;

#[tokio::main]
async fn main() {
    let cfg = BrowserConfig { headless: HeadlessPolicy::Always, allow_local_urls: true, ..BrowserConfig::default() };
    let m = BrowserManager::connect(cfg).await.expect("connect");
    let page = m.ensure_page().await.expect("page");
    println!("page: {} session: {}", page.target_id, page.session_id);
    let nav = m.raw_eval_diag("Page.navigate", serde_json::json!({"url":"https://example.com"})).await;
    println!("raw nav: {nav:?}");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let ev = m.raw_eval_diag("Runtime.evaluate", serde_json::json!({"expression":"document.title","returnByValue":true})).await;
    println!("raw eval: {ev:?}");
    m.disconnect_arc().await.unwrap();
}
