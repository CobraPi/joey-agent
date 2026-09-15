# Contract: checkpoint / resume token

Resume token recorded at each turn boundary in the resource record's `checkpoint` field:

{ "last_completed_turn": <usize>, "transcript_digest": "<short hash>", "recorded_at": "<ISO 8601>" }

Acceptance rule: a presented token is valid only if its transcript_digest matches the digest of the completed turns it claims (stale token → treat as no token → full restart, counted fully against retry budget). A resume skips completed turns and continues at the next turn boundary; a timed-out resume re-checkpoints and may re-resume within the retry budget.
