//! Markdown → ANSI renderer for the streaming finalize step (FR-003).
//!
//! See `specs/004-claude-code-cli-style/contracts/render-animation-seam.md`
//! Contract 2. Pure function: no I/O, no globals, deterministic given
//! (input, theme). Tested at the seam (Constitution Principle IV).

use joey_core::theme::{Rgb, Theme};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// Render the given CommonMark markdown string as ANSI-styled text using the
/// Pantera `theme` colors. Output is a single `String` safe to `println!`.
pub(crate) fn markdown_to_ansi(input: &str, theme: &Theme) -> String {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    let parser = Parser::new_ext(input, opts);
    let mut out = String::with_capacity(input.len() + 64);
    let mut heading_level: Option<HeadingLevel> = None;
    let mut in_code_block = false;
    // Some(n) = ordered (with start counter), None = bullet
    let mut list_stack: Vec<Option<u64>> = Vec::new();

    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => {
                    heading_level = Some(level);
                    out.push('\n');
                    out.push_str("\x1b[1m"); // bold
                }
                Tag::CodeBlock(kind) => {
                    in_code_block = true;
                    out.push('\n');
                    // T051a: emit a language label line for fenced blocks so
                    // the finalize reflow shows the block's language (Contract 2).
                    if let CodeBlockKind::Fenced(lang) = kind {
                        if !lang.is_empty() {
                            out.push_str(
                                &theme.fg_more_subtle.ansi().paint(format!("╭─[ {} ]", lang)).to_string(),
                            );
                            out.push('\n');
                        }
                    }
                }
                Tag::List(start) => list_stack.push(start),
                Tag::Item => {
                    for _ in 0..list_stack.len().saturating_sub(1) {
                        out.push_str("  ");
                    }
                    match list_stack.last_mut() {
                        Some(Some(n)) => {
                            let marker = format!("{}. ", n);
                            out.push_str(&theme.info.ansi().paint(marker).to_string());
                            *n += 1;
                        }
                        _ => {
                            out.push_str(&theme.info.ansi().paint("• ").to_string());
                        }
                    }
                }
                Tag::BlockQuote(_) => {
                    out.push_str(&theme.fg_more_subtle.ansi().paint("│ ").to_string());
                }
                Tag::Emphasis => out.push_str("\x1b[3m"),
                Tag::Strong => out.push_str("\x1b[1m"),
                Tag::Strikethrough => out.push_str("\x1b[9m"),
                Tag::Paragraph => {
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                }
                Tag::Link { dest_url, .. } => {
                    out.push('[');
                    out.push_str(&dest_url);
                    out.push_str("] ");
                }
                _ => {}
            },
            Event::End(tag_end) => match tag_end {
                TagEnd::Heading(_) => {
                    heading_level = None;
                    out.push_str("\x1b[0m");
                    out.push('\n');
                }
                TagEnd::CodeBlock => {
                    in_code_block = false;
                    out.push('\n');
                }
                TagEnd::List(_) => {
                    list_stack.pop();
                }
                TagEnd::Item => out.push('\n'),
                TagEnd::BlockQuote(_) => out.push('\n'),
                TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                    out.push_str("\x1b[0m")
                }
                TagEnd::Paragraph => out.push('\n'),
                TagEnd::Link => { /* URL appended on Start; nothing on End */ }
                _ => {}
            },
            Event::Text(s) => {
                if in_code_block {
                    out.push_str(&theme.accent.ansi().paint(s.as_ref()).to_string());
                } else if let Some(lvl) = heading_level {
                    let color = heading_color(lvl, theme);
                    out.push_str(&color.ansi().paint(s.as_ref()).to_string());
                } else {
                    out.push_str(&s);
                }
            }
            Event::Code(s) => {
                out.push_str(&theme.accent.ansi().paint(s.as_ref()).to_string());
            }
            Event::SoftBreak => out.push('\n'),
            Event::HardBreak => out.push('\n'),
            Event::Rule => {
                let field = joey_core::theme::gradient_diagonal_field(
                    40,
                    theme.info_most_subtle,
                    theme.fg_most_subtle,
                );
                out.push_str(&field);
                out.push('\n');
            }
            _ => {}
        }
    }
    out
}

fn heading_color(level: HeadingLevel, theme: &Theme) -> Rgb {
    use HeadingLevel::*;
    match level {
        H1 | H2 => theme.primary,
        H3 => theme.secondary,
        H4 => theme.accent,
        _ => theme.info,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Theme {
        Theme::pantera()
    }

    /// ANSI truecolor escape for an Rgb, used to assert presence in output.
    fn tc(rgb: Rgb) -> String {
        format!("\x1b[38;2;{};{};{}m", rgb.0, rgb.1, rgb.2)
    }

    #[test]
    fn heading_uses_primary_color_and_bold() {
        let t = theme();
        let out = markdown_to_ansi("# Heading One", &t);
        assert!(out.contains("\x1b[1m"), "heading must be bold; got: {:?}", out);
        assert!(
            out.contains(&tc(t.primary)),
            "H1 must use primary color; got: {:?}",
            out
        );
    }

    #[test]
    fn code_block_uses_accent_color() {
        let t = theme();
        let out = markdown_to_ansi("```\ncode body\n```", &t);
        assert!(
            out.contains(&tc(t.accent)),
            "code block must use accent color; got: {:?}",
            out
        );
    }

    #[test]
    fn inline_code_uses_accent_color() {
        let t = theme();
        let out = markdown_to_ansi("this is `inline` code", &t);
        assert!(
            out.contains(&tc(t.accent)),
            "inline code must use accent color; got: {:?}",
            out
        );
    }

    #[test]
    fn bullet_list_uses_info_color_marker() {
        let t = theme();
        let out = markdown_to_ansi("- one\n- two\n", &t);
        assert!(
            out.contains(&tc(t.info)),
            "list marker must use info color; got: {:?}",
            out
        );
    }

    #[test]
    fn bold_uses_ansi_bold_attribute() {
        let t = theme();
        let out = markdown_to_ansi("**bold** text", &t);
        assert!(out.contains("\x1b[1m"), "bold must use ANSI bold; got: {:?}", out);
    }

    #[test]
    fn blockquote_uses_pipe_marker() {
        let t = theme();
        let out = markdown_to_ansi("> quoted text", &t);
        assert!(
            out.contains(&tc(t.fg_more_subtle)),
            "blockquote marker must use fg_more_subtle; got: {:?}",
            out
        );
        assert!(out.contains('│'), "blockquote must use │ marker; got: {:?}", out);
    }

    #[test]
    fn horizontal_rule_renders() {
        let t = theme();
        let out = markdown_to_ansi("---\n", &t);
        assert!(!out.trim().is_empty(), "hr must render; got: {:?}", out);
    }

    #[test]
    fn deterministic_pure_function() {
        let t = theme();
        let a = markdown_to_ansi("# Hi\n\ntext `c`\n", &t);
        let b = markdown_to_ansi("# Hi\n\ntext `c`\n", &t);
        assert_eq!(a, b, "markdown_to_ansi must be deterministic");
    }
}
