//! T015 badge-rendering tests (specs/021-please-enhance-neurocode, FR-014
//! + FR-008): the TUI result view over the rag crate's search payload must
//! render symbol-aligned vs fallback-chunk badges distinctly, carry the
//! mode/degradation banner, and keep the no-match response a CLEAR
//! (non-failure) rendering — fed purely by `SearchOutcome`-shaped structs
//! (constitution Principles II/III: same payload, no TUI-only state).
//!
//! Integration-style per the joey-tui tests convention: exercise the
//! public `joey_tui::neurocode_search` surface; a TestBackend frame smoke
//! test pins that the lines render inside a real ratatui frame.

use joey_neurocode_rag::search::hybrid::{
    ChunkBadge, RankedResult, RelatedEntity, SearchMode, SearchOutcome,
};
use joey_tui::neurocode_search::{
    badge_label, badge_span, badge_style, outcome_lines, result_line,
};
use joey_tui::theme::Theme;
use ratatui::style::Color;

// ─── fixtures ────────────────────────────────────────────────────────────────

fn symbol_result() -> RankedResult {
    RankedResult {
        chunk_id: "src/auth.rs:10-30:symbol:validate_token".into(),
        file: "src/auth.rs".into(),
        symbol: Some("validate_token".into()),
        symbol_kind: Some("function".into()),
        start_line: 10,
        end_line: 30,
        chunk_kind: ChunkBadge::SymbolAligned,
        fused_score: 0.8123,
        semantic_rank: Some(1),
        keyword_rank: None,
        degradation_note: None,
        context: Some("fn validate_token() {\n    …\n}".into()),
        relations: Vec::new(),
    }
}

fn fallback_result() -> RankedResult {
    RankedResult {
        chunk_id: "scripts/top.py:1-6:fallback".into(),
        file: "scripts/top.py".into(),
        symbol: None,
        symbol_kind: None,
        start_line: 1,
        end_line: 6,
        chunk_kind: ChunkBadge::Fallback,
        fused_score: 0.0164,
        semantic_rank: None,
        keyword_rank: Some(1),
        degradation_note: Some("no embedding backend available".into()),
        context: Some("import os\nAPI_KEY=\"secret-literal\"\n".into()),
        relations: Vec::new(),
    }
}

fn outcome(results: Vec<RankedResult>, mode: SearchMode, reason: Option<String>) -> SearchOutcome {
    SearchOutcome {
        total_candidates: results.len(),
        index_chunk_count: results.len() as u64,
        results,
        mode,
        mode_reason: reason,
    }
}

fn plain(line: &ratatui::text::Line<'_>) -> String {
    line.spans
        .iter()
        .map(|s| s.content.to_string())
        .collect::<Vec<_>>()
        .join("")
}

// ─── FR-014: badges are visible and distinct ─────────────────────────────────

/// The badge labels match the CLI renderer's strings exactly (parity).
#[test]
fn badge_labels_match_the_cli_renderer() {
    assert_eq!(badge_label(ChunkBadge::SymbolAligned), "symbol-aligned");
    assert_eq!(badge_label(ChunkBadge::Fallback), "fallback-chunk");
}

/// The two badge kinds are visibly distinct: different labels AND
/// different colors (primary vs warning).
#[test]
fn badge_kinds_render_distinct_colors() {
    let theme = Theme::aurora();
    let sym = badge_style(theme, ChunkBadge::SymbolAligned);
    let fb = badge_style(theme, ChunkBadge::Fallback);
    let sym_fg = sym.fg.expect("symbol-aligned badge is colored");
    let fb_fg = fb.fg.expect("fallback badge is colored");
    assert_ne!(sym_fg, fb_fg, "the two badges must differ in color");
    assert_eq!(sym_fg, Color::Rgb(theme.primary.0, theme.primary.1, theme.primary.2));
    assert_eq!(fb_fg, Color::Rgb(theme.warning.0, theme.warning.1, theme.warning.2));
}

/// Result lines embed the bracketed badge and the same fields the CLI
/// renders: file, symbol/kind, line range, score.
#[test]
fn result_line_embeds_badge_and_fields() {
    let theme = Theme::aurora();
    let line = result_line(theme, 1, &symbol_result());
    let text = plain(&line);
    assert!(text.contains("src/auth.rs"), "{text}");
    assert!(text.contains("[symbol-aligned]"), "{text}");
    assert!(text.contains("validate_token"), "{text}");
    assert!(text.contains("L10-30"), "{text}");
    assert!(text.contains("0.8123"), "{text}");

    let line = result_line(theme, 2, &fallback_result());
    let text = plain(&line);
    assert!(text.contains("[fallback-chunk]"), "{text}");
    // Fallback chunks have no symbol → the top-level marker, same as CLI.
    assert!(text.contains("(top-level code)"), "{text}");
    assert!(text.contains("L1"), "{text}");
}

/// The fallback badge span exists as its own span inside the line (so the
/// color distinction survives any wrapping).
#[test]
fn badge_is_a_distinct_span_in_the_line() {
    let theme = Theme::aurora();
    let line = result_line(theme, 1, &fallback_result());
    let expected = badge_span(theme, ChunkBadge::Fallback);
    assert!(
        line.spans.iter().any(|s| s.content == expected.content),
        "the badge span is embedded verbatim"
    );
}

// ─── FR-008: mode banner + no-match clarity ──────────────────────────────────

/// Keyword-only outcomes carry the degradation reason in the banner;
/// hybrid outcomes show the plain mode and no reason.
#[test]
fn mode_banner_reflects_degradation() {
    let theme = Theme::aurora();
    let degraded = outcome(
        vec![fallback_result()],
        SearchMode::KeywordOnly,
        Some("no embedding backend available — keyword-only fallback".into()),
    );
    let lines = outcome_lines(theme, &degraded, "secret-literal");
    let banner = plain(&lines[0]);
    assert!(banner.contains("keyword-only mode"), "{banner}");
    assert!(banner.contains("degraded"), "{banner}");
    assert!(
        banner.contains("keyword-only fallback"),
        "reason surfaced: {banner}"
    );

    let hybrid = outcome(vec![symbol_result()], SearchMode::Hybrid, None);
    let lines = outcome_lines(theme, &hybrid, "token");
    let banner = plain(&lines[0]);
    assert!(banner.contains("hybrid mode"), "{banner}");
    assert!(!banner.contains("degraded"), "{banner}");
}

/// No-match is a CLEAR response ('Nothing matched'), distinct from
/// failure, with the empty-index variant adding the /neurocode index hint.
#[test]
fn no_match_is_clear_and_distinct_from_failure() {
    let theme = Theme::aurora();
    // Indexed but no hits → plain no-match with the chunk count.
    let mut indexed = outcome(vec![], SearchMode::KeywordOnly, Some("reason".into()));
    indexed.index_chunk_count = 7;
    let lines = outcome_lines(theme, &indexed, "zzzz");
    let closer = plain(&lines[1]);
    assert!(closer.contains("Nothing matched"), "{closer}");
    assert!(closer.contains("7 indexed chunks searched"), "{closer}");
    assert!(!closer.to_lowercase().contains("fail"), "{closer}");

    // Empty index → the /neurocode index hint appears.
    let empty = outcome(vec![], SearchMode::KeywordOnly, None);
    let lines = outcome_lines(theme, &empty, "zzzz");
    let closer = plain(&lines[1]);
    assert!(closer.contains("Nothing matched"), "{closer}");
    assert!(closer.contains("/neurocode index"), "{closer}");
}

/// Full outcome: banner + both result kinds + context preview + relations.
#[test]
fn outcome_renders_banner_results_context_and_relations() {
    let theme = Theme::aurora();
    let mut fb = fallback_result();
    fb.relations = vec![RelatedEntity {
        file: "src/auth.rs".into(),
        symbol: Some("validate_token".into()),
        relation_kind: "calls".into(),
    }];
    let out = outcome(
        vec![symbol_result(), fb],
        SearchMode::KeywordOnly,
        Some("no embedding backend available — keyword-only fallback".into()),
    );
    let lines = outcome_lines(theme, &out, "secret-literal");
    let all: Vec<String> = lines.iter().map(plain).collect();
    assert!(all[0].contains("keyword-only mode"), "{all:?}");
    assert!(all.iter().any(|l| l.contains("[symbol-aligned]")), "{all:?}");
    assert!(all.iter().any(|l| l.contains("[fallback-chunk]")), "{all:?}");
    assert!(
        all.iter().any(|l| l.contains("import os")),
        "context preview rendered: {all:?}"
    );
    assert!(
        all.iter().any(|l| l.contains("↳ relates via calls")),
        "relation line rendered: {all:?}"
    );
}

/// Render into a real ratatui frame without panicking (TestBackend smoke —
/// the joey-tui tests convention).
#[test]
fn renders_inside_a_ratatui_frame() {
    use ratatui::backend::TestBackend;
    use ratatui::widgets::Paragraph;
    use ratatui::Terminal;

    let theme = Theme::aurora();
    let out = outcome(
        vec![symbol_result(), fallback_result()],
        SearchMode::KeywordOnly,
        Some("no embedding backend available — keyword-only fallback".into()),
    );
    let lines = outcome_lines(theme, &out, "secret-literal");

    let backend = TestBackend::new(100, 12);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            let area = f.area();
            f.render_widget(Paragraph::new(lines.clone()), area);
        })
        .unwrap();
}


// ─── T043: the TUI output path renders RAG search notices styled ────────────
//
// The TUI receives /neurocode heavy-job results as one transcript Notice
// per output line (joey-cli tui.rs `HeavyJobFinished`). These tests pin
// that search-rendered lines flow through the styled renderer (FR-014
// badge colors, result_line rank/symbol/kind layout) while EVERYTHING
// else keeps the exact plain Notice rendering — non-search /neurocode
// subcommands and the RAG-disabled path stay byte-identical to the
// pre-T043 dump (FR-009/SC-005 parity is load-bearing).
mod tui_output_path {
    use super::*;
    use joey_tui::state::{NoticeKind, TranscriptItem};
    use joey_tui::widgets::item_lines_for_test;
    use ratatui::style::Color;

    fn notice(text: &str) -> TranscriptItem {
        TranscriptItem::Notice {
            text: text.to_string(),
            kind: NoticeKind::Info,
        }
    }

    /// Render one Notice the way the transcript does; assert + return the
    /// content line (index 0; index 1 is the uniform blank separator).
    fn render(text: &str) -> ratatui::text::Line<'static> {
        let lines = item_lines_for_test(&notice(text), 120, Theme::aurora());
        assert_eq!(lines.len(), 2, "notice renders content + blank separator");
        lines.into_iter().next().unwrap()
    }

    fn fg(color: joey_tui::theme::Rgb) -> Option<Color> {
        Some(color.to_color())
    }

    /// FR-014 + FR-010: a fallback-chunk result line arriving as a Notice
    /// renders through `result_line` — identical span layout/styles — so
    /// the badge carries the warning color. Text stays byte-identical.
    #[test]
    fn fallback_result_notice_uses_result_line_layout_and_badge_color() {
        let theme = Theme::aurora();
        // The exact line the CLI renderer emits for `fallback_result()`.
        let cli_line = "1. scripts/top.py [fallback-chunk] (top-level code) (region):L1-6 — score 0.0164";
        let line = render(cli_line);
        // Byte-identical text: bullet + verbatim input (no collapsing).
        assert_eq!(plain(&line), format!("  · {cli_line}"));
        // Span-for-span identity with the styled renderer's result_line.
        let expected = result_line(theme, 1, &fallback_result());
        assert_eq!(
            line.spans.len(),
            expected.spans.len() + 1,
            "bullet + result_line spans"
        );
        assert_eq!(line.spans[0].content.to_string(), "  · ");
        for (got, want) in line.spans[1..].iter().zip(expected.spans.iter()) {
            assert_eq!(got.content, want.content, "span content");
            assert_eq!(got.style, want.style, "span style");
        }
        // The fallback badge is visibly the warning color (FR-014).
        let got = line
            .spans
            .iter()
            .find(|s| s.content == badge_span(theme, ChunkBadge::Fallback).content)
            .expect("fallback badge span present");
        assert_eq!(got.style.fg, fg(theme.warning));
    }

    /// Same wiring for symbol-aligned results: primary-color badge, the
    /// rank/symbol/kind/score layout produced by `result_line`.
    #[test]
    fn symbol_result_notice_uses_result_line_layout_and_primary_badge() {
        let theme = Theme::aurora();
        let cli_line = "2. src/auth.rs [symbol-aligned] validate_token (function):L10-30 — score 0.8123";
        let line = render(cli_line);
        assert_eq!(plain(&line), format!("  · {cli_line}"));
        let expected = result_line(theme, 2, &symbol_result());
        for (got, want) in line.spans[1..].iter().zip(expected.spans.iter()) {
            assert_eq!(got.content, want.content);
            assert_eq!(got.style, want.style);
        }
        let got = line
            .spans
            .iter()
            .find(|s| s.content == badge_span(theme, ChunkBadge::SymbolAligned).content)
            .expect("symbol-aligned badge span present");
        assert_eq!(got.style.fg, fg(theme.primary));
    }

    /// FR-008 mode banners: keyword-only (degraded) warns, hybrid informs —
    /// same colors `outcome_lines` uses; text unchanged.
    #[test]
    fn mode_banner_notices_carry_mode_colors() {
        let theme = Theme::aurora();
        let degraded = render(
            "Search \"secret-literal\" — keyword-only mode (semantic backend degraded: no backend)",
        );
        assert_eq!(
            plain(&degraded),
            "  · Search \"secret-literal\" — keyword-only mode (semantic backend degraded: no backend)"
        );
        assert_eq!(degraded.spans[1].style.fg, fg(theme.warning));

        let hybrid = render("Search \"token\" — hybrid mode");
        assert_eq!(plain(&hybrid), "  · Search \"token\" — hybrid mode");
        assert_eq!(hybrid.spans[1].style.fg, fg(theme.info));
    }

    /// No-match closers stay CLEAR + subtle, text byte-identical, both the
    /// indexed and empty-index variants.
    #[test]
    fn no_match_closers_render_subtle_text_unchanged() {
        let theme = Theme::aurora();
        let indexed = render("Nothing matched 'zzzz' (7 indexed chunks searched).");
        assert_eq!(
            plain(&indexed),
            "  · Nothing matched 'zzzz' (7 indexed chunks searched)."
        );
        assert_eq!(indexed.spans[1].style.fg, fg(theme.fg_subtle));

        let empty = render(
            "Nothing matched — the semantic index is empty. Run /neurocode index to build it.",
        );
        assert_eq!(empty.spans[1].style.fg, fg(theme.fg_subtle));
    }

    /// Relation lines render dimmed (more subtle), mirroring
    /// `relation_lines`. Context-preview lines (bare 5-space indent, no
    /// distinguishing marker) are DELIBERATELY left on the plain notice
    /// rendering — claiming that shape would restyle unrelated notices
    /// pushed line-by-line by other heavy jobs (text parity risk), and
    /// `outcome_lines` renders them unstyled anyway.
    #[test]
    fn context_and_relation_lines_render_styled() {
        let theme = Theme::aurora();
        // Context preview: plain notice treatment, byte-identical to the
        // pre-T043 dump (one_line collapses the indentation).
        let ctx = render("     import os");
        assert_eq!(plain(&ctx), "  · import os");
        assert_eq!(ctx.spans.len(), 2, "no extra spans for context lines");
        assert_eq!(ctx.spans[1].style.fg, fg(theme.fg_more_subtle));

        // Relation line: claimed — the ↳ marker is unambiguous.
        let rel = render("     ↳ relates via calls → src/auth.rs validate_token");
        assert_eq!(
            plain(&rel),
            "  ·      ↳ relates via calls → src/auth.rs validate_token"
        );
        assert_eq!(rel.spans[1].style.fg, fg(theme.fg_more_subtle));
    }

    /// Parity: every NON-search notice — other /neurocode subcommands, the
    /// validation/usage lines, the busy banner, and anything else — keeps
    /// the exact plain rendering (bullet + one subtle span, bytes intact).
    #[test]
    fn non_search_notices_stay_plain_and_byte_identical() {
        let theme = Theme::aurora();
        for text in [
            "NeuroCode: off for this profile",
            "Search: query is empty or whitespace-only.",
            "Usage: /neurocode search <query...> [--path <glob>] [--limit <n>]",
            "⧗ /neurocode running on the engine… (GUI stays live)",
            "Search \"unfinished", // banner shape requires the trailing mode
        ] {
            let line = render(text);
            assert_eq!(line.spans.len(), 2, "plain notice = bullet + one span: {text}");
            assert_eq!(plain(&line), format!("  · {text}"), "byte-identical: {text}");
            assert_eq!(line.spans[1].style.fg, fg(theme.fg_more_subtle), "{text}");
        }
    }

    /// The round-trip guard: a result-SHAPED line that was not produced by
    /// the search renderer (here: wrong score precision) falls back to the
    /// plain rendering instead of guessing a payload.
    #[test]
    fn malformed_result_line_falls_back_to_plain() {
        let theme = Theme::aurora();
        let bad = "1. src/x.rs [fallback-chunk] top (fn):L1-2 — score 0.016";
        let line = render(bad);
        assert_eq!(line.spans.len(), 2, "no badge spans for a malformed line");
        assert_eq!(plain(&line), format!("  · {bad}"));
        assert_eq!(line.spans[1].style.fg, fg(theme.fg_more_subtle));
    }
}
