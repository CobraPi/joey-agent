//! Ingest a FROZEN copy of the HEAD tree (stable input for A/B comparison).
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let home = std::env::temp_dir().join("ingest-frozen-home");
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home)?;
    let _guard = joey_core::constants::HomeOverrideGuard::new(home.clone());
    let graph = joey_neurocode::graph::DependencyGraph::open(&home.join("frozen.db"))?;
    let t0 = Instant::now();
    let res = joey_neurocode::parse::ingest_project(&graph, std::path::Path::new("/tmp/frozen-tree"));
    println!(
        "ingest-frozen: {:?} ({} files, {} artifacts, {} edges, {} errors)",
        t0.elapsed(),
        res.files_scanned,
        res.artifacts_seen,
        res.edges_created,
        res.errors.len()
    );
    Ok(())
}
