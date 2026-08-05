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

    // FR-018/SC-014: start-up history expiry sweep + hourly periodic sweep.
    {
        let sweep_home = state.joey_home();
        tokio::spawn(async move {
            loop {
                let now = chrono::Utc::now();
                match joey_speckit_ui::history::sweep_expired(&sweep_home, now) {
                    Ok(removed) if removed > 0 => {
                        tracing::info!(removed, "history expiry sweep removed records");
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "history expiry sweep failed");
                    }
                }
                // Run hourly.
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            }
        });
    }

    // FR-033: restart recovery — scan for in-progress attempts on startup.
    {
        let recovery_home = state.joey_home();
        let recovery_state = state.clone();
        tokio::spawn(async move {
            let recoverable = joey_speckit_ui::recovery::scan_all_for_recovery(&recovery_home);
            for (feature_id, attempts) in recoverable {
                for mut attempt in attempts {
                    tracing::info!(
                        attempt = %attempt.attempt_id,
                        feature = %feature_id,
                        status = ?attempt.status,
                        "recovering in-progress attempt on startup"
                    );
                    match joey_speckit_ui::recovery::evaluate_recovery(&attempt) {
                        joey_speckit_ui::recovery::RecoveryOutcome::Resume { .. } => {
                            let _ = joey_speckit_ui::recovery::mark_resumed(
                                &recovery_home,
                                &mut attempt,
                            );
                        }
                        joey_speckit_ui::recovery::RecoveryOutcome::Failed { .. } => {
                            let _ = joey_speckit_ui::recovery::mark_recovery_failed(
                                &recovery_home,
                                &mut attempt,
                            );
                        }
                    }
                }
            }
            let _ = recovery_state;
        });
    }

    let app = api::build_router(state);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    tracing::info!(%addr, "joey-speckit-ui backend listening");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
