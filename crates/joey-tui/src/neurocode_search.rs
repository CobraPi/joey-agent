//! TUI result view for `/neurocode search` (T015, FR-014 + FR-008).
//!
//! Pure rendering surface over the rag crate's search payload
//! (`SearchOutcome` / `RankedResult` / `ChunkBadge` from
//! `joey_neurocode_rag::search::hybrid`): no TUI-only state, no divergent
//! formatting decisions — the same payload the CLI renders as text and the
//! `neurocode_search` agent tool returns as JSON (contracts/
//! neurocode-rag-command.md § Grammar additions; constitution Principles
//! II/III parity).
//!
//! Rendering rules mirrored from the CLI text renderer
//! (`joey-cli commands/neurocode.rs`):
//!
//! - the FR-014 badge is VISIBLE and distinct per chunk kind:
//!   `symbol-aligned` (primary color) vs `fallback-chunk` (warning color);
//! - the FR-008 mode banner: keyword-only results carry the degradation
//!   reason; hybrid results show the plain mode;
//! - no-match is a CLEAR response ('Nothing matched …'), distinct from
//!   failure, with the empty-index variant appending the
//!   `/neurocode index` hint;
//! - fallback chunks with no symbol render as `(top-level code)`.
//!
//! Everything here is deterministic and pure with respect to the outcome:
//! functions map `(outcome, theme, width) → Vec<Line<'static>>` which the
//! host renders into any pane.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use joey_neurocode_rag::search::hybrid::{
    ChunkBadge, RankedResult, SearchMode, SearchOutcome,
};

use crate::theme::Theme;

// ─── FR-014 badge ────────────────────────────────────────────────────────────

/// The visible badge label for a chunk kind — the SAME strings the CLI
/// renders (`badge_text` in joey-cli), so both surfaces show identical
/// badge text for identical payloads.
pub fn badge_label(kind: ChunkBadge) -> &'static str {
    match kind {
        ChunkBadge::SymbolAligned => "symbol-aligned",
        ChunkBadge::Fallback => "fallback-chunk",
    }
}

/// Badge styling: symbol-aligned results use the primary accent, fallback
/// chunks the warning color — the two kinds are visibly distinct even
/// before the label is read (FR-014's "visibly badged" requirement).
pub fn badge_style(theme: Theme, kind: ChunkBadge) -> Style {
    match kind {
        ChunkBadge::SymbolAligned => Style::default()
            .fg(theme.primary.to_color())
            .add_modifier(Modifier::BOLD),
        ChunkBadge::Fallback => Style::default()
            .fg(theme.warning.to_color())
            .add_modifier(Modifier::BOLD),
    }
}

/// The badge span (label + style) as embedded in result lines.
pub fn badge_span(theme: Theme, kind: ChunkBadge) -> Span<'static> {
    Span::styled(
        format!("[{}]", badge_label(kind)),
        badge_style(theme, kind),
    )
}

// ─── Result lines ────────────────────────────────────────────────────────────

/// `L12` / `L12-40` — same range shape as the CLI renderer.
fn line_range(start: u32, end: u32) -> String {
    if start == end {
        format!("L{start}")
    } else {
        format!("L{start}-{end}")
    }
}

/// One ranked result as a single styled line:
/// `<rank>. <file> <[badge]> <symbol> (<kind>):<lines> — score <s>`.
pub fn result_line(theme: Theme, rank: usize, r: &RankedResult) -> Line<'static> {
    let symbol = r.symbol.clone().unwrap_or_else(|| "(top-level code)".to_string());
    let kind = r.symbol_kind.clone().unwrap_or_else(|| "region".to_string());
    Line::from(vec![
        Span::styled(
            format!("{}. ", rank),
            Style::default().fg(theme.fg_most_subtle.to_color()),
        ),
        Span::styled(r.file.clone(), Style::default().fg(theme.fg_base.to_color())),
        Span::raw(" "),
        badge_span(theme, r.chunk_kind),
        Span::raw(" "),
        Span::styled(symbol, Style::default().fg(theme.keyword.to_color())),
        Span::styled(
            format!(" ({kind}):{} ", line_range(r.start_line, r.end_line)),
            Style::default().fg(theme.fg_subtle.to_color()),
        ),
        Span::styled(
            format!("— score {:.4}", r.fused_score),
            Style::default().fg(theme.fg_more_subtle.to_color()),
        ),
    ])
}

/// Context preview lines for one result (indented, same ≤3-line preview as
/// the CLI renderer).
pub fn context_lines(r: &RankedResult) -> Vec<Line<'static>> {
    r.context
        .as_deref()
        .map(|ctx| {
            ctx.lines()
                .take(3)
                .map(|l| Line::raw(format!("     {}", l.trim_end())))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

/// Relation lines for one result (populated by T029; rendered when present).
pub fn relation_lines(theme: Theme, r: &RankedResult) -> Vec<Line<'static>> {
    r.relations
        .iter()
        .map(|rel| {
            let sym = rel.symbol.clone().unwrap_or_else(|| "(top-level code)".to_string());
            Line::styled(
                format!("     ↳ relates via {} → {} {}", rel.relation_kind, rel.file, sym),
                Style::default().fg(theme.fg_more_subtle.to_color()),
            )
        })
        .collect()
}

// ─── T043: transcript wiring (FR-010 + Constitution II) ─────────────────────
//
// The TUI host receives `/neurocode` heavy-job results as ONE transcript
// `Notice` per output line (joey-cli `HeavyJobFinished` → push_item). That
// raw-text dump previously bypassed this module entirely. `item_lines`
// (widgets.rs) now calls [`styled_notice_spans`] on every notice: lines
// the CLI search renderer (`render_search_text`) could have produced are
// re-rendered through the styled helpers above — FR-014 badge colors,
// `result_line` rank/symbol/kind layout, FR-008 banner colors — while
// EVERY other notice keeps the exact plain rendering (non-search
// /neurocode subcommands and the RAG-disabled path stay byte-identical;
// FR-009/SC-005 parity is load-bearing).
//
// Recognition is per-line and exact: each parser rebuilds the input from
// the pieces it extracted and only claims the line when the rebuild is
// byte-identical, so a line is styled ONLY if it round-trips through the
// renderer's own format strings. Bare context-preview lines (5-space
// indent, no other marker) are deliberately NOT claimed — the shape is
// indistinguishable from any other indented notice line, and
// `outcome_lines` renders them unstyled anyway.

/// If `text` is a line produced by the CLI `/neurocode search` renderer,
/// return its styled spans (same styles [`outcome_lines`] produces);
/// otherwise `None` (the caller keeps the plain notice rendering).
pub fn styled_notice_spans(text: &str, theme: Theme) -> Option<Vec<Span<'static>>> {
    banner_spans(text, theme)
        .or_else(|| closer_spans(text, theme))
        .or_else(|| result_spans(text, theme))
        .or_else(|| relation_spans(text, theme))
}

/// FR-008 mode banner: `Search "<q>" — keyword-only mode (semantic backend
/// degraded: <reason>)` (warning) or `Search "<q>" — <mode> mode` (info).
fn banner_spans(text: &str, theme: Theme) -> Option<Vec<Span<'static>>> {
    let rest = text.strip_prefix("Search \"")?;
    let sep = "\" — "; // closing quote, space, em-dash, space
    let q_end = rest.find(sep)?;
    let query = &rest[..q_end];
    let tail = &rest[q_end + sep.len()..];
    if let Some(reason) = tail
        .strip_prefix("keyword-only mode (semantic backend degraded: ")
        .and_then(|r| r.strip_suffix(')'))
    {
        let rebuilt = format!(
            "Search \"{query}\" — keyword-only mode (semantic backend degraded: {reason})"
        );
        if rebuilt != text {
            return None;
        }
        return Some(vec![Span::styled(
            rebuilt,
            Style::default().fg(theme.warning.to_color()),
        )]);
    }
    let mode = tail.strip_suffix(" mode")?;
    if mode.is_empty()
        || !mode
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return None;
    }
    let rebuilt = format!("Search \"{query}\" — {mode} mode");
    if rebuilt != text {
        return None;
    }
    Some(vec![Span::styled(
        rebuilt,
        Style::default().fg(theme.info.to_color()),
    )])
}

/// No-match closers (CLEAR responses, subtle color): the empty-index hint
/// (exact string) and `Nothing matched '<q>' (<n> indexed chunks searched).`.
fn closer_spans(text: &str, theme: Theme) -> Option<Vec<Span<'static>>> {
    const EMPTY_INDEX: &str = "Nothing matched — the semantic index is empty. \
Run /neurocode index to build it.";
    let subtle = Style::default().fg(theme.fg_subtle.to_color());
    if text == EMPTY_INDEX {
        return Some(vec![Span::styled(EMPTY_INDEX, subtle)]);
    }
    let rest = text.strip_prefix("Nothing matched '")?;
    let tail = rest.strip_suffix(" indexed chunks searched).")?;
    let sep = tail.rfind("' (")?;
    let query = &tail[..sep];
    let count: u64 = tail[sep + 3..].parse().ok()?;
    let rebuilt = format!("Nothing matched '{query}' ({count} indexed chunks searched).");
    if rebuilt != text {
        return None;
    }
    Some(vec![Span::styled(rebuilt, subtle)])
}

/// `L12` / `L12-40` (mirror of [`line_range`], parse side).
fn parse_range(r: &str) -> Option<(u32, u32)> {
    let r = r.strip_prefix('L')?;
    match r.split_once('-') {
        Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
        None => {
            let v = r.parse().ok()?;
            Some((v, v))
        }
    }
}

/// One ranked result line, byte-identical round-trip through the CLI
/// format `"{rank}. {file} [{badge}] {symbol} ({kind}):{range} — score
/// {score:.4}"`, re-rendered via [`result_line`] (FR-014 badge colors +
/// the rank/symbol/kind/score span layout).
fn result_spans(text: &str, theme: Theme) -> Option<Vec<Span<'static>>> {
    let score_idx = text.rfind(" — score ")?;
    let score_str = &text[score_idx + " — score ".len()..];
    let score: f64 = score_str.parse().ok()?;
    if format!("{score:.4}") != score_str {
        return None; // renderer always emits exactly 4 decimals
    }
    let head = &text[..score_idx]; // "{rank}. {file} [{badge}] {symbol} ({kind}):{range}"
    let colon = head.rfind("):")?;
    let range = &head[colon + 2..];
    let (start_line, end_line) = parse_range(range)?;
    let left = &head[..colon + 1]; // "... ({kind})"
    if !left.ends_with(')') {
        return None;
    }
    let kparen = left.rfind(" (")?;
    let kind = &left[kparen + 2..left.len() - 1];
    let prefix = &left[..kparen]; // "{rank}. {file} [{badge}] {symbol}"
    let bopen = prefix.find(" [")?;
    let bclose = prefix[bopen..].find(']')? + bopen;
    let badge = &prefix[bopen + 2..bclose];
    let chunk_kind = match badge {
        "symbol-aligned" => ChunkBadge::SymbolAligned,
        "fallback-chunk" => ChunkBadge::Fallback,
        _ => return None,
    };
    let dot = prefix.find(". ")?;
    let rank: usize = prefix[..dot].parse().ok()?;
    let file = &prefix[dot + 2..bopen];
    let symbol = &prefix[bclose + 2..];
    if file.is_empty() || symbol.is_empty() || kind.is_empty() {
        return None;
    }
    // Exact round-trip through the CLI renderer's format string.
    let rebuilt = format!(
        "{rank}. {file} [{badge}] {symbol} ({kind}):{range} — score {score:.4}"
    );
    if rebuilt != text {
        return None;
    }
    let r = RankedResult {
        chunk_id: String::new(),
        file: file.to_string(),
        symbol: if symbol == "(top-level code)" {
            None
        } else {
            Some(symbol.to_string())
        },
        symbol_kind: if kind == "region" { None } else { Some(kind.to_string()) },
        start_line,
        end_line,
        chunk_kind,
        fused_score: score,
        semantic_rank: None,
        keyword_rank: None,
        degradation_note: None,
        context: None,
        relations: Vec::new(),
    };
    Some(result_line(theme, rank, &r).spans)
}

/// Relation line `     ↳ relates via <kind> → <file> <symbol>` (more
/// subtle color, mirroring [`relation_lines`]).
fn relation_spans(text: &str, theme: Theme) -> Option<Vec<Span<'static>>> {
    let rest = text.strip_prefix("     ↳ relates via ")?;
    let arrow = " → ";
    let arrow_at = rest.find(arrow)?;
    let relation_kind = &rest[..arrow_at];
    let tail = &rest[arrow_at + arrow.len()..]; // "{file} {symbol}"
    let (file, symbol) = match tail.strip_suffix(" (top-level code)") {
        Some(head) => (head, None),
        None => {
            let sp = tail.find(' ')?;
            (&tail[..sp], Some(&tail[sp + 1..]))
        }
    };
    if relation_kind.is_empty() || file.is_empty() {
        return None;
    }
    let sym = symbol.unwrap_or("(top-level code)");
    let rebuilt = format!("     ↳ relates via {relation_kind} → {file} {sym}");
    if rebuilt != text {
        return None;
    }
    Some(vec![Span::styled(
        rebuilt,
        Style::default().fg(theme.fg_more_subtle.to_color()),
    )])
}

// ─── Outcome rendering ───────────────────────────────────────────────────────

/// The full result view: mode banner (FR-008), ranked results with badges
/// (FR-014), and the no-match closers — shape-identical to the CLI text
/// output line-for-line.
pub fn outcome_lines(theme: Theme, outcome: &SearchOutcome, query: &str) -> Vec<Line<'static>> {
    let mut out = Vec::new();

    // Mode banner: keyword-only carries the degradation reason.
    match outcome.mode {
        SearchMode::KeywordOnly => {
            let reason = outcome
                .mode_reason
                .as_deref()
                .unwrap_or("unavailable");
            out.push(Line::styled(
                format!(
                    "Search \"{query}\" — keyword-only mode (semantic backend degraded: {reason})"
                ),
                Style::default().fg(theme.warning.to_color()),
            ));
        }
        mode => {
            out.push(Line::styled(
                format!("Search \"{query}\" — {mode} mode"),
                Style::default().fg(theme.info.to_color()),
            ));
        }
    }

    if outcome.results.is_empty() {
        if outcome.index_chunk_count == 0 {
            out.push(Line::styled(
                "Nothing matched — the semantic index is empty. Run /neurocode index to build it.",
                Style::default().fg(theme.fg_subtle.to_color()),
            ));
        } else {
            out.push(Line::styled(
                format!(
                    "Nothing matched '{query}' ({} indexed chunks searched).",
                    outcome.index_chunk_count
                ),
                Style::default().fg(theme.fg_subtle.to_color()),
            ));
        }
        return out;
    }

    for (idx, r) in outcome.results.iter().enumerate() {
        out.push(result_line(theme, idx + 1, r));
        out.extend(context_lines(r));
        out.extend(relation_lines(theme, r));
    }
    out
}
