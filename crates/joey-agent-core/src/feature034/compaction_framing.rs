//! US4 wiring — Agent::new reads compaction.calm_framing (default true).

use super::support::fixture;

#[tokio::test]
async fn compaction_framing_agent_wiring_follows_config() {
    // fixture() holds the global home-override lock for its lifetime, so the
    // two fixtures must not coexist — read the flag, drop, then rebuild.
    let calm_default = {
        let on = fixture("\n", vec![]);
        on.agent.compressor.calm_framing
    };
    assert!(calm_default, "config default true must enable calm framing");
    let calm_disabled = {
        let off = fixture("compaction:\n  calm_framing: false\n", vec![]);
        off.agent.compressor.calm_framing
    };
    assert!(!calm_disabled, "config false must disable");
}
