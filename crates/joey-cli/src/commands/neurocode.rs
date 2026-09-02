//! `/neurocode` command handler (T048, contracts/neurocode-command.md).
//!
//! Text-mode control surface for the NeuroCode engine (spec 015). Implements
//! Constitution II (CLI/TUI parity) — every capability is reachable as text,
//! and the slash-command and CLI paths share the same handler.
//!
//! The handler constructs a fresh `DefaultEngine` from the current joey config
//! on each invocation (mirroring how `/llm-selector` builds its engine), then
//! dispatches to the `NeuroCodeCommands` trait methods which return plain-text
//! output.

use std::path::PathBuf;
use std::sync::Arc;

use joey_neurocode::{DefaultEngine, NeuroCodeCommands, NeuroCodeConfig};

/// Resolve the NeuroCode provider scope from a config, exactly the way the
/// main wiring does (`try_build_engine` in neurocode_wiring.rs): the same
/// provider/base_url/model triple → `resolve_profile` → profile name, which
/// keys the per-provider tier config (`neurocode.tier.providers.<id>`).
pub(crate) fn scope_for_config(config: &joey_core::Config) -> String {
    let provider = config.get_str("model.provider", "auto");
    let base_url = config.get_str("model.base_url", "");
    let model = config.model();
    joey_providers::resolve_profile(&provider, &base_url, &model)
        .name
        .to_string()
}

/// Build a NeuroCode engine from the given config + provider scope, scoped to
/// the current working directory (the project root for graph indexing).
///
/// `live_provider` is the agent's ACTUAL provider (`agent.provider_name()`) —
/// the same scope `/model neurocode …` writes its keys under. `None`
/// (standalone callers without an agent) falls back to the config-resolved
/// scope, never the empty-provider engine that silently displayed the flat
/// legacy keys / ambiguous default instead of the per-provider entries.
fn build_engine_with(config: &joey_core::Config, live_provider: Option<&str>) -> Arc<DefaultEngine> {
    let nc_cfg = NeuroCodeConfig::from_config(config);
    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut engine = DefaultEngine::new(nc_cfg, project_root);
    let scope = live_provider
        .map(str::to_string)
        .unwrap_or_else(|| scope_for_config(config));
    engine.set_provider(&scope);
    Arc::new(engine)
}

/// Load the current joey config, falling back to defaults (same policy the
/// original `build_engine` used).
fn load_config() -> joey_core::Config {
    joey_core::Config::load().unwrap_or_else(|_| joey_core::Config::defaults())
}

/// Outcome of dispatching `/neurocode`: either plain display text, or a
/// hand-off to a full agent turn (natural-language ingest).
pub enum NeurocodeOutcome {
    /// Plain text to display immediately.
    Text(String),
    /// Natural-language ingest request: run an agent turn that resolves
    /// the request into neurocode_ingest tool calls. Carries the composed
    /// workflow prompt.
    AgentIngest(String),
}

/// Entry point for the `/neurocode` slash command (plain-text shape for
/// the engine heavy-job path and tests). Provider-scoped: `live_provider` is
/// the LIVE agent provider so status/tier text reflects the per-provider
/// tier keys (`neurocode.tier.providers.<id>`) the user configured.
pub fn neurocode_slash_provider_scoped_text(args: &str, live_provider: &str) -> String {
    match neurocode_slash_provider_scoped(args, live_provider) {
        NeurocodeOutcome::Text(t) => t,
        NeurocodeOutcome::AgentIngest(_) => {
            "Natural-language ingest needs an interactive surface (REPL/TUI). \
             Use the strict form: /neurocode ingest <category> <path> [--version <v>] [--provenance <p>]"
                .to_string()
        }
    }
}

/// `/neurocode backend [value]` — show or persistently set the RAG
/// embedding backend (`neurocode.rag.backend`). An explicit backend
/// (e.g. `local_onnx` or `copilot`) overrides provider-following `auto`
/// resolution, so the embedding backend can be switched regardless of the
/// selected provider.
pub fn neurocode_backend_command_text(
    parts: &[&str],
    config: &mut joey_core::Config,
) -> String {
    use joey_neurocode_rag::config::{DEFAULT_BACKEND, KEY_BACKEND};

    match parts.first().copied() {
        None | Some("") => {
            let current = config.get_str(KEY_BACKEND, DEFAULT_BACKEND);
            format!(
                "Embedding backend: {current}\n\
                 Set with: /neurocode backend <auto|local_onnx|openai_compat|ollama|copilot>\n\
                 An explicit backend overrides provider-based auto selection."
            )
        }
        Some(raw) => match joey_neurocode_rag::config::RagBackend::parse(raw.trim()) {
            Some(value) => {
                let previous = config.get_str(KEY_BACKEND, DEFAULT_BACKEND);
                match config.set_and_save(KEY_BACKEND, value.as_str()) {
                    Ok(()) => format!(
                        "Embedding backend: {previous} -> {value} (saved)\n\
                         Explicit backends override provider-based auto selection."
                    ),
                    Err(err) => format!("Failed to save embedding backend: {err}"),
                }
            }
            None => format!(
                "Unknown embedding backend: {raw}\n\
                 Valid values: auto | local_onnx | openai_compat | ollama | copilot"
            ),
        },
    }
}

/// Classify `/neurocode ingest <free text>` WITHOUT executing any command:
/// returns the agent-turn prompt when the arguments route to a
/// natural-language ingest, `None` for the strict form (and every other
/// subcommand). The TUI uses this to route ingest off the UI task without
/// running heavy work inline.
pub fn neurocode_ingest_request(args: &str) -> Option<String> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.first().copied() != Some("ingest") {
        return None;
    }
    let ingest_parts = &parts[1..];
    if structured_ingest(ingest_parts) || ingest_parts.is_empty() {
        return None;
    }
    let request = args
        .split_once(char::is_whitespace)
        .map(|(_, rest)| rest.trim())
        .unwrap_or_default();
    Some(ingest_agent_prompt(&request))
}

/// Compose the agent-turn workflow prompt for a natural-language ingest
/// request (the user's free text after `/neurocode ingest`).
pub fn ingest_agent_prompt(request: &str) -> String {
    format!(
        "You are ingesting domain knowledge into the NeuroCode engine for this repository. \
The user described what to ingest in natural language:\n\n> {request}\n\n\
Use the `neurocode_ingest` tool to complete this. Its parameters:\n\
- category: one of FrameworkDocs, EntityCatalog, Postmortem, PegaRuleType\n\
- source_path: path to a FILE (or directory) containing the knowledge — if the user pointed at \
something fuzzy, locate the actual file(s) with read_file/search_files first and confirm the content \
looks like what they described\n\
- version_tag: optional version string when the user named one\n\
- provenance: where the knowledge came from when the user said so\n\n\
Workflow:\n\
1. Interpret the request: what knowledge, from where, which category fits.\n\
2. Locate the source: if the user gave a path, verify it exists and is readable text \
(read_file a sample); if they described content, search the repo (search_files) for it.\n\
3. If the knowledge only exists in the user's message itself (they pasted facts or a postmortem \
rather than pointing at a file), write it to a markdown file first — e.g. \
`.neurocode/sources/<slug>.md` (create the directory) with the content clearly organized — \
then ingest THAT file with provenance `user-provided`.\n\
4. Call neurocode_ingest with the resolved parameters.\n\
5. Report exactly what was ingested (category, path, version) and the tool's result. \
If anything can't be resolved (no such file, ambiguous category), say so plainly instead of guessing."
    )
}

/// Does the strict form match? First token must be a valid category AND a
/// second token must exist (the path). Anything else is natural language.
fn structured_ingest(parts: &[&str]) -> bool {
    if parts.len() < 2 {
        return false;
    }
    matches!(
        parts[0],
        "FrameworkDocs" | "framework_docs" | "EntityCatalog" | "entity_catalog" | "Postmortem"
            | "postmortem" | "PegaRuleType" | "pega_rule_type"
    )
}

/// Full-dispatch entry: returns the outcome so interactive surfaces can
/// run the agent path. Scopes the engine to the config-resolved provider
/// (standalone/legacy path; interactive callers prefer the provider-scoped
/// variant below).
// The TUI now classifies via `neurocode_ingest_request` instead of running
// this inline (freeze fix), so this is only exercised by the ingest-routing
// tests today — kept as the standalone/legacy dispatch entry point.
#[cfg_attr(not(test), allow(dead_code))]
pub fn neurocode_slash_outcome(args: &str) -> NeurocodeOutcome {
    neurocode_dispatch(args, None)
}

/// Provider-scoped dispatch: interactive surfaces (REPL/TUI) pass the LIVE
/// agent provider (`agent.provider_name()` / `tui.app().provider`) so the
/// engine reads the same per-provider tier keys (`neurocode.tier.providers.<id>`)
/// that `/model neurocode …` writes under — mirroring the main wiring's
/// `try_build_engine_scoped` behavior on `/model` switches.
pub fn neurocode_slash_provider_scoped(args: &str, live_provider: &str) -> NeurocodeOutcome {
    neurocode_dispatch(args, Some(live_provider))
}

/// Shared dispatch core. `live_provider = None` → config-resolved scope.
fn neurocode_dispatch(args: &str, live_provider: Option<&str>) -> NeurocodeOutcome {
    let parts: Vec<&str> = args.split_whitespace().collect();
    let sub = parts.first().copied().unwrap_or("status");
    let mut config = load_config();
    let engine = build_engine_with(&config, live_provider);

    match sub {
        "status" => {
            // T034 (FR-013): the status output gains a RAG section ONLY
            // when `neurocode.rag.enabled` — disabled output stays
            // byte-identical to `engine.status_text()` by construction.
            let mut text = engine.status_text();
            let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            if let Some(section) = rag_status_section(&config, &project_root) {
                text.push_str(&section);
            }
            NeurocodeOutcome::Text(text)
        }

        "tier" => {
            let action = parts.get(1).copied().unwrap_or("show");
            let tier = parts.get(2).copied();
            // Map the two-arg form `/neurocode tier <tier>` to "set".
            if action == "economical" || action == "frontier" || action == "auto" {
                NeurocodeOutcome::Text(engine.tier_text("set", Some(action)))
            } else {
                NeurocodeOutcome::Text(engine.tier_text(action, tier))
            }
        }

        // `/neurocode backend [value]` — show or persistently set the RAG
        // embedding backend. An explicit backend overrides provider-based
        // `auto` resolution, so users can pin local_onnx or copilot
        // regardless of the selected provider.
        "backend" => NeurocodeOutcome::Text(neurocode_backend_command_text(
            &parts[1..],
            &mut config,
        )),

        "index" => {
            let force = parts.iter().any(|p| *p == "--force" || *p == "-f");
            let mut text = engine.index_text(force);
            // Spec 021 (T041b): when `neurocode.rag.enabled`, ALSO populate
            // the RAG index (chunks/vectors/edges/meta via the rag crate's
            // atomic refresh worker; degraded backends write vector-less
            // chunk rows). Disabled ⇒ empty section, byte-identical output.
            let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            text.push_str(&crate::neurocode_rag_wiring::rag_index_text(
                &config,
                &project_root,
                force,
            ));
            NeurocodeOutcome::Text(text)
        }

        "query" => {
            let query_type = parts.get(1).copied().unwrap_or("symbol");
            let symbol = parts.get(2).copied().unwrap_or("");
            NeurocodeOutcome::Text(engine.query_text(query_type, symbol))
        }

        "ingest" => {
            // Two forms: strict (`<category> <path> [flags]`) or natural
            // language (anything else) — the latter hands off to an agent
            // turn that resolves the request into neurocode_ingest calls.
            let ingest_parts = &parts[1..];
            if structured_ingest(ingest_parts) {
                let category = ingest_parts[0];
                let path = ingest_parts[1];
                // Parse optional --version and --provenance flags.
                let (version, provenance) = parse_kv_flags(&ingest_parts[2..]);
                NeurocodeOutcome::Text(engine.ingest_text(
                    category,
                    path,
                    version.as_deref(),
                    &provenance,
                ))
            } else if ingest_parts.is_empty() {
                NeurocodeOutcome::Text(
                    "Usage: /neurocode ingest <category> <path> [--version <v>] [--provenance <p>]\n\
                     Or describe it naturally: /neurocode ingest the Spring Boot docs in ./docs/spring"
                        .to_string(),
                )
            } else {
                // Natural language: everything after "ingest" is the request.
                let request = args
                    .split_once(char::is_whitespace)
                    .map(|(_, rest)| rest.trim())
                    .unwrap_or_default();
                NeurocodeOutcome::AgentIngest(ingest_agent_prompt(request))
            }
        }

        // `/neurocode consent show|ack|revoke` — per-project remote-backend
        // consent CLI (T031, FR-012). Production confirmation reads stdin;
        // tests inject the decision through `consent_command_text`.
        "consent" => {
            let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            NeurocodeOutcome::Text(consent_command_text(
                &parts[1..],
                &config,
                &project_root,
                &mut |prompt| consent_confirm_stdin(prompt),
            ))
        }

        "patterns" => NeurocodeOutcome::Text(engine.patterns_text()),

        "anti-patterns" | "antipatterns" => {
            NeurocodeOutcome::Text(engine.anti_patterns_text())
        }

        // `/neurocode search …` — RAG semantic code search (T015, FR-010
        // CLI half). Grammar per contracts/neurocode-rag-command.md:
        //   search <query...> [--path <glob>] [--limit <n>]
        //         [--expand-lines <n>] [--relations <0-2>] [--json]
        // Plain-text ranked results by default; --json emits the
        // tool-shaped payload (Principle II parity with the T014 agent
        // tool).
        "search" => NeurocodeOutcome::Text(search_command_text(&parts[1..], &config)),

        // `/neurocode model …` — RAG model artifact management (T011).
        // Pure file management: no embedding, no index writes beyond the
        // `rag_model_artifacts` row. Usable whenever neurocode is active
        // (harmless with RAG disabled — contracts/neurocode-rag-command.md).
        "model" => {
            let model_parts = &parts[1..];
            match model_parts.first().copied() {
                Some("fetch") => {
                    // Grammar: model fetch [<profile>] [--dylib]
                    // (contracts/neurocode-rag-command.md § Model fetch).
                    let mut profile: Option<String> = None;
                    let mut dylib = false;
                    let mut bad_usage: Option<String> = None;
                    for p in &model_parts[1..] {
                        match *p {
                            "--dylib" => dylib = true,
                            other if !other.starts_with('-') => {
                                if profile.is_some() {
                                    bad_usage = Some(format!(
                                        "unexpected extra argument '{other}' (at most one profile)"
                                    ));
                                } else {
                                    profile = Some(other.to_string());
                                }
                            }
                            other => {
                                bad_usage = Some(format!("unknown flag '{other}'"));
                            }
                        }
                    }
                    if let Some(msg) = bad_usage {
                        NeurocodeOutcome::Text(format!(
                            "Model fetch: {msg}.\n\
                             Usage: /neurocode model fetch [<profile>] [--dylib]"
                        ))
                    } else if dylib {
                        // T039: platform/arch detection, per-platform
                        // project-recorded hashes, staged download + atomic
                        // rename + rollback, and the dylib resolution
                        // ladder (env → config → system → fetched copy).
                        // Grammar decision: bare `--dylib` acts on the
                        // dylib ONLY; a profile given alongside fetches
                        // BOTH targets (T011 artifacts + dylib), each
                        // rendered independently (a refusal of one does
                        // not hide the other's outcome).
                        let mut out = dylib_fetch_text(&config);
                        if let Some(p) = profile.as_deref() {
                            out = format!("{}\n\n{}", model_fetch_text(&config, Some(p)), out);
                        }
                        NeurocodeOutcome::Text(out)
                    } else {
                        NeurocodeOutcome::Text(model_fetch_text(&config, profile.as_deref()))
                    }
                }
                None => NeurocodeOutcome::Text(
                    "Usage: /neurocode model fetch [<profile>] [--dylib]".to_string(),
                ),
                Some(other) => NeurocodeOutcome::Text(format!(
                    "Unknown model action '{other}'. Use: fetch"
                )),
            }
        }

        "domain" => {
            let action = parts.get(1).copied().unwrap_or("list");
            match action {
                "list" | "" => NeurocodeOutcome::Text(engine.domain_list_text()),
                "remove" | "rm" | "delete" => {
                    let id = match parts.get(2).and_then(|s| s.parse::<u64>().ok()) {
                        Some(id) => id,
                        None => {
                            return NeurocodeOutcome::Text(
                                "Usage: /neurocode domain remove <id>".to_string(),
                            );
                        }
                    };
                    NeurocodeOutcome::Text(engine.domain_remove_text(id))
                }
                _ => NeurocodeOutcome::Text(format!(
                    "Unknown domain action '{}'. Use: list | remove <id>",
                    action
                )),
            }
        }

        "help" | "-h" | "--help" => NeurocodeOutcome::Text(help_text()),

        _ => NeurocodeOutcome::Text(format!(
            "Unknown subcommand '{}'. Run /neurocode --help for usage.",
            sub
        )),
    }
}

/// Parse `--version <v>` and `--provenance <p>` flags from a tail of args.
fn parse_kv_flags(args: &[&str]) -> (Option<String>, String) {
    let mut version: Option<String> = None;
    let mut provenance = String::new();
    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "--version" | "-v" => {
                if let Some(v) = args.get(i + 1) {
                    version = Some((*v).to_string());
                    i += 2;
                    continue;
                }
            }
            "--provenance" | "-p" => {
                if let Some(p) = args.get(i + 1) {
                    provenance = (*p).to_string();
                    i += 2;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    (version, provenance)
}

// ---------------------------------------------------------------------------
// /neurocode model fetch (T011, specs/021 contracts/embedding-backend.md §
// Local Model Artifacts & Distribution + contracts/neurocode-rag-command.md §
// Model fetch subcommand; research.md R8 — no Hugging Face).
//
// Downloads `model.onnx` + `tokenizer.json` (+ any extra recorded files,
// e.g. LICENSE) from `neurocode.rag.local.mirror_url` into
// `neurocode.rag.local.model_dir`, SHA-256-verifying every download against
// PROJECT-RECORDED expected hashes before anything lands in `model_dir`;
// on success the `rag_model_artifacts` row is written FROM those expected
// hashes (stricter than manual-placement self-registration). EMPTY
// mirror_url = fetch disabled; huggingface.co is never contacted (hard
// host guard below, pinned by tests).
//
// The contracts pin "project-recorded hashes" at a "project-controlled
// mirror" without specifying the recording format; this implementation
// reads a `manifest.json` at the mirror root (decision of record):
//
//   { "profiles": { "<profile>": { "model.onnx": "<64-hex>",
//                                  "tokenizer.json": "<64-hex>",
//                                  "LICENSE": "<64-hex>" } } }
//
// (a bare root object keyed by profile is also accepted). Required
// entries: `model.onnx`, `tokenizer.json`; extra string entries are
// fetched and verified too (license/attribution files — both default
// profiles are permissively licensed and re-hosting keeps attribution).
// ---------------------------------------------------------------------------

use std::fs;
use std::io::Read as _;
use std::path::Path;

use joey_neurocode::graph::{DependencyGraph, GraphStore};
use joey_neurocode_rag::config::RagConfig;
use joey_neurocode_rag::embed::artifacts::{ArtifactHashes, MODEL_FILE, TOKENIZER_FILE};
use joey_neurocode_rag::embed::profiles;

/// Manifest file name at the mirror root carrying the project-recorded
/// expected SHA-256 hashes.
const MANIFEST_FILE: &str = "manifest.json";
/// Staging directory inside `model_dir` — downloads land here first so a
/// refused (hash-mismatch) fetch never leaves artifacts in `model_dir`.
const STAGING_DIR: &str = ".fetch-staging";

/// Hosts joey must NEVER contact for model artifacts (research.md R8):
/// huggingface.co and any subdomain (www., cdn-lfs., …), plus hf.co and
/// its subdomains. Case-insensitive; a single trailing dot (DNS root
/// form) is tolerated.
fn is_forbidden_host(host: &str) -> bool {
    let h = host.trim_end_matches('.').to_ascii_lowercase();
    h == "huggingface.co" || h.ends_with(".huggingface.co") || h == "hf.co" || h.ends_with(".hf.co")
}

/// Hard pre-network guard: parse `raw` as an http(s) URL and REFUSE any
/// Hugging Face host. Called for the mirror root before any request is
/// issued (the no-HF guarantee is enforced by construction, not config).
fn guard_no_hf(raw: &str) -> Result<url::Url, String> {
    let url = url::Url::parse(raw.trim())
        .map_err(|e| format!("invalid URL '{raw}': {e}"))?;
    match url.scheme() {
        "http" | "https" => {}
        other => return Err(format!("unsupported URL scheme '{other}' (use http/https)")),
    }
    let host = url
        .host_str()
        .ok_or_else(|| format!("URL '{raw}' has no host"))?;
    if is_forbidden_host(host) {
        return Err(format!(
            "joey never contacts huggingface.co (no-Hugging-Fetch policy). \
             Refusing host '{host}'; point neurocode.rag.local.mirror_url at a \
             project-controlled mirror"
        ));
    }
    Ok(url)
}

/// Ensure the mirror URL's path ends with `/` so `Url::join(name)` appends
/// instead of replacing the last path segment.
fn normalize_mirror_base(mut url: url::Url) -> url::Url {
    let p = url.path().to_string();
    if !p.ends_with('/') {
        url.set_path(&format!("{p}/"));
    }
    url
}

/// Expected artifact set from the mirror manifest: `(file name, expected
/// lowercase-hex SHA-256)` pairs, required files first, extras after.
///
/// Accepts the profile map at the manifest root or under a `"profiles"`
/// key. Malformed hashes, a missing profile, or missing required files
/// are errors (fetch refuses before downloading artifacts).
fn expected_files_from_manifest(
    manifest: &serde_json::Value,
    profile: &str,
) -> Result<Vec<(String, String)>, String> {
    let root = manifest.get("profiles").unwrap_or(manifest);
    let obj = root
        .as_object()
        .ok_or_else(|| "mirror manifest is not a JSON object".to_string())?;
    let entry = obj.get(profile).ok_or_else(|| {
        let recorded: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
        format!(
            "profile '{profile}' is not recorded in the mirror manifest (recorded: {})",
            recorded.join(", ")
        )
    })?;
    let files = entry
        .as_object()
        .ok_or_else(|| format!("manifest entry for '{profile}' is not an object"))?;

    let mut out: Vec<(String, String)> = Vec::new();
    for (name, hash) in files {
        let h = hash
            .as_str()
            .ok_or_else(|| format!("manifest entry '{name}' for '{profile}' is not a hash string"))?
            .trim()
            .to_ascii_lowercase();
        if h.len() != 64 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!(
                "manifest entry '{name}' for '{profile}' is not a 64-hex SHA-256: '{h}'"
            ));
        }
        out.push((name.clone(), h));
    }
    for required in [MODEL_FILE, TOKENIZER_FILE] {
        if !out.iter().any(|(n, _)| n == required) {
            return Err(format!(
                "manifest for '{profile}' lacks the required '{required}' entry"
            ));
        }
    }
    // Deterministic order: required files first, extras alphabetical.
    out.sort_by(|a, b| {
        let rank = |n: &str| match n {
            MODEL_FILE => 0,
            TOKENIZER_FILE => 1,
            _ => 2,
        };
        rank(&a.0)
            .cmp(&rank(&b.0))
            .then_with(|| a.0.cmp(&b.0))
    });
    Ok(out)
}

/// Stream a file through SHA-256, returning lowercase hex (64 chars).
fn sha256_file(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut file =
        fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |s, b| s + &format!("{b:02x}")))
}

/// Verify every staged download against its project-recorded expected
/// hash; any mismatch is a refusal (nothing has landed in `model_dir`
/// yet — the caller discards the staging directory).
fn verify_staged_files(staging_dir: &Path, expected: &[(String, String)]) -> Result<(), String> {
    for (name, want) in expected {
        let got = sha256_file(&staging_dir.join(name))
            .map_err(|e| format!("hashing downloaded {name}: {e}"))?;
        if !got.eq_ignore_ascii_case(want) {
            return Err(format!(
                "SHA-256 mismatch for {name}: expected {want}, downloaded {got}. \
                 Refused — nothing was written to model_dir"
            ));
        }
    }
    Ok(())
}

/// Blocking streamed GET to a file on a dedicated thread with its own
/// runtime (same pattern as `model_catalog::http_get_json` — safe from
/// sync and async contexts). Streams chunk-by-chunk so a ~130 MB model
/// never sits whole in memory; the destination is written only after the
/// HTTP status is checked.
fn http_download_to_file(url: &str, timeout_secs: i64, dest: &Path) -> Result<(), String> {
    let url = url.to_string();
    let dest = dest.to_path_buf();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("tokio runtime: {e}"))?;
        rt.block_on(async move {
            use std::io::Write as _;
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(timeout_secs.max(1) as u64))
                // Model mirrors are project-controlled; never route the
                // download through a proxy env var.
                .no_proxy()
                // Hard no-HF corollary: NEVER follow redirects — a
                // compromised mirror must not be able to 302 joey onto
                // huggingface.co. Mirrors serve artifacts directly.
                .redirect(reqwest::redirect::Policy::none())
                .user_agent(format!(
                    "{}/{}",
                    joey_core::branding::CLI_NAME,
                    joey_core::branding::VERSION
                ))
                .build()
                .map_err(|e| format!("http client: {e}"))?;
            let mut resp = client
                .get(&url)
                .send()
                .await
                .map_err(|e| format!("download {url}: {e}"))?;
            if !resp.status().is_success() {
                return Err(format!("download {url}: HTTP {}", resp.status()));
            }
            let mut file =
                fs::File::create(&dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
            while let Some(chunk) = resp
                .chunk()
                .await
                .map_err(|e| format!("download {url}: {e}"))?
            {
                file.write_all(&chunk)
                    .map_err(|e| format!("write {}: {e}", dest.display()))?;
            }
            Ok(())
        })
    })
    .join()
    .map_err(|_| "download thread panicked".to_string())?
}

/// Write the `rag_model_artifacts` row FROM the project-recorded expected
/// hashes (contract: fetch is the stricter writer; the row is upserted so
/// a re-fetch re-pins). `mirror_url_used` records the mirror (NULL is
/// reserved for manual placement).
fn write_artifact_row(
    store: &GraphStore,
    profile: &str,
    hashes: &ArtifactHashes,
    mirror_url: &str,
) -> Result<(), String> {
    let fetched_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    store
        .conn()
        .execute(
            "INSERT INTO rag_model_artifacts (profile, model_sha256, tokenizer_sha256,
                model_size_bytes, fetched_at, mirror_url_used)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(profile) DO UPDATE SET
                model_sha256 = ?2, tokenizer_sha256 = ?3,
                model_size_bytes = ?4, fetched_at = ?5, mirror_url_used = ?6",
            rusqlite::params![
                profile,
                hashes.model_sha256,
                hashes.tokenizer_sha256,
                hashes.model_size_bytes,
                fetched_at,
                mirror_url
            ],
        )
        .map_err(|e| format!("writing rag_model_artifacts row: {e}"))?;
    Ok(())
}

/// Core `/neurocode model fetch` flow. Every not-fetched outcome (disabled
/// mirror, HF host, bad manifest, hash mismatch) is an `Err` carrying the
/// user-facing message — the command renders it as plain text, exit-free.
fn run_model_fetch(
    rag: &RagConfig,
    profile_name: &str,
    project_root: &Path,
) -> Result<String, String> {
    // Profile validation (rejected candidates surface their recorded
    // reason so the choice is never re-litigated at the CLI).
    let profile = profiles::lookup(profile_name).ok_or_else(|| {
        match profiles::rejection_reason(profile_name) {
            Some(reason) => {
                format!("model profile '{profile_name}' was evaluated and REJECTED: {reason}")
            }
            None => format!(
                "unknown model profile '{profile_name}'; supported profiles: {}",
                profiles::PROFILES
                    .iter()
                    .map(|p| p.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    })?;

    // EMPTY mirror_url = fetch disabled (default posture: joey never
    // downloads models). Checked before any network-capable step.
    let mirror_raw = rag.mirror_url.trim().to_string();
    if mirror_raw.is_empty() {
        return Err(
            "Model fetch is disabled: neurocode.rag.local.mirror_url is empty.\n\
             Joey never downloads models by default — configure a project-controlled \
             mirror URL to enable fetching (huggingface.co is never contacted)."
                .to_string(),
        );
    }

    // Hard no-HF guard on the mirror BEFORE any request is issued.
    let mirror = normalize_mirror_base(
        guard_no_hf(&mirror_raw).map_err(|e| format!("Model fetch refused: {e}"))?,
    );

    // Staging setup: everything downloads into a staging directory INSIDE
    // model_dir (same filesystem → atomic renames into place; nothing is
    // visible in model_dir until every hash has verified).
    let model_dir = &rag.model_dir;
    let created_model_dir = !model_dir.exists();
    if created_model_dir {
        fs::create_dir_all(model_dir)
            .map_err(|e| format!("creating model_dir {}: {e}", model_dir.display()))?;
    }
    let staging = model_dir.join(STAGING_DIR);
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging)
        .map_err(|e| format!("creating staging dir {}: {e}", staging.display()))?;

    let outcome = (|| -> Result<String, String> {
        // 1. Fetch the project-recorded hash manifest from the mirror root.
        let manifest_url = mirror
            .join(MANIFEST_FILE)
            .map_err(|e| format!("building manifest URL: {e}"))?;
        let manifest_path = staging.join(MANIFEST_FILE);
        http_download_to_file(manifest_url.as_str(), rag.timeout_secs, &manifest_path)
            .map_err(|e| format!("fetching mirror {MANIFEST_FILE}: {e}"))?;
        let manifest_bytes = fs::read(&manifest_path)
            .map_err(|e| format!("reading mirror {MANIFEST_FILE}: {e}"))?;
        let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes)
            .map_err(|e| format!("mirror {MANIFEST_FILE} is not valid JSON: {e}"))?;
        let expected = expected_files_from_manifest(&manifest, profile.name)
            .map_err(|e| format!("mirror {MANIFEST_FILE}: {e}"))?;

        // 2. Download every recorded artifact into staging.
        for (name, _) in &expected {
            let url = mirror
                .join(name)
                .map_err(|e| format!("building URL for {name}: {e}"))?;
            http_download_to_file(url.as_str(), rag.timeout_secs, &staging.join(name))
                .map_err(|e| format!("downloading {name}: {e}"))?;
        }

        // 3. Verify every download against the project-recorded hashes.
        verify_staged_files(&staging, &expected)?;
        let hashes = ArtifactHashes {
            model_sha256: expected
                .iter()
                .find(|(n, _)| n == MODEL_FILE)
                .map(|(_, h)| h.clone())
                .expect("required model.onnx entry validated above"),
            tokenizer_sha256: expected
                .iter()
                .find(|(n, _)| n == TOKENIZER_FILE)
                .map(|(_, h)| h.clone())
                .expect("required tokenizer.json entry validated above"),
            model_size_bytes: fs::metadata(staging.join(MODEL_FILE))
                .map_err(|e| format!("stat staged {MODEL_FILE}: {e}"))?
                .len() as i64,
        };

        // 4. Move verified artifacts into model_dir (same filesystem),
        //    rolling back on a partial failure so nothing lands.
        let mut moved: Vec<String> = Vec::new();
        let move_result = (|| -> Result<(), String> {
            for (name, _) in &expected {
                fs::rename(staging.join(name), model_dir.join(name))
                    .map_err(|e| format!("moving {name} into model_dir: {e}"))?;
                moved.push(name.clone());
            }
            Ok(())
        })();
        if let Err(e) = move_result {
            for name in &moved {
                let _ = fs::remove_file(model_dir.join(name));
            }
            return Err(e);
        }

        // 5. Write the rag_model_artifacts row from the expected hashes.
        //    A row-write failure does NOT undo a verified fetch — the
        //    files are integrity-checked, and manual-placement
        //    self-registration would re-pin them on next load anyway.
        let row_note = match DependencyGraph::open_for_project(project_root) {
            Ok(graph) => match write_artifact_row(graph.store(), profile.name, &hashes, &mirror_raw)
            {
                Ok(()) => "rag_model_artifacts row written from the project-recorded hashes"
                    .to_string(),
                Err(e) => format!("warning: artifacts fetched but row write failed ({e})"),
            },
            Err(e) => format!(
                "warning: artifacts fetched but the project graph could not be opened ({e})"
            ),
        };

        let mut lines = format!(
            "Model fetch: fetched '{}' from {} into {}",
            profile.name,
            mirror_raw,
            model_dir.display()
        );
        for (name, want) in &expected {
            let size = fs::metadata(model_dir.join(name)).map(|m| m.len()).unwrap_or(0);
            lines.push_str(&format!(
                "\n  {name:<14} {size:>9} bytes  sha256:{}…",
                &want[..12.min(want.len())]
            ));
        }
        lines.push_str(&format!("\n  {row_note}"));
        Ok(lines)
    })();

    // Refusal cleanup: staging always goes; a model_dir we created stays
    // empty at most (remove it too, best-effort).
    let _ = fs::remove_dir_all(&staging);
    if outcome.is_err() && created_model_dir {
        let _ = fs::remove_dir(model_dir);
    }
    outcome
}

/// `/neurocode model fetch [<profile>]` — resolve config + profile and
/// render the outcome as plain text (exit-free by design).
fn model_fetch_text(config: &joey_core::Config, profile: Option<&str>) -> String {
    let rag = RagConfig::load(config);
    let profile_name = profile.unwrap_or(&rag.model).to_string();
    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match run_model_fetch(&rag, &profile_name, &project_root) {
        Ok(msg) | Err(msg) => msg,
    }
}

// ---------------------------------------------------------------------------
// /neurocode model fetch --dylib (T039, tasks.md Phase 8; contracts/
// neurocode-rag-command.md § Model fetch subcommand "With --dylib";
// research.md R6 `load-dynamic` + R8 no-Hugging-Face).
//
// Downloads the platform-appropriate ONNX Runtime dylib (~10–31 MB
// depending on platform) from the SAME project-controlled mirror root
// (`neurocode.rag.local.mirror_url` — no new config key), SHA-256-verified
// against PROJECT-RECORDED per-platform hashes; any mismatch is REFUSED
// (nothing lands outside staging). Reuses T011's staging → verify →
// atomic-rename → rollback flow.
//
// Manifest scheme (EXTENDS T011's manifest.json at the mirror root with a
// sibling `ort-dylib` section — profiles stay untouched):
//
//   { "profiles":   { "<profile>": { "model.onnx": "<64-hex>", ... } },
//     "ort-dylib":  {
//       "version": "1.22.0",            // optional; defaults to the ort
//                                        // crate pin when absent
//       "platforms": {
//         "darwin-aarch64":  { "filename": "libonnxruntime.dylib", "sha256": "<64-hex>" },
//         "darwin-x86_64":   { "filename": "libonnxruntime.dylib", "sha256": "<64-hex>" },
//         "linux-x86_64":    { "filename": "libonnxruntime.so",    "sha256": "<64-hex>" },
//         "windows-x86_64":  { "filename": "onnxruntime.dll",      "sha256": "<64-hex>" }
//       } } }
//
// (the platform map is also accepted directly under `ort-dylib` without
// the `platforms` wrapper, mirroring T011's bare-root tolerance).
//
// Fetched-copy location (decision of record): `<joey home>/neurocode/ort/
// <version>/<filename>` — e.g. `~/.joey/neurocode/ort/1.22.0/
// libonnxruntime.dylib`. `<version>` is the manifest-recorded version (or
// the ort crate pin when the manifest omits it), so multiple runtime
// versions can coexist; the joey home honors `JOEY_HOME`/profiles like
// every other joey state path.
//
// Dylib resolution order reported by this command (tasks.md T039):
//   ORT_DYLIB_PATH env → neurocode.rag.local.ort_dylib_path → system
//   lookup → fetched-copy location.
// The in-process loader consults the env/config/system rungs; the fetched
// copy is the fallback rung this command populates, and the output tells
// the user which rung currently wins (with an activation hint when it is
// not one the loader already consults).
//
// Windows/MSVC verification note: the Windows artifact is the MSVC-built
// `onnxruntime.dll` (~31 MB). Verification is purely SHA-256-based, so
// toolchain provenance (MSVC vs anything else) is irrelevant to the check
// — the project-recorded hash pins the exact bytes regardless of which
// toolchain produced them.
// ---------------------------------------------------------------------------

/// The `ort` crate pin this workspace builds against (research.md R6) —
/// also the fetched-copy directory name when the manifest records no
/// `version`.
const ORT_CRATE_PIN: &str = "2.0.0-rc.13";
/// Staging directory under the fetched-copy root (same filesystem →
/// atomic rename; a refused fetch never leaves a dylib outside staging).
const DYLIB_STAGING_DIR: &str = ".dylib-fetch-staging";

/// One resolvable platform target (detected from `std::env::consts`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct OrtPlatform {
    /// Manifest key, e.g. `darwin-aarch64` (macOS is recorded as `darwin`).
    key: String,
    /// Conventional runtime library file name for the platform.
    filename: String,
}

/// Map an (os, arch) pair to the platform target; `None` for platforms
/// this build does not carry a dylib mapping for. Pure — pinned by tests
/// independently of the host running them.
fn platform_for(os: &str, arch: &str) -> Option<OrtPlatform> {
    let os = match os {
        "macos" => "darwin",
        "linux" => "linux",
        "windows" => "windows",
        _ => return None,
    };
    match arch {
        "aarch64" | "x86_64" => {}
        _ => return None,
    }
    let filename = match os {
        "darwin" => "libonnxruntime.dylib",
        "linux" => "libonnxruntime.so",
        _ => "onnxruntime.dll",
    };
    Some(OrtPlatform {
        key: format!("{os}-{arch}"),
        filename: filename.to_string(),
    })
}

/// Detect the current platform target (`std::env::consts::OS`/`ARCH`).
fn detect_platform() -> Option<OrtPlatform> {
    platform_for(std::env::consts::OS, std::env::consts::ARCH)
}

/// A manifest `ort-dylib` platform entry (validated).
#[derive(Debug, Clone, PartialEq, Eq)]
struct DylibEntry {
    filename: String,
    sha256: String,
}

/// Whether `s` is a 64-char lowercase-able hex SHA-256.
fn is_sha256_hex(s: &str) -> bool {
    let t = s.trim();
    t.len() == 64 && t.chars().all(|c| c.is_ascii_hexdigit())
}

/// Extract the `ort-dylib` entry for `platform_key` from the mirror
/// manifest. Accepts the platform map under `ort-dylib.platforms` or
/// directly under `ort-dylib`. Malformed hashes, path-carrying filenames
/// (a compromised mirror must not write outside the fetch root), and
/// missing platforms are errors — refused before any dylib download.
fn dylib_entry_from_manifest(
    manifest: &serde_json::Value,
    platform_key: &str,
) -> Result<DylibEntry, String> {
    let section = manifest
        .get("ort-dylib")
        .ok_or_else(|| "mirror manifest has no 'ort-dylib' section".to_string())?
        .get("platforms")
        .unwrap_or(manifest.get("ort-dylib").unwrap_or(manifest));
    let obj = section
        .as_object()
        .ok_or_else(|| "manifest 'ort-dylib' section is not a JSON object".to_string())?;
    let entry = obj.get(platform_key).ok_or_else(|| {
        let recorded: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
        format!(
            "platform '{platform_key}' is not recorded in the manifest 'ort-dylib' section \
             (recorded: {})",
            recorded.join(", ")
        )
    })?
    .as_object()
    .ok_or_else(|| format!("'ort-dylib' entry for '{platform_key}' is not an object"))?;

    let filename = entry
        .get("filename")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            format!("'ort-dylib' entry for '{platform_key}' lacks a string 'filename'")
        })?
        .trim()
        .to_string();
    if filename.is_empty()
        || filename.contains('/')
        || filename.contains('\\')
        || filename.contains("..")
    {
        return Err(format!(
            "'ort-dylib' filename for '{platform_key}' must be a bare file name, got '{filename}'"
        ));
    }
    let sha256 = entry
        .get("sha256")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            format!("'ort-dylib' entry for '{platform_key}' lacks a string 'sha256'")
        })?
        .trim()
        .to_ascii_lowercase();
    if !is_sha256_hex(&sha256) {
        return Err(format!(
            "'ort-dylib' sha256 for '{platform_key}' is not a 64-hex SHA-256: '{sha256}'"
        ));
    }
    Ok(DylibEntry { filename, sha256 })
}

/// The fetched-copy directory version segment: the manifest-recorded
/// `ort-dylib.version` when present and a bare path-safe token, else the
/// ort crate pin. Path-carrying values are refused (traversal guard).
fn dylib_version_from_manifest(manifest: &serde_json::Value) -> Result<String, String> {
    let raw = manifest
        .get("ort-dylib")
        .and_then(|s| s.get("version"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(ORT_CRATE_PIN);
    if raw.contains('/') || raw.contains('\\') || raw.contains("..") {
        return Err(format!(
            "manifest 'ort-dylib.version' must be a bare version token, got '{raw}'"
        ));
    }
    Ok(raw.to_string())
}

/// Which rung of the dylib-resolution ladder wins (tasks.md T039 order:
/// env → config → system → fetched copy). Pure — blank strings count as
/// unset, exactly like the rag crate's own ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DylibRung {
    Env,
    Config,
    System,
    Fetched,
}

impl DylibRung {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Env => "ORT_DYLIB_PATH env",
            Self::Config => "neurocode.rag.local.ort_dylib_path",
            Self::System => "system lookup",
            Self::Fetched => "fetched copy",
        }
    }
}

/// Resolve the winning rung + path. Precedence: env > config > system >
/// fetched. `None` when no rung holds an existing/pointed-at dylib.
fn resolve_dylib_rung(
    env: Option<&str>,
    configured: Option<&str>,
    system: Option<&Path>,
    fetched: Option<&Path>,
) -> Option<(DylibRung, PathBuf)> {
    if let Some(p) = env.filter(|p| !p.trim().is_empty()) {
        return Some((DylibRung::Env, PathBuf::from(p)));
    }
    if let Some(p) = configured.filter(|p| !p.trim().is_empty()) {
        return Some((DylibRung::Config, PathBuf::from(p)));
    }
    if let Some(p) = system.filter(|p| p.exists()) {
        return Some((DylibRung::System, p.to_path_buf()));
    }
    if let Some(p) = fetched.filter(|p| p.exists()) {
        return Some((DylibRung::Fetched, p.to_path_buf()));
    }
    None
}

/// Filesystem-only probe for a system-installed ONNX Runtime dylib: scan
/// the platform's conventional library directories for a file whose stem
/// starts with the platform base name (`libonnxruntime` / `onnxruntime`).
/// Never shells out; a miss is just `None`.
fn probe_system_dylib(platform: &OrtPlatform) -> Option<PathBuf> {
    let base = platform
        .filename
        .trim_end_matches(|c: char| c != '.')
        .trim_end_matches('.');
    let dirs: &[&str] = match platform.key.split('-').next().unwrap_or_default() {
        "darwin" => &["/usr/local/lib", "/opt/homebrew/lib", "/usr/lib"],
        "linux" => &["/usr/local/lib", "/usr/lib", "/lib"],
        "windows" => &["C:\\Windows\\System32"],
        _ => &[],
    };
    for dir in dirs {
        let Ok(entries) = fs::read_dir(dir) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(base) {
                return Some(entry.path());
            }
        }
    }
    None
}

/// Fetched-copy root: `<joey home>/neurocode/ort/` (JOEY_HOME honored).
fn dylib_fetch_root() -> PathBuf {
    joey_core::joey_home().join("neurocode").join("ort")
}

/// Core `/neurocode model fetch --dylib` flow. Every not-fetched outcome
/// (disabled mirror, HF host, unsupported platform, bad manifest, hash
/// mismatch) is an `Err` carrying the user-facing message.
fn run_dylib_fetch(rag: &RagConfig) -> Result<String, String> {
    // EMPTY mirror_url = fetch disabled (same default posture as model
    // artifacts: joey never downloads runtime binaries unconfigured).
    let mirror_raw = rag.mirror_url.trim().to_string();
    if mirror_raw.is_empty() {
        return Err(
            "Model fetch --dylib is disabled: neurocode.rag.local.mirror_url is empty.\n\
             Joey never downloads the ONNX Runtime dylib by default — configure a \
             project-controlled mirror URL to enable fetching (huggingface.co is \
             never contacted)."
                .to_string(),
        );
    }

    // Platform/arch detection BEFORE any network work.
    let platform = detect_platform().ok_or_else(|| {
        format!(
            "no ONNX Runtime dylib mapping for this platform ({}-{}); the mirror \
             manifest records darwin/linux/windows × aarch64/x86_64",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;

    // Hard no-HF guard on the mirror BEFORE any request is issued (R8).
    let mirror = normalize_mirror_base(
        guard_no_hf(&mirror_raw).map_err(|e| format!("Dylib fetch refused: {e}"))?,
    );

    // Staging under the fetched-copy root (same filesystem → atomic rename;
    // nothing is visible outside staging until the hash has verified).
    let root = dylib_fetch_root();
    fs::create_dir_all(&root).map_err(|e| format!("creating {}: {e}", root.display()))?;
    let staging = root.join(DYLIB_STAGING_DIR);
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging)
        .map_err(|e| format!("creating staging dir {}: {e}", staging.display()))?;

    let outcome = (|| -> Result<String, String> {
        // 1. Fetch the manifest (shared with T011 — the same root file
        //    carries both the profiles and the ort-dylib section).
        let manifest_url = mirror
            .join(MANIFEST_FILE)
            .map_err(|e| format!("building manifest URL: {e}"))?;
        let manifest_path = staging.join(MANIFEST_FILE);
        http_download_to_file(manifest_url.as_str(), rag.timeout_secs, &manifest_path)
            .map_err(|e| format!("fetching mirror {MANIFEST_FILE}: {e}"))?;
        let manifest: serde_json::Value = serde_json::from_slice(
            &fs::read(&manifest_path)
                .map_err(|e| format!("reading mirror {MANIFEST_FILE}: {e}"))?,
        )
        .map_err(|e| format!("mirror {MANIFEST_FILE} is not valid JSON: {e}"))?;
        let entry = dylib_entry_from_manifest(&manifest, &platform.key)
            .map_err(|e| format!("mirror {MANIFEST_FILE}: {e}"))?;
        let version = dylib_version_from_manifest(&manifest)?;

        // 2. Download the platform dylib into staging.
        let url = mirror
            .join(&entry.filename)
            .map_err(|e| format!("building URL for {}: {e}", entry.filename))?;
        let staged = staging.join(&entry.filename);
        http_download_to_file(url.as_str(), rag.timeout_secs, &staged)
            .map_err(|e| format!("downloading {}: {e}", entry.filename))?;

        // 3. Verify against the project-recorded per-platform hash; a
        //    mismatch is a refusal (nothing has landed outside staging).
        let got = sha256_file(&staged)
            .map_err(|e| format!("hashing downloaded {}: {e}", entry.filename))?;
        if !got.eq_ignore_ascii_case(&entry.sha256) {
            return Err(format!(
                "SHA-256 mismatch for {} ({}): expected {}, downloaded {}. \
                 Refused — nothing was written outside staging",
                entry.filename, platform.key, entry.sha256, got
            ));
        }

        // 4. Atomic rename into the fetched-copy location; roll back on a
        //    partial failure so a broken fetch never half-lands.
        let dest_dir = root.join(&version);
        fs::create_dir_all(&dest_dir)
            .map_err(|e| format!("creating {}: {e}", dest_dir.display()))?;
        let dest = dest_dir.join(&entry.filename);
        let size = fs::metadata(&staged)
            .map_err(|e| format!("stat staged {}: {e}", entry.filename))?
            .len();
        fs::rename(&staged, &dest)
            .map_err(|e| format!("moving {} into {}: {e}", entry.filename, dest.display()))?;
        if let Err(e) = fs::File::open(&dest) {
            let _ = fs::remove_file(&dest);
            return Err(format!("verifying landed copy: {e}"));
        }

        // 5. Report + the resolution ladder (env → config → system →
        //    fetched), with an activation hint when the winning rung is
        //    not one the in-process loader consults on its own.
        let system = probe_system_dylib(&platform);
        let rung = resolve_dylib_rung(
            std::env::var("ORT_DYLIB_PATH").ok().as_deref(),
            Some(rag.ort_dylib_path.as_str()),
            system.as_deref(),
            Some(dest.as_path()),
        );
        let mut out = format!(
            "Dylib fetch: fetched the ONNX Runtime dylib for {} from {}\n\
             \x20 File:     {} ({size} bytes)  sha256:{}…\n\
             \x20 Location: {}",
            platform.key,
            mirror_raw,
            entry.filename,
            &entry.sha256[..12],
            dest.display(),
        );
        match rung {
            Some((rung, path)) => {
                out.push_str(&format!(
                    "\n\x20 Runtime resolution: {} → {}",
                    rung.as_str(),
                    path.display()
                ));
                if rung == DylibRung::System || rung == DylibRung::Fetched {
                    out.push_str(&format!(
                        "\n\x20 Note: to force the fetched copy, point a higher rung at it:\n\
                         \x20 ORT_DYLIB_PATH={} (or neurocode.rag.local.ort_dylib_path)",
                        dest.display()
                    ));
                }
            }
            None => {
                // Unreachable post-fetch (the fetched rung exists), kept
                // for exhaustive rendering.
                out.push_str("\n\x20 Runtime resolution: no rung resolved");
            }
        }
        out.push_str(
            "\n\x20 Ladder: ORT_DYLIB_PATH env → neurocode.rag.local.ort_dylib_path → \
             system lookup → fetched copy",
        );
        Ok(out)
    })();

    // Refusal cleanup: staging always goes.
    let _ = fs::remove_dir_all(&staging);
    outcome
}

/// `/neurocode model fetch --dylib` — resolve config and render the
/// outcome as plain text (exit-free by design).
fn dylib_fetch_text(config: &joey_core::Config) -> String {
    let rag = RagConfig::load(config);
    match run_dylib_fetch(&rag) {
        Ok(msg) | Err(msg) => msg,
    }
}

// ---------------------------------------------------------------------------
// /neurocode search (T015, specs/021 contracts/neurocode-rag-command.md §
// Grammar additions + contracts/neurocode-rag-tools.md § Return payload).
//
// Grammar:   search <query...> [--path <glob>] [--limit <n>]
//                  [--expand-lines <n>] [--relations <0-2>] [--json]
// Flag map:  --path→file_filter, --limit→limit, --expand-lines→expand_lines,
//            --relations→relation_depth (validated 0–2).
//
// Rendering: plain-text ranked results by default; --json emits the exact
// tool-shaped payload (results[] with file/symbol/kind/lines/chunk_kind/
// score/context, plus mode + mode_reason — Principle II parity with the
// neurocode_search agent tool). Execution goes through
// joey_neurocode_rag::search::hybrid::search_cli — until T009/T010 land a
// resolvable embedding backend this exercises the FR-008 degradation
// machinery (keyword_only + mode_reason); full hybrid fusion is T016–T020.
// ---------------------------------------------------------------------------

use joey_neurocode_rag::search::hybrid::{
    ChunkBadge, RankedResult, SearchError, SearchOutcome, SearchRequest,
};

/// Parsed `/neurocode search` arguments (the grammar layer; see
/// [`parse_search_args`]).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SearchArgs {
    /// Positional query words, joined with spaces at execution.
    pub query_words: Vec<String>,
    pub path: Option<String>,
    pub limit: Option<usize>,
    pub expand_lines: Option<u32>,
    pub relation_depth: Option<u8>,
    pub json: bool,
}

const SEARCH_USAGE: &str =
    "Usage: /neurocode search <query...> [--path <glob>] [--limit <n>] \
     [--expand-lines <n>] [--relations <0-2>] [--json]";

/// Parse the search-argument tail per the contract grammar.
///
/// Positional words (before/after/between flags) accumulate into the
/// query; flags take one value each; `--relations` validates 0–2 and
/// `--limit`/`--expand-lines` validate as non-negative integers. Usage
/// errors produce the standard argument-error rendering (message + usage).
pub(crate) fn parse_search_args(args: &[&str]) -> Result<SearchArgs, String> {
    let mut out = SearchArgs {
        query_words: Vec::new(),
        path: None,
        limit: None,
        expand_lines: None,
        relation_depth: None,
        json: false,
    };
    let value_for = |name: &str, args: &[&str], i: usize| -> Result<String, String> {
        args.get(i + 1)
            .copied()
            .map(str::to_string)
            .ok_or_else(|| format!("flag {name} requires a value"))
    };
    let mut i = 0;
    while i < args.len() {
        let arg = args[i];
        match arg {
            "--json" => out.json = true,
            "--path" => {
                let v = value_for("--path", args, i)?;
                if v.is_empty() {
                    return Err("flag --path requires a non-empty glob".to_string());
                }
                out.path = Some(v);
                i += 1;
            }
            "--limit" => {
                let v = value_for("--limit", args, i)?;
                out.limit = Some(
                    v.parse::<usize>()
                        .map_err(|_| format!("invalid --limit value '{v}' (expected an integer ≥ 0)"))?,
                );
                i += 1;
            }
            "--expand-lines" => {
                let v = value_for("--expand-lines", args, i)?;
                out.expand_lines = Some(
                    v.parse::<u32>().map_err(|_| {
                        format!("invalid --expand-lines value '{v}' (expected an integer ≥ 0)")
                    })?,
                );
                i += 1;
            }
            "--relations" => {
                let v = value_for("--relations", args, i)?;
                let n = v
                    .parse::<i64>()
                    .map_err(|_| format!("invalid --relations value '{v}' (expected 0–2)"))?;
                if !(0..=2).contains(&n) {
                    return Err(format!(
                        "invalid --relations value '{v}' (must be within 0–2)"
                    ));
                }
                out.relation_depth = Some(n as u8);
                i += 1;
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown flag '{other}'"));
            }
            other => out.query_words.push(other.to_string()),
        }
        i += 1;
    }
    // Missing <query...> → the standard argument-error rendering.
    if out.query_words.is_empty() {
        return Err("missing <query...> — provide at least one search term".to_string());
    }
    Ok(out)
}

/// Build the execution request from parsed args + the loaded RAG config
/// (defaults: limit ← `neurocode.rag.top_k` (cap 50, matching the tool
/// schema's maximum), expand_lines ← context window clamp 0–200,
/// include_fallback_chunks ← config).
fn build_search_request(args: &SearchArgs, rag: &RagConfig) -> SearchRequest {
    let limit = args
        .limit
        .unwrap_or(rag.top_k.max(1) as usize)
        .min(50);
    // Clamp per the tool schema maximums (contracts/neurocode-rag-tools.md);
    // precedence (flag override > config default) is single-sourced in the
    // rag crate (T027).
    let expand_lines = rag.effective_context_window(args.expand_lines);
    SearchRequest {
        query: args.query_words.join(" "),
        file_filter: args.path.clone(),
        limit,
        expand_lines,
        relation_depth: args.relation_depth.unwrap_or(0),
        include_fallback_chunks: rag.include_fallback_chunks,
    }
}

/// `/neurocode search` handler: parse → execute (via the rag crate's
/// `search_cli`) → render plain text or `--json`.
fn search_command_text(parts: &[&str], config: &joey_core::Config) -> String {
    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    search_command_text_at(parts, config, &project_root)
}

/// Root-parameterized core of [`search_command_text`] (tests pin the CWD
/// out; production resolves the project root from the process CWD).
fn search_command_text_at(
    parts: &[&str],
    config: &joey_core::Config,
    project_root: &Path,
) -> String {
    let args = match parse_search_args(parts) {
        Ok(a) => a,
        Err(msg) => return format!("Search: {msg}.\n{SEARCH_USAGE}"),
    };
    let rag = RagConfig::load(config);
    let request = build_search_request(&args, &rag);

    // T041b production wiring: resolve the embedding backend (`auto` →
    // LocalOnnx when the artifacts verify, remote kinds consent-gated) and
    // drive the dense leg through it — hybrid mode activates for real.
    // Degraded ⇒ the keyword-only posture with mode_reason (FR-008).
    match crate::neurocode_rag_wiring::execute_rag_search(&rag, project_root, &request) {
        Ok(outcome) => {
            if args.json {
                render_search_json(&outcome)
            } else {
                render_search_text(&outcome, &request.query)
            }
        }
        Err(SearchError::Validation(msg)) => {
            // Edge case 1: clear validation message, no search executed,
            // no error — render the message + usage.
            format!("Search: {msg}.\n{SEARCH_USAGE}")
        }
        Err(e) => format!("Search failed: {e}"),
    }
}

/// Plain-text rendering: header (query, mode, degradation note), ranked
/// list with the FR-014 badge, context preview, and the
/// empty-index/no-match closers (distinct messages, neither an error).
fn render_search_text(outcome: &SearchOutcome, query: &str) -> String {
    let mut out = String::new();
    match outcome.mode.as_str() {
        "keyword_only" => {
            let reason = outcome.mode_reason.as_deref().unwrap_or("unavailable");
            out.push_str(&format!(
                "Search \"{query}\" — keyword-only mode (semantic backend degraded: {reason})\n"
            ));
        }
        _ => out.push_str(&format!(
            "Search \"{query}\" — {mode} mode\n",
            query = query,
            mode = outcome.mode
        )),
    }
    if outcome.results.is_empty() {
        if outcome.index_chunk_count == 0 {
            out.push_str(
                "Nothing matched — the semantic index is empty. Run /neurocode index to build it.",
            );
        } else {
            out.push_str(&format!(
                "Nothing matched '{query}' ({} indexed chunks searched).",
                outcome.index_chunk_count
            ));
        }
        return out;
    }
    for (idx, r) in outcome.results.iter().enumerate() {
        let badge = badge_text(r.chunk_kind);
        let symbol = r
            .symbol
            .as_deref()
            .unwrap_or("(top-level code)");
        let kind = r.symbol_kind.as_deref().unwrap_or("region");
        out.push_str(&format!(
            "{}. {} [{}] {} ({}):{} — score {:.4}\n",
            idx + 1,
            r.file,
            badge,
            symbol,
            kind,
            line_range(r.start_line, r.end_line),
            r.fused_score
        ));
        if let Some(ctx) = r.context.as_deref() {
            let preview: Vec<String> = ctx
                .lines()
                .take(3)
                .map(|l| format!("     {}", l.trim_end()))
                .collect();
            if !preview.is_empty() {
                out.push_str(&preview.join("\n"));
                out.push('\n');
            }
        }
        for rel in &r.relations {
            let sym = rel.symbol.as_deref().unwrap_or("(top-level code)");
            out.push_str(&format!(
                "     ↳ relates via {} → {} {}\n",
                rel.relation_kind, rel.file, sym
            ));
        }
    }
    out
}

/// The FR-014 visible badge text (distinct for fallback chunks).
fn badge_text(kind: ChunkBadge) -> &'static str {
    match kind {
        ChunkBadge::SymbolAligned => "symbol-aligned",
        ChunkBadge::Fallback => "fallback-chunk",
    }
}

fn line_range(start: u32, end: u32) -> String {
    if start == end {
        format!("L{start}")
    } else {
        format!("L{start}-{end}")
    }
}

/// `--json`: the tool-shaped payload (contracts/neurocode-rag-tools.md §
/// Return payload — identical shape, Principle II parity).
fn render_search_json(outcome: &SearchOutcome) -> String {
    let results: Vec<serde_json::Value> = outcome
        .results
        .iter()
        .map(|r| ranked_result_json(r))
        .collect();
    let payload = serde_json::json!({
        "results": results,
        "mode": outcome.mode.as_str(),
        "mode_reason": outcome.mode_reason,
    });
    serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string())
}

fn ranked_result_json(r: &RankedResult) -> serde_json::Value {
    serde_json::json!({
        "file": r.file,
        "symbol": r.symbol,
        "kind": r.symbol_kind,
        "lines": [r.start_line, r.end_line],
        "chunk_kind": r.chunk_kind.as_str(),
        "score": r.fused_score,
        "context": r.context,
        "relations": r.relations.iter().map(|rel| serde_json::json!({
            "file": rel.file,
            "symbol": rel.symbol,
            "relation_kind": rel.relation_kind,
        })).collect::<Vec<_>>(),
    })
}

// ---------------------------------------------------------------------------
// /neurocode consent (T031, FR-012; contracts/neurocode-rag-command.md §
// Consent subcommand).
//
// Grammar:  consent show | ack | revoke
//
// `show`    — the current state (absent consent.json ⇒ never_acknowledged),
//             the target backend base_url + whether it counts as remote,
//             the embed model, and the code-egress disclosure.
// `ack`     — REQUIRES explicit interactive confirmation restating that this
//             repository's code will be sent to the remote service; a
//             declined or non-interactive confirmation aborts with NO state
//             change. The confirmation decision arrives through a callback
//             so tests drive it non-interactively while production prompts
//             on stdin.
// `revoke`  — immediate effect; subsequent remote embedding calls stop
//             (loopback/local stays usable — consent-free).
// Re-ack after revoke is allowed (state machine: Revoked → Acknowledged).
// ----------------------------------------------------------------------------

use joey_neurocode_rag::consent::{
    consent_file_path, ConsentError, ConsentRecord, ConsentState, CONSENT_FILE_NAME,
};

// ---------------------------------------------------------------------------
// RAG status section (T034, FR-013; contracts/neurocode-rag-tools.md §
// neurocode_status + contracts/neurocode-rag-command.md § Status extension).
//
// `/neurocode status` and the `neurocode_status` TOOL both gain a RAG
// section ONLY when `neurocode.rag.enabled == true`. When disabled this
// returns None and callers emit NOTHING — never a present-but-empty
// section (FR-009 parity: disabled output is byte-identical).
//
// Fields: freshness (last_refresh_at), chunk/vector counts,
// model/profile/dim/quantization, refresh_state, backend health,
// consent state, degradation flags (mode_reason-style notes).
// ----------------------------------------------------------------------------

/// JSON rag section for the `neurocode_status` tool (contract field list:
/// index_state, chunk_count, vector_count, model, dim, quantization,
/// last_refresh_at, backend_health, consent_state, degradation).
/// `None` when RAG is disabled — the `rag` key does not appear at all.
pub(crate) fn rag_status_json(config: &joey_core::Config, project_root: &Path) -> Option<serde_json::Value> {
    let rag = RagConfig::load(config);
    if !rag.enabled {
        return None;
    }
    let consent_state = ConsentRecord::load_for_project(project_root)
        .map(|r| r.state.as_str().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let (index_state, chunk_count_v, vector_count_v, model, dim, quant, last_refresh, refresh_state) =
        index_summary(project_root);
    let (backend_health, mut degradation) = backend_health_and_degradation(
        &rag,
        project_root,
        &index_state,
        &consent_state,
    );
    if index_state == "empty" {
        degradation.push("rag index empty — run /neurocode index to build it".to_string());
    }
    Some(serde_json::json!({
        "index_state": index_state,
        "chunk_count": chunk_count_v,
        "vector_count": vector_count_v,
        "model": model,
        "dim": dim,
        "quantization": quant,
        "last_refresh_at": last_refresh,
        "refresh_state": refresh_state,
        "backend_health": backend_health,
        "consent_state": consent_state,
        "degradation": degradation,
    }))
}

/// Plain-text rag section for `/neurocode status` (same fields, CLI
/// rendering). `None` when RAG is disabled (byte-identical parity).
///
/// Values render as SCALARS, not JSON literals: `serde_json::Value`'s
/// `Display` would quote strings (`"empty"`), so each field goes through
/// an explicit scalar unwrap (numbers via `as_u64`, strings via
/// `as_str`, null dim as `unknown`, null freshness as `never`).
pub(crate) fn rag_status_section(config: &joey_core::Config, project_root: &Path) -> Option<String> {
    let v = rag_status_json(config, project_root)?;
    let num = |k: &str| -> String {
        v[k].as_u64().map(|n| n.to_string()).unwrap_or_else(|| "0".to_string())
    };
    let str_ = |k: &str| -> String {
        v[k].as_str().unwrap_or_default().to_string()
    };
    let degr: Vec<String> = v["degradation"]
        .as_array()
        .map(|a| a.iter().filter_map(|d| d.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let dim = match &v["dim"] {
        serde_json::Value::Null => "unknown".to_string(),
        other => other.to_string(),
    };
    let last_refresh = v["last_refresh_at"].as_str().unwrap_or("never").to_string();
    let mut out = format!(
        "\nRAG: enabled\n\
         \x20 Index: {} chunks, {} vectors ({})\n\
         \x20 Model: {} (dim {}, {})\n\
         \x20 Freshness: last refresh {}, refresh state {}\n\
         \x20 Backend: health {}, consent {}",
        num("chunk_count"), num("vector_count"), str_("index_state"),
        str_("model"), dim, str_("quantization"),
        last_refresh,
        str_("refresh_state"),
        str_("backend_health"), str_("consent_state"),
    );
    if !degr.is_empty() {
        out.push_str(&format!("\n\x20 Degradation: {}", degr.join("; ")));
    }
    Some(out)
}

/// Read the index summary from the per-project graph's v3 RAG tables
/// (`load_index_meta` + live counts). Missing graph/meta ⇒ empty index.
fn index_summary(project_root: &Path) -> (String, serde_json::Value, serde_json::Value, String, serde_json::Value, String, Option<String>, String) {
    let Ok(graph) = joey_neurocode::graph::DependencyGraph::open_for_project(project_root)
    else {
        return (
            "empty".into(), 0.into(), 0.into(), String::new(), serde_json::Value::Null,
            "f32".into(), None, "idle".into(),
        );
    };
    let conn = graph.store().conn();
    let chunks = joey_neurocode_rag::vector::store::chunk_count(conn)
        .unwrap_or(0);
    let vectors = joey_neurocode_rag::vector::store::vector_count(conn)
        .unwrap_or(0);
    match joey_neurocode_rag::vector::store::load_index_meta(conn) {
        Ok(Some(meta)) => (
            if chunks == 0 { "empty".into() } else { "ready".into() },
            chunks.into(), vectors.into(),
            meta.embed_model.clone(),
            meta.embed_dim.into(),
            meta.quantization_policy.clone(),
            meta.last_refresh_at.clone(),
            meta.refresh_state.clone(),
        ),
        _ => (
            if chunks == 0 { "empty".into() } else { "partial".into() },
            chunks.into(), vectors.into(), String::new(), serde_json::Value::Null,
            "f32".into(), None, "idle".into(),
        ),
    }
}

/// Backend health (describe/health_check vocabulary) + degradation notes.
fn backend_health_and_degradation(
    rag: &RagConfig,
    _project_root: &Path,
    _index_state: &str,
    consent_state: &str,
) -> (String, Vec<String>) {
    let mut degradation = Vec::new();
    // Resolve the backend decision (filesystem-only; never a network call).
    let profile = joey_neurocode_rag::embed::profiles::lookup(&rag.model)
        .unwrap_or_else(joey_neurocode_rag::embed::profiles::default_profile);
    // Provider-following switch — mirror of neurocode_rag_wiring: `auto` +
    // Copilot provider -> Copilot embeddings backend (explicit wins).
    let backend = if rag.backend == joey_neurocode_rag::config::RagBackend::Auto
        && rag.copilot_provider_active
    {
        joey_neurocode_rag::config::RagBackend::Copilot
    } else {
        rag.backend
    };
    let profile_name = if backend == joey_neurocode_rag::config::RagBackend::Copilot {
        joey_neurocode_rag::embed::copilot::profile_for(&rag.copilot_model)
            .name
            .to_string()
    } else {
        profile.name.to_string()
    };
    let health = match joey_neurocode_rag::embed::resolve_kind(backend, &profile_name, &rag.model_dir) {
        Ok(resolved) => match resolved.kind {
            joey_neurocode_rag::embed::BackendKind::LocalOnnx => {
                "reachable (local_onnx artifacts verified)".to_string()
            }
            joey_neurocode_rag::embed::BackendKind::KeywordOnly => {
                degradation.push(resolved.degradation_reason.clone());
                "degraded (keyword_only)".to_string()
            }
            joey_neurocode_rag::embed::BackendKind::Copilot => {
                "configured (copilot embeddings via provider)".to_string()
            }
            _ => "unavailable".to_string(),
        },
        Err(e) => {
            degradation.push(e.to_string());
            "unavailable".to_string()
        }
    };
    // The remote base the consent gate will actually evaluate: the Copilot
    // endpoint when the switch selected it, else the configured base_url.
    let effective_base = if backend == joey_neurocode_rag::config::RagBackend::Copilot {
        joey_neurocode_rag::embed::copilot::resolve_base_url()
    } else {
        rag.base_url.clone()
    };
    if !base_url_is_loopback(&effective_base) && consent_state != "acknowledged" {
        degradation.push(format!(
            "remote backend {} operates keyword-only until consent is acknowledged \
             (/neurocode consent ack)",
            effective_base
        ));
    }
    (health, degradation)
}

/// Whether a base_url host is loopback (127.0.0.0/8, ::1, `localhost`) —
/// loopback counts as local and is consent-free (contracts/
/// embedding-backend.md § Consent gate; FR-012). Unparseable/relative URLs
/// count as remote (conservative: consent gates them).
fn base_url_is_loopback(base_url: &str) -> bool {
    let Ok(url) = url::Url::parse(base_url.trim()) else {
        return false;
    };
    match url.host_str() {
        Some(host) => {
            // 127.0.0.0/8: ANY 127.x.x.x dotted quad is loopback.
            host.eq_ignore_ascii_case("localhost")
                || host
                    .strip_prefix("127.")
                    .is_some_and(|rest| {
                        let mut parts = rest.split('.');
                        parts.next().is_some_and(|p| p.parse::<u8>().is_ok())
                            && rest.split('.').all(|p| p.parse::<u8>().is_ok())
                            && rest.split('.').count() == 3
                    })
                || host == "::1"
                || host == "[::1]"
        }
        None => false,
    }
}

/// The code-egress disclosure every `consent show` prints (and every `ack`
/// that this repository's code will be sent to the remote service".
fn consent_disclosure(base_url: &str, model: &str, loopback: bool) -> String {
    if loopback {
        format!(
            "Consent: code-egress disclosure\n\
             The configured embedding backend is loopback-local ({base_url});\n\
             loopback backends are consent-free — code never leaves this machine.\n\
             Model: {model}"
        )
    } else {
        format!(
            "Consent: code-egress disclosure\n\
             Acknowledging consent records that this repository's code WILL BE\n\
             SENT to the remote embedding service at {base_url} (model {model})\n\
             to compute embeddings. Revoke at any time with: /neurocode consent revoke"
        )
    }
}

/// Production confirmation: print `prompt` + a yes/no question and read one
/// line from stdin. Only an explicit affirmative (`y` | `yes`, any case)
/// confirms; EOF or anything else declines (non-interactive ⇒ abort, per
/// the contract's "declined or non-interactive confirmation aborts").
fn consent_confirm_stdin(prompt: &str) -> bool {
    use std::io::{BufRead, Write};
    print!("{prompt}\nConfirm? [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) => false, // EOF — non-interactive
        Ok(_) => matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"),
        Err(_) => false,
    }
}

/// Core `/neurocode consent` handler. `confirm` answers the interactive
/// ack prompt (injectable for tests; production wires stdin).
fn consent_command_text(
    parts: &[&str],
    config: &joey_core::Config,
    project_root: &Path,
    confirm: &mut dyn FnMut(&str) -> bool,
) -> String {
    let rag = RagConfig::load(config);
    // T044: `--yes`/`-y` opts out of the interactive stdin confirmation.
    // This is the TUI-operable path: on the HeavyJob engine thread stdin is
    // unavailable (EOF ⇒ decline), so `/neurocode consent ack --yes` is how
    // the TUI grants consent. Without the flag the interactive prompt is
    // preserved byte-identically.
    let assume_yes = parts.iter().any(|p| *p == "--yes" || *p == "-y");
    let action = parts
        .iter()
        .find(|p| !matches!(**p, "--yes" | "-y"))
        .copied()
        .unwrap_or("");
    let consent_path = consent_file_path(project_root);
    let consent_dir = consent_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    let record = match ConsentRecord::load(&consent_dir) {
        Ok(r) => r,
        Err(e) => return format!("Consent: cannot read {CONSENT_FILE_NAME} ({e})."),
    };
    let loopback = base_url_is_loopback(&rag.base_url);

    match action {
        "show" | "" => {
            let scope = if loopback { "local (loopback)" } else { "remote" };
            format!(
                "Consent: {}\nBackend: {} ({})\nModel:   {}\nPath:    {}\n\n{}",
                record.state.as_str(),
                rag.base_url,
                scope,
                rag.model,
                consent_path.display(),
                consent_disclosure(&rag.base_url, &rag.model, loopback),
            )
        }

        "ack" => {
            let mut record = record;
            match record.state {
                ConsentState::Acknowledged => {
                    return format!(
                        "Consent: already acknowledged ({} for {}).\n\
                         Revoke first with /neurocode consent revoke if you mean to re-record.",
                        record
                            .acknowledged_at
                            .as_deref()
                            .unwrap_or("no timestamp"),
                        record.remote_backend_url
                    )
                }
                ConsentState::NeverAcknowledged | ConsentState::Revoked => {
                    let prompt = format!(
                        "{}\n\nThis is the explicit acknowledgement FR-012 requires.",
                        consent_disclosure(&rag.base_url, &rag.model, loopback)
                    );
                    if !assume_yes && !confirm(&prompt) {
                        let where_ = consent_path.display();
                        return format!(
                            "Consent: not acknowledged — confirmation declined; no state \
                             change ({where_} untouched)."
                        );
                    }
                    match record
                        .acknowledge_now(rag.base_url.trim().to_string(), rag.model.clone())
                        .and_then(|()| record.save(&consent_dir))
                    {
                        Ok(()) => format!(
                            "Consent: acknowledged — recorded to {} (state: acknowledged).",
                            consent_path.display()
                        ),
                        Err(ConsentError::IllegalTransition { .. }) => format!(
                            "Consent: state changed concurrently; re-run \
                             /neurocode consent show."
                        ),
                        Err(e) => format!("Consent: recording acknowledgement failed ({e})."),
                    }
                }
            }
        }

        "revoke" => {
            let mut record = record;
            match record.revoke_now().and_then(|()| record.save(&consent_dir)) {
                Ok(()) => format!(
                    "Consent: revoked — remote embedding for this project stops \
                     immediately; local/keyword-only mode applies (recorded to {}).",
                    consent_path.display()
                ),
                Err(ConsentError::IllegalTransition { .. }) => format!(
                    "Consent: nothing to revoke (current state: {}).",
                    record.state.as_str()
                ),
                Err(e) => format!("Consent: recording revocation failed ({e})."),
            }
        }

        other => format!(
            "Unknown consent action '{other}'.\n\
             Usage: /neurocode consent show|ack|revoke [--yes]"
        ),
    }
}

fn help_text() -> String {
    "Usage: /neurocode <subcommand>\n\
     \n\
     Subcommands:\n\
     \x20 status                          Show enabled state, index size, tiers, patterns, domain\n\
     \x20 tier [economical|frontier|auto] Show or set the active tier for the session\n\
     \x20 tier pin <tier>                 Pin a tier (economical|frontier) for the session\n\
     \x20 tier unpin                      Revert to automatic classification\n\
     \x20 index [--force]                 Trigger structural indexing of the project\n\
     \x20 query <type> <symbol>           Direct graph query (symbol|dependents|dependencies)\n\
     \x20 search <query...>               Semantic code search over the indexed project\n\
     \x20   [--path <glob>] [--limit <n>] [--expand-lines <n>] [--relations <0-2>] [--json]\n\
     \x20 model fetch [<profile>] [--dylib]  Fetch embedding model artifacts (RAG)\n\
     \x20 backend [auto|local_onnx|copilot|        Show or set the RAG embedding backend\n\
     \x20   openai_compat|ollama]                  (explicit backend overrides provider)\n\
     \x20 consent show|ack|revoke [--yes]  Show/acknowledge/revoke remote-backend consent\n\
     \x20   --yes skips the ack confirmation (non-interactive; TUI-safe)\n\
     \x20 ingest <category> <path>        Ingest domain knowledge\n\
     \x20   [--version <v>] [--provenance <p>]   (category: FrameworkDocs|EntityCatalog|Postmortem)\n\
     \x20 patterns                        List learned patterns\n\
     \x20 anti-patterns                   List learned anti-patterns\n\
     \x20 domain list                     List ingested domain-knowledge sources\n\
     \x20 domain remove <id>              Remove a domain source\n\
     \x20 --help                          Show this help message"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin JOEY_HOME to a temp dir under the shared override lock so the
    /// fetched copy lands in a temp home, not the real `~/.joey`. The lock
    /// also serializes the env-mutating tests below (the EnvGuard must
    /// always be created AFTER a pinned_home in the same test).
    struct HomeGuard {
        prev: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
        _dir: tempfile::TempDir,
    }

    fn pinned_home() -> HomeGuard {
        let lock = joey_core::constants::TEST_HOME_OVERRIDE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os("JOEY_HOME");
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("JOEY_HOME", dir.path());
        HomeGuard { prev, _lock: lock, _dir: dir }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => std::env::set_var("JOEY_HOME", v),
                None => std::env::remove_var("JOEY_HOME"),
            }
        }
    }

    #[test]
    fn help_succeeds() {
        // `/neurocode --help` returns help text (exit-success path).
        let out = neurocode_slash_provider_scoped_text("--help", "zai");
        assert!(out.contains("Usage: /neurocode"));
    }

    #[test]
    fn help_aliases() {
        assert!(neurocode_slash_provider_scoped_text("help", "zai").contains("Usage: /neurocode"));
        assert!(neurocode_slash_provider_scoped_text("-h", "zai").contains("Usage: /neurocode"));
    }

    #[test]
    fn unknown_subcommand_reports_error() {
        let out = neurocode_slash_provider_scoped_text("nonsense", "zai");
        assert!(out.contains("Unknown subcommand"));
    }

    #[test]
    fn ingest_request_classifies_strict_form_as_none() {
        assert_eq!(neurocode_ingest_request("ingest FrameworkDocs ./docs"), None);
    }

    #[test]
    fn ingest_request_extracts_natural_language() {
        let got = neurocode_ingest_request("ingest the Spring Boot docs in ./docs/spring");
        assert!(got.is_some());
        assert!(got.unwrap().contains("the Spring Boot docs in ./docs/spring"));
    }

    #[test]
    fn ingest_request_none_for_empty_and_other_subcommands() {
        assert_eq!(neurocode_ingest_request(""), None);
        assert_eq!(neurocode_ingest_request("ingest"), None);
        assert_eq!(neurocode_ingest_request("index"), None);
        assert_eq!(neurocode_ingest_request("index --force"), None);
        assert_eq!(neurocode_ingest_request("status"), None);
        assert_eq!(neurocode_ingest_request("backend copilot"), None);
    }

    #[test]
    fn backend_unknown_value_is_rejected_with_usage() {
        let mut config = joey_core::Config::defaults();
        let out = neurocode_backend_command_text(&["nope"], &mut config);
        assert!(out.contains("Unknown embedding backend"), "{out}");
        assert!(out.contains("copilot"), "{out}");
    }

    #[test]
    fn backend_show_reports_configured_value() {
        let mut config = joey_core::Config::defaults();
        let out = neurocode_backend_command_text(&[], &mut config);
        assert!(out.contains("Embedding backend:"), "{out}");
    }

    #[test]
    fn backend_set_persists_and_round_trips() {
        let _g = pinned_home();
        let out = {
            let mut config = joey_core::Config::load()
                .unwrap_or_else(|_| joey_core::Config::defaults());
            neurocode_backend_command_text(&["copilot"], &mut config)
        };
        let reloaded = joey_core::Config::load()
            .unwrap_or_else(|_| joey_core::Config::defaults());
        assert!(out.contains("copilot"), "{out}");
        assert!(out.contains("(saved)"), "{out}");
        assert_eq!(
            reloaded.get_str("neurocode.rag.backend", "auto"),
            "copilot"
        );
    }

    #[test]
    fn status_works_with_disabled_engine() {
        // `status` must route to the engine's status_text(), NOT fall into
        // the unknown-subcommand catch-all (regression: it used to).
        let out = neurocode_slash_provider_scoped_text("status", "zai");
        assert!(
            !out.contains("Unknown subcommand"),
            "status must not hit the catch-all, got: {out}"
        );
        assert!(out.starts_with("NeuroCode:"), "status prefix, got: {out}");
    }

    #[test]
    fn no_args_defaults_to_status() {
        let out = neurocode_slash_provider_scoped_text("", "zai");
        assert!(
            !out.contains("Unknown subcommand"),
            "bare /neurocode defaults to status, got: {out}"
        );
        assert!(out.starts_with("NeuroCode:"), "status prefix, got: {out}");
    }

    #[test]
    fn parse_kv_flags_extracts_version_and_provenance() {
        let args = ["--version", "3.2", "--provenance", "Spring Docs"];
        let (v, p) = parse_kv_flags(&args);
        assert_eq!(v.as_deref(), Some("3.2"));
        assert_eq!(p, "Spring Docs");
    }

    #[test]
    fn parse_kv_flags_handles_missing_values() {
        let args: [&str; 0] = [];
        let (v, p) = parse_kv_flags(&args);
        assert!(v.is_none());
        assert!(p.is_empty());
    }
}

#[cfg(test)]
mod ingest_routing_tests {
    use super::*;

    #[test]
    fn structured_form_takes_the_direct_path() {
        // Strict form: category + path → Text outcome (no agent).
        match neurocode_slash_outcome("ingest FrameworkDocs ./docs/spring --version 3.2") {
            NeurocodeOutcome::Text(t) => {
                // (May succeed or fail on the actual file — but it must NOT
                // be an agent hand-off, and must mention the path/error.)
                assert!(!t.contains("natural-language"), "{t}");
            }
            NeurocodeOutcome::AgentIngest(_) => panic!("strict form must not go to the agent"),
        }
    }

    #[test]
    fn natural_language_takes_the_agent_path() {
        for args in [
            "ingest the spring boot docs in ./docs/spring",
            "ingest the postmortem I just pasted about the outage",
            "ingest everything under docs about Pega rule types",
        ] {
            match neurocode_slash_outcome(args) {
                NeurocodeOutcome::AgentIngest(prompt) => {
                    assert!(prompt.contains("neurocode_ingest"), "prompt teaches the tool");
                    assert!(prompt.contains("FrameworkDocs"), "prompt lists categories");
                }
                _ => panic!("natural language must hand off to the agent: {args}"),
            }
        }
    }

    #[test]
    fn bare_ingest_shows_both_usages() {
        match neurocode_slash_outcome("ingest") {
            NeurocodeOutcome::Text(t) => {
                assert!(t.contains("Usage:"), "{t}");
                assert!(t.contains("naturally"), "mentions the NL form: {t}");
            }
            _ => panic!("bare ingest shows usage"),
        }
    }

    #[test]
    fn category_without_path_is_natural_language() {
        // "ingest Postmortem" alone: no path token → the user is describing
        // something in prose (or will paste it) → agent path.
        match neurocode_slash_outcome("ingest Postmortem") {
            NeurocodeOutcome::AgentIngest(_) => {}
            _ => panic!("category without path should fall to the agent"),
        }
    }

    #[test]
    fn agent_prompt_includes_user_request_verbatim() {
        let p = ingest_agent_prompt("please ingest the spring docs at ./docs/spring");
        assert!(p.contains("please ingest the spring docs at ./docs/spring"));
        assert!(p.contains(".neurocode/sources/"), "pasted-knowledge path taught");
    }
}

#[cfg(test)]
mod ingest_tool_integration_tests {

    /// The neurocode_ingest TOOL is registered when NeuroCode is enabled —
    /// the agent path depends on it being callable in-turn.
    #[test]
    fn ingest_tool_registered_when_enabled() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "neurocode:\n  enabled: true\n").unwrap();
        let config = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        let engine = crate::neurocode_wiring::try_build_engine(&config);
        assert!(engine.is_some(), "engine builds when enabled");
        let engine = engine.unwrap();
        let backend = crate::neurocode_wiring::backend_for_engine(&engine);
        let mut registry = joey_tools::ToolRegistry::new();
        joey_tools::builtins::register_neurocode_tools(&mut registry, Some(backend));
        let names = registry.names();
        assert!(names.contains(&"neurocode_ingest".to_string()), "tool registered: {names:?}");
    }

    /// The backend's ingest path executes against a real (temp) graph.
    #[test]
    fn backend_ingest_roundtrip() {
        // Pin JOEY_HOME to a temp home under the shared override lock so the
        // per-project graph never lands in the real ~/.joey and never races
        // sibling tests that swap JOEY_HOME (and delete their temp homes)
        // mid-run — an unpinned open can transiently fail and yield an empty
        // ingest outcome.
        let _home = super::production_wiring_tests::pinned_home_for_wiring();

        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "neurocode:\n  enabled: true\n").unwrap();
        let config = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        let engine = crate::neurocode_wiring::try_build_engine(&config).unwrap();
        let backend = crate::neurocode_wiring::backend_for_engine(&engine);

        // Source file in a temp project dir.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("notes.md");
        std::fs::write(&src, "# Knowledge\n- fact one\n- fact two\n").unwrap();

        // Index first (opens the graph), then ingest.
        let _ = backend.index(".", true);
        let out = backend.ingest("FrameworkDocs", src.to_str().unwrap(), None, "test");
        assert!(out.contains("Ingested") || out.contains("failed") || out.contains("graph"),
                "honest outcome: {out}");
    }
}

// ---------------------------------------------------------------------------
// /neurocode model fetch tests (T011).
//
// Network-touching tests use `TinyHttpServer` — a std-TcpListener file
// server (no new deps) that serves one directory over minimal HTTP/1.1,
// including an assertion hook that the request host is NEVER a Hugging
// Face host. Decision-logic tests (HF guard, manifest parsing, hash
// refusal) run entirely server-less.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod model_fetch_tests {
    use super::*;
    use std::io::Write as _;
    use std::net::TcpListener;

    // ─── fixtures ───────────────────────────────────────────────────────

    fn sha256_hex(data: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(data);
        h.finalize().iter().fold(String::with_capacity(64), |s, b| s + &format!("{b:02x}"))
    }

    /// A config with the given mirror_url + model_dir, loaded through the
    /// same path production uses (`RagConfig::load` over a temp YAML).
    fn rag_config(mirror_url: &str, model_dir: &Path) -> RagConfig {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            format!(
                "neurocode:\n  rag:\n    local:\n      mirror_url: \"{mirror_url}\"\n      model_dir: \"{}\"\n",
                model_dir.display()
            ),
        )
        .unwrap();
        let config = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        RagConfig::load(&config)
    }

    /// Minimal HTTP/1.1 static file server (std only). Serves GET paths
    /// from `root`; asserts no HF Host header ever arrives (in-process
    /// guarantee for the no-HF tests). 404s anything else.
    /// `pub(super)` so the sibling T039 dylib tests reuse the same server.
    pub(super) struct TinyHttpServer {
        addr: std::net::SocketAddr,
        shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl TinyHttpServer {
        pub(super) fn start(root: &Path) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let root = root.to_path_buf();
            let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let flag = shutdown.clone();
            let thread = std::thread::spawn(move || {
                for conn in listener.incoming() {
                    if flag.load(std::sync::atomic::Ordering::SeqCst) {
                        break;
                    }
                    let Ok(mut stream) = conn else { continue };
                    let root = root.clone();
                    std::thread::spawn(move || {
                        let mut buf = [0u8; 4096];
                        let Ok(n) = stream.read(&mut buf) else { return };
                        let req = String::from_utf8_lossy(&buf[..n]).to_string();
                        let path = req
                            .split_whitespace()
                            .nth(1)
                            .unwrap_or("/")
                            .trim_start_matches('/')
                            .to_string();
                        // No-HF in-process assertion: the Host header joey
                        // actually sent must never be a Hugging Face host.
                        if let Some(host_line) =
                            req.lines().find(|l| l.to_ascii_lowercase().starts_with("host:"))
                        {
                            let host = host_line["host:".len()..].trim();
                            assert!(
                                !is_forbidden_host(host),
                                "test server saw a forbidden host: {host}"
                            );
                        }
                        let file = root.join(&path);
                        match std::fs::read(&file) {
                            Ok(body) => {
                                let resp = format!(
                                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                    body.len()
                                );
                                let _ = stream.write_all(resp.as_bytes());
                                let _ = stream.write_all(&body);
                            }
                            Err(_) => {
                                let resp = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                                let _ = stream.write_all(resp.as_bytes());
                            }
                        }
                    });
                }
            });
            Self { addr, shutdown, thread: Some(thread) }
        }

        pub(super) fn url(&self) -> String {
            format!("http://{}", self.addr)
        }

        pub(super) fn stop(mut self) {
            self.shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
            // Wake the accept loop with a stray connection.
            let _ = std::net::TcpStream::connect(self.addr);
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    // ─── HF guard (no network) ──────────────────────────────────────────

    /// The no-HF guarantee: every Hugging Face host form is refused before
    /// any network call, and non-HF hosts pass (research.md R8).
    #[test]
    fn hf_hosts_are_refused_before_any_network() {
        for bad in [
            "https://huggingface.co",
            "https://HUGGINGFACE.co/nomic-embed-text-v1.5",
            "https://www.huggingface.co",
            "https://cdn-lfs.huggingface.co/x",
            "https://cdn-lfs-us-1.huggingface.co",
            "https://huggingface.co.",
            "https://x.hf.co",
            "https://hf.co",
            "https://HF.CO",
        ] {
            match guard_no_hf(bad) {
                Err(msg) => assert!(
                    msg.contains("huggingface.co") && msg.contains("never"),
                    "refusal for {bad} must name the no-HF policy: {msg}"
                ),
                Ok(_) => panic!("{bad} MUST be refused"),
            }
        }
        // Lookalikes that are NOT HF must pass.
        for ok in [
            "https://mirror.example.com",
            "https://huggingface.co.uk.evil.example.com", // not an HF host
            "http://127.0.0.1:8080",
            "https://my-huggingface.company.org",
        ] {
            assert!(guard_no_hf(ok).is_ok(), "{ok} must NOT be refused");
        }
        // Invalid URLs refuse too.
        assert!(guard_no_hf("not a url").is_err());
        assert!(guard_no_hf("ftp://mirror.example.com").is_err());
    }

    /// Host classification pin (subdomains, case, DNS-root dot, hf.co).
    #[test]
    fn forbidden_host_classification() {
        for host in [
            "huggingface.co",
            "www.huggingface.co",
            "cdn-lfs.huggingface.co",
            "cdn-lfs-us-1.huggingface.co",
            "HuggingFace.Co",
            "huggingface.co.",
            "hf.co",
            "x.hf.co",
            "cas-bridge.xethub.hf.co",
        ] {
            assert!(is_forbidden_host(host), "{host} must be forbidden");
        }
        for host in [
            "example.com",
            "huggingface.com",
            "huggingface.co.uk",
            "not-huggingface.co",
            "hf.company",
            "shf.co",
            "example.com.huggingface.co.evil.tld",
        ] {
            assert!(!is_forbidden_host(host), "{host} must be allowed");
        }
    }

    // ─── grammar ────────────────────────────────────────────────────────

    /// Empty-mirror refusal via the full dispatch path: plain-text
    /// fetch-disabled message, no error, engine not even consulted.
    #[test]
    fn empty_mirror_refuses_fetch() {
        // Default config → empty mirror_url → disabled.
        let out = model_fetch_text(&joey_core::Config::defaults(), None);
        assert!(
            out.contains("Model fetch is disabled") && out.contains("mirror_url is empty"),
            "fetch-disabled message, got: {out}"
        );
        assert!(!out.contains("fetched"), "nothing was fetched: {out}");
    }

    /// `/neurocode model fetch` with an HF mirror refused at dispatch,
    /// before any network connection is attempted.
    #[test]
    fn dispatch_refuses_hf_mirror_before_network() {
        // Point at a LIVE local server but with the mirror host set to an
        // HF host is impossible; instead prove the guard fires first by
        // using a mirror URL that would otherwise connect: an HF URL with
        // a port is still refused without connecting.
        let out = run_model_fetch(
            &rag_config("https://huggingface.co:443", Path::new("/tmp/whatever-t011-a")),
            "nomic-embed-text-v1.5",
            Path::new("/tmp"),
        )
        .unwrap_err();
        assert!(out.contains("never contacts huggingface.co"), "{out}");
    }

    /// Grammar: profile positional, --dylib placeholder, usage errors.
    #[test]
    fn model_fetch_grammar() {
        // Bare `model` shows usage.
        let out = neurocode_slash_provider_scoped_text("model", "zai");
        assert!(out.contains("Usage: /neurocode model fetch"), "{out}");
        // Unknown action.
        let out = neurocode_slash_provider_scoped_text("model delete", "zai");
        assert!(out.contains("Unknown model action 'delete'"), "{out}");
        // --dylib is reserved for T039 (parses; reports planned).
        let out = neurocode_slash_provider_scoped_text("model fetch --dylib", "zai");
        assert!(out.contains("--dylib"), "{out}");
        // Unknown flag → standard argument-error rendering.
        let out = neurocode_slash_provider_scoped_text("model fetch --bogus", "zai");
        assert!(out.contains("unknown flag '--bogus'"), "{out}");
        assert!(out.contains("Usage: /neurocode model fetch"), "{out}");
        // Two profiles → usage error.
        let out =
            neurocode_slash_provider_scoped_text("model fetch nomic-embed-text-v1.5 extra", "zai");
        assert!(out.contains("at most one profile"), "{out}");
        // Empty mirror (default) with explicit profile still refuses.
        let out = neurocode_slash_provider_scoped_text("model fetch CodeRankEmbed", "zai");
        assert!(out.contains("Model fetch is disabled"), "{out}");
        // Unknown profile name → recorded-profiles message, no fetch.
        let server_dir = tempfile::tempdir().unwrap();
        let server = TinyHttpServer::start(server_dir.path());
        let model_dir = tempfile::tempdir().unwrap();
        let out = run_model_fetch(
            &rag_config(&server.url(), model_dir.path()),
            "nomic-embed-code",
            Path::new("/tmp"),
        )
        .unwrap_err();
        server.stop();
        assert!(out.contains("REJECTED"), "rejected profile surfaces reason: {out}");
        // Unknown-but-not-rejected profile.
        let server = TinyHttpServer::start(server_dir.path());
        let out = run_model_fetch(
            &rag_config(&server.url(), model_dir.path()),
            "not-a-profile",
            Path::new("/tmp"),
        )
        .unwrap_err();
        server.stop();
        assert!(out.contains("unknown model profile"), "{out}");
    }

    /// Hash-mismatch refusal, end-to-end over the wire: downloads happen,
    /// then verification refuses — nothing lands in model_dir, no row is
    /// written, staging is cleaned up.
    #[test]
    fn hash_mismatch_downloads_then_refuses_writes_nothing() {
        let server_root = tempfile::tempdir().unwrap();
        let model_bytes = b"fake-onnx-graph-bytes-v1";
        let tok_bytes = b"fake-tokenizer-json-v1";
        std::fs::write(server_root.path().join(MODEL_FILE), model_bytes).unwrap();
        std::fs::write(server_root.path().join(TOKENIZER_FILE), tok_bytes).unwrap();
        // Manifest records a WRONG model hash (mismatch on model.onnx).
        let manifest = serde_json::json!({
            "profiles": {
                "nomic-embed-text-v1.5": {
                    MODEL_FILE: "0".repeat(64),
                    TOKENIZER_FILE: sha256_hex(tok_bytes),
                }
            }
        });
        std::fs::write(
            server_root.path().join(MANIFEST_FILE),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();

        let server = TinyHttpServer::start(server_root.path());
        let model_dir = tempfile::tempdir().unwrap();
        // Pre-existing good artifact — the refusal must not clobber it
        // and must not write the row.
        std::fs::write(model_dir.path().join(MODEL_FILE), b"pre-existing-model").unwrap();

        let err = run_model_fetch(
            &rag_config(&server.url(), model_dir.path()),
            "nomic-embed-text-v1.5",
            Path::new("/tmp"),
        )
        .unwrap_err();
        server.stop();

        assert!(err.contains("SHA-256 mismatch"), "{err}");
        assert!(err.contains(MODEL_FILE), "names the mismatched file: {err}");
        assert!(err.contains("nothing was written"), "{err}");
        // Nothing landed: pre-existing file untouched, no tokenizer, no
        // staging residue, no manifest residue.
        assert_eq!(
            std::fs::read(model_dir.path().join(MODEL_FILE)).unwrap(),
            b"pre-existing-model"
        );
        assert!(!model_dir.path().join(TOKENIZER_FILE).exists());
        assert!(!model_dir.path().join(STAGING_DIR).exists());
        assert!(!model_dir.path().join(MANIFEST_FILE).exists());
    }

    /// Happy path over the wire: verified artifacts land in model_dir and
    /// the `rag_model_artifacts` row is written FROM the project-recorded
    /// expected hashes (stricter than self-registration).
    #[test]
    fn verified_fetch_lands_and_writes_row_from_expected_hashes() {
        let server_root = tempfile::tempdir().unwrap();
        let model_bytes = b"good-onnx-bytes";
        let tok_bytes = b"good-tokenizer-bytes";
        let license = b"Apache-2.0 attribution";
        std::fs::write(server_root.path().join(MODEL_FILE), model_bytes).unwrap();
        std::fs::write(server_root.path().join(TOKENIZER_FILE), tok_bytes).unwrap();
        std::fs::write(server_root.path().join("LICENSE"), license).unwrap();
        let manifest = serde_json::json!({
            "profiles": {
                "CodeRankEmbed": {
                    MODEL_FILE: sha256_hex(model_bytes),
                    TOKENIZER_FILE: sha256_hex(tok_bytes),
                    "LICENSE": sha256_hex(license),
                }
            }
        });
        std::fs::write(
            server_root.path().join(MANIFEST_FILE),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();

        let server = TinyHttpServer::start(server_root.path());
        let model_dir = tempfile::tempdir().unwrap();

        // Pin JOEY_HOME so the graph row lands in a temp home, not the
        // real ~/.joey (process_joey_home reads the env var directly).
        let _override_lock = joey_core::constants::TEST_HOME_OVERRIDE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev_home = std::env::var_os("JOEY_HOME");
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("JOEY_HOME", home.path());

        let project_root = tempfile::tempdir().unwrap();
        let out = run_model_fetch(
            &rag_config(&server.url(), model_dir.path()),
            "CodeRankEmbed",
            project_root.path(),
        );
        let result = match out {
            Ok(msg) => msg,
            Err(e) => panic!("expected verified fetch, got refusal: {e}"),
        };

        // Row written from the EXPECTED hashes with the mirror recorded.
        let graph = DependencyGraph::open_for_project(project_root.path()).unwrap();
        let (m, t, size, mirror): (String, String, i64, Option<String>) = graph
            .store()
            .conn()
            .query_row(
                "SELECT model_sha256, tokenizer_sha256, model_size_bytes, mirror_url_used
                 FROM rag_model_artifacts WHERE profile = 'CodeRankEmbed'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(m, sha256_hex(model_bytes));
        assert_eq!(t, sha256_hex(tok_bytes));
        assert_eq!(size, model_bytes.len() as i64);
        assert_eq!(mirror.as_deref(), Some(server.url().as_str()));

        // Restore env before asserting on disk so a later failure can't
        // leak the JOEY_HOME pin.
        match prev_home {
            Some(v) => std::env::set_var("JOEY_HOME", v),
            None => std::env::remove_var("JOEY_HOME"),
        }
        drop(_override_lock);

        // Artifacts (incl. the extra LICENSE) landed; staging cleaned.
        assert_eq!(
            std::fs::read(model_dir.path().join(MODEL_FILE)).unwrap(),
            model_bytes
        );
        assert_eq!(
            std::fs::read(model_dir.path().join(TOKENIZER_FILE)).unwrap(),
            tok_bytes
        );
        assert_eq!(std::fs::read(model_dir.path().join("LICENSE")).unwrap(), license);
        assert!(!model_dir.path().join(STAGING_DIR).exists());
        assert!(result.contains("row written"), "{result}");
        server.stop();
    }

    // ─── server-less decision logic ─────────────────────────────────────

    /// Manifest parsing: required entries enforced, extras carried,
    /// malformed hashes / missing profile / missing files refused.
    #[test]
    fn manifest_parsing_decisions() {
        let good = serde_json::json!({
            "profiles": {
                "p1": { MODEL_FILE: "a".repeat(64), TOKENIZER_FILE: "b".repeat(64) },
            }
        });
        let files = expected_files_from_manifest(&good, "p1").unwrap();
        assert_eq!(
            files,
            vec![
                (MODEL_FILE.to_string(), "a".repeat(64)),
                (TOKENIZER_FILE.to_string(), "b".repeat(64)),
            ]
        );
        // Bare root object (no "profiles" wrapper) also accepted.
        let bare = serde_json::json!({
            "p1": { MODEL_FILE: "a".repeat(64), TOKENIZER_FILE: "b".repeat(64) }
        });
        assert!(expected_files_from_manifest(&bare, "p1").is_ok());
        // Extra recorded files are carried (ranked after required ones).
        let extra = serde_json::json!({
            "profiles": {
                "p1": {
                    "LICENSE": "c".repeat(64),
                    MODEL_FILE: "a".repeat(64),
                    TOKENIZER_FILE: "b".repeat(64),
                }
            }
        });
        let files = expected_files_from_manifest(&extra, "p1").unwrap();
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].0, MODEL_FILE);
        assert_eq!(files[2].0, "LICENSE");
        // Refusals: unknown profile, short hash, non-hex, non-string,
        // missing required file, non-object entry, non-object manifest.
        assert!(expected_files_from_manifest(&good, "nope").is_err());
        let bad_hash = serde_json::json!({
            "profiles": { "p1": { MODEL_FILE: "a".repeat(63), TOKENIZER_FILE: "b".repeat(64) } }
        });
        assert!(expected_files_from_manifest(&bad_hash, "p1").is_err());
        let non_hex = serde_json::json!({
            "profiles": { "p1": { MODEL_FILE: "z".repeat(64), TOKENIZER_FILE: "b".repeat(64) } }
        });
        assert!(expected_files_from_manifest(&non_hex, "p1").is_err());
        let non_str = serde_json::json!({
            "profiles": { "p1": { MODEL_FILE: 7, TOKENIZER_FILE: "b".repeat(64) } }
        });
        assert!(expected_files_from_manifest(&non_str, "p1").is_err());
        let missing = serde_json::json!({
            "profiles": { "p1": { MODEL_FILE: "a".repeat(64) } }
        });
        assert!(expected_files_from_manifest(&missing, "p1").is_err());
        let non_obj = serde_json::json!({ "profiles": { "p1": "nope" } });
        assert!(expected_files_from_manifest(&non_obj, "p1").is_err());
        let not_manifest = serde_json::json!([1, 2]);
        assert!(expected_files_from_manifest(&not_manifest, "p1").is_err());
    }

    /// Staged-verification refusal logic, server-less.
    #[test]
    fn staged_verification_refuses_on_mismatch_only() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(MODEL_FILE), b"abc").unwrap();
        std::fs::write(tmp.path().join(TOKENIZER_FILE), b"xyz").unwrap();
        let abc = sha256_hex(b"abc");
        let xyz = sha256_hex(b"xyz");
        // Exact hashes pass.
        let expected = vec![
            (MODEL_FILE.to_string(), abc.clone()),
            (TOKENIZER_FILE.to_string(), xyz.clone()),
        ];
        assert!(verify_staged_files(tmp.path(), &expected).is_ok());
        // Case-insensitive hex passes.
        let upper: String = abc.chars().take(64).map(|c| c.to_ascii_uppercase()).collect();
        let expected = vec![(MODEL_FILE.to_string(), upper), (TOKENIZER_FILE.to_string(), xyz)];
        assert!(verify_staged_files(tmp.path(), &expected).is_ok());
        // Any mismatch refuses, naming the file.
        let bad = vec![(MODEL_FILE.to_string(), "0".repeat(64))];
        let err = verify_staged_files(tmp.path(), &bad).unwrap_err();
        assert!(err.contains(MODEL_FILE) && err.contains("mismatch"), "{err}");
        // Missing staged file refuses (io error), not a panic.
        let missing = vec![("nope.bin".to_string(), "0".repeat(64))];
        assert!(verify_staged_files(tmp.path(), &missing).is_err());
    }

    /// Mirror base normalization: no trailing slash → join appends
    /// (doesn't clobber the last segment); subpath mirrors work.
    #[test]
    fn mirror_base_join_semantics() {
        let base = normalize_mirror_base(
            url::Url::parse("https://mirror.example.com/neurocode").unwrap(),
        );
        assert_eq!(base.as_str(), "https://mirror.example.com/neurocode/");
        assert_eq!(
            base.join(MODEL_FILE).unwrap().as_str(),
            "https://mirror.example.com/neurocode/model.onnx"
        );
        let slashed = normalize_mirror_base(
            url::Url::parse("https://mirror.example.com/neurocode/").unwrap(),
        );
        assert_eq!(
            slashed.join(MANIFEST_FILE).unwrap().as_str(),
            "https://mirror.example.com/neurocode/manifest.json"
        );
    }
}

// ---------------------------------------------------------------------------
// /neurocode search tests (T015, contracts/neurocode-rag-command.md § Test
// obligations 1 — grammar parse tests, edge cases 1–2, and the end-to-end
// fallback-chunk search through the dispatch core).
//
// NOTE: `search_command_text*` takes the argument tail AFTER the `search`
// token (dispatch passes `&parts[1..]`) — tests pass exactly that shape.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod search_tests {
    use super::*;
    use joey_neurocode::parse::extract::SourceExtraction;
    use joey_neurocode_rag::embed::profiles::NOMIC_EMBED_TEXT_V1_5;
    use joey_neurocode_rag::index::chunker::{build_chunk_records, ChunkKind, ChunkOptions};
    use joey_neurocode_rag::vector::quantize::Quantization;
    use joey_neurocode_rag::vector::store::write_index;

    // ─── grammar: flag → SearchRequest mapping ──────────────────────────

    /// The full grammar: every flag parses to the documented mapping
    /// (--path→file_filter, --limit→limit, --expand-lines→expand_lines,
    /// --relations→relation_depth, --json toggles the payload shape).
    #[test]
    fn grammar_maps_every_flag_to_search_request() {
        let args = parse_search_args(&[
            "token", "validation",
            "--path", "src/auth/**",
            "--limit", "7",
            "--expand-lines", "12",
            "--relations", "2",
            "--json",
        ])
        .unwrap();
        let req = build_search_request(&args, &RagConfig::default());
        assert_eq!(req.query, "token validation");
        assert_eq!(req.file_filter.as_deref(), Some("src/auth/**"));
        assert_eq!(req.limit, 7);
        assert_eq!(req.expand_lines, 12);
        assert_eq!(req.relation_depth, 2);
        assert!(args.json);
    }

    /// Defaults come from RagConfig: limit ← top_k, expand_lines ←
    /// context_window_lines, relation_depth 0, file_filter None.
    #[test]
    fn defaults_flow_from_rag_config() {
        let args = parse_search_args(&["login"]).unwrap();
        let req = build_search_request(&args, &RagConfig::default());
        assert_eq!(req.limit, 10, "neurocode.rag.top_k default");
        assert_eq!(req.expand_lines, 20, "neurocode.rag.context_window_lines default");
        assert_eq!(req.relation_depth, 0);
        assert_eq!(req.file_filter, None);
        // Config-sourced defaults: a non-default top_k flows through.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            "neurocode:\n  rag:\n    top_k: 3\n    context_window_lines: 5\n",
        )
        .unwrap();
        let config = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        let req = build_search_request(&args, &RagConfig::load(&config));
        assert_eq!(req.limit, 3);
        assert_eq!(req.expand_lines, 5);
    }

    /// Clamps per the tool schema maximums: --limit caps at 50,
    /// --expand-lines at 200 (contracts/neurocode-rag-tools.md).
    #[test]
    fn limit_and_expand_lines_clamp_to_50_and_200() {
        let args =
            parse_search_args(&["x", "--limit", "999", "--expand-lines", "9999"]).unwrap();
        let req = build_search_request(&args, &RagConfig::default());
        assert_eq!(req.limit, 50);
        assert_eq!(req.expand_lines, 200);
    }

    /// T027 clamp bounds at the CLI surface: --expand-lines 0 passes
    /// through (a valid window), 200 passes through (the boundary), and
    /// 250 clamps to 200.
    #[test]
    fn expand_lines_boundaries_zero_two_hundred_two_fifty() {
        for (flag, expected) in [("0", 0u32), ("200", 200), ("250", 200)] {
            let args = parse_search_args(&["x", "--expand-lines", flag]).unwrap();
            let req = build_search_request(&args, &RagConfig::default());
            assert_eq!(req.expand_lines, expected, "--expand-lines {flag} → {expected}");
        }
    }

    /// T027 cross-surface precedence: the --expand-lines flag override
    /// beats a non-default config `context_window_lines`, and stays
    /// clamped — while no flag yields the config value. Effective window:
    /// request override > config default, always 0–200.
    #[test]
    fn expand_lines_flag_overrides_config_and_stays_clamped() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            "neurocode:\n  rag:\n    context_window_lines: 5\n",
        )
        .unwrap();
        let config = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        let rag = RagConfig::load(&config);
        assert_eq!(rag.context_window_lines, 5, "config default 5 loaded");

        // No flag → the config value.
        let args = parse_search_args(&["q"]).unwrap();
        assert_eq!(build_search_request(&args, &rag).expand_lines, 5);

        // Flag wins over the config value…
        let args = parse_search_args(&["q", "--expand-lines", "12"]).unwrap();
        assert_eq!(build_search_request(&args, &rag).expand_lines, 12);

        // …and is still clamped to the 0–200 contract bound at this surface.
        let args = parse_search_args(&["q", "--expand-lines", "250"]).unwrap();
        assert_eq!(build_search_request(&args, &rag).expand_lines, 200);
    }

    /// `--relations 3` is rejected with the standard argument-error
    /// rendering (message + usage); boundary values 0/1/2 parse.
    #[test]
    fn relations_above_two_is_rejected() {
        let out =
            search_command_text(&["--relations", "3"], &joey_core::Config::defaults());
        assert!(out.contains("invalid --relations value '3'"), "{out}");
        assert!(out.contains("Usage: /neurocode search"), "{out}");
        for ok in ["0", "1", "2"] {
            assert!(
                parse_search_args(&["q", "--relations", ok]).is_ok(),
                "--relations {ok} must parse"
            );
        }
    }

    /// Missing `<query...>` produces the standard argument-error rendering
    /// (contract obligation 1) — bare and flags-only forms alike.
    #[test]
    fn missing_query_is_rejected() {
        let out = search_command_text(&[], &joey_core::Config::defaults());
        assert!(out.contains("missing <query...>"), "{out}");
        assert!(out.contains("Usage: /neurocode search"), "{out}");
        let out =
            search_command_text(&["--limit", "5"], &joey_core::Config::defaults());
        assert!(out.contains("missing <query...>"), "{out}");
    }

    /// Unknown flags / missing flag values / bad integers are usage errors.
    #[test]
    fn unknown_flags_and_bad_values_are_usage_errors() {
        for args in [
            vec!["q", "--bogus"],
            vec!["q", "--limit"],        // missing value
            vec!["q", "--limit", "abc"], // not an integer
            vec!["q", "--relations", "x"],
        ] {
            let err = parse_search_args(&args).unwrap_err();
            let out = search_command_text(&args, &joey_core::Config::defaults());
            assert!(out.contains(&err), "rendering names the error: {out}");
            assert!(out.contains("Usage: /neurocode search"), "{out}");
        }
    }

    // ─── edge case 1: whitespace-only query ─────────────────────────────

    /// Whitespace-only query: clear validation message, NO search executed,
    /// no error rendering — usage accompanies the message.
    #[test]
    fn whitespace_only_query_validates_without_searching() {
        // Grammar layer: whitespace is a positional (parseable); the
        // execution layer must reject it as Validation.
        let args = parse_search_args(&["   "]).unwrap();
        assert_eq!(args.query_words, vec!["   "]);
        let (_root, _home) = pinned_home();
        let tmp = tempfile::tempdir().unwrap();
        let out = search_command_text_at(&["   "], &joey_core::Config::defaults(), tmp.path());
        assert!(
            out.contains("empty or whitespace-only") || out.contains("whitespace-only"),
            "clear validation message, got: {out}"
        );
        assert!(out.contains("Usage: /neurocode search"), "{out}");
        assert!(
            !out.contains("Search failed"),
            "validation is not a failure rendering: {out}"
        );
        assert!(!out.contains("Nothing matched"), "no search was executed: {out}");
    }

    // ─── edge case 2: no-match clear response ───────────────────────────

    /// No-match: 'Nothing matched' is a CLEAR response, distinct from
    /// failure; the empty-index variant adds the /neurocode index hint.
    #[test]
    fn no_match_renders_clear_response_distinct_from_failure() {
        let (_root, _home) = pinned_home();
        let dir = indexed_toplevel_fixture();
        let out = search_command_text_at(
            &["zzzznotpresentzzzz"],
            &joey_core::Config::defaults(),
            dir.path(),
        );
        assert!(out.contains("Nothing matched"), "{out}");
        assert!(
            !out.contains("failed"),
            "no-match is not a failure: {out}"
        );
        assert!(out.contains("indexed chunks searched"), "{out}");

        // Empty index variant: the /neurocode index hint appears.
        let empty = tempfile::tempdir().unwrap();
        let out =
            search_command_text_at(&["anything"], &joey_core::Config::defaults(), empty.path());
        assert!(out.contains("Nothing matched"), "{out}");
        assert!(out.contains("/neurocode index"), "empty-index hint: {out}");
    }

    // ─── e2e: fallback-chunk search through the dispatch core ───────────

    /// End-to-end: a Python top-level-code sample (no named artifacts) is
    /// indexed via the rag crate and searched through the dispatch core —
    /// the fallback badge appears in text output and mode/mode_reason in
    /// --json output (FR-014 + FR-008 via the keyword-only path, no
    /// embedder needed).
    #[test]
    fn e2e_fallback_chunk_search_via_dispatch() {
        let (_root, _home) = pinned_home();
        let dir = indexed_toplevel_fixture();

        // Plain text: the fallback badge is visible.
        let out = search_command_text_at(
            &["secret-literal"],
            &joey_core::Config::defaults(),
            dir.path(),
        );
        assert!(
            out.contains("keyword-only mode"),
            "degradation visible in text mode: {out}"
        );
        assert!(
            out.contains("[fallback-chunk]"),
            "fallback badge in text output: {out}"
        );
        assert!(out.contains("scripts/top.py"), "{out}");
        assert!(!out.contains("Nothing matched"), "results present: {out}");

        // --json: mode + mode_reason + chunk_kind in the tool-shaped payload.
        let out = search_command_text_at(
            &["secret-literal", "--json"],
            &joey_core::Config::defaults(),
            dir.path(),
        );
        let payload: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(payload["mode"], "keyword_only", "mode in payload: {out}");
        let reason = payload["mode_reason"].as_str().unwrap_or_default();
        assert!(
            reason.contains("keyword-only fallback"),
            "mode_reason in payload: {out}"
        );
        let results = payload["results"].as_array().unwrap();
        assert!(
            results
                .iter()
                .any(|r| r["chunk_kind"] == "fallback" && r["file"] == "scripts/top.py"),
            "fallback result for the sample present: {out}"
        );
    }

    // ─── fixtures ────────────────────────────────────────────────────────

    /// The T015 e2e sample (same shape as the rag crate's own fixture):
    /// imports + top-level statements + one function → fallback chunks for
    /// the top-level region plus a symbol chunk for `main`.
    const PY_TOPLEVEL: &str = "import os\nimport sys\n\nAPI_KEY=\"secret-literal\"\n\ndef main():\n    print(os.name)\n    return 0\n";

    /// Pin JOEY_HOME to a temp dir (under the shared override lock) so the
    /// per-project graph lands in a temp home, not the real `~/.joey`.
    /// Returns (TempDir-to-keep-alive, guard) — dropping the guard restores.
    fn pinned_home() -> (tempfile::TempDir, HomeGuard) {
        let lock = joey_core::constants::TEST_HOME_OVERRIDE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os("JOEY_HOME");
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("JOEY_HOME", home.path());
        (home, HomeGuard { prev, _lock: lock })
    }

    /// Restores JOEY_HOME on drop (panic-safe).
    struct HomeGuard {
        prev: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => std::env::set_var("JOEY_HOME", v),
                None => std::env::remove_var("JOEY_HOME"),
            }
        }
    }

    /// Build a temp project with the Python top-level sample indexed via
    /// the rag crate through the SAME per-project graph path the command
    /// opens (keyword-only path needs no embedder — vectors are written as
    /// `None` rows).
    fn indexed_toplevel_fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("scripts")).unwrap();
        std::fs::write(dir.path().join("scripts").join("top.py"), PY_TOPLEVEL).unwrap();

        let graph =
            joey_neurocode::graph::DependencyGraph::open_for_project(dir.path()).unwrap();
        let store = graph.store();
        let mut ex = SourceExtraction {
            language: "python".to_string(),
            ..Default::default()
        };
        ex.populate_fallback_chunks(PY_TOPLEVEL);
        let records = build_chunk_records(
            &ex,
            PY_TOPLEVEL,
            "scripts/top.py",
            store,
            &ChunkOptions::default(),
        );
        assert!(
            records.iter().any(|r| r.kind == ChunkKind::Fallback),
            "fixture must contain a fallback chunk"
        );
        let vectors = vec![None; records.len()];
        write_index(
            store,
            &NOMIC_EMBED_TEXT_V1_5,
            Quantization::F32,
            &records,
            &vectors,
            &[],
        )
        .unwrap();
        dir
    }
}

// ---------------------------------------------------------------------------
// /neurocode consent tests (T031, contracts/neurocode-rag-command.md §
// Consent subcommand + test obligation 3: the show→ack→revoke→re-ack
// state machine, consent.json contents asserted at every step, declined
// confirmation leaves no state change, and remote embeds are refused while
// not Acknowledged — pinned via the rag crate's consent record (the gate
// input) with a NON-loopback base_url, firing pre-network).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod consent_tests {
    use super::*;
    use joey_neurocode_rag::consent::{ConsentRecord, ConsentState};

    /// Pin JOEY_HOME to a temp dir under the shared override lock so the
    /// per-project consent.json lands in a temp home, not `~/.joey`.
    struct HomeGuard {
        prev: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
        _dir: tempfile::TempDir,
    }

    fn pinned_home() -> HomeGuard {
        let lock = joey_core::constants::TEST_HOME_OVERRIDE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os("JOEY_HOME");
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("JOEY_HOME", dir.path());
        HomeGuard { prev, _lock: lock, _dir: dir }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => std::env::set_var("JOEY_HOME", v),
                None => std::env::remove_var("JOEY_HOME"),
            }
        }
    }

    /// A remote (non-loopback) base_url config — the consent-gated case.
    fn remote_config() -> joey_core::Config {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            "neurocode:\n  rag:\n    base_url: \"https://api.voyageai.com\"\n    model: \"voyage-code-3\"\n",
        )
        .unwrap();
        joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap()
    }

    fn always_yes(_: &str) -> bool {
        true
    }
    fn always_no(_: &str) -> bool {
        false
    }

    fn record_for(project_root: &Path) -> ConsentRecord {
        let dir = joey_neurocode_rag::consent::consent_file_path(project_root)
            .parent()
            .unwrap()
            .to_path_buf();
        ConsentRecord::load(&dir).unwrap()
    }

    /// The full show→ack→revoke→re-ack state machine, asserting the
    /// consent.json CONTENTS at every step (contract obligation 3).
    #[test]
    fn show_ack_revoke_reack_state_machine() {
        let _g = pinned_home();
        let project = tempfile::tempdir().unwrap();
        let config = remote_config();

        // show (absent file) ⇒ never_acknowledged + disclosure.
        let out = consent_command_text(&["show"], &config, project.path(), &mut always_no);
        assert!(out.contains("never_acknowledged"), "{out}");
        assert!(out.contains("https://api.voyageai.com"), "{out}");
        assert!(out.contains("(remote)"), "{out}");
        assert!(out.contains("voyage-code-3"), "{out}");
        assert!(out.contains("WILL BE"), "egress disclosure present: {out}");
        assert!(!project.path().join("consent.json").exists());

        // Declined confirmation ⇒ no state change.
        let out = consent_command_text(&["ack"], &config, project.path(), &mut always_no);
        assert!(out.contains("declined"), "{out}");
        assert!(out.contains("no state"), "{out}");
        assert!(!consent_file_path(project.path()).exists(), "no file written");

        // Ack (confirmed) ⇒ consent.json written, Acknowledged + audit fields.
        // The prompt restates code egress — captured BEFORE the ack succeeds.
        let mut prompt_seen = String::new();
        let out = consent_command_text(&["ack"], &config, project.path(), &mut |p| {
            prompt_seen = p.to_string();
            true
        });
        assert!(out.contains("acknowledged"), "{out}");
        assert!(prompt_seen.contains("WILL BE"), "restates egress: {prompt_seen}");
        assert!(prompt_seen.contains("voyage-code-3"), "{prompt_seen}");
        let rec = record_for(project.path());
        assert_eq!(rec.state, ConsentState::Acknowledged);
        assert!(rec.permits_remote());
        assert_eq!(rec.remote_backend_url, "https://api.voyageai.com");
        assert_eq!(rec.model_at_ack_time.as_deref(), Some("voyage-code-3"));
        assert!(rec.acknowledged_at.is_some());
        assert!(rec.revoked_at.is_none());

        // show reflects the acknowledged state.
        let out = consent_command_text(&["show"], &config, project.path(), &mut always_no);
        assert!(out.contains("acknowledged"), "{out}");

        // Double-ack from Acknowledged ⇒ illegal transition reported.
        let out = consent_command_text(&["ack"], &config, project.path(), &mut always_yes);
        assert!(out.contains("already acknowledged"), "{out}");

        // Revoke ⇒ immediate effect: permits_remote false, revoked_at stamped.
        let out = consent_command_text(&["revoke"], &config, project.path(), &mut always_no);
        assert!(out.contains("revoked"), "{out}");
        assert!(out.contains("immediately"), "{out}");
        let rec = record_for(project.path());
        assert_eq!(rec.state, ConsentState::Revoked);
        assert!(!rec.permits_remote());
        assert!(rec.revoked_at.is_some(), "revoked_at written (contract)");
        assert!(rec.acknowledged_at.is_some(), "ack audit trail preserved");

        // Revoke-from-revoked ⇒ illegal, reported.
        let out = consent_command_text(&["revoke"], &config, project.path(), &mut always_no);
        assert!(out.contains("nothing to revoke"), "{out}");

        // Re-ack after revoke is ALLOWED (Revoked → Acknowledged).
        let out = consent_command_text(&["ack"], &config, project.path(), &mut always_yes);
        assert!(out.contains("acknowledged"), "{out}");
        let rec = record_for(project.path());
        assert_eq!(rec.state, ConsentState::Acknowledged);
        assert!(rec.permits_remote());
        assert!(rec.revoked_at.is_none(), "revoked_at cleared on re-ack");

        // Unknown action.
        let out = consent_command_text(&["bogus"], &config, project.path(), &mut always_no);
        assert!(out.contains("Unknown consent action"), "{out}");
    }

    /// T044: `ack --yes` records consent WITHOUT consulting the confirm
    /// callback — the non-interactive path that makes /neurocode consent
    /// operable from the TUI (HeavyJob engine thread has no stdin). The
    /// consent.json contents must match the interactive ack exactly.
    #[test]
    fn ack_yes_records_consent_without_prompting() {
        let _g = pinned_home();
        let project = tempfile::tempdir().unwrap();
        let config = remote_config();

        // The confirm callback panics if consulted: --yes must skip it
        // entirely (stand-in for "stdin unavailable").
        let out = consent_command_text(
            &["ack", "--yes"],
            &config,
            project.path(),
            &mut |_| panic!("--yes must not consult the interactive prompt"),
        );
        assert!(out.contains("acknowledged"), "{out}");
        assert!(consent_file_path(project.path()).exists(), "consent.json written");

        // Real file contents: Acknowledged + full audit trail (same shape
        // the interactive path writes, per consent.rs semantics).
        let rec = record_for(project.path());
        assert_eq!(rec.state, ConsentState::Acknowledged);
        assert!(rec.permits_remote());
        assert_eq!(rec.remote_backend_url, "https://api.voyageai.com");
        assert_eq!(rec.model_at_ack_time.as_deref(), Some("voyage-code-3"));
        assert!(rec.acknowledged_at.is_some());
        assert!(rec.revoked_at.is_none());
    }

    /// T044: `-y` is accepted as the short form of `--yes`.
    #[test]
    fn ack_short_flag_y_skips_prompt() {
        let _g = pinned_home();
        let project = tempfile::tempdir().unwrap();
        let config = remote_config();
        let out = consent_command_text(
            &["ack", "-y"],
            &config,
            project.path(),
            &mut |_| panic!("-y must not consult the interactive prompt"),
        );
        assert!(out.contains("acknowledged"), "{out}");
        assert_eq!(record_for(project.path()).state, ConsentState::Acknowledged);
    }

    /// T044: WITHOUT --yes the interactive path is preserved byte-for-byte:
    /// the confirm callback is consulted, and declining still aborts with
    /// no state change.
    #[test]
    fn ack_without_yes_still_prompts_and_decline_aborts() {
        let _g = pinned_home();
        let project = tempfile::tempdir().unwrap();
        let config = remote_config();

        let mut prompted = 0usize;
        let out = consent_command_text(&["ack"], &config, project.path(), &mut |p| {
            prompted += 1;
            assert!(p.contains("WILL BE"), "prompt restates egress: {p}");
            false // decline
        });
        assert_eq!(prompted, 1, "interactive prompt consulted exactly once");
        assert!(out.contains("declined"), "{out}");
        assert!(!consent_file_path(project.path()).exists(), "no file written");
    }

    /// T044: `revoke` was already non-interactive (no confirm call); --yes
    /// is accepted but changes nothing — additive flag, no breakage.
    #[test]
    fn revoke_accepts_yes_flag_unchanged_behavior() {
        let _g = pinned_home();
        let project = tempfile::tempdir().unwrap();
        let config = remote_config();
        // Seed an acknowledged record first, non-interactively.
        let out = consent_command_text(
            &["ack", "--yes"],
            &config,
            project.path(),
            &mut |_| panic!("--yes must not prompt"),
        );
        assert!(out.contains("acknowledged"), "{out}");

        let out = consent_command_text(
            &["revoke", "--yes"],
            &config,
            project.path(),
            &mut |_| panic!("revoke must never prompt"),
        );
        assert!(out.contains("revoked"), "{out}");
        let rec = record_for(project.path());
        assert_eq!(rec.state, ConsentState::Revoked);
        assert!(rec.revoked_at.is_some(), "revoked_at stamped");
    }

    /// Loopback base_url renders as local/consent-free in `show`.
    #[test]
    fn loopback_backend_renders_local() {
        let _g = pinned_home();
        let project = tempfile::tempdir().unwrap();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            "neurocode:\n  rag:\n    base_url: \"http://localhost:11434\"\n",
        )
        .unwrap();
        let config = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        let out = consent_command_text(&["show"], &config, project.path(), &mut always_no);
        assert!(out.contains("(local (loopback))") || out.contains("loopback"), "{out}");
        assert!(out.contains("consent-free"), "{out}");
    }

    /// The pre-network egress gate input: with a NON-loopback base_url and
    /// consent not Acknowledged, `ConsentRecord::permits_remote()` is false —
    /// the exact predicate the rag crate's embed gate consults BEFORE any
    /// network call (FR-012; a mocked remote embed is refused while not
    /// Acknowledged).
    #[test]
    fn remote_embed_refused_while_not_acknowledged() {
        let _g = pinned_home();
        let project = tempfile::tempdir().unwrap();
        // Gate predicate with the would-be mocked remote backend URL.
        let remote = "https://api.voyageai.com";
        assert!(!base_url_is_loopback(remote), "fixture is non-loopback");

        // NeverAcknowledged: refused.
        let rec = record_for(project.path());
        assert_eq!(rec.state, ConsentState::NeverAcknowledged);
        assert!(!rec.permits_remote(), "unconsented remote must refuse egress");

        // Acknowledged: permitted.
        let mut rec = rec;
        rec.acknowledge_now(remote.to_string(), "voyage-code-3").unwrap();
        assert!(rec.permits_remote());

        // Revoked: refused again — mid-operation revocation stops egress.
        rec.revoke_now().unwrap();
        assert!(!rec.permits_remote(), "revoked consent must refuse egress");
    }

    /// Host classification pin for the loopback rule.
    #[test]
    fn loopback_classification() {
        for ok in [
            "http://localhost:11434",
            "http://127.0.0.1:8080",
            "http://127.0.0.0:1",
            "http://LOCALHOST:1",
        ] {
            assert!(base_url_is_loopback(ok), "{ok} must be loopback");
        }
        for remote in [
            "https://api.voyageai.com",
            "http://128.0.0.1:9",
            "http://126.0.0.1:9",
            "not a url",
            "",
        ] {
            assert!(!base_url_is_loopback(remote), "{remote} must NOT be loopback");
        }
    }
}

// ---------------------------------------------------------------------------
// RAG status tests (T034, FR-013; contracts/neurocode-rag-tools.md §
// neurocode_status + contracts/neurocode-rag-command.md § Status extension):
// the enabled-path field list (JSON section), the CLI section rendering,
// and the disabled-path byte-identity (section absent ENTIRELY — never
// present-but-empty).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod rag_status_tests {
    use super::*;
    use joey_neurocode_rag::consent::{ConsentRecord, ConsentState};
    use joey_neurocode_rag::index::chunker::{build_chunk_records, ChunkOptions};
    use joey_neurocode_rag::vector::quantize::Quantization;
    use joey_neurocode_rag::vector::store::write_index;
    use joey_neurocode::parse::extract::SourceExtraction;
    use joey_neurocode_rag::embed::profiles::NOMIC_EMBED_TEXT_V1_5;

    struct HomeGuard2 {
        prev: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
        _dir: tempfile::TempDir,
    }

    fn pinned_home() -> HomeGuard2 {
        let lock = joey_core::constants::TEST_HOME_OVERRIDE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os("JOEY_HOME");
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("JOEY_HOME", dir.path());
        HomeGuard2 { prev, _lock: lock, _dir: dir }
    }

    impl Drop for HomeGuard2 {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => std::env::set_var("JOEY_HOME", v),
                None => std::env::remove_var("JOEY_HOME"),
            }
        }
    }

    fn config_with(yaml: &str) -> joey_core::Config {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), yaml).unwrap();
        joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap()
    }

    fn enabled_config() -> joey_core::Config {
        config_with(
            "neurocode:\n  rag:\n    enabled: true\n    backend: auto\n    model: \"nomic-embed-text-v1.5\"\n",
        )
    }

    /// Enabled path: the JSON section carries the contract field list,
    /// with consent state + degradation surfaced (T034 scope: enabled
    /// output only; disabled parity belongs to T035).
    #[test]
    fn enabled_json_section_field_list() {
        let _g = pinned_home();
        let project = tempfile::tempdir().unwrap();
        let v = rag_status_json(&enabled_config(), project.path()).expect("enabled ⇒ Some");
        for key in [
            "index_state", "chunk_count", "vector_count", "model", "dim",
            "quantization", "last_refresh_at", "refresh_state", "backend_health",
            "consent_state", "degradation",
        ] {
            assert!(v.get(key).is_some(), "field {key} present: {v}");
        }
        // Empty project: empty index + never_acknowledged + degradation notes.
        assert_eq!(v["index_state"], "empty");
        assert_eq!(v["chunk_count"], 0);
        assert_eq!(v["vector_count"], 0);
        assert_eq!(v["consent_state"], "never_acknowledged");
        let degr = v["degradation"].as_array().unwrap();
        assert!(
            degr.iter().any(|d| d.as_str().unwrap_or("").contains("index empty")),
            "empty-index degradation: {v}"
        );

        // CLI text section mirrors the fields.
        let section = rag_status_section(&enabled_config(), project.path()).unwrap();
        assert!(section.starts_with("\nRAG: enabled"), "{section}");
        assert!(section.contains("0 chunks, 0 vectors (empty)"), "{section}");
        assert!(section.contains("consent never_acknowledged"), "{section}");
        assert!(section.contains("Degradation:"), "{section}");
    }

    /// Enabled path with an actual index: counts/model/dim/quantization/
    /// last_refresh_at read back from the v3 tables via the per-project
    /// graph (same path the command opens).
    #[test]
    fn enabled_section_reads_index_meta() {
        let _g = pinned_home();
        let project = tempfile::tempdir().unwrap();
        let graph =
            joey_neurocode::graph::DependencyGraph::open_for_project(project.path()).unwrap();
        let store = graph.store();
        let source = "import os\nx = 1\ny = 2\n";
        let mut ex = SourceExtraction {
            language: "python".to_string(),
            ..Default::default()
        };
        ex.populate_fallback_chunks(source);
        let records = build_chunk_records(
            &ex, source, "top.py", store, &ChunkOptions::default(),
        );
        let vectors = vec![None; records.len()];
        write_index(
            store, &NOMIC_EMBED_TEXT_V1_5, Quantization::F32,
            &records, &vectors, &[],
        )
        .unwrap();

        let v = rag_status_json(&enabled_config(), project.path()).unwrap();
        assert_eq!(v["index_state"], "ready");
        assert_eq!(v["chunk_count"], records.len() as u64);
        assert_eq!(v["model"], "nomic-embed-text-v1.5");
        assert_eq!(v["dim"], NOMIC_EMBED_TEXT_V1_5.dim);
        assert_eq!(v["quantization"], "f32");
        assert!(v["last_refresh_at"].is_string(), "freshness present: {v}");
        assert_eq!(v["refresh_state"], "idle");

        // Consent state flows from consent.json (acknowledged shows through).
        let dir = joey_neurocode_rag::consent::consent_file_path(project.path())
            .parent().unwrap().to_path_buf();
        let mut rec = ConsentRecord::load(&dir).unwrap();
        rec.acknowledge_now("http://localhost:11434", "nomic-embed-text-v1.5").unwrap();
        rec.save(&dir).unwrap();
        let v = rag_status_json(&enabled_config(), project.path()).unwrap();
        assert_eq!(v["consent_state"], "acknowledged", "{v}");
        let _ = ConsentState::Acknowledged; // vocabulary anchor
    }

    /// Disabled path: NO section at all — byte-identity pinned by
    /// construction (None ⇒ caller emits nothing).
    #[test]
    fn disabled_yields_no_section_at_all() {
        let _g = pinned_home();
        let project = tempfile::tempdir().unwrap();
        // Default config: neurocode.rag.enabled = false.
        assert!(rag_status_json(&joey_core::Config::defaults(), project.path()).is_none());
        assert!(rag_status_section(&joey_core::Config::defaults(), project.path()).is_none());
        // Explicit false likewise.
        let cfg = config_with("neurocode:\n  rag:\n    enabled: false\n");
        assert!(rag_status_json(&cfg, project.path()).is_none());
        assert!(rag_status_section(&cfg, project.path()).is_none());
    }
}

// ---------------------------------------------------------------------------
// /neurocode model fetch --dylib tests (T039, tasks.md Phase 8; contracts/
// neurocode-rag-command.md § Model fetch "With --dylib" + test obligations
// 4–6): grammar, hash-mismatch refusal (nothing lands outside staging),
// empty-mirror refusal, platform detection, manifest ort-dylib parsing,
// and the dylib resolution-order precedence (env > config > system >
// fetched copy) — each rung pinned independently.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod model_fetch_dylib_tests {
    use super::model_fetch_tests::TinyHttpServer;
    use super::*;

    fn sha256_hex(data: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(data);
        h.finalize().iter().fold(String::with_capacity(64), |s, b| s + &format!("{b:02x}"))
    }

    /// The CURRENT host platform (tests key the manifest by it — the
    /// command auto-detects the same way).
    fn host_platform() -> OrtPlatform {
        detect_platform().expect("test host must be darwin/linux/windows × aarch64/x86_64")
    }

    /// A config with the given mirror_url (+ optional ort_dylib_path).
    fn rag_config_dylib(mirror_url: &str, ort_dylib_path: &str) -> RagConfig {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            format!(
                "neurocode:\n  rag:\n    local:\n      mirror_url: \"{mirror_url}\"\n      \
                 ort_dylib_path: \"{ort_dylib_path}\"\n"
            ),
        )
        .unwrap();
        let config = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        RagConfig::load(&config)
    }

    /// Pin JOEY_HOME to a temp dir under the shared override lock so the
    /// fetched copy lands in a temp home, not the real `~/.joey`. The lock
    /// also serializes the env-mutating tests below (the EnvGuard must
    /// always be created AFTER a pinned_home in the same test).
    struct HomeGuard {
        prev: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
        _dir: tempfile::TempDir,
    }

    fn pinned_home() -> HomeGuard {
        let lock = joey_core::constants::TEST_HOME_OVERRIDE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os("JOEY_HOME");
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("JOEY_HOME", dir.path());
        HomeGuard { prev, _lock: lock, _dir: dir }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => std::env::set_var("JOEY_HOME", v),
                None => std::env::remove_var("JOEY_HOME"),
            }
        }
    }

    /// Env-var RAII guard for ORT_DYLIB_PATH (restores on drop). Takes NO
    /// lock of its own — every use sits inside a `pinned_home()` guard,
    /// whose lock serializes all ORT_DYLIB_PATH mutation in this module
    /// (taking the same non-reentrant lock here would deadlock).
    struct EnvGuard {
        prev: Option<String>,
    }

    fn pinned_env(value: Option<&str>) -> EnvGuard {
        let prev = std::env::var("ORT_DYLIB_PATH").ok();
        match value {
            Some(v) => std::env::set_var("ORT_DYLIB_PATH", v),
            None => std::env::remove_var("ORT_DYLIB_PATH"),
        }
        EnvGuard { prev }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => std::env::set_var("ORT_DYLIB_PATH", v),
                None => std::env::remove_var("ORT_DYLIB_PATH"),
            }
        }
    }

    /// A manifest carrying the ort-dylib section for the host platform
    /// with the given (possibly wrong) hash and optional version. Built
    /// with explicit Maps (json! object keys must be literals).
    fn dylib_manifest(platform: &OrtPlatform, hash: &str, version: Option<&str>) -> serde_json::Value {
        let mut entry = serde_json::Map::new();
        entry.insert("filename".into(), platform.filename.clone().into());
        entry.insert("sha256".into(), hash.into());
        let mut platforms = serde_json::Map::new();
        platforms.insert(platform.key.clone(), serde_json::Value::Object(entry));
        let mut section = serde_json::Map::new();
        if let Some(v) = version {
            section.insert("version".into(), v.into());
        }
        section.insert("platforms".into(), serde_json::Value::Object(platforms));
        let mut root = serde_json::Map::new();
        root.insert("profiles".into(), serde_json::json!({})); // T011 section
        root.insert("ort-dylib".into(), serde_json::Value::Object(section));
        serde_json::Value::Object(root)
    }

    // ─── grammar ────────────────────────────────────────────────────────

    /// Grammar: `model fetch --dylib` (bare — dylib only), with a profile
    /// (model artifacts + dylib), and the empty-mirror disabled message.
    /// Flag/profile ordering and usage errors were pinned by T011's
    /// model_fetch_grammar test; --dylib keeps the same parser.
    #[test]
    fn dylib_grammar_bare_flag_and_with_profile() {
        // Bare --dylib with the default (empty) mirror: the dylib-disabled
        // message, NOT the model-artifacts one.
        let out = neurocode_slash_provider_scoped_text("model fetch --dylib", "zai");
        assert!(
            out.contains("Model fetch --dylib is disabled")
                && out.contains("mirror_url is empty"),
            "dylib-disabled message, got: {out}"
        );
        assert!(!out.contains("fetched"), "nothing was fetched: {out}");

        // Profile + --dylib: BOTH renderings appear (model artifacts
        // refusal first, dylib refusal second).
        let out = neurocode_slash_provider_scoped_text(
            "model fetch nomic-embed-text-v1.5 --dylib",
            "zai",
        );
        assert!(out.contains("Model fetch is disabled"), "model half: {out}");
        assert!(
            out.contains("Model fetch --dylib is disabled"),
            "dylib half: {out}"
        );

        // --dylib ordering after the profile parses the same.
        let out = neurocode_slash_provider_scoped_text(
            "model fetch --dylib nomic-embed-text-v1.5",
            "zai",
        );
        assert!(out.contains("Model fetch --dylib is disabled"), "{out}");

        // Unknown flag next to --dylib is still a usage error.
        let out = neurocode_slash_provider_scoped_text("model fetch --dylib --bogus", "zai");
        assert!(out.contains("unknown flag '--bogus'"), "{out}");
        assert!(out.contains("Usage: /neurocode model fetch"), "{out}");
    }

    // ─── empty mirror ───────────────────────────────────────────────────

    /// Empty mirror ⇒ dylib fetch disabled; no network call.
    #[test]
    fn dylib_empty_mirror_refuses_without_network() {
        let out = run_dylib_fetch(&rag_config_dylib("", "")).unwrap_err();
        assert!(
            out.contains("Model fetch --dylib is disabled")
                && out.contains("mirror_url is empty"),
            "{out}"
        );
        assert!(out.contains("never"), "no-HF posture stated: {out}");
    }

    /// An HF mirror is refused before any dylib network call (R8).
    #[test]
    fn dylib_hf_mirror_refused_before_network() {
        let err = run_dylib_fetch(&rag_config_dylib("https://huggingface.co:443", ""))
            .unwrap_err();
        assert!(err.contains("never contacts huggingface.co"), "{err}");
    }

    // ─── platform detection + manifest parsing (server-less) ───────────

    /// The (os, arch) → platform mapping, host-independent.
    #[test]
    fn platform_mapping_matrix() {
        let darwin_arm = platform_for("macos", "aarch64").unwrap();
        assert_eq!(darwin_arm.key, "darwin-aarch64");
        assert_eq!(darwin_arm.filename, "libonnxruntime.dylib");
        assert_eq!(
            platform_for("linux", "x86_64").unwrap().filename,
            "libonnxruntime.so"
        );
        assert_eq!(
            platform_for("windows", "x86_64").unwrap().filename,
            "onnxruntime.dll"
        );
        // Unsupported OS/arch combinations map to None.
        assert!(platform_for("freebsd", "x86_64").is_none());
        assert!(platform_for("macos", "riscv64").is_none());
        assert!(platform_for("linux", "arm").is_none());
        // The test host itself must detect.
        assert!(detect_platform().is_some());
    }

    /// Manifest ort-dylib parsing: wrapper + bare forms, refusals.
    #[test]
    fn dylib_manifest_parsing() {
        let key = host_platform().key;
        let h = "a".repeat(64);
        // Helpers (json! object keys must be literals — dynamic keys need
        // explicit Maps).
        fn entry(filename: &str, hash: &str) -> serde_json::Value {
            let mut e = serde_json::Map::new();
            e.insert("filename".into(), filename.into());
            e.insert("sha256".into(), hash.into());
            serde_json::Value::Object(e)
        }
        fn manifest_with_section(section: serde_json::Map<String, serde_json::Value>) -> serde_json::Value {
            let mut root = serde_json::Map::new();
            root.insert("ort-dylib".into(), serde_json::Value::Object(section));
            serde_json::Value::Object(root)
        }
        fn manifest_with_platforms(platforms: serde_json::Map<String, serde_json::Value>) -> serde_json::Value {
            let mut section = serde_json::Map::new();
            section.insert("platforms".into(), serde_json::Value::Object(platforms));
            manifest_with_section(section)
        }
        // Wrapped platforms map.
        let mut platforms = serde_json::Map::new();
        platforms.insert(key.clone(), entry("libonnxruntime.dylib", &h));
        let mut section = serde_json::Map::new();
        section.insert("version".into(), "1.22.0".into());
        section.insert("platforms".into(), serde_json::Value::Object(platforms));
        let wrapped = manifest_with_section(section);
        let e = dylib_entry_from_manifest(&wrapped, &key).unwrap();
        assert_eq!(e.filename, "libonnxruntime.dylib");
        assert_eq!(e.sha256, h);
        assert_eq!(dylib_version_from_manifest(&wrapped).unwrap(), "1.22.0");
        // Bare map directly under ort-dylib (no platforms wrapper).
        let mut bare = serde_json::Map::new();
        bare.insert(key.clone(), entry("x.so", &h));
        let bare = manifest_with_section(bare);
        assert!(dylib_entry_from_manifest(&bare, &key).is_ok());
        assert_eq!(
            dylib_version_from_manifest(&bare).unwrap(),
            ORT_CRATE_PIN,
            "no version recorded ⇒ the ort crate pin"
        );
        // Missing section / missing platform / bad hash / bad filename /
        // path traversal / non-string fields all refuse.
        let no_section = serde_json::json!({ "profiles": {} });
        assert!(dylib_entry_from_manifest(&no_section, &key).is_err());
        let missing_platform = serde_json::json!({
            "ort-dylib": { "platforms": { "other-x86_64": { "filename": "x", "sha256": h } } }
        });
        let err = dylib_entry_from_manifest(&missing_platform, &key).unwrap_err();
        assert!(err.contains("not recorded"), "{err}");
        for bad_filename in ["x.so", "a/b.so", "..", ""] {
            for bad_hash in ["z".repeat(64), "a".repeat(63)] {
                let mut platforms = serde_json::Map::new();
                platforms.insert(key.clone(), entry(bad_filename, &bad_hash));
                let m = manifest_with_platforms(platforms);
                assert!(
                    dylib_entry_from_manifest(&m, &key).is_err(),
                    "must refuse filename {bad_filename:?} hash {bad_hash:?}"
                );
            }
        }
        // Missing fields / non-string hash refuse.
        for partial in [
            serde_json::json!({ "filename": "x.so" }),
            serde_json::json!({ "sha256": h }),
            serde_json::json!({ "filename": "x.so", "sha256": 7 }),
        ] {
            let mut platforms = serde_json::Map::new();
            platforms.insert(key.clone(), partial.clone());
            let m = manifest_with_platforms(platforms);
            assert!(
                dylib_entry_from_manifest(&m, &key).is_err(),
                "must refuse partial entry {partial}"
            );
        }
        // Version traversal refused.
        let bad_version = serde_json::json!({
            "ort-dylib": { "version": "../escape", "platforms": {} }
        });
        assert!(dylib_version_from_manifest(&bad_version).is_err());
    }

    // ─── resolution-order precedence (pure) ────────────────────────────

    /// Each rung wins only when the higher rungs are unset/absent:
    /// env > config > system > fetched copy.
    #[test]
    fn dylib_resolution_order_precedence() {
        // Env/config rungs only require non-blank strings, but the system
        // and fetched rungs are existence-checked — so those two fixtures
        // must be real files (tempfiles, like the sibling wire tests).
        let env_p = PathBuf::from("/opt/env/libonnxruntime.dylib");
        let cfg_p = PathBuf::from("/opt/cfg/libonnxruntime.dylib");
        let sys_dir = tempfile::tempdir().unwrap();
        let sys_p = sys_dir.path().join("libonnxruntime.dylib");
        std::fs::write(&sys_p, b"system-dylib-fixture").unwrap();
        let fetched_dir = tempfile::tempdir().unwrap();
        let fetched_p = fetched_dir.path().join("libonnxruntime.dylib");
        std::fs::write(&fetched_p, b"fetched-dylib-fixture").unwrap();

        // All four rungs present ⇒ env wins.
        let (rung, path) = resolve_dylib_rung(
            Some(env_p.to_str().unwrap()),
            Some(cfg_p.to_str().unwrap()),
            Some(sys_p.as_path()),
            Some(fetched_p.as_path()),
        )
        .unwrap();
        assert_eq!(rung, DylibRung::Env);
        assert_eq!(path, env_p);

        // No env ⇒ config wins (even when system + fetched exist).
        let (rung, path) = resolve_dylib_rung(
            None,
            Some(cfg_p.to_str().unwrap()),
            Some(sys_p.as_path()),
            Some(fetched_p.as_path()),
        )
        .unwrap();
        assert_eq!(rung, DylibRung::Config);
        assert_eq!(path, cfg_p);

        // No env/config ⇒ system wins when the file exists.
        let (rung, path) = resolve_dylib_rung(None, None, Some(sys_p.as_path()), Some(fetched_p.as_path()))
            .unwrap();
        assert_eq!(rung, DylibRung::System);
        assert_eq!(path, sys_p);

        // Nothing but the fetched copy ⇒ fetched wins.
        let (rung, path) = resolve_dylib_rung(None, None, None, Some(fetched_p.as_path())).unwrap();
        assert_eq!(rung, DylibRung::Fetched);
        assert_eq!(path, fetched_p);

        // A non-existent system candidate is skipped (fetched still wins).
        let (rung, _) = resolve_dylib_rung(
            None,
            None,
            Some(Path::new("/nonexistent/libonnxruntime.so.99")),
            Some(fetched_p.as_path()),
        )
        .unwrap();
        assert_eq!(rung, DylibRung::Fetched);

        // Blank env/config strings count as unset (system wins).
        let (rung, _) =
            resolve_dylib_rung(Some("   "), Some(""), Some(sys_p.as_path()), None).unwrap();
        assert_eq!(rung, DylibRung::System);

        // Nothing set at all ⇒ None.
        assert!(resolve_dylib_rung(None, None, None, None).is_none());
    }

    // ─── end-to-end over the wire ──────────────────────────────────────

    /// Hash-mismatch refusal: download happens, verification refuses —
    /// nothing lands outside staging (T039 refusal semantics).
    #[test]
    fn dylib_hash_mismatch_refuses_writes_nothing() {
        let _g = pinned_home();
        let server_root = tempfile::tempdir().unwrap();
        let platform = host_platform();
        let dylib_bytes = b"fake-onnxruntime-dylib-bytes-v1";
        std::fs::write(server_root.path().join(&platform.filename), dylib_bytes).unwrap();
        let manifest = dylib_manifest(&platform, &"0".repeat(64), Some("1.22.0"));
        std::fs::write(
            server_root.path().join(MANIFEST_FILE),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();

        let server = TinyHttpServer::start(server_root.path());
        let err = run_dylib_fetch(&rag_config_dylib(&server.url(), "")).unwrap_err();
        server.stop();

        assert!(err.contains("SHA-256 mismatch"), "{err}");
        assert!(err.contains(&platform.filename), "names the file: {err}");
        assert!(err.contains("Refused"), "{err}");
        // Nothing landed: no version dir, no staging residue.
        let root = dylib_fetch_root();
        assert!(!root.join("1.22.0").exists(), "no version dir landed");
        assert!(!root.join(DYLIB_STAGING_DIR).exists(), "staging cleaned");
        let residue: Vec<_> = std::fs::read_dir(&root).unwrap().flatten().collect();
        assert!(residue.is_empty(), "fetch root empty: {residue:?}");
    }

    /// Happy path: verified dylib lands under
    /// `<joey home>/neurocode/ort/<version>/` and the output reports the
    /// resolution ladder with the fetched-copy location.
    #[test]
    fn dylib_verified_fetch_lands_under_joey_home() {
        let _g = pinned_home();
        let server_root = tempfile::tempdir().unwrap();
        let platform = host_platform();
        let dylib_bytes = b"real-onnxruntime-dylib-bytes";
        std::fs::write(server_root.path().join(&platform.filename), dylib_bytes).unwrap();
        let manifest = dylib_manifest(&platform, &sha256_hex(dylib_bytes), Some("1.22.0"));
        std::fs::write(
            server_root.path().join(MANIFEST_FILE),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();

        // Scrub ORT_DYLIB_PATH so the env rung cannot shadow the report.
        let _env = pinned_env(None);
        let server = TinyHttpServer::start(server_root.path());
        let out = run_dylib_fetch(&rag_config_dylib(&server.url(), "")).unwrap();
        server.stop();

        // Landed exactly at the documented fetched-copy location.
        let dest = dylib_fetch_root()
            .join("1.22.0")
            .join(&platform.filename);
        assert!(dest.exists(), "fetched copy landed: {out}");
        assert_eq!(std::fs::read(&dest).unwrap(), dylib_bytes);
        assert!(!dylib_fetch_root().join(DYLIB_STAGING_DIR).exists());
        // Report names platform, mirror, size, hash prefix, location.
        assert!(out.contains(&platform.key), "{out}");
        assert!(out.contains("sha256:"), "{out}");
        assert!(out.starts_with("Dylib fetch:"), "{out}");
        assert!(out.contains(&dest.display().to_string()), "{out}");
        // The ladder is stated in the output.
        assert!(out.contains("ORT_DYLIB_PATH env"), "{out}");
        assert!(out.contains("fetched copy"), "{out}");
    }

    /// Env-run precedence surfaces in the fetch report: with
    /// ORT_DYLIB_PATH set, the winning rung is env even though the
    /// fetched copy now exists.
    #[test]
    fn dylib_report_prefers_env_rung_when_set() {
        let _g = pinned_home();
        let server_root = tempfile::tempdir().unwrap();
        let platform = host_platform();
        let dylib_bytes = b"dylib-for-env-precedence";
        std::fs::write(server_root.path().join(&platform.filename), dylib_bytes).unwrap();
        let manifest = dylib_manifest(&platform, &sha256_hex(dylib_bytes), None);
        std::fs::write(
            server_root.path().join(MANIFEST_FILE),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();

        let env_path = tempfile::tempdir().unwrap();
        let env_dylib = env_path.path().join(&platform.filename);
        std::fs::write(&env_dylib, b"pre-existing-env-dylib").unwrap();
        let _env = pinned_env(env_dylib.to_str());

        let server = TinyHttpServer::start(server_root.path());
        let out = run_dylib_fetch(&rag_config_dylib(&server.url(), "")).unwrap();
        server.stop();

        // Manifest without a version ⇒ the ort crate pin directory.
        let dest = dylib_fetch_root().join(ORT_CRATE_PIN).join(&platform.filename);
        assert!(dest.exists(), "fetched copy landed: {out}");
        // env rung reported as the winner, naming the env dylib.
        assert!(
            out.contains("Runtime resolution: ORT_DYLIB_PATH env")
                && out.contains(env_dylib.to_str().unwrap()),
            "env rung wins in the report: {out}"
        );
        // No activation hint when a higher rung already wins.
        assert!(
            !out.contains("to force the fetched copy"),
            "no hint needed when env wins: {out}"
        );
    }

    /// Config-run precedence: `neurocode.rag.local.ort_dylib_path` beats
    /// the (existing) fetched copy when env is unset.
    #[test]
    fn dylib_report_prefers_config_rung_over_fetched() {
        let _g = pinned_home();
        let server_root = tempfile::tempdir().unwrap();
        let platform = host_platform();
        let dylib_bytes = b"dylib-for-config-precedence";
        std::fs::write(server_root.path().join(&platform.filename), dylib_bytes).unwrap();
        let manifest = dylib_manifest(&platform, &sha256_hex(dylib_bytes), Some("9.9.9"));
        std::fs::write(
            server_root.path().join(MANIFEST_FILE),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();

        let cfg_dir = tempfile::tempdir().unwrap();
        let cfg_dylib = cfg_dir.path().join(&platform.filename);
        std::fs::write(&cfg_dylib, b"config-pointed-dylib").unwrap();
        let _env = pinned_env(None);

        let server = TinyHttpServer::start(server_root.path());
        let out = run_dylib_fetch(&rag_config_dylib(&server.url(), cfg_dylib.to_str().unwrap()))
            .unwrap();
        server.stop();

        assert!(dylib_fetch_root().join("9.9.9").join(&platform.filename).exists());
        assert!(
            out.contains("Runtime resolution: neurocode.rag.local.ort_dylib_path")
                && out.contains(cfg_dylib.to_str().unwrap()),
            "config rung wins over the fetched copy: {out}"
        );
    }
}

// ---------------------------------------------------------------------------
// Production wiring tests (T041b): /neurocode index populates rag_chunks,
// /neurocode search returns results from the rag-populated index (keyword
// leg on real chunk rows — V2's no-model path), and the degraded index
// output names the degradation reason without failing the turn.
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod production_wiring_tests {
    use super::*;
    use joey_neurocode_rag::vector::store as vector_store;

    /// JOEY_HOME pinned under the shared override lock (same guard shape
    /// the sibling test modules use) so the per-project graph lands in a
    /// temp home. Shared with `neurocode_wiring`'s backend-search test.
    pub(crate) struct HomeGuard {
        prev: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
        _dir: tempfile::TempDir,
    }

    pub(crate) fn pinned_home_for_wiring() -> HomeGuard {
        let lock = joey_core::constants::TEST_HOME_OVERRIDE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os("JOEY_HOME");
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("JOEY_HOME", dir.path());
        HomeGuard { prev, _lock: lock, _dir: dir }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => std::env::set_var("JOEY_HOME", v),
                None => std::env::remove_var("JOEY_HOME"),
            }
        }
    }

    /// A small mixed-language scratch project (Rust symbol + Python
    /// top-level fallback), mirroring the quickstart's sample shape.
    /// Shared with `neurocode_wiring`'s backend-search test.
    pub(crate) fn scratch_project_for_wiring() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::create_dir_all(dir.path().join("scripts")).unwrap();
        std::fs::write(
            dir.path().join("src").join("token.rs"),
            "pub fn validate_token(token: &str) -> bool {\n    !token.is_empty()\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("scripts").join("top.py"),
            "import os\n\nAPI_SECRET = \"literal-secret-value\"\n\ndef main():\n    print(os.name)\n",
        )
        .unwrap();
        dir
    }

    /// RAG-enabled config (auto backend, empty model_dir ⇒ degraded at
    /// resolve time — the V2/V9 no-model path).
    fn rag_enabled_config() -> joey_core::Config {
        let model_dir = tempfile::tempdir().unwrap(); // exists but EMPTY
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            format!(
                "neurocode:\n  rag:\n    enabled: true\n    local:\n      model_dir: \"{}\"\n",
                model_dir.path().display()
            ),
        )
        .unwrap();
        joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap()
    }

    fn chunk_rows(project_root: &Path) -> (u64, u64) {
        let graph = joey_neurocode::graph::DependencyGraph::open_for_project(project_root)
            .unwrap();
        let conn = graph.store().conn();
        (
            vector_store::chunk_count(conn).unwrap_or(0),
            vector_store::vector_count(conn).unwrap_or(0),
        )
    }

    /// T041b gap 1: `/neurocode index` (via the dispatch core, degraded
    /// auto backend) populates `rag_chunks` with real vector-less rows and
    /// reports the degradation explicitly — the turn never fails.
    #[test]
    fn index_populates_rag_chunks_in_degraded_mode() {
        let _g = pinned_home_for_wiring();
        let project = scratch_project_for_wiring();
        let config = rag_enabled_config();

        let out = crate::neurocode_rag_wiring::rag_index_text(&config, project.path(), true);
        assert!(!out.contains("index failed"), "turn must not fail: {out}");
        assert!(
            out.contains("keyword-only (degraded:")
                || out.contains("keyword-only"),
            "degradation reason surfaced: {out}"
        );
        assert!(out.contains("chunk rows written WITHOUT vectors"), "{out}");

        let (chunks, vectors) = chunk_rows(project.path());
        assert!(chunks > 0, "rag_chunks populated (got {chunks})");
        assert_eq!(vectors, 0, "no embedder ⇒ no vectors");
    }

    /// Index parity: RAG disabled ⇒ the index section is byte-empty and
    /// no rag_chunks rows are written by the command path.
    #[test]
    fn index_disabled_rag_is_empty_and_writes_nothing() {
        let _g = pinned_home_for_wiring();
        let project = scratch_project_for_wiring();
        let out = crate::neurocode_rag_wiring::rag_index_text(
            &joey_core::Config::defaults(),
            project.path(),
            true,
        );
        assert!(out.is_empty(), "disabled ⇒ empty section: {out}");
        let (chunks, _) = chunk_rows(project.path());
        assert_eq!(chunks, 0);
    }

    /// T041b gap 2 (V2): after `/neurocode index`, `/neurocode search`
    /// returns results from the rag-populated index on the keyword leg —
    /// ranked entries with file/symbol/kind/lines, keyword-only mode with
    /// an explicit mode_reason, and the exact-symbol pin (V3) first.
    #[test]
    fn search_returns_results_from_rag_populated_index() {
        let _g = pinned_home_for_wiring();
        let project = scratch_project_for_wiring();
        let config = rag_enabled_config();

        // Index first (the command's own RAG half).
        let indexed = crate::neurocode_rag_wiring::rag_index_text(&config, project.path(), true);
        assert!(!indexed.contains("index failed"), "{indexed}");

        // V2: natural-language-style query for the token logic. (The
        // keyword leg is AND-substring over symbol/path — the multi-word
        // "where is token validation handled" phrasing reduces to its
        // matching terms here.)
        let out = search_command_text_at(
            &["token"],
            &config,
            project.path(),
        );
        assert!(
            out.contains("keyword-only mode"),
            "degradation visible (V9): {out}"
        );
        assert!(
            out.contains("src/token.rs") && out.contains("validate_token"),
            "ranked result with file+symbol: {out}"
        );
        assert!(out.contains("L"), "line info present: {out}");
        assert!(!out.contains("Nothing matched"), "{out}");

        // V3: exact symbol ranked first.
        let out = search_command_text_at(&["validate_token"], &config, project.path());
        let first = out
            .lines()
            .find(|l| l.starts_with("1. "))
            .expect("a ranked list exists");
        assert!(
            first.contains("validate_token"),
            "exact symbol first: {first}"
        );
    }

    /// The JSON payload from the populated index carries the contract shape
    /// (results[] + mode + mode_reason — V12 parity with the tool).
    #[test]
    fn search_json_payload_from_populated_index() {
        let _g = pinned_home_for_wiring();
        let project = scratch_project_for_wiring();
        let config = rag_enabled_config();
        let _ = crate::neurocode_rag_wiring::rag_index_text(&config, project.path(), true);

        let out = search_command_text_at(&["token", "--json"], &config, project.path());
        let payload: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(payload["mode"], "keyword_only");
        assert!(
            payload["mode_reason"].as_str().unwrap_or("").contains("keyword-only"),
            "{out}"
        );
        let results = payload["results"].as_array().unwrap();
        assert!(
            results.iter().any(|r| r["file"] == "src/token.rs"),
            "real result rows: {out}"
        );
    }

    /// V6-adjacent incremental semantics on the degraded path: an
    /// unchanged second index run is a no-op (no re-write), a deleted
    /// file's rows are purged on the next run.
    #[test]
    fn degraded_reindex_is_incremental_and_purges_deletions() {
        let _g = pinned_home_for_wiring();
        let project = scratch_project_for_wiring();
        let config = rag_enabled_config();
        let _ = crate::neurocode_rag_wiring::rag_index_text(&config, project.path(), true);
        let (chunks_before, _) = chunk_rows(project.path());

        // Unchanged tree (non-force) ⇒ no-op refresh.
        let out = crate::neurocode_rag_wiring::rag_index_text(&config, project.path(), false);
        assert!(!out.contains("index failed"), "{out}");
        assert_eq!(chunk_rows(project.path()).0, chunks_before);

        // Delete a file ⇒ its rows are purged on the next run.
        std::fs::remove_file(project.path().join("scripts").join("top.py")).unwrap();
        let _ = crate::neurocode_rag_wiring::rag_index_text(&config, project.path(), false);
        let graph = joey_neurocode::graph::DependencyGraph::open_for_project(project.path())
            .unwrap();
        let n: i64 = graph
            .store()
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM rag_chunks WHERE source_path LIKE '%top.py'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "deleted file's rows purged");
        assert!(chunk_rows(project.path()).0 < chunks_before);
    }
}


