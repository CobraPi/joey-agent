# Contract: resource record (JSONL)

One JSON object per line, appended at each terminal task outcome to `~/.joey/delegation/resource-records.jsonl` (append-only; tolerant load skips a malformed trailing line). Exact field set:

| Field | Type | Notes |
|-------|------|-------|
| record_id | string | Unique per record |
| task_signature | string | Canonical signature (data-model.md: Task Signature) |
| priority | "critical" \| "normal" \| "background" | Lane at execution |
| outcome | "completed" \| "failed" \| "timeout" \| "aborted_by_resource_limit" \| "busy_refused" \| "cache_hit" | Terminal kind |
| queue_wait_ms | u64 | Submit → admission |
| compute_ms | u64 (sampled) | Admission → terminal |
| cpu_ms | u64 (sampled) | Sampled CPU attribution |
| memory_peak_kb | u64 (sampled, advisory) | Never enforced |
| retries | u16 | Attempts beyond the first |
| checkpoint | object \| null | Resume token (contracts/checkpoint-token.md) or null |
| token_usage | object { prompt_tokens, completion_tokens, total_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens } | Mirror of joey-providers Usage; joinable with token telemetry |
| degraded | bool | Produced under degraded mode |
| created_at | ISO 8601 string | Append time |
