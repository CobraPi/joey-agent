//! Parser for `spec.md` into the `Specification` model.
//!
//! Uses a tolerant line-scan approach (informed by pulldown-cmark's Markdown
//! tokenization rules for headings/lists) so odd/malformed entries degrade
//! to `Status::Unparsed` fields instead of panicking or being dropped.

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};

use crate::model::{ClarificationEntry, Requirement, Specification, Status, UserStory};

/// Parse the full contents of a `spec.md` file.
pub fn parse_spec(content: &str) -> Specification {
    let mut spec = Specification::default();

    // Title: first H1.
    if let Some(title) = first_heading(content, HeadingLevel::H1) {
        spec.title = strip_prefix_label(&title);
    }

    // Metadata line "**Created**: 2026-..." "**Status**: Draft"
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("**Created**:") {
            spec.created = Some(rest.trim().to_string());
        }
        if let Some(rest) = trimmed.strip_prefix("**Status**:") {
            spec.status = parse_status(rest.trim());
        }
    }

    spec.user_stories = parse_user_stories(content);
    spec.requirements = parse_requirements(content);
    spec.clarifications = parse_clarifications(content);
    spec.key_entities = parse_bulleted_section(content, "Key Entities");
    spec.success_criteria = parse_bulleted_section(content, "Success Criteria");

    spec
}

fn parse_status(text: &str) -> Status {
    match text.to_lowercase().as_str() {
        s if s.contains("draft") => Status::Draft,
        s if s.contains("in progress") || s.contains("inprogress") => Status::InProgress,
        s if s.contains("completed") || s.contains("done") => Status::Completed,
        s if s.contains("approved") => Status::Approved,
        _ => Status::Unparsed,
    }
}

fn strip_prefix_label(s: &str) -> String {
    // Handles headings like "# Feature Specification: SpecKit Visual UI"
    if let Some(idx) = s.find(':') {
        s[idx + 1..].trim().to_string()
    } else {
        s.trim().to_string()
    }
}

/// Returns the text of the first heading at the given level, if any.
fn first_heading(content: &str, level: HeadingLevel) -> Option<String> {
    let parser = Parser::new(content);
    let mut in_target = false;
    let mut buf = String::new();
    for event in parser {
        match event {
            Event::Start(Tag::Heading { level: l, .. }) if l == level => {
                in_target = true;
                buf.clear();
            }
            Event::End(TagEnd::Heading(l)) if l == level && in_target => {
                return Some(buf.trim().to_string());
            }
            Event::Text(t) if in_target => buf.push_str(&t),
            Event::Code(t) if in_target => buf.push_str(&t),
            _ => {}
        }
    }
    None
}

/// Parse `### User Story N - Title (Priority: PX)` sections.
fn parse_user_stories(content: &str) -> Vec<UserStory> {
    let mut stories = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        if let Some(rest) = line.strip_prefix("### ") {
            if rest.to_lowercase().starts_with("user story") {
                let mut story = UserStory::default();
                // e.g. "User Story 1 - Visualize the Spec-to-Task Hierarchy (Priority: P1)"
                let (id, title, priority) = parse_user_story_heading(rest);
                story.id = id;
                story.title = title;
                story.priority = priority;

                // Scan forward for a status marker and acceptance scenarios
                // until the next heading.
                let mut j = i + 1;
                while j < lines.len() && !lines[j].trim_start().starts_with('#') {
                    let l = lines[j].trim();
                    if let Some(rest) = l.strip_prefix("**Status**:") {
                        story.status = parse_status(rest.trim());
                    }
                    if l.starts_with("- Given") || l.to_lowercase().contains("acceptance scenario")
                    {
                        story.acceptance_scenarios.push(l.trim_start_matches("- ").to_string());
                    }
                    j += 1;
                }
                stories.push(story);
            }
        }
        i += 1;
    }
    stories
}

fn parse_user_story_heading(rest: &str) -> (String, String, Option<String>) {
    // Try to split off "(Priority: PX)"
    let mut priority = None;
    let mut base = rest.to_string();
    if let Some(start) = rest.rfind('(') {
        if let Some(end) = rest.rfind(')') {
            if end > start {
                let inner = &rest[start + 1..end];
                if let Some(p) = inner.split(':').nth(1) {
                    priority = Some(p.trim().to_string());
                }
                base = rest[..start].trim().to_string();
            }
        }
    }
    // "User Story 1 - Title" or "User Story 1: Title"
    let id = base
        .split(['-', ':'])
        .next()
        .unwrap_or(&base)
        .trim()
        .replace(' ', "");
    let id = if id.is_empty() {
        "Unparsed".to_string()
    } else {
        format!("US{}", id.trim_start_matches("UserStory").trim())
    };
    (id, base, priority)
}

/// Parse `- **FR-NNN**: text` lines from the Functional Requirements section.
fn parse_requirements(content: &str) -> Vec<Requirement> {
    let mut reqs = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("- **") {
            if let Some(bold_end) = rest.find("**") {
                let id_candidate = &rest[..bold_end];
                if id_candidate.starts_with("FR-") || id_candidate.starts_with("NFR-") {
                    let after = rest[bold_end + 2..].trim_start_matches(':').trim();
                    reqs.push(Requirement {
                        id: id_candidate.to_string(),
                        text: after.to_string(),
                        user_story_ref: None,
                    });
                }
            }
        }
    }
    reqs
}

/// Parse Clarifications sessions: "- Q: ... -> A: ..." lines, tolerant of
/// missing answers (unanswered questions get `answer: None`).
fn parse_clarifications(content: &str) -> Vec<ClarificationEntry> {
    let mut entries = Vec::new();
    let mut current_session: Option<String> = None;
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("### Session ") {
            current_session = Some(rest.trim().to_string());
        }
        if let Some(rest) = trimmed.strip_prefix("- Q:").or_else(|| trimmed.strip_prefix("- **Q**:"))
        {
            if let Some((q, a)) = rest.split_once("A:") {
                entries.push(ClarificationEntry {
                    session_date: current_session.clone(),
                    question: q.trim().to_string(),
                    answer: Some(a.trim().to_string()),
                });
            } else {
                entries.push(ClarificationEntry {
                    session_date: current_session.clone(),
                    question: rest.trim().to_string(),
                    answer: None,
                });
            }
        }
    }
    entries
}

/// Parse a simple bulleted list under a heading whose text contains `name`.
fn parse_bulleted_section(content: &str, name: &str) -> Vec<String> {
    let mut items = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut in_section = false;
    for line in lines {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            in_section = trimmed.to_lowercase().contains(&name.to_lowercase());
            continue;
        }
        if in_section {
            if let Some(rest) = trimmed.strip_prefix("- ") {
                items.push(rest.trim().to_string());
            }
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_title_and_status() {
        let md = "# Feature Specification: My Feature\n\n**Created**: 2026-01-01\n**Status**: Draft\n";
        let spec = parse_spec(md);
        assert_eq!(spec.title, "My Feature");
        assert_eq!(spec.status, Status::Draft);
    }

    #[test]
    fn malformed_status_is_unparsed() {
        let md = "# Feature Specification: X\n\n**Status**: ???\n";
        let spec = parse_spec(md);
        assert_eq!(spec.status, Status::Unparsed);
    }

    #[test]
    fn parses_requirements() {
        let md = "## Requirements\n- **FR-001**: Must do a thing.\n- **FR-002**: Must do another.\n";
        let spec = parse_spec(md);
        assert_eq!(spec.requirements.len(), 2);
        assert_eq!(spec.requirements[0].id, "FR-001");
    }
}
