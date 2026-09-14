//! MANUAL validation (feature 030, T031 v2): quickstart M1/M2 executed
//! against the REAL provider configured in ~/.joey (zai glm-5.3). These
//! tests are `#[ignore]`d because they cost real provider tokens; run via:
//!
//!   cargo test -p joey-orchestration --test manual_m1m2 -- --ignored --test-threads=1
//!
//! M1 (end-to-end feel): governance ENABLED with data_dir None, i.e. real
//! ~/.joey/delegation paths — 3 concurrent identical dispatches must all
//! succeed, append exactly 3 resource records, and create the persistent
//! result cache at ~/.joey/delegation/result-cache.json.
//!
//! M2 (governance-off parity): governance DISABLED with a tempdir
//! data_dir — 2 dispatches succeed, ZERO files appear in the tempdir, and
//! the real-home record count is unchanged (pre-feature behavior).
//!
//! JOEY_HOME is deliberately NOT overridden: M1 exercising the real home
//! is the point; M2's assertions are on the tempdir + home-count delta.

use joey_agent_core::AgentConfig;
use joey_core::Config;
use joey_orchestration::governance::GovernanceConfig;
use joey_orchestration::resource_records::ResourceRecordStore;
use joey_orchestration::{DelegationRequest, ManagerConfig, SubagentManager};
use joey_tools::ToolRegistry;

#[ignore = "manual M1: costs real provider tokens"]
#[tokio::test]
async fn m1_end_to_end_feel() {
    let cfg = Config::load().expect("Config::load");
    let ac = AgentConfig::from_config(&cfg);
    let mgr = SubagentManager::new(ManagerConfig {
        governance: GovernanceConfig {
            enabled: true,
            task_timeout_secs: 120,
            data_dir: None,
            ..Default::default()
        },
        ..Default::default()
    });

    let store = ResourceRecordStore::open(None);
    let before = store.count();

    let registry = ToolRegistry::new(); // no tools
    let reqs: Vec<DelegationRequest> = (0..3)
        .map(|_| {
            let mut r = DelegationRequest::single("Reply with exactly: OK");
            r.max_turns = Some(2);
            r
        })
        .collect();
    let futs = reqs
        .iter()
        .map(|req| mgr.dispatch_single(req, &ac, &cfg, &registry, None));
    let results = futures::future::join_all(futs).await;

    for (i, r) in results.iter().enumerate() {
        assert!(
            r.success,
            "M1 dispatch {i} failed: {:?}",
            r.error.as_deref().unwrap_or("<no error text>")
        );
        println!(
            "M1 dispatch {i}: success=true wall_clock={:.3}s iterations={} model={}",
            r.wall_clock.as_secs_f64(),
            r.iterations,
            r.model
        );
    }

    let after = store.count();
    assert_eq!(
        after - before,
        3,
        "M1 expected exactly 3 new resource records (before={before}, after={after})"
    );

    let cache_path = std::path::PathBuf::from(std::env::var("HOME").unwrap())
        .join(".joey/delegation/result-cache.json");
    assert!(
        cache_path.exists(),
        "M1 result cache must exist at {}",
        cache_path.display()
    );

    let mut tail: Vec<_> = store.load().into_iter().rev().take(3).collect();
    tail.reverse();
    for rec in &tail {
        println!(
            "M1 record: outcome={:?} compute_ms={}",
            rec.outcome, rec.compute_ms
        );
    }
}

#[ignore = "manual M2: parity, costs real tokens"]
#[tokio::test]
async fn m2_governance_off_parity() {
    let cfg = Config::load().expect("Config::load");
    let ac = AgentConfig::from_config(&cfg);
    let tmp = tempfile::tempdir().expect("tempdir");

    let home_store = ResourceRecordStore::open(None);
    let home_before = home_store.count();

    let mgr = SubagentManager::new(ManagerConfig {
        governance: GovernanceConfig {
            enabled: false,
            data_dir: Some(tmp.path().into()),
            ..Default::default()
        },
        ..Default::default()
    });

    let registry = ToolRegistry::new(); // no tools
    let reqs: Vec<DelegationRequest> = (0..2)
        .map(|_| {
            let mut r = DelegationRequest::single("Reply with exactly: OK");
            r.max_turns = Some(2);
            r
        })
        .collect();
    let futs = reqs
        .iter()
        .map(|req| mgr.dispatch_single(req, &ac, &cfg, &registry, None));
    let results = futures::future::join_all(futs).await;

    for (i, r) in results.iter().enumerate() {
        assert!(
            r.success,
            "M2 dispatch {i} failed: {:?}",
            r.error.as_deref().unwrap_or("<no error text>")
        );
        println!(
            "M2 dispatch {i}: success=true wall_clock={:.3}s iterations={} model={}",
            r.wall_clock.as_secs_f64(),
            r.iterations,
            r.model
        );
    }

    assert!(
        !tmp.path().join("resource-records.jsonl").exists(),
        "M2 governance off => no resource-records.jsonl in tempdir"
    );
    assert!(
        !tmp.path().join("result-cache.json").exists(),
        "M2 governance off => no result-cache.json in tempdir"
    );

    let home_after = home_store.count();
    assert_eq!(
        home_after, home_before,
        "M2 governance off => real-home record count unchanged"
    );
}
