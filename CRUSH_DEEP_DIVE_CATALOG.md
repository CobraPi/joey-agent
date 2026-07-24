# Crush Feature Catalog — Beyond the Existing Gap Analysis

Deep-dive findings from the crush codebase (charmbracelet/crush). These features are NOT in the existing FEATURE_GAP_ANALYSIS.md. Each entry has file path, what it does, implementation details, and why it matters.

---

## 1. Multi-Question Tool with Structured Types
**File:** `internal/agent/tools/question.go`, `internal/question/question.go`
**What:** A `question` tool that supports batched questions (yes_no, single_choice, multi_choice, free_text) in a single tool call. Multiple questions render as a tabbed form with a confirmation step.
**Implementation:** `QuestionParams` has a `Questions []QuestionItem` array, `ConfirmTitle`, and `ConfirmDescription`. The `question.Service` publishes a `Request` over pubsub and blocks until the UI resolves. Max 10 questions per batch. LLM-friendly validation errors. Answers include optional per-question `Notes` (key-value annotations). Double-serialized JSON fallback in `UnmarshalJSON` handles models that wrap the array as a string.
**Why:** Lets the agent ask multiple related questions in one round-trip, reducing turns. The structured types (choice vs free-text) give better UX than a generic "ask user" tool. Notes allow the user to annotate decisions.

## 2. Agentic Fetch Tool (Sub-Agent Web Research)
**File:** `internal/agent/agentic_fetch_tool.go`
**What:** A `agentic_fetch` tool that spawns a sub-agent to research a URL or search query. For large web pages (>threshold), content is written to a temp file and the sub-agent uses `view`/`grep` to extract relevant information rather than dumping everything into context.
**Implementation:** `fantasy.NewParallelAgentTool`. URL mode: fetches content, if large, saves to temp file and tells sub-agent to analyze it with file tools. Search mode: sub-agent uses `web_search` + `web_fetch` iteratively. Sub-agent uses the small model. Runs in its own temp directory.
**Why:** Prevents context bloat from large web pages. Delegates research to a sub-agent that can iteratively search, read, and synthesize — far better than a single fetch.

## 3. Auto-Background Shell with Threshold
**File:** `internal/agent/tools/bash.go`, `internal/shell/background.go`
**What:** Long-running bash commands automatically move to background after a configurable threshold (default 60s). The agent gets a job ID and can poll output later.
**Implementation:** `BashParams.AutoBackgroundAfter` (default 60). A ticker polls every 100ms; if the command hasn't finished by the threshold, it returns a background job ID. `BackgroundShellManager` is a singleton managing up to 50 concurrent jobs with 8-hour retention. Jobs have `job_output` and `job_kill` tools.
**Why:** Prevents the agent from blocking indefinitely on long builds/tests. The auto-promotion threshold is a much better UX than requiring the agent to explicitly request background mode.

## 4. Banned Command System with Argument-Level Blocking
**File:** `internal/agent/tools/bash.go` (lines 75-195), `internal/shell/dispatch.go`
**What:** A comprehensive command safety system with two layers: (a) fully banned commands (sudo, curl, wget, ssh, package managers, system modification tools), and (b) argument-level blockers (e.g., `brew install` blocked, `brew` allowed; `go test -exec` blocked, `go test` allowed).
**Implementation:** `CommandsBlocker` for exact matches, `ArgumentsBlocker(cmd, blockedSubargs, blockedFlags)` for granular control. `blockFuncs()` returns a combined list. The shell dispatch checks these before execution.
**Why:** Granular safety: allows `git diff` but blocks `git push`, allows `pip` but blocks `pip install --user`. Much better than blanket command bans.

## 5. Safe Read-Only Command Auto-Approval
**File:** `internal/agent/tools/bash.go` (lines 209-246), `internal/agent/tools/safe.go`
**What:** A curated allowlist of read-only commands that bypass the permission prompt entirely (ls, cat, git log, git status, ps, find, etc.). Only single commands without chaining metacharacters (`|`, `;`, `&&`, `$()`, backticks) qualify.
**Implementation:** `safeCommands` list + `containsCommandChaining` check. If the command matches and has no chaining, `isSafeReadOnly = true` and permission is skipped.
**Why:** Eliminates permission fatigue for obviously-safe read operations while maintaining safety for anything that modifies state.

## 6. File Modification Race Detection
**File:** `internal/agent/tools/write.go` (lines 74-79)
**What:** Before writing a file, checks if the file was modified on disk since the agent last read it. If so, returns an error telling the agent to re-read.
**Implementation:** Compares `fileInfo.ModTime()` against `filetracker.LastReadTime(sessionID, filePath)`. If `modTime.After(lastRead)`, blocks the write.
**Why:** Prevents the agent from silently overwriting changes made by the user or another process between read and write. Critical for collaborative editing safety.

## 7. Streaming Markdown Prefix Caching
**File:** `internal/ui/chat/streaming_markdown.go`
**What:** An incremental markdown renderer that caches a "stable prefix" of the document and only re-renders the trailing portion on each streaming flush. Detects safe boundaries (after blank lines where no markdown construct is open).
**Implementation:** `streamingMarkdown` struct caches `stablePrefix`, `stablePrefixRender`, and `width`. `findSafeMarkdownBoundary` finds positions after blank lines where no fenced code block, list, table, blockquote, or setext header is open. Falls back to full render on any uncertainty.
**Why:** Massive perf win during streaming — avoids re-rendering the entire markdown document on every token. The boundary detection is deliberately conservative to prevent render artifacts.

## 8. MCP Initialization Barrier
**File:** `internal/agent/tools/mcp/init.go`, `internal/agent/coordinator.go` (line 230)
**What:** `WaitForInit` blocks the first agent run until all MCP servers have completed initialization. `ArmInit` must be called before the init goroutine starts, so coordinators that don't arm it never block.
**Implementation:** `initOnce` + `initDone` channel pattern. `ArmInit()` sets `initStarted`. `WaitForInit(ctx)` blocks on `initDone` if armed. Coordinator.Run calls `mcp.WaitForInit(ctx)` before building the tool list.
**Why:** Prevents slow MCP servers (e.g. stdio Python via uv) from silently missing their tools in the first run — a race condition that would make tools appear "connected" but unavailable.

## 9. MCP OAuth Token Store with Process-Wide Sharing
**File:** `internal/agent/tools/mcp/oauth.go`
**What:** Persistent OAuth token store for MCP servers. Process-wide singleton prevents concurrent servers from overwriting each other's tokens. 3-minute browser flow timeout.
**Implementation:** `tokenStore` with `storeCache` map keyed by config path. `mcptoken` struct holds `oauth2.Token`, `ClientID`, `ClientSecret`, `AuthStyle`, and `oauthEndpoints` (for refresh without re-browser). Uses `modelcontextprotocol/go-sdk/oauthex`.
**Why:** Enables MCP servers requiring OAuth (e.g. GitHub MCP). The shared store prevents data loss when multiple servers authenticate concurrently.

## 10. LSP-Integrated Edit Tools (Automatic Diagnostics)
**File:** `internal/agent/tools/edit.go`, `internal/agent/tools/multiedit.go`, `internal/agent/tools/write.go`
**What:** Every file modification tool (edit, multiedit, write) automatically notifies LSP servers and appends diagnostics to the tool response. The agent sees type/lint errors immediately after each edit.
**Implementation:** `notifyLSPs(ctx, lspManager, filePath)` called after each write. `getDiagnostics(filePath, lspManager)` formats diagnostics into the response. Response wrapped in `<result>...</result>` tags followed by diagnostics.
**Why:** Creates a tight feedback loop: edit → instant diagnostics → fix. The agent doesn't need a separate "check for errors" step. This is the core advantage of LSP-integrated editing.

## 11. Multi-Edit Tool (Batch Operations)
**File:** `internal/agent/tools/multiedit.go`
**What:** Applies multiple find-and-replace operations to a single file in one tool call. Validates all edits before applying any (atomicity). Reports partial failures with `FailedEdit` details.
**Implementation:** `MultiEditParams` has `Edits []MultiEditOperation`. `validateEdits` ensures only the first edit can have empty `old_string` (file creation). `applyEditsToContent` applies sequentially, collecting `FailedEdit` entries. Response metadata includes `EditsApplied`, `EditsFailed`.
**Why:** Reduces tool-call count for multi-spot edits. Atomic validation prevents partial application. Better than N separate edit calls.

## 12. File Version History with Intermediate Snapshots
**File:** `internal/history/file.go`
**What:** Per-session, per-file version history. Every write creates a version. If the file was manually modified between agent operations (content differs from last stored), an intermediate version is saved first.
**Implementation:** `history.Service` with `Create` (initial v0), `CreateVersion` (auto-incrementing). `write.go` checks if `file.Content != oldContent` and creates an intermediate version to capture user manual edits. Version conflicts retried 3x with version bump.
**Why:** Enables undo/rollback to any point. Captures user manual edits between agent operations — essential for understanding what changed when.

## 13. Token Usage Fallback Estimation
**File:** `internal/agent/usage_fallback.go`
**What:** When a provider returns zero usage (some providers don't report token counts), crush estimates tokens by walking the message/step content. Uses `len(s)/4` approximation.
**Implementation:** `fallbackStepUsage` checks if `usageIsZero`, then `estimateMessageTokens` walks all message parts (text, reasoning, files, tool calls, tool results) with type-specific estimation. `estimateMediaTokens` handles binary data by byte count.
**Why:** Ensures cost tracking and auto-summarization thresholds work even with providers that don't report usage. Without this, sessions would never trigger auto-summarization on some providers.

## 14. Prompt Caching via Anthropic cache_control
**File:** `internal/agent/agent.go` (lines 840-855)
**What:** Automatically adds Anthropic `cache_control` headers to the system message and the last 2 messages, plus the last tool definition.
**Implementation:** In `PrepareStep`, finds the last system message and sets `ProviderOptions` with cache control. Last 2 messages also get cache control. In `Run`, `agentTools[len-1].SetProviderOptions(cacheControlOptions)` caches the tool palette.
**Why:** Reduces API costs significantly for Anthropic models by enabling prompt caching of the system prompt, recent context, and tool definitions.

## 15. Provider Reasoning Effort Control
**File:** `internal/agent/coordinator.go` (lines 324-399)
**What:** User-configurable reasoning effort per model. Falls back through: user-selected → model default → first configured level. Maps to provider-specific parameters (`reasoning_effort` for OpenAI, thinking budget for Anthropic, etc.).
**Implementation:** `effectiveReasoningEffort` checks `model.CatwalkCfg.CanReason`, `ReasoningLevels`, `DefaultReasoningEffort`. Provider-specific application in `mergeCallOptions` (OpenAI `reasoning_effort`, Anthropic thinking budget). UI dialog for selection (`dialog/reasoning.go`).
**Why:** Lets users trade cost/latency for reasoning depth. Critical for models like o1/o3/Claude with extended thinking.

## 16. Provider-Specific Reasoning Signature Handling
**File:** `internal/agent/agent.go` (lines 881-908)
**What:** Captures and persists provider-specific reasoning metadata (Anthropic signatures, Google thought signatures, OpenAI Responses reasoning data) for multi-turn reasoning continuity.
**Implementation:** `OnReasoningEnd` callback checks `ProviderMetadata` for each provider: `anthropic.ReasoningOptionMetadata.Signature`, `google.ReasoningMetadata.Signature/ToolID`, `openai.ResponsesReasoningMetadata`. Calls `AppendReasoningSignature`, `AppendThoughtSignature`, `SetReasoningResponsesData`.
**Why:** Enables proper multi-turn reasoning for models that require signature continuity (Anthropic) or thought-chain references (Google). Without this, reasoning degrades across turns.

## 17. Run ID Correlation System
**File:** `internal/agent/agent.go` (RunID in SessionAgentCall), `internal/agent/notify/notify.go`
**What:** Every run can carry a caller-supplied `RunID` that's echoed back in the terminal `RunComplete` event. Enables non-interactive clients (`crush run`) to correlate a request with its completion even on busy sessions.
**Implementation:** `RunID` threaded through `SessionAgentCall` → streaming → `RunComplete.RunID`. `RunIDFromContext(ctx)` extracts it. `PublishMustDeliver` ensures the terminal event isn't dropped by a full subscriber buffer.
**Why:** Essential for programmatic/headless usage. Without RunID correlation, a client waiting on a specific prompt's completion would have to guess which RunComplete belongs to it.

## 18. Message Queue with Cancel Semantics
**File:** `internal/agent/agent.go` (lines 347-490)
**What:** When a session is busy, new prompts are queued. Queued prompts can be canceled individually. Uses a monotonic accept sequence to distinguish "queued before cancel" (dropped) from "queued after cancel" (kept). RunID-bearing queued prompts each get their own turn; others fold into the active turn.
**Implementation:** `enqueueCall`, `drainQueueForStep`, `canceledBySeq`. `cancelMark` is a high-water mark of accept sequences. `AcceptedRun` with `seq` field. Per-session `dispatchMu` serializes the accept→run transition.
**Why:** Sophisticated queue management prevents lost prompts and race conditions between cancel and queue operations. The sequence-based cancel coverage is a well-reasoned concurrency design.

## 19. Provider Auth Refresh with Transparent Retry
**File:** `internal/agent/agent.go` (OnAuthRefresh), `internal/agent/coordinator.go` (makeAuthRefreshCallback)
**What:** When a stream fails with HTTP 401, the `OnAuthRefresh` callback refreshes credentials and retries transparently. The coordinator coalesces the unauthorized → re-auth → retry chain into a single terminal event.
**Implementation:** `SessionAgentCall.OnAuthRefresh func(ctx, err) error`. Coordinator's callback refreshes OAuth2 tokens. `OnComplete` hook in coordinator.Run captures the latest RunComplete, publishing only the final outcome.
**Why:** Seamless credential refresh — the user never sees an auth failure on expired tokens. The coalesce prevents non-interactive clients from exiting on a stale error before the retry succeeds.

## 20. MCP Server Instructions Injection
**File:** `internal/agent/agent.go` (lines 656-668)
**What:** Connected MCP servers' `Instructions` field is collected and injected into the system prompt wrapped in `<mcp-instructions>` tags.
**Implementation:** Iterates `mcp.GetStates()`, for each connected server reads `InitializeResult().Instructions`, concatenates, appends to system prompt.
**Why:** MCP servers can provide behavioral instructions (e.g. "always use this format"). Giving them system-prompt-level visibility ensures the model follows them.

## 21. Agent Skills Open Standard
**File:** `internal/skills/skills.go`, `internal/skills/manager.go`, `internal/skills/tracker.go`
**What:** Full implementation of the Agent Skills open standard (agentskills.io). SKILL.md files with YAML frontmatter (name, description, user-invocable, disable-model-invocation, license, compatibility). Discovery from builtin embedded FS + user paths. Skills override builtins by name.
**Implementation:** `Parse` handles YAML frontmatter + markdown body. `Discover` uses `fastwalk` for directory traversal. `Manager` is workspace-scoped with pubsub events. `Tracker` tracks which skills were loaded during a session. Builtin skills use `crush://skills/...` virtual paths.
**Why:** Standardized skill format enables sharing skills across agent platforms. The tracker enables "skills used this session" reporting and ensures skills are loaded before use.

## 22. Context File Loading (Project + Global)
**File:** `internal/agent/prompt/prompt.go` (lines 110-163)
**What:** System prompt includes files from `ContextPaths` (project-level, e.g. `.crush/context/`) and `GlobalContextPaths` (user-level preferences). Paths support `~` expansion and env var resolution. Directory walking supported.
**Implementation:** `loadContextFiles` expands paths, deduplicates, walks directories. `PromptDat` includes `ContextFiles` and `GlobalContextFiles` rendered in `<project_context>` and `<user_preferences>` XML tags respectively.
**Why:** Persistent context injection without requiring the agent to read files each session. Global paths enable "always follow these preferences" across all projects.

## 23. File Read Permission by Working Directory Boundary
**File:** `internal/agent/tools/view.go` (lines 116-155)
**What:** Reading files outside the working directory requires permission. Skill files are exempt. Provides "did you mean?" suggestions for typos.
**Implementation:** `filepath.Rel(workingDir, filePath)` — if path starts with `..`, it's outside. `isInSkillsPath` checks exemption. On `os.IsNotExist`, scans directory for similar names (substring match).
**Why:** Security boundary prevents the agent from reading sensitive files outside the project without consent. The suggestion system reduces friction from typos.

## 24. Skill-Loaded Detection for Builtin Skills
**File:** `internal/agent/tools/view.go` (lines 106-110), `internal/skills/skills.go`
**What:** Builtin skill files are accessed via `crush://skills/...` virtual paths. The `view` tool recognizes this prefix and reads from the embedded builtin FS. Loaded skills are tracked.
**Implementation:** `skills.BuiltinPrefix = "crush:"`. `readBuiltinFile` handles the prefix. `skillTracker.MarkLoaded(name)` records access. The coder prompt instructs the model to use `crush://` locations verbatim.
**Why:** Builtin skills are embedded in the binary — no filesystem dependency. The tracking enables reporting and ensures the model doesn't skip loading steps.

## 25. Code Reference Formatting (file:line)
**File:** `internal/agent/templates/coder.md.tpl` (lines 55-59)
**What:** The system prompt instructs the agent to use `file_path:line_number` format when referencing code locations, making them clickable in terminals.
**Implementation:** `<code_references>` section in the prompt with examples: "src/main.go:45", "pkg/utils/helper.go:123-145".
**Why:** Makes agent responses actionable — users can Cmd+Click to jump to code in their editor. Small UX detail with big productivity impact.

## 26. Embedded jq Implementation
**File:** `internal/shell/jq.go`
**What:** A built-in `jq` command implemented in Go (gojq) that runs inside the shell sandbox. Supports `-r`, `-c`, `-s`, `-n`, `-e`, `-R`, `--arg`, `--argjson` flags.
**Implementation:** `handleJQ` parses flags, compiles gojq query, reads input from stdin/files. Context-polled for cancellation. Full `--help` output.
**Why:** Agents can manipulate JSON without requiring jq to be installed on the host system. Works identically across all platforms including Windows (POSIX emulation).

## 27. Cross-Platform POSIX Shell Emulation
**File:** `internal/shell/shell.go`
**What:** Uses `mvdan.cc/sh/v3` for POSIX shell emulation even on Windows. Commands use forward slashes on all platforms. Windows has ShellTypeCmd/PowerShell options but default is POSIX.
**Implementation:** `Shell` struct with env, cwd, blockFuncs. `Exec` and `ExecStream` methods. `CrushEnvMarkers()` sets `CRUSH=1`, `AGENT=crush`, `AI_AGENT=crush` so subprocesses can detect agent execution. Strips herdr pane-ownership vars for security.
**Why:** Consistent shell behavior across platforms. The env markers enable hooks and tools to detect they're running inside an agent.

## 28. Desktop Notification Multi-Backend
**File:** `internal/ui/notification/`
**What:** Four notification backends: Native (OS notifications), OSC (OSC 99/777 terminal escape sequences for SSH), Bell (audible), Noop. Auto-selects based on environment: SSH → OSC, local → native.
**Implementation:** `Backend` interface with `Send(Notification) tea.Cmd`. `native.go` (macOS `osascript`, Linux `notify-send`), `osc.go` (OSC 99 preferred, 777 fallback), `bell.go` (`\x07`), `noop.go`. Platform-specific icon embedding (`icon_darwin.go`).
**Why:** Notifications work in all contexts — SSH sessions, local terminals, headless. The auto-detection means users don't need to configure anything.

## 29. Update Checker with Pre-release Awareness
**File:** `internal/update/update.go`
**What:** Checks GitHub releases for newer versions. Aware of pre-release semantics: won't suggest a pre-release to a stable user, will suggest stable to a pre-release user. Detects dev builds (`devel`, `dirty`, go-install pseudo-versions).
**Implementation:** `Info.Available()` compares with pre-release awareness. `goInstallRegexp` matches pseudo-versions from `go install`. GitHub API client.
**Why:** Non-intrusive update notifications that respect the user's release channel. Prevents suggesting pre-releases to production users.

## 30. Session Title Auto-Generation
**File:** `internal/agent/agent.go` (line 694-699), `internal/agent/templates/title.md`
**What:** Generates a session title from the first user prompt using the small model. Runs concurrently with the main stream (errgroup). Strips `<think>` tags from generated titles.
**Implementation:** `GenerateTitle` runs on first message. `thinkTagRegex` and `orphanThinkTagRegex` remove think tags. Title generation session has ID `title-{parentSessionID}`.
**Why:** Automatic session naming for history/search without requiring user action. Uses the cheap small model.

## 31. System Prompt Prefix Separation
**File:** `internal/agent/agent.go` (lines 857-859)
**What:** Provider-level system prompt prefix is prepended as a separate system message before the main system prompt, rather than concatenated.
**Implementation:** `systemPromptPrefix` stored separately via `csync.Value`. In `PrepareStep`, if prefix exists, prepends `fantasy.NewSystemMessage(promptPrefix)` to messages.
**Why:** Enables provider-specific instructions (e.g. "you are running on Azure OpenAI") to be injected without polluting the main prompt template.

## 32. Loop Detection with Tool I/O Signatures
**File:** `internal/agent/loop_detection.go`
**What:** Detects agent loops by hashing tool name + input + output together (not just name+input). Window of 10 steps, max 5 repeats.
**Implementation:** `getToolInteractionSignature` pairs tool calls with their results by ToolCallID, SHA-256 hashes `toolName\x00input\x00output\x00`. `hasRepeatedToolCalls` counts signatures in a sliding window.
**Why:** More precise than name-only detection — same tool with different I/O isn't a loop, same tool with identical I/O is. Prevents false positives on tools like `view` that are called legitimately many times.

## 33. Provider Options Merging (3-Layer)
**File:** `internal/agent/coordinator.go` (lines 344-399)
**What:** Merges provider options from three layers: catwalk (model catalog defaults), provider config (crush.json), and model config (user selection). Uses JSON merge (`go-jsons`) for deep merge.
**Implementation:** Three JSON blobs merged: `catwalkOpts`, `providerCfgOpts`, `cfgOpts`. Provider-specific reasoning effort applied after merge (OpenAI `reasoning_effort`, Anthropic thinking).
**Why:** Flexible configuration hierarchy — model catalog provides sane defaults, users override via config, per-call options override both. Deep merge preserves all layers.

## 34. Provider-Specific Media Limitation Workarounds
**File:** `internal/agent/agent.go` (line 839)
**What:** `workaroundProviderMediaLimitations` transforms messages to handle providers that don't support certain media types (e.g. models that don't support images).
**Implementation:** Called in `PrepareStep` before sending messages. Checks `largeModel.CatwalkCfg.SupportsImages`. The `SupportsImagesContextKey` in context tells tools whether to generate image content.
**Why:** Prevents API errors from providers with limited media support. Graceful degradation instead of failure.

## 35. Git Status in System Prompt
**File:** `internal/agent/prompt/prompt.go` (PromptDat.GitStatus), `internal/agent/templates/coder.md.tpl` (lines 376-380)
**What:** Captures `git status` at conversation start and includes it in the system prompt. Labeled as "snapshot at conversation start - may be outdated."
**Implementation:** `PromptDat.GitStatus` populated during prompt build. Rendered in `<env>` section with timestamp caveat.
**Why:** Gives the agent context about the working tree state without needing to run git. The staleness warning prevents acting on outdated info.

## 36. Hyper Credit Balance Tracking
**File:** `internal/agent/hyper/provider.go`
**What:** Tracks Charm Hyper (Charm's own LLM proxy) credit balance. Extracts balance from API response metadata to avoid a separate API call.
**Implementation:** `lastKnownBalance atomic.Int64`, `hasBalance atomic.Bool`. `SetBalance` called during response processing. `FetchCredits` checks cached balance first, falls back to `/v1/credits` endpoint.
**Why:** Users can see remaining credits without a separate API round-trip. The atomic operations make it thread-safe.

## 37. Worker Pool for Onboarding/Readiness
**File:** `internal/agent/coordinator.go` (line 132, 221)
**What:** `readyWg errgroup.Group` gates `Run` until all startup tasks (MCP init, model loading, OAuth) complete. `coordinator.Run` calls `c.readyWg.Wait()` before proceeding.
**Implementation:** `errgroup.Group` from `golang.org/x/sync`. Background tasks (LSP startup, model discovery) are added to the group during initialization.
**Why:** Prevents race conditions during startup — the agent won't accept prompts until all subsystems are ready.

## 38. Stale Content Detection on Retry
**File:** `internal/agent/agent.go` (lines 932-942)
**What:** On provider retry, resets streamed content so the retried response doesn't concatenate with partial content from the failed attempt.
**Implementation:** `OnRetry` callback calls `currentAssistant.ResetStreamedContent()`. On final attempt (no more retries), partial content stays as context beneath the error.
**Why:** Prevents garbled output from partial streams. On final failure, the partial content is preserved as useful context for debugging.

## 39. Tool Input Sanitization
**File:** `internal/agent/agent.go` (lines 951-967)
**What:** Validates tool input JSON before execution. If invalid, the tool result is replaced with a helpful error instead of a crash.
**Implementation:** `sanitizeToolInput(toolName, toolCallID, input)` returns `(sanitizedInput, wasSanitized)`. If sanitized, the result content is overridden with "Tool call failed: arguments were not valid JSON."
**Why:** Graceful handling of malformed tool calls (common with smaller models). Prevents panics and gives the model actionable feedback.

## 40. Workspace Cache with TTL Backstop (Client/Server Mode)
**File:** `internal/ui/model/workspace_cache.go`
**What:** In client/server mode, workspace probes (busy checks, permissions, queue) are cached with TTL to avoid blocking the render loop. State edges invalidate caches; a TTL backstop re-fetches stale values.
**Implementation:** `ttlCache` with `fresh(ttl)`, `set(val)`, `invalidate()`. `busyCacheTTL = 500ms`, `promptQueueTTL = 2s`. Generation counters (`busyFetchGen`) discard stale in-flight results. Optimistic sends on state edges.
**Why:** Prevents UI freezes in client/server mode where every probe is an HTTP round-trip. The generation guard ensures the latest state is always eventually fetched.
