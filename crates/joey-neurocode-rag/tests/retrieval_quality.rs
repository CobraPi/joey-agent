//! T040 — automated retrieval-quality benchmark harness (SC-001).
//!
//! Synthetic multi-file corpus + ≥30 natural-language queries whose
//! wording does NOT literally appear in the code, asserting a
//! correct-location-in-top-5 hit rate ≥ 80% (SC-001) through the REAL
//! pipeline: parse → chunk (symbols + fallback) → LocalOnnx embedder →
//! `search_cli` hybrid search.
//!
//! SKIP POLICY (research.md R8 — no Hugging Face, no downloads, ever):
//! the measurement test is `#[ignore]`d with a clear reason and, when run,
//! AUTO-SKIPS (prints the documented reason, returns Ok) when no verified
//! local model artifacts are present. It NEVER downloads and never
//! contacts huggingface.co. Opt-in run:
//!
//! ```text
//! cargo test -p joey-neurocode-rag --test retrieval_quality -- --ignored --nocapture
//! ```
//!
//! The companion `skip_logic_returns_cleanly_when_artifacts_absent` test
//! ALWAYS runs (not ignored): it pins the skip path itself — absent
//! artifacts ⇒ the filesystem-only probe fails cleanly with the
//! documented reason and the auto-resolution degrades to keyword-only,
//! with no network anywhere in the path.
//!
//! Corpus/query design: natural-language descriptions of functionality
//! vs the code implementing it (e.g. "where is retry backoff computed" →
//! `calculate_backoff_with_jitter`), phrased so the query string does not
//! literally occur in any source file. The corpus spans symbol chunks
//! (functions/classes), module-level fallback regions, and a symbol-free
//! bootstrap script covered only by fallback chunks (FR-014).

use std::path::{Path, PathBuf};

use joey_neurocode::graph::{ArtifactKind, CodeArtifactNode, GraphStore};
use joey_neurocode::parse::registry::parse_any;

use joey_neurocode_rag::embed::artifacts::{compute_hashes, ArtifactError};
use joey_neurocode_rag::embed::local_onnx::{LocalOnnx, LocalOnnxSettings};
use joey_neurocode_rag::embed::profiles::{default_profile, EmbedProfile};
use joey_neurocode_rag::embed::{resolve_kind, BackendKind};
use joey_neurocode_rag::index::chunker::{index_file, ChunkEmbedder, ChunkOptions};
use joey_neurocode_rag::search::hybrid::{search_cli_with_embedder, SearchMode, SearchRequest};
use joey_neurocode_rag::vector::quantize::Quantization;

/// Explicit opt-in artifact directory override (same knob the T037 bench
/// honors). Never required, never fetched from.
const ENV_MODEL_DIR: &str = "JOEY_RAG_MODEL_DIR";

/// The documented auto-skip reason (pinned by the always-run companion).
pub const SKIP_REASON_PREFIX: &str =
    "SKIP (T040 retrieval_quality): no verified local model artifacts";

/// SC-001 acceptance threshold.
const REQUIRED_HIT_RATE: f64 = 0.80;

/// Filesystem-only artifact probe — the exact gate the ignored test uses.
fn artifacts_available() -> Option<PathBuf> {
    let dir = std::env::var(ENV_MODEL_DIR)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            joey_neurocode_rag::config::default_model_dir(default_profile().name)
        });
    compute_hashes(&dir).ok().map(|_| dir)
}

// ---------------------------------------------------------------------------
// Synthetic corpus (wording deliberately divergent from the queries)
// ---------------------------------------------------------------------------

/// (relative path, source) pairs — a miniature but realistic codebase.
fn corpus() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "src/net_retry.py",
            r#"
import math
import random


def calculate_backoff_with_jitter(attempt, base_ms, cap_ms, seed):
    exponent = min(attempt - 1, 16)
    delay = base_ms * (2 ** exponent)
    delay = min(delay, cap_ms)
    jitter = random.Random(seed).uniform(0.0, delay * 0.25)
    return int(delay + jitter)


class RetryPolicy:
    def __init__(self, max_attempts, base_ms, cap_ms):
        self.max_attempts = max_attempts
        self.base_ms = base_ms
        self.cap_ms = cap_ms

    def should_try_again(self, attempt):
        return attempt < self.max_attempts


def is_retryable_status(code):
    if code in (408, 429, 500, 502, 503, 504):
        return True
    return 520 <= code <= 599
"#,
        ),
        (
            "src/store_vectors.py",
            r#"
import json
import struct


def persist_embeddings(conn, rows):
    cur = conn.cursor()
    for chunk_id, dim, blob in rows:
        cur.execute(
            "INSERT OR REPLACE INTO rag_vectors (chunk_id, dim, vector) VALUES (?, ?, ?)",
            (chunk_id, dim, blob),
        )
    conn.commit()


def fetch_vector_blob(conn, chunk_id):
    cur = conn.cursor()
    cur.execute("SELECT vector FROM rag_vectors WHERE chunk_id = ?", (chunk_id,))
    row = cur.fetchone()
    return bytes(row[0]) if row else None


def cosine_similarity(a, b):
    dot = sum(x * y for x, y in zip(a, b))
    na = math_sqrt(a)
    nb = math_sqrt(b)
    if na == 0.0 or nb == 0.0:
        return 0.0
    return dot / (na * nb)


def math_sqrt(v):
    return sum(x * x for x in v) ** 0.5


def quantize_int8(values):
    peak = max(abs(x) for x in values) or 1.0
    scale = 127.0 / peak
    return bytes(max(-128, min(127, int(x * scale))) for x in values)
"#,
        ),
        (
            "src/crypto_hashes.py",
            r#"
import hashlib
import hmac


def canonical_payload(parts):
    out = []
    for p in parts:
        if isinstance(p, str):
            p = p.encode("utf-8")
        out.append(len(p).to_bytes(4, "big") + p)
    return b"".join(out)


def canonical_sha256(parts):
    blob = canonical_payload(parts)
    return hashlib.sha256(blob).hexdigest()


def hmac_sign(key, msg):
    if isinstance(key, str):
        key = key.encode("utf-8")
    return hmac.new(key, msg, hashlib.sha256).digest()


def constant_time_equal(a, b):
    if len(a) != len(b):
        return False
    acc = 0
    for x, y in zip(a, b):
        acc |= x ^ y
    return acc == 0
"#,
        ),
        (
            "src/sched_ticker.py",
            r#"
import time


class Ticker:
    def __init__(self, interval_ms):
        self.interval_ms = interval_ms
        self.last_fire = None

    def next_fire_time(self, now=None):
        now = now if now is not None else time.time()
        step = self.interval_ms / 1000.0
        if self.last_fire is None:
            return now
        elapsed = now - self.last_fire
        periods = int(elapsed / step) + 1
        return self.last_fire + periods * step

    def due(self, now=None):
        t = self.next_fire_time(now)
        return (now if now is not None else time.time()) >= t


def align_to_boundary(ts, interval_ms):
    step = interval_ms / 1000.0
    return int(ts / step) * step
"#,
        ),
        (
            "src/config_loader.py",
            r#"
import yaml


def load_yaml_layered(paths):
    merged = {}
    for p in paths:
        with open(p, "r", encoding="utf-8") as fh:
            doc = yaml.safe_load(fh) or {}
        merged = deep_merge(merged, doc)
    return merged


def deep_merge(base, overlay):
    out = dict(base)
    for k, v in overlay.items():
        if k in out and isinstance(out[k], dict) and isinstance(v, dict):
            out[k] = deep_merge(out[k], v)
        else:
            out[k] = v
    return out


def coerce_bool(raw):
    if isinstance(raw, bool):
        return raw
    s = str(raw).strip().lower()
    if s in ("yes", "on", "true", "1"):
        return True
    if s in ("no", "off", "false", "0"):
        return False
    raise ValueError("not a boolean: %r" % (raw,))
"#,
        ),
        (
            "src/authz_roles.py",
            r#"
class Role:
    def __init__(self, name, permissions):
        self.name = name
        self.permissions = set(permissions)

    def grants(self, action):
        return action in expand_wildcards(self.permissions)


def expand_wildcards(permissions):
    out = set()
    for p in permissions:
        if p.endswith(":*"):
            prefix = p[:-1]
            out.update(q for q in KNOWN_ACTIONS if q.startswith(prefix))
        else:
            out.add(p)
    return out


def check_permission(role, action):
    if role.grants(action):
        return "allow"
    if action.startswith("admin."):
        return "deny"
    return "deny"


KNOWN_ACTIONS = [
    "repo.read", "repo.write", "repo.delete",
    "issue.read", "issue.write",
    "admin.settings",
]
"#,
        ),
        (
            "src/cache_lru.py",
            r#"
from collections import OrderedDict


class LruCache:
    def __init__(self, capacity):
        self.capacity = capacity
        self.entries = OrderedDict()

    def get(self, key):
        if key not in self.entries:
            return None
        self.entries.move_to_end(key)
        return self.entries[key]

    def put(self, key, value):
        if key in self.entries:
            self.entries.move_to_end(key)
        self.entries[key] = value
        if len(self.entries) > self.capacity:
            self.evict_oldest()

    def evict_oldest(self):
        self.entries.popitem(last=False)
"#,
        ),
        (
            "src/render_tables.py",
            r#"
def render_markdown_table(headers, rows):
    widths = [len(h) for h in headers]
    for row in rows:
        for i, cell in enumerate(row):
            widths[i] = max(widths[i], len(str(cell)))
    lines = []
    lines.append("| " + " | ".join(pad_cell(h, widths[i]) for i, h in enumerate(headers)) + " |")
    lines.append("|" + "|".join("-" * (w + 2) for w in widths) + "|")
    for row in rows:
        cells = [pad_cell(escape_pipe(str(c)), widths[i]) for i, c in enumerate(row)]
        lines.append("| " + " | ".join(cells) + " |")
    return "\n".join(lines)


def pad_cell(cell, width):
    return cell.ljust(width)


def escape_pipe(text):
    return text.replace("|", "\\|")
"#,
        ),
        (
            "src/fs_watcher.py",
            r#"
import os
import time


def watch_directory(root, callback, poll_s=0.5):
    snapshot = scan_tree(root)
    while True:
        time.sleep(poll_s)
        current = scan_tree(root)
        events = diff_snapshots(snapshot, current)
        snapshot = current
        if events:
            callback(debounce(events, window_ms=300))


def scan_tree(root):
    out = {}
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if not is_hidden(d)]
        for f in filenames:
            if is_hidden(f):
                continue
            p = os.path.join(dirpath, f)
            out[p] = os.stat(p).st_mtime
    return out


def diff_snapshots(old, new):
    events = []
    for p, m in new.items():
        if p not in old:
            events.append(("created", p))
        elif old[p] != m:
            events.append(("modified", p))
    for p in old:
        if p not in new:
            events.append(("removed", p))
    return events


def debounce(events, window_ms):
    by_dir = {}
    for kind, p in events:
        by_dir.setdefault(os.path.dirname(p), []).append((kind, p))
    collapsed = []
    for d, evs in by_dir.items():
        if len(evs) > 20:
            collapsed.append(("bulk", d))
        else:
            collapsed.extend(evs)
    return collapsed


def is_hidden(name):
    return name.startswith(".")
"#,
        ),
        (
            "src/queue_workers.py",
            r#"
import queue
import threading


class WorkerPool:
    def __init__(self, size, handler):
        self.q = queue.Queue()
        self.handler = handler
        self.workers = [
            threading.Thread(target=self._loop, name="worker-%d" % i, daemon=True)
            for i in range(size)
        ]
        for w in self.workers:
            w.start()

    def _loop(self):
        while True:
            item = self.q.get()
            if item is _POISON:
                self.q.task_done()
                break
            try:
                self.handler(item)
            finally:
                self.q.task_done()

    def submit(self, item):
        self.q.put(item)

    def shutdown(self):
        for _ in self.workers:
            self.q.put(_POISON)
        for w in self.workers:
            w.join()


_POISON = object()


def drain_queue(q, handler, failures=None):
    failures = failures if failures is not None else []
    while not q.empty():
        item = q.get_nowait()
        try:
            handler(item)
        except Exception as exc:  # noqa: BLE001
            failures.append((item, exc))
    return failures
"#,
        ),
        (
            "src/http_client.py",
            r#"
import json
import urllib.request


def build_request(url, headers, body=None):
    data = None
    if body is not None:
        data = json.dumps(body).encode("utf-8")
    req = urllib.request.Request(url, data=data, method="POST" if data else "GET")
    for k, v in headers.items():
        req.add_header(k, v)
    req.add_header("accept", "application/json")
    return req


def parse_response(status, body):
    try:
        doc = json.loads(body.decode("utf-8"))
    except (ValueError, UnicodeDecodeError):
        doc = {"raw": repr(body[:512])}
    if status >= 400:
        return {"ok": False, "status": status, "error": doc}
    return {"ok": True, "status": status, "body": doc}


def raise_for_status(status, url):
    if status >= 500:
        raise TransientHttpError(url, status)
    if status >= 400:
        raise HttpError(url, status)
    return None


class HttpError(Exception):
    def __init__(self, url, status):
        super().__init__("%s -> %d" % (url, status))
        self.status = status


class TransientHttpError(HttpError):
    pass
"#,
        ),
        (
            "src/metrics_counter.py",
            r#"
import math


class Counter:
    def __init__(self, name):
        self.name = name
        self.series = {}

    def record(self, labels, value=1):
        key = tuple(sorted(labels.items()))
        bucket = self.series.setdefault(key, [])
        bucket.append(value)

    def total(self, labels=None):
        if labels is None:
            return sum(sum(b) for b in self.series.values())
        key = tuple(sorted(labels.items()))
        return sum(self.series.get(key, ()))


def snapshot_percentiles(samples, ps=(0.5, 0.9, 0.99)):
    if not samples:
        return {}
    ordered = sorted(samples)
    out = {}
    for p in ps:
        idx = min(len(ordered) - 1, int(math.ceil(p * len(ordered))) - 1)
        out[percentile_key(p)] = ordered[max(idx, 0)]
    return out


def percentile_key(p):
    return "p%d" % int(p * 100)
"#,
        ),
        (
            "src/throttle_bucket.py",
            r#"
import time


class TokenBucket:
    def __init__(self, capacity, refill_per_sec):
        self.capacity = capacity
        self.tokens = float(capacity)
        self.refill_per_sec = refill_per_sec
        self.last = time.monotonic()

    def try_take(self, tokens=1):
        self._refill()
        if self.tokens >= tokens:
            self.tokens -= tokens
            return True
        return False

    def _refill(self):
        now = time.monotonic()
        elapsed = now - self.last
        self.last = now
        self.tokens = min(float(self.capacity), self.tokens + elapsed * self.refill_per_sec)
"#,
        ),
        (
            "src/temp_cleanup.py",
            r#"
import os
import shutil
import time


def purge_tempdirs(root, older_than_s, now=None):
    now = now if now is not None else time.time()
    removed = []
    for name in os.listdir(root):
        p = os.path.join(root, name)
        if not os.path.isdir(p) or not name.startswith("tmp-"):
            continue
        age = now - os.stat(p).st_mtime
        if age >= older_than_s:
            safe_remove(p)
            removed.append(p)
    return removed


def safe_remove(path):
    try:
        shutil.rmtree(path)
    except FileNotFoundError:
        pass
    except OSError:
        shutil.rmtree(path, ignore_errors=True)
"#,
        ),
        (
            "src/redact_logs.py",
            r#"
import re

PATTERNS = [
    (re.compile(r"\b(sk-)[A-Za-z0-9]{16,}\b"), r"\1***"),
    (re.compile(r"\b(\d[ /-]*\d{3}[ /-]*\d{4})\b"), "***-****"),
    (re.compile(r"(Bearer\s+)[A-Za-z0-9._-]+"), r"\1***"),
]


def redact_line(line):
    out = line
    for rx, repl in PATTERNS:
        out = rx.sub(repl, out)
    return out


def redact_record(record):
    fields = ("message", "context", "span")
    for f in fields:
        v = record.get(f)
        if isinstance(v, str):
            record[f] = redact_line(v)
    return record
"#,
        ),
        (
            "src/bootstrap.py",
            r#"
import argparse
import logging
import sys

VERSION = "1.4.2"
DEFAULT_LEVEL = "info"
LOG_FORMAT = "%(asctime)s %(levelname)s %(name)s %(message)s"

parser = argparse.ArgumentParser(prog="app")
parser.add_argument("--verbose", action="store_true")
parser.add_argument("--quiet", action="store_true")
parser.add_argument("--config", default="app.yaml")
args = parser.parse_args(sys.argv[1:])

if args.quiet:
    level = logging.ERROR
elif args.verbose:
    level = logging.DEBUG
else:
    level = DEFAULT_LEVEL.upper()

logging.basicConfig(level=getattr(logging, level, logging.INFO), format=LOG_FORMAT)
logging.getLogger("app").info("starting version %s with config %s", VERSION, args.config)
"#,
        ),
    ]
}

/// The natural-language query set: (query, expected file). Wording is
/// deliberately ABSENT from every corpus source — these test semantic
/// matching, not literal substring hits.
fn queries() -> Vec<(&'static str, &'static str)> {
    vec![
        // net_retry.py
        ("where is retry backoff computed", "src/net_retry.py"),
        ("which http status codes are safe to try again", "src/net_retry.py"),
        ("exponential delay with randomization between attempts", "src/net_retry.py"),
        ("how many times is a failed request repeated before giving up", "src/net_retry.py"),
        // store_vectors.py
        ("where does the code store embedding vectors in the database", "src/store_vectors.py"),
        ("how is the similarity of two vectors measured", "src/store_vectors.py"),
        ("compressing floating point numbers to single bytes to save space", "src/store_vectors.py"),
        // crypto_hashes.py
        ("how are payloads hashed before signing", "src/crypto_hashes.py"),
        ("comparing secrets without leaking information through timing", "src/crypto_hashes.py"),
        ("canonical serialization before computing a checksum", "src/crypto_hashes.py"),
        // sched_ticker.py
        ("when does the periodic timer wake up next", "src/sched_ticker.py"),
        ("rounding a timestamp to the nearest interval boundary", "src/sched_ticker.py"),
        // config_loader.py
        ("combining several yaml files into one effective configuration", "src/config_loader.py"),
        ("parsing truthy strings like yes and on into booleans", "src/config_loader.py"),
        ("later configuration files take precedence over earlier ones", "src/config_loader.py"),
        // authz_roles.py
        ("deciding whether a role is allowed to perform an action", "src/authz_roles.py"),
        ("expanding wildcard permissions into concrete actions", "src/authz_roles.py"),
        // cache_lru.py
        ("which entry gets dropped when the cache is full", "src/cache_lru.py"),
        ("bounded in memory cache tracking recency of use", "src/cache_lru.py"),
        // render_tables.py
        ("printing rows of data as a markdown table", "src/render_tables.py"),
        ("escaping pipe characters inside cell text", "src/render_tables.py"),
        // fs_watcher.py
        ("noticing files changed on disk without busy polling", "src/fs_watcher.py"),
        ("collapsing bursts of filesystem events into one notification", "src/fs_watcher.py"),
        // queue_workers.py
        ("distributing jobs across a fixed set of threads", "src/queue_workers.py"),
        ("shutting the worker pool down when a task keeps failing", "src/queue_workers.py"),
        // http_client.py
        ("preparing an outgoing http request with custom headers", "src/http_client.py"),
        ("handling a server response that contains an error body", "src/http_client.py"),
        // metrics_counter.py
        ("tracking how often something happens grouped by labels", "src/metrics_counter.py"),
        ("computing the median and other percentiles of samples", "src/metrics_counter.py"),
        // throttle_bucket.py
        ("limiting how fast operations are allowed to proceed", "src/throttle_bucket.py"),
        ("allowing bursts but enforcing an average rate", "src/throttle_bucket.py"),
        // temp_cleanup.py
        ("cleaning up stale temporary directories after a failure", "src/temp_cleanup.py"),
        // redact_logs.py
        ("masking api keys before log lines are shipped", "src/redact_logs.py"),
        // bootstrap.py (fallback-chunk-only file, FR-014)
        ("where does the program set itself up at startup", "src/bootstrap.py"),
        ("choosing the log verbosity from command line flags", "src/bootstrap.py"),
    ]
}

// ---------------------------------------------------------------------------
// Real-pipeline plumbing
// ---------------------------------------------------------------------------

/// ChunkEmbedder adapter over a loaded LocalOnnx for the INDEXING path.
///
/// Prefix bookkeeping (contracts/embedding-backend.md rule 1): `index_file`
/// hands this boundary DOCUMENT-PREFIXED text, while `LocalOnnx` applies
/// the profile prefix itself per InputKind — the adapter strips the
/// pipeline's prefix and lets the backend re-apply it so exactly ONE
/// prefix ever reaches the tokenizer (identity when absent).
struct LocalOnnxChunkEmbedder {
    inner: std::sync::Arc<LocalOnnx>,
    document_prefix: &'static str,
}

impl ChunkEmbedder for LocalOnnxChunkEmbedder {
    fn embed_texts(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        let raw: Vec<String> = texts
            .iter()
            .map(|t| t.strip_prefix(self.document_prefix).unwrap_or(t).to_string())
            .collect();
        self.inner.embed_documents(&raw).map_err(|e| e.to_string())
    }
}

/// Index the whole corpus through the real chunker + embedder + store.
fn index_corpus(root: &Path, store: &GraphStore, model: &std::sync::Arc<LocalOnnx>, profile: &EmbedProfile) {
    let mut embedder = LocalOnnxChunkEmbedder {
        inner: std::sync::Arc::clone(model),
        document_prefix: profile.prefix_document,
    };
    for (rel, source) in corpus() {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, source).unwrap();

        let mut extraction = parse_any(&path, source)
            .unwrap_or_else(|| panic!("no extractor for {rel}"))
            .unwrap_or_else(|e| panic!("parse {rel}: {e}"));
        extraction.populate_fallback_chunks(source);

        // Production posture: the typed graph holds the artifacts (this is
        // what makes the FTS keyword leg live alongside the dense leg).
        for ty in &extraction.types {
            let node = CodeArtifactNode::new(
                ArtifactKind::Class,
                ty.name.clone(),
                String::new(),
                rel.to_string(),
            );
            store.upsert_node(&node).unwrap();
            for m in &ty.methods {
                let node = CodeArtifactNode::new(
                    ArtifactKind::Method,
                    m.name.clone(),
                    String::new(),
                    rel.to_string(),
                );
                store.upsert_node(&node).unwrap();
            }
        }
        for f in &extraction.module_functions {
            let node = CodeArtifactNode::new(
                ArtifactKind::Method,
                f.name.clone(),
                String::new(),
                rel.to_string(),
            );
            store.upsert_node(&node).unwrap();
        }

        index_file(
            store,
            &extraction,
            source,
            rel,
            &mut embedder,
            profile,
            Quantization::F32,
            &ChunkOptions::default(),
            &[],
        )
        .unwrap_or_else(|e| panic!("index {rel}: {e}"));
    }
}

/// The SC-001 measurement. AUTO-SKIPS (never an error, never a download)
/// when local model artifacts are absent.
#[test]
#[ignore = "opt-in: needs locally placed model artifacts (never downloaded); run with -- --ignored --nocapture"]
fn top5_correct_location_hit_rate_sc001() {
    let Some(model_dir) = artifacts_available() else {
        println!(
            "{SKIP_REASON_PREFIX} at {} — this test NEVER downloads and never \
             contacts huggingface.co (research.md R8). Place model.onnx + \
             tokenizer.json manually (or set {ENV_MODEL_DIR}) and re-run with \
             -- --ignored to execute the measurement.",
            joey_neurocode_rag::config::default_model_dir(default_profile().name).display()
        );
        return;
    };

    let profile = default_profile();
    let settings = LocalOnnxSettings {
        model_dir: model_dir.clone(),
        ort_dylib_path: String::new(), // env/system rungs of the dylib ladder
        batch_size: 64,
    };
    let model = std::sync::Arc::new(
        LocalOnnx::load(profile, &settings, None).unwrap_or_else(|e| {
            panic!(
                "artifacts verified at {} but backend load failed (dylib \
                 configured? ORT_DYLIB_PATH / system lookup): {e}",
                model_dir.display()
            )
        }),
    );

    let tmp = tempfile::tempdir().unwrap();
    let store = GraphStore::open(&tmp.path().join("graph.db")).unwrap();
    index_corpus(tmp.path(), &store, &model, profile);

    let qs = queries();
    assert!(qs.len() >= 30, "SC-001 harness needs ≥30 queries (have {})", qs.len());

    let mut hits = 0usize;
    let mut misses: Vec<&str> = Vec::new();
    for (q, expected) in &qs {
        // `search_cli_with_embedder` takes the embedder by value
        // (`F: FnOnce`), so a fresh closure is constructed per iteration,
        // each capturing its own `Arc::clone(&model)`.
        let embed_query = {
            let query_embedder_model = std::sync::Arc::clone(&model);
            let query_prefix = profile.prefix_query;
            move |texts: &[String]| -> Result<Vec<Vec<f32>>, String> {
                // The dense leg hands over QUERY-PREFIXED text; LocalOnnx
                // applies the prefix itself — strip, then embed as queries
                // (one prefix).
                let raw: Vec<String> = texts
                    .iter()
                    .map(|t| t.strip_prefix(query_prefix).unwrap_or(t).to_string())
                    .collect();
                query_embedder_model.embed_queries(&raw).map_err(|e| e.to_string())
            }
        };
        let req = SearchRequest {
            query: (*q).to_string(),
            file_filter: None,
            limit: 5,
            expand_lines: 0,
            relation_depth: 0,
            include_fallback_chunks: true,
        };
        let out = search_cli_with_embedder(&store, tmp.path(), profile, &req, embed_query)
            .unwrap_or_else(|e| panic!("search {q:?}: {e}"));
        assert_eq!(out.mode, SearchMode::Hybrid, "dense leg degraded for {q:?}: {:?}", out.mode_reason);
        let top5: Vec<&str> = out.results.iter().map(|r| r.file.as_str()).collect();
        if top5.iter().any(|p| p == expected) {
            hits += 1;
            println!("[retrieval_quality] HIT  {q:?} -> {expected} (top5: {top5:?})");
        } else {
            misses.push(q);
            println!("[retrieval_quality] MISS {q:?} wanted {expected} (top5: {top5:?})");
        }
    }

    let rate = hits as f64 / qs.len() as f64;
    println!(
        "[retrieval_quality] SC-001 top-5 correct-location hit rate: {}/{} = {:.1}% \
         (required ≥ {:.0}%)",
        hits,
        qs.len(),
        rate * 100.0,
        REQUIRED_HIT_RATE * 100.0
    );
    assert!(
        rate >= REQUIRED_HIT_RATE,
        "SC-001 violated: hit rate {:.1}% < {:.0}% — misses: {misses:?}",
        rate * 100.0,
        REQUIRED_HIT_RATE * 100.0
    );
}

// ---------------------------------------------------------------------------
// Always-run companion (NOT ignored): pins the skip logic itself.
// ---------------------------------------------------------------------------

/// Absent artifacts ⇒ the gate returns cleanly with the documented reason
/// and the auto resolution degrades to keyword-only — filesystem-only, no
/// network anywhere in the path (research.md R8).
#[test]
fn skip_logic_returns_cleanly_when_artifacts_absent() {
    // 1. The probe gate: an empty directory is a clean ModelFilesMissing —
    //    the exact condition the ignored test's auto-skip keys off.
    let tmp = tempfile::tempdir().unwrap();
    match compute_hashes(tmp.path()) {
        Err(ArtifactError::ModelFilesMissing(msg)) => {
            assert!(msg.contains("model.onnx") || msg.contains("tokenizer.json"));
        }
        other => panic!("empty dir must be ModelFilesMissing, got {other:?}"),
    }

    // 2. Auto resolution degrades (never errors, never fetches): the
    //    decision is KeywordOnly with a reason naming the artifacts and
    //    stating that no network call was made.
    let resolved = resolve_kind(
        joey_neurocode_rag::config::RagBackend::Auto,
        default_profile().name,
        tmp.path(),
    )
    .expect("auto resolution must degrade, not fail");
    assert_eq!(resolved.kind, BackendKind::KeywordOnly);
    assert!(resolved.degradation_reason.contains("no verifiable model artifacts"));
    assert!(resolved.degradation_reason.contains("no network call was made"));

    // 3. Explicit local_onnx + absent artifacts is a clean error (never a
    //    silent fetch).
    let err = resolve_kind(
        joey_neurocode_rag::config::RagBackend::LocalOnnx,
        default_profile().name,
        tmp.path(),
    )
    .unwrap_err();
    assert!(matches!(
        err,
        joey_neurocode_rag::embed::EmbedError::MissingArtifacts(_)
    ));

    // 4. The documented reason string the ignored test prints on skip.
    assert!(SKIP_REASON_PREFIX.contains("no verified local model artifacts"));
    assert!(SKIP_REASON_PREFIX.contains("T040"));

    // 5. Harness self-checks: ≥30 queries, every expected path exists in
    //    the corpus, and no query wording literally occurs in any source
    //    (SC-001's semantic-matching premise).
    let qs = queries();
    assert!(qs.len() >= 30, "need ≥30 queries, have {}", qs.len());
    let corpus = corpus();
    let paths: Vec<&str> = corpus.iter().map(|(p, _)| *p).collect();
    for (_q, expected) in &qs {
        assert!(paths.contains(expected), "unknown expected path {expected}");
    }
    for (q, _) in &qs {
        let needle = q.to_lowercase();
        for (_p, src) in &corpus {
            let hay = src.to_lowercase();
            assert!(
                !hay.contains(&needle),
                "query {q:?} literally occurs in corpus file {_p} — reword it"
            );
        }
    }
}
