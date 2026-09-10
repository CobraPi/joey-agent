# Contract: /neurocode memory command surface

Additive subcommand on the existing `/neurocode` handler (slash/CLI/TUI parity — one implementation, `crates/joey-cli/src/commands/neurocode.rs`, pattern of `consent_command_text`). Existing subcommands and the registration line's existing grammar are unchanged; the slash registration description gains `memory`.

## Grammar

```
/neurocode memory                -> status overview (enabled, counts, active/superseded, unresolved conflicts)
/neurocode memory list [episodes|preferences]   -> bounded recent listing with ids, recency, origin
/neurocode memory show <id>      -> full record (episode fields / preference + evidence trail)
/neurocode memory search <text...> -> ranked matches over episodes + preferences (retrieval leg)
/neurocode memory correct <id> <text...> -> user correction: new preference revision superseding <id>
/neurocode memory delete <id>    -> hard delete (row + vector); confirms unless --yes/-y
/neurocode memory enable|disable -> sets neurocode.memory.enabled (persisted config)
/neurocode memory status         -> same as bare `memory`
```

## Behavior rules

- Unknown sub-subcommand or malformed args -> usage text, exit path identical to other neurocode text commands (no panic, no partial writes).
- All output is plain text through the existing `NeurocodeOutcome::Text` path.
- `delete` requires confirmation via the existing consent-style stdin confirm unless `--yes`/`-y` (mirrors consent T044 pattern).
- `correct` on an episode id is an error with guidance (corrections apply to preferences; episodes are immutable).
- Command works with memory disabled for `status|enable|list` (read/admin), and clearly reports "memory disabled" for capture-dependent views when disabled.
