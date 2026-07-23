use std::path::PathBuf;

use joey_speckit_ui::{api, AppState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let repo_root: PathBuf = std::env::var("JOEY_SPECKIT_UI_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().expect("current dir"));

    let port: u16 = std::env::var("JOEY_SPECKIT_UI_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(4173);

    let state = AppState::new(repo_root);
    let app = api::build_router(state);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    tracing::info!(%addr, "joey-speckit-ui backend listening");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
