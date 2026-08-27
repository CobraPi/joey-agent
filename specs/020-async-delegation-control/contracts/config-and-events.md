# Contracts: Configuration & Events
## Configuration keys (layered YAML + env, dotted paths; all optional with defaults — absence preserves current behavior)
- delegation.parent_reserved_permits — positive int, default 1; orchestrator's guaranteed minimum capacity share; 0 disables reservation (FR-018/SC-007).
- delegation.wind_down_timeout_secs — positive int, default 10; bounded wait when winding down children at session end (FR-015).
- (existing keys unchanged: delegation.max_concurrent_children, max_concurrent_requests, max_spawn_depth, default_max_turns, default_persist, default_model, auto_mem_*; omo.* unchanged.)
## AgentEvent additions (additive; enum is #[non_exhaustive])
- SubagentStopped { id, goal, reason, summary_preview } — emitted when a child stops for any non-natural reason (orchestrator/operator/budget/session-end).
- (optional) SubagentBudgetBreach { id, limit, observed } — emitted at breach detection before stop — DEFERRED: not scheduled in tasks.md v1 (plan scopes it out of the initial milestones); revisit after MVP.
Existing variants unchanged; consumers with wildcard arms unaffected (verified pattern in joey-tui/joey-cli).
## Completion notice wire format (into existing pending-completions queue)
`[SUBAGENT <COMPLETE|FAILED|STOPPED>] id=<id> goal=<goal> outcome=<...> tokens=<n> duration=<secs>s\n<summary ≤500 tokens>` — delivered at next turn boundary mid-turn, or via engine idle-wake (FR-003); queue cap 64 (existing), oldest dropped.
