//! Perf smoke: ingest THIS repo's src tree through the parallel parse path.
//! Run with `cargo run -p joey-neurocode --example ingest_perf`.

use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let home = std::env::temp_dir().join("ingest-perf-home");
    std::fs::create_dir_all(&home)?;
    let _guard = joey_core::constants::HomeOverrideGuard::new(home.clone());
    let graph = joey_neurocode::graph::DependencyGraph::open(&home.join("perf-graph.db"))?;
    let t0 = Instant::now();
    let res = joey_neurocode::parse::ingest_project(&graph, std::path::Path::new("."));
    println!(
        "ingest: {:?} ({} files, {} artifacts, {} edges, {} errors)",
        t0.elapsed(),
        res.files_scanned,
        res.artifacts_seen,
        res.edges_created,
        res.errors.len()
    );
    Ok(())
}
