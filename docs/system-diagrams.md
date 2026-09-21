# System Diagrams

Detailed, high-level system diagrams for Joey Agent, drawn as Mermaid
flowcharts and sequence diagrams (GitHub renders these natively). Each
diagram links to its reference page for full prose detail. For the
workspace-level crate graph and system context, start with
[architecture.md](architecture.md) instead — this page goes one level
deeper, into the runtime behavior of each subsystem.

Diagram index:

1. [The agent turn loop](#1-the-agent-turn-loop)
2. [Tool dispatch pipeline](#2-tool-dispatch-pipeline)
3. [Provider stack](#3-provider-stack)
4. [System prompt assembly](#4-system-prompt-assembly)
5. [Context management pipeline](#5-context-management-pipeline)
6. [Subagent delegation (delegate_task)](#6-subagent-delegation-delegate_task)
7. [HyperCode orchestration pipeline](#7-hypercode-orchestration-pipeline)
8. [Browser explorer steering loop (feature 035)](#8-browser-explorer-steering-loop-feature-035)
9. [Enterprise TaskGraph runtime (spec 023)](#9-enterprise-taskgraph-runtime-spec-023)
10. [NeuroCode code intelligence + RAG](#10-neurocode-code-intelligence--rag)
11. [Compute pool (feature 033)](#11-compute-pool-feature-033)
12. [Dynamic LLM selector](#12-dynamic-llm-selector)
13. [Security and sanitization layers](#13-security-and-sanitization-layers)
14. [TUI engine-actor decoupling](#14-tui-engine-actor-decoupling)
15. [Cron scheduler lifecycle](#15-cron-scheduler-lifecycle)
16. [MCP client lifecycle](#16-mcp-client-lifecycle)
17. [SpecKit workflow + Visual UI](#17-speckit-workflow--visual-ui)
18. [State and configuration layout](#18-state-and-configuration-layout)

---

## 1. The agent turn loop

The heart of the system: `Agent::run_turn`
(`crates/joey-agent-core/src/agent.rs`). A **turn** is one user message
plus every model iteration and tool execution needed to answer it.
→ [agent-turn-loop.md](agent-turn-loop.md),
[agent-core-reference.md](agent-core-reference.md)

```mermaid
flowchart TD
    START(["run_turn — user input, cron prompt,<br/>or subagent brief"]) --> PERSIST["append user message to history<br/>+ persist to SQLite"]
    PERSIST --> REPAIR["repair dangling tool-call tail<br/>from a prior crash"]
    REPAIR --> ITER{"iterations left<br/>(≤ agent.max_turns,<br/>default 90)?"}

    ITER -- "no" --> SUMMARY["strip tools → ask for<br/>final summary answer"]
    SUMMARY --> DONE

    ITER -- "yes" --> PRESSURE{"pre-API context<br/>pressure check"}
    PRESSURE -- "over threshold" --> COMPRESS["context compressor:<br/>prune/summarize middle history,<br/>protect head/tail"]
    PRESSURE -- "ok" --> ASSEMBLE
    COMPRESS --> ASSEMBLE["assemble request:<br/>system prompt + history<br/>+ context gauge tail (034)"]

    ASSEMBLE --> CALL["call_with_retries<br/>(jittered backoff,<br/>api_max_retries = 3)"]
    CALL --> FALLBACK{"hard provider<br/>failure?"}
    FALLBACK -- "yes" --> FB["walk fallback_providers<br/>chain, patch identity lines"]
    FB --> CALL
    FALLBACK -- "413 / length error" --> OVERFLOW["handle_context_overflow_error:<br/>compress, retry"]
    OVERFLOW --> CALL
    FALLBACK -- "ok" --> RESP{"response has<br/>tool_calls?"}

    RESP -- "no" --> DONE(["Done event → TurnResult"])
    RESP -- "yes" --> VALIDATE["validate + repair calls:<br/>fuzzy-match hallucinated names,<br/>normalize empty args to {}"]
    VALIDATE --> CLASSIFY{"classification"}
    CLASSIFY -- "all-valid" --> EXEC
    CLASSIFY -- "mixed" --> EXEC["execute valid subset,<br/>report invalid to model"]
    CLASSIFY -- "all-invalid ×3 strikes" --> ABORT(["abort turn with error"])

    EXEC --> PERSIST2["persist assistant tool-call<br/>message BEFORE side effects"]
    PERSIST2 --> DISPATCH["execute_tool_calls:<br/>parallel-safe batch runs concurrently,<br/>others sequential with tool_delay"]
    DISPATCH --> WRAP["wrap untrusted tool output,<br/>oversized results spill to storage"]
    WRAP --> PERSIST3["persist tool results"]
    PERSIST3 --> STEER{"pending /steer<br/>or queued message?"}
    STEER -- "yes" --> INJECT["inject out-of-band<br/>user message"]
    STEER -- "no" --> ITER
    INJECT --> ITER
```

Key invariants: the system prompt is built once per session and never
re-rendered; every durable message is persisted the moment it exists;
ephemeral repair scaffolding is never written to the DB.

## 2. Tool dispatch pipeline

How one batch of tool calls flows from model output to persisted results.
→ [tools.md](tools.md)

```mermaid
flowchart TD
    IN(["assistant tool_calls"]) --> HOOK{"PreToolUse hook<br/>configured?"}
    HOOK -- "yes" --> HOOKRUN["run shell hook<br/>(JSON on stdin)"]
    HOOKRUN --> HOOKDEC{"exit code"}
    HOOKDEC -- "deny" --> DENY["tool result: denied reason"]
    HOOKDEC -- "rewrite" --> REWRITE["apply argument rewrite"]
    HOOKDEC -- "allow" --> GUARDS
    HOOK -- "no" --> GUARDS

    REWRITE --> GUARDS["per-tool guard layers:<br/>file-path safety, URL/SSRF check,<br/>dangerous-command approval"]
    GUARDS --> SPLIT{"parallel-safe<br/>(read-only)?"}

    SPLIT -- "yes" --> CONC["dispatch concurrently<br/>(join all results)"]
    SPLIT -- "no" --> SEQ["dispatch sequentially,<br/>tool_delay between calls"]

    CONC --> EXEC["Tool::execute — one of:<br/>files · terminal · process · web · browser ·<br/>memory · todo · skills · session search ·<br/>clarify · LSP · NeuroCode · MCP · delegate_task"]
    SEQ --> EXEC

    EXEC --> CPUHEAVY{"CPU-heavy output?<br/>(terminal post-processing)"}
    CPUHEAVY -- "yes" --> POOL["joey-compute pool<br/>(dedicated OS threads)"]
    CPUHEAVY -- "no" --> POST
    POOL --> POST["post-process:<br/>truncate → strip ANSI → redact secrets"]

    POST --> BIG{"oversized output?"}
    BIG -- "yes" --> SPILL["spill to ~/.joey/storage,<br/>return head preview + pointer"]
    BIG -- "no" --> WRAP2["wrap untrusted content<br/>(threat scan for web/browser/MCP)"]
    SPILL --> WRAP2
    WRAP2 --> HEAVYIN{"heavyweight inline payload?<br/>(script / file content, 034)"}
    HEAVYIN -- "yes" --> PAYLOAD["payload store:<br/>persist to session dir,<br/>replace with @payload reference"]
    HEAVYIN -- "no" --> OUT
    PAYLOAD --> OUT(["tool results → history + SQLite"])
```

## 3. Provider stack

`joey-providers`: one client, ten registered profiles, three wire
protocols. → [providers.md](providers.md)

```mermaid
flowchart TD
    subgraph client ["ProviderClient (joey-providers)"]
        direction TB
        RESOLVE["resolve profile<br/>model.provider / base_url / API key<br/>(env → .env → config)"]
        WIRE{"wire protocol"}
        CC["OpenAI Chat Completions<br/>(+ tool calls, Copilot-compatible)"]
        RESP["OpenAI Responses API"]
        MSG["Anthropic Messages<br/>(+ thinking blocks)"]
        RETRY["retry policy:<br/>error classification →<br/>jittered exponential backoff,<br/>api_max_retries total attempts"]
        FB2["fallback_providers chain<br/>on hard failure"]
        SSE["SSE stream normalizer<br/>→ content/reasoning/tool-call deltas"]
    end

    RESOLVE --> WIRE
    WIRE --> CC & RESP & MSG
    CC & RESP & MSG --> RETRY
    RETRY --> FB2
    FB2 --> SSE

    subgraph profiles ["Registered profiles (10)"]
        P1["OpenRouter"]
        P2["Anthropic"]
        P3["OpenAI"]
        P4["GitHub Copilot<br/>(device-flow OAuth)"]
        P5["ai-usage-hud"]
        P6["Nous Portal"]
        P7["DeepSeek"]
        P8["Google Gemini"]
        P9["Z.AI"]
        P10["xAI"]
    end

    subgraph custom ["Custom endpoints"]
        C1["any OpenAI-compatible<br/>base_url + key"]
        C2["local servers discovered by<br/>joey discover (Ollama, LM Studio,<br/>llama.cpp, LiteLLM, MLX)"]
    end

    profiles --> RESOLVE
    custom --> RESOLVE

    SSE --> OUT(["NormalizedResponse / stream<br/>→ turn loop"])
```

## 4. System prompt assembly

Built exactly once per session (`Agent::new` → `build_system_prompt`) so
provider prompt-prefix caches stay warm. →
[agent-core-reference.md](agent-core-reference.md#2-system-prompt-assembly-promptrs-guidancers)

```mermaid
flowchart TD
    subgraph once ["Agent::new — once per session"]
        direction TB
        ID["identity block<br/>(SOUL.md / branding)"]
        ENV["environment section<br/>(cwd, platform, date, git)"]
        GUID["guidance constants<br/>(verbatim upstream + joey additions:<br/>context economy, adaptive coding,<br/>goal-directed execution 031)"]
        SKILLS["skills index<br/>+ available_skills preamble"]
        PROJ["project context files<br/>(AGENTS.md-style, sanitized)"]
        MEM["persistent memory +<br/>user profile (char-budgeted)"]
        BUILD["concatenate → cache String<br/>for the whole session"]
    end

    ID & ENV & GUID & SKILLS & PROJ & MEM --> BUILD

    BUILD --> NARROW["two narrow in-place rewrites only:<br/>set_session_store splices Session ID;<br/>switch_model/fallback patches identity lines"]

    subgraph perturn ["per request (tail-only, cache-safe)"]
        direction TB
        T1["system prompt (frozen prefix)"]
        T2["message history"]
        T3["dynamic context assembly:<br/>relevance-ranked tool schemas,<br/>state block budget (opt-in)"]
        T4["context gauge tail line (034,<br/>default on): total_tokens left"]
    end

    NARROW --> T1
    T1 --> T2 --> T3 --> T4
    T4 --> REQ(["ProviderRequest"])
```

## 5. Context management pipeline

Four cooperating layers keep long sessions inside the model window.
→ [context-economy.md](context-economy.md),
[context-assembly.md](context-assembly.md),
[agent-core-reference.md](agent-core-reference.md#3-context-compression-compression)

```mermaid
flowchart TD
    subgraph economy ["Context economy (feature 028, default on)"]
        E1["session scratchpad tool:<br/>record discoveries as found"]
        E2["deterministic state block:<br/>Goal / Constraints / Completed /<br/>Active / Blocked / Decisions"]
        E3["mid-turn tool-result hygiene"]
        E4["boundary-aligned cleanup<br/>at turn boundaries"]
        E5["economy guidance +<br/>retrieval-verification nudge"]
    end

    subgraph assembly ["Dynamic context assembly (opt-in)"]
        A1["relevance-rank tool schemas<br/>→ top-k selection"]
        A2["always-keep tool pinning"]
        A3["state-block character budget"]
        A4["per-step JSONL assembly log"]
    end

    subgraph f034 ["Feature 034 additions"]
        F1["live context gauge<br/>(context_gauge.enabled, default on)<br/>— LOW flag at compression threshold"]
        F2["unified notice channel<br/>(notice_channel.enabled, default off):<br/>system-notice markers"]
        F3["reasoning prune<br/>(reasoning_prune.enabled, default off):<br/>strip thinking from completed turns,<br/>request copy only"]
        F4["calm compaction framing<br/>(compaction.calm_framing, default on)"]
        F5["payload store:<br/>heavyweight inline payloads →<br/>@payload references, cross-session auto-copy"]
    end

    subgraph compressor ["Context compressor (compression/)"]
        C1{"trigger: pre-API pressure,<br/>usage threshold, or 413 overflow"}
        C2["protect head (task) +<br/>tail (recent turns)"]
        C3["structured summary of pruned middle:<br/>goal, state, decisions, key facts"]
        C4["per-session lock —<br/>one compression at a time"]
        C5["retry request after compression"]
    end

    E1 & E2 & E3 & E4 & E5 --> assembly
    A1 & A2 & A3 & A4 --> f034
    F1 & F2 & F3 & F4 & F5 --> C1
    C1 -- "fire" --> C2 --> C3 --> C4 --> C5
    C1 -- "ok" --> OUT(["request sent"])
    C5 --> OUT
```

## 6. Subagent delegation (`delegate_task`)

`joey-orchestration::SubagentManager` — isolated child agents under shared
governance. → [orchestration.md](orchestration.md)

```mermaid
flowchart TD
    CALL(["model calls delegate_task<br/>(single or parallel batch)"]) --> ADMIT{"resource governance<br/>(feature 030):<br/>slot pool, budgets, watchdog"}
    ADMIT -- "denied" --> WAITED(["queued / rejected with reason"])
    ADMIT -- "admitted" --> CTX["build isolated child context:<br/>fresh history, task brief only"]
    CTX --> MODEL["pick model:<br/>explicit per-task pick → llm-selector<br/>allocation → parent default"]
    MODEL --> CHILD["child Agent::run_turn<br/>with own toolset + budgets<br/>(read-only explorers or full implementors)"]
    CHILD --> EVENTS["orchestration events →<br/>TUI panes / rail / job board"]
    CHILD --> RESULT["distilled conclusions only<br/>(context economy: parent keeps summaries,<br/>not raw child transcripts)"]
    RESULT --> GRANT["finished child's slot passes<br/>to next waiter immediately"]
    GRANT --> OUT(["results → parent history"])
```

## 7. HyperCode orchestration pipeline

`/hypercode run <goal>` — the orchestrator owns all thinking; children are
execution-only. → [orchestration.md](orchestration.md)

```mermaid
flowchart TD
    GOAL(["/hypercode run &lt;goal&gt;"]) --> PLAN["orchestrator plans:<br/>phase briefs, exact file paths,<br/>exact edits, expected outcomes"]
    PLAN --> EXPLORE["explore wave:<br/>read-only explorer subagents answer<br/>single-question lookups with raw facts<br/>(paths, line numbers, quotes, output)"]
    EXPLORE --> BUILD["build wave:<br/>implementor subagents apply<br/>fully-specified minimal briefs verbatim,<br/>verify with targeted crate-scoped checks only"]
    BUILD --> GATE{"final gate:<br/>full test suite,<br/>exactly once"}
    GATE -- "failures" --> FIX["one fix round:<br/>dispatch targeted repair briefs"]
    FIX --> GATE
    GATE -- "pass" --> SYNTH["orchestrator synthesizes result"]
    SYNTH --> OUT(["Done"])

    PLAN -.-> TEAMS["agent teams (feature 022, opt-in):<br/>lead-coordinated, shared task list,<br/>per-teammate mailboxes, roles"]
    BUILD -.-> GOV["wave children governed (030):<br/>slot pool, no chunk barrier"]
    BUILD -.-> COMP["compute weights (033):<br/>heavier briefs → higher pool priority"]
```

## 8. Browser explorer steering loop (feature 035)

One persistent, orchestrator-steered browser explorer — the orchestrator's
eyes and hands. Opt-out via `hypercode.browser_explorer.enabled` (default
`true`). → [orchestration.md](orchestration.md), [browser.md](browser.md)

```mermaid
sequenceDiagram
    autonumber
    participant ORCH as HyperCode orchestrator
    participant REG as BrowserExplorerRegistry<br/>(joey-orchestration)
    participant EXPL as Explorer subagent<br/>(persistent child)
    participant BR as joey-browser session<br/>(Chromium via CDP)

    ORCH->>REG: designate(child_id, goal)
    alt another explorer already active
        REG-->>ORCH: reject — names active child + goal
    else first designation
        REG-->>ORCH: active explorer recorded
    end
    ORCH->>EXPL: navigation instruction<br/>(via subagent_control steering)
    EXPL->>BR: browser_* action on open page
    Note over BR: should_skip_navigate:<br/>same URL + live tab + no reload<br/>= skip re-navigation (page stays warm)
    BR-->>EXPL: resulting page state
    EXPL-->>ORCH: [PAGE-REPORT]: outcome +<br/>complete page snapshot + current URL
    Note over EXPL: never improvises — failures report<br/>error + unchanged page state
    ORCH->>EXPL: next instruction (same explorer,<br/>same session, same open page)
    Note over ORCH,EXPL: loop repeats without restarts —<br/>steered run ≤ 50% of fresh-subagent-per-step baseline
    ORCH->>REG: clear on stop / completion
```

## 9. Enterprise TaskGraph runtime (spec 023)

The typed, evidence-producing execution plane behind
`hypercode.execution_graph.enabled` (default off). →
[architecture.md](architecture.md#enterprise-orchestration-runtime-spec-023)

```mermaid
flowchart TD
    PLAN(["planner output / legacy plan"]) --> CONVERT["convert to TaskGraph<br/>(joey-taskgraph/1 JSON,<br/>six validation invariants)"]
    CONVERT --> ANALYZE["enterprise analysis plane<br/>(joey-neurocode, additive):<br/>policy combine · risk scoring ·<br/>verification-plan derivation"]
    ANALYZE --> SCHED["deterministic wave scheduler:<br/>ConflictAnalyzer partitioning,<br/>semaphore concurrency cap"]
    SCHED --> ISO["workspace isolation:<br/>git worktree per wave<br/>(full-copy fallback)"]
    ISO --> WORKERS["wave workers execute<br/>(max_concurrent_workers = 16)"]
    WORKERS --> EV["append-only evidence:<br/>~/.joey/hypercode/…/runs/&lt;id&gt;/<br/>decisions.jsonl (13-cause vocabulary)"]
    EV --> JOIN["joiner: ChangeBundle,<br/>git apply --3way,<br/>pre-apply conflict surfacing,<br/>no commits"]
    JOIN --> GATES{"VerificationGate:<br/>awaited gates, repair ledger,<br/>exhaustion ladder (3 attempts)"}
    GATES -- "defects" --> REPAIR["repair round → back to workers"]
    REPAIR --> WORKERS
    GATES -- "pass / degraded" --> RESULT(["run outcome + evidence bundle"])
```

## 10. NeuroCode code intelligence + RAG

Per-project code graph + semantic retrieval. →
[features/joey-neurocode.md](features/joey-neurocode.md),
[features/joey-neurocode-rag.md](features/joey-neurocode-rag.md)

```mermaid
flowchart LR
    subgraph ingest ["Ingest (consent-gated)"]
        direction TB
        SRC["project sources<br/>(Java/Pega-aware, tree-sitter)"]
        PARSE["parse → symbols, calls,<br/>dependencies, hubs"]
        GRAPH[("code graph SQLite<br/>graph.db, schema v3")]
        TIER["tiered complexity classifier<br/>+ GraphHub signals"]
    end

    subgraph rag ["RAG (joey-neurocode-rag)"]
        direction TB
        EMB["embedding backends ×5:<br/>Copilot · ONNX nomic-embed local ·<br/>OpenAI-compatible · Gemini · Ollama"]
        VEC[("memory_vectors<br/>schema v4, quantized")]
        HYB["hybrid search:<br/>BM25 + vector, RRF fusion"]
    end

    subgraph consumers ["Consumers"]
        direction TB
        CTX["context assembly:<br/>ranked code context injected<br/>into subagent briefs"]
        TOOLS2["neurocode_* model tools<br/>+ /neurocode command family<br/>+ NL ingest"]
        ROUTE["tier-informed model routing<br/>(feeds llm-selector)"]
        MEM["adaptive memory (027):<br/>session distillate + outcome memory"]
        FEED["TUI live context panel<br/>+ active badge"]
    end

    SRC --> PARSE --> GRAPH --> TIER
    GRAPH --> EMB --> VEC --> HYB
    TIER --> CTX
    HYB --> CTX
    TIER & HYB --> TOOLS2 & ROUTE & MEM & FEED
```

## 11. Compute pool (feature 033)

`joey-compute` — CPU work never touches tokio workers. →
[compute-pool.md](compute-pool.md)

```mermaid
flowchart TD
    SUB(["submit(agent, weight, op)<br/>— e.g. terminal output post-processing"]) --> SEM{"tokio semaphore:<br/>queued + running ≤ max_inflight<br/>(default 256)"}
    SEM -- "full" --> BACKPRESSURE(["submitter awaits permit<br/>(backpressure point)"])
    BACKPRESSURE --> SEM
    SEM -- "permit granted" --> ENQ["enqueue Entry:<br/>deadline = now + scale/weight<br/>(scale = 2000 ms, weight ∈ [1,100])"]
    ENQ --> HEAP["max-heap, inverted Ord:<br/>earliest-deadline-first,<br/>FIFO tiebreak by seq no."]
    HEAP --> WORKER["worker thread compute-{i}<br/>(workers = max(cores − 1, 1))"]
    WORKER --> PANIC{"op panics?<br/>(catch_unwind)"}
    PANIC -- "yes" --> ISO(["JobError::Panicked to caller,<br/>pool unaffected"])
    PANIC -- "long op" --> CHUNK["run_chunked: check<br/>CancelToken between chunks"]
    PANIC -- "no" --> POST2["metrics: counters, in-flight gauge,<br/>per-weight-class timings"]
    CHUNK --> POST2
    POST2 --> OUT(["one-shot channel → result;<br/>permit released"])
    WATCH["watchdog (warn-only)<br/>+ close-drain on shutdown"] -.-> WORKER
```

Starvation bound: a weight-1 job waits at most `scale` even under
sustained weight-100 pressure — verified by the fairness suite.

## 12. Dynamic LLM selector

`model.default = auto` engages per-module model allocation. →
[features/joey-llm-selector.md](features/joey-llm-selector.md)

```mermaid
flowchart TD
    AUTO(["model.default = auto"]) --> POOL["CandidateModelPool<br/>by tier: Flash / Standard /<br/>Versatile / Frontier"]
    ASK(["call-site asks:<br/>main turn · compression · subagent"]) --> ALLOC{"AllocationMap<br/>(persisted allocations.json)"}
    ALLOC -- "warm" --> USE(["allocated model for module"])
    ALLOC -- "cold start" --> SCORE["ColdStartScorer:<br/>tier heuristics per module"]
    SCORE --> USE
    USE --> RUN["turn runs with picked model"]
    RUN --> DIAG["diagnoser observes:<br/>latency, failures, token spend<br/>vs per-module budgets"]
    DIAG --> LEARN{"reallocation<br/>warranted?"}
    LEARN -- "yes" --> UPDATE["update AllocationMap<br/>(persisted)"]
    LEARN -- "no" --> RUN
    UPDATE --> ALLOC
```

Explicit `--model`/`--provider` pins always win — dynamic routing never
rewrites an explicit pick.

## 13. Security and sanitization layers

Defense in depth: every untrusted path has its own layer. →
[security.md](security.md)

```mermaid
flowchart TD
    subgraph sources ["Untrusted sources"]
        S1["web_search / web_extract"]
        S2["browser_* page content"]
        S3["mcp_* tool results"]
        S4["context files (AGENTS.md etc.)"]
        S5["terminal command output"]
        S6["MCP server configs"]
    end

    subgraph layers ["Sanitization / threat-scan layers"]
        direction TB
        L1["untrusted-content wrapping<br/>(marked, never inside<br/>system-notice markers — 034)"]
        L2["threat scanning before model<br/>or shell sees content"]
        L3["SSRF / URL safety<br/>(url_safety)"]
        L4["file-safety guards<br/>(path traversal, protected paths)"]
        L5["dangerous-command approval<br/>(rm -rf, sudo, …)"]
        L6["secret redaction everywhere<br/>(joey-core::redact)"]
    end

    subgraph sink ["Protected sinks"]
        K1["model context"]
        K2["shell / filesystem"]
        K3["logs + persisted state"]
    end

    S1 & S2 & S3 --> L1 --> L2
    S4 --> L2
    S5 --> L6
    S6 --> L2
    L2 --> K1
    L3 --> K2
    L4 --> K2
    L5 --> K2
    L6 --> K3
```

Plus: credentials only in `.env` (auto-routed `_KEY`/`_TOKEN`/`_SECRET`/
`_PASSWORD` keys), sandboxing knobs, safe mode, and the deliberate
omission of Anthropic-OAuth subscription impersonation.

## 14. TUI engine-actor decoupling

The GUI never blocks on compute; a stuck engine can't freeze the UI. →
[tui.md](tui.md#guicompute-decoupling-engine-actor-model)

```mermaid
flowchart TD
    subgraph gui ["GUI task (ratatui event loop)"]
        direction TB
        RENDER["render widgets: transcript, reasoning panel,<br/>agent roster, NeuroCode feed, popups"]
        INPUT["input editor + keybindings<br/>+ shared history (↑/↓)"]
    end

    subgraph channels ["Channels (the only coupling)"]
        direction TB
        CH1["user input → engine"]
        CH2["AgentEvents → GUI<br/>(deltas, tool start/end,<br/>orchestration taps)"]
        CH3["control: interrupt / steer / kill"]
    end

    subgraph engine ["Engine task (owns the Agent)"]
        direction TB
        TURN["Agent::run_turn"]
        TOOLS3["tool execution,<br/>subagent fan-out, heavy jobs"]
    end

    INPUT --> CH1 --> TURN
    TURN --> TOOLS3
    TOOLS3 --> CH2 --> RENDER
    CTRL["Ctrl-C 1st press:<br/>cooperative interrupt<br/>(children interrupted too)"] --> CH3
    CTRL2["Ctrl-C 2nd press:<br/>force-kill engine, restart,<br/>history restored"] --> CH3
    CH3 --> engine
```

## 15. Cron scheduler lifecycle

Self-contained scheduler, Hermes-compatible `jobs.json`. →
[cron.md](cron.md)

```mermaid
flowchart TD
    CREATE(["joey cron add:<br/>duration · interval · cron expr<br/>(croniter semantics)"]) --> STORE["job store:<br/>~/.joey/cron/jobs.json"]
    STORE --> TICKER["ticker loop (60 s)"]
    TICKER --> DUE{"jobs due?"}
    DUE -- "no" --> TICKER
    DUE -- "yes" --> CLAIM["at-most-once claim<br/>(survives restarts)"]
    CLAIM --> KIND{"job kind"}
    KIND -- "agent job" --> AGENT["run agent turn with<br/>job prompt → delivery target"]
    KIND -- "script job" --> SCRIPT["run script, capture output"]
    AGENT & SCRIPT --> RETAIN["output retention window"]
    RETAIN --> TICKER
```

## 16. MCP client lifecycle

`joey-mcp`: stdio JSON-RPC. → [mcp.md](mcp.md)

```mermaid
flowchart TD
    CFG["config: mcp servers<br/>(validated: safe env, no secrets)"] --> SPAWN["spawn server subprocess<br/>(stdio, timeouts)"]
    SPAWN --> HS["initialize handshake<br/>(capabilities, protocol version)"]
    HS --> LIST["tools/list (+ pagination)"]
    LIST --> NORM["normalize schemas<br/>(sanitizer) + namespace as<br/>mcp__&lt;server&gt;__&lt;tool&gt;"]
    NORM --> REG["register into ToolRegistry<br/>alongside built-ins"]
    REG --> CALL(["model calls tool →<br/>JSON-RPC → result →<br/>sanitize → wrap untrusted"])
```

## 17. SpecKit workflow + Visual UI

The spec-kit lifecycle as native commands, plus the browser UI.
→ [speckit-workflow.md](speckit-workflow.md),
[speckit-ui.md](speckit-ui.md)

```mermaid
flowchart TD
    subgraph lifecycle ["Slash-command lifecycle (joey-cli)"]
        direction LR
        SP["/speckit-specify"] --> CL["/speckit-clarify"]
        CL --> PL["/speckit-plan"]
        PL --> TK["/speckit-tasks"]
        TK --> IM["/speckit-implement"]
        IM --> CV["/speckit-converge"]
    end

    subgraph artifacts ["Source of truth: .specify/ + specs/&lt;feature&gt;/"]
        direction TB
        A1["spec.md"]
        A2["plan.md"]
        A3["tasks.md"]
        A4["checklists · contracts · data-model"]
    end

    subgraph visual ["Visual UI (joey speckit)"]
        direction TB
        BACK["joey-speckit-ui backend:<br/>parsers (shared with joey-cli),<br/>workflow engine, git-backed staging"]
        API["REST + WebSocket API"]
        FE["web/speckit-ui frontend:<br/>artifact editing, run events"]
    end

    lifecycle <--> artifacts
    artifacts <--> BACK
    BACK --> API <--> FE
    LOCK["hash-based optimistic locking:<br/>conflict-checked writes —<br/>UI never diverges from files"] -.-> BACK
```

## 18. State and configuration layout

Layered config + one SQLite store per home. →
[state-and-config.md](state-and-config.md)

```mermaid
flowchart TD
    subgraph layers ["Config resolution (later wins)"]
        direction TB
        Y1["built-in defaults<br/>(~80 documented keys)"]
        Y2["~/.joey/config.yaml"]
        Y3["profile overlay:<br/>~/.joey/profiles/&lt;name&gt;/"]
        Y4[".env (secrets auto-routed:<br/>*_KEY / *_TOKEN / *_SECRET / *_PASSWORD)"]
        Y5["process environment"]
    end

    Y1 --> Y2 --> Y3 --> Y4 --> Y5 --> CFG(["resolved Config"])

    CFG --> HOME

    subgraph home ["~/.joey (JOEY_HOME override)"]
        direction TB
        DB2[("state.db — SQLite schema v22:<br/>sessions, messages, FTS5 search,<br/>non-destructive compaction")]
        STORE2["storage/ — spilled tool results,<br/>payload store per session"]
        CRON2["cron/jobs.json"]
        TEAMS["teams/ — persisted task lists"]
        HYPER["hypercode/ — run evidence,<br/>decisions.jsonl"]
        HIST[".joey_history — shared<br/>CLI/TUI input history"]
        OTHER["skills/ · memory ·<br/>SOUL.md · theme"]
    end

    CFG --> HOME
```

---

## Maintenance note

These diagrams describe behavior, not line numbers — they should stay
accurate as code moves. When a subsystem changes shape (new phase, new
gate, new layer), update the diagram in the same commit. Verified against
workspace source September 2026; if code and diagrams disagree, the code
wins — and please fix the diagram.
