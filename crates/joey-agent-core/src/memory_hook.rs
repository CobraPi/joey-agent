//! Adaptive-memory hook points for the turn loop (feature 027, spec
//! specs/027-please-enhance-neurocode, contracts/neurocode-memory-injection.md).
//! Provider-free by design: the production implementation lives in joey-cli
//! wiring (T012) and composes the joey-neurocode stores + joey-neurocode-rag
//! injection leg; agent-core sees only this narrow trait, mirroring the
//! RagPrefetchSource precedent.

/// Compact summary of one completed turn, handed to the memory runtime for
/// episodic capture (Q2: one episode per completed task).
#[derive(Debug, Clone, Default)]
pub struct MemoryTurnSummary {
    /// The user's request text for the turn.
    pub user_prompt: String,
    /// The assistant's final text answer (approach proxy).
    pub assistant_final: String,
    /// File paths mutated by write-class tools this turn.
    pub files_touched: Vec<String>,
    /// Tool names invoked this turn (context proxy).
    pub tools_used: Vec<String>,
    /// Whether the turn ended in an error/aborted state (outcome proxy).
    pub turn_error: bool,
    /// Session key / run id (origin_run).
    pub session_key: String,
}

/// Narrow memory runtime boundary (implemented by joey-cli wiring).
pub trait MemoryRuntime: Send + Sync {
    /// Whether the runtime is active (double-gate beside the config check).
    fn enabled(&self) -> bool;
    /// Bounded injection block for the current prompt (None = omit silently).
    fn prefetch_block(&self, prompt: &str) -> Option<String>;
    /// Post-turn capture; MUST do heavy work off the caller's thread
    /// (spawn internally) — called from the turn-loop exit paths.
    fn capture_turn(&self, summary: &MemoryTurnSummary);
}

/// Format the injected memory block. Ordering: preferences section first
/// (the caller passes them explicit-first, then recency), episodes second.
/// Hard char cap: truncation drops WHOLE entries, never mid-entry; if even
/// the first entry does not fit, the section is omitted; an all-empty input
/// yields an empty String (caller treats empty as "omit").
pub fn format_memory_block(preferences: &[String], episodes: &[String], char_limit: usize) -> String {
    const PREFS_HEADER: &str = "## Learned preferences (applied automatically)";
    const EPISODES_HEADER: &str = "## Relevant past episodes";

    let mut out = String::new();
    for (header, entries) in [(PREFS_HEADER, preferences), (EPISODES_HEADER, episodes)] {
        let mut section = String::new();
        let mut accepted = 0usize;
        for entry in entries {
            if entry.trim().is_empty() {
                continue;
            }
            let line = format!("- {entry}\n");
            // Total block length IF this line were appended: sections already
            // committed (+ the blank line separating them), this section's
            // committed content, the header line when this is the first
            // accepted entry, and the line itself.
            let mut prospective = out.len() + section.len() + line.len();
            if !out.is_empty() {
                prospective += 1; // blank line between sections
            }
            if accepted == 0 {
                prospective += header.len() + 1; // header line
            }
            if prospective > char_limit {
                // Whole-entry truncation: this entry and every later one
                // (ordering is significant) is dropped.
                break;
            }
            if accepted == 0 {
                section.push_str(header);
                section.push('\n');
            }
            section.push_str(&line);
            accepted += 1;
        }
        if accepted == 0 {
            continue; // no fitting entries ⇒ section (header included) omitted
        }
        if !out.is_empty() {
            out.push('\n'); // blank line between sections
        }
        out.push_str(&section);
    }
    out
}

/// Clamp helper shared with the wiring (contract bounds already enforced at
/// MemoryConfig load; this is the last line of defense).
pub fn clamp_char_limit(v: i64) -> usize {
    v.clamp(256, 8192) as usize
}
