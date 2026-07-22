//! V4A patch format parser + applier — port of `tools/patch_parser.py`.
//!
//! Two-phase validate-then-apply: all operations are validated against the
//! current file contents first (no writes); only when everything validates is
//! the apply phase run. Update hunks go through the fuzzy matcher.

use once_cell::sync::Lazy;
use regex::Regex;

use crate::difflib::{split_lines_keepends, unified_diff, SequenceMatcher, Tag};
use crate::fuzzy::{format_no_match_hint, fuzzy_find_and_replace};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationType {
    Add,
    Update,
    Delete,
    Move,
}

#[derive(Debug, Clone)]
pub struct HunkLine {
    pub prefix: char, // ' ', '-', or '+'
    pub content: String,
}

#[derive(Debug, Clone, Default)]
pub struct Hunk {
    pub context_hint: Option<String>,
    pub lines: Vec<HunkLine>,
}

#[derive(Debug, Clone)]
pub struct PatchOperation {
    pub operation: OperationType,
    pub file_path: String,
    pub new_path: Option<String>,
    pub hunks: Vec<Hunk>,
}

/// Minimal file-ops interface the applier needs (read raw / write / delete /
/// move). Errors are strings, matching the Python ReadResult/WriteResult shapes.
pub trait V4aFileOps {
    fn read_file_raw(&self, path: &str) -> Result<String, String>;
    fn write_file(&self, path: &str, content: &str) -> Result<(), String>;
    fn delete_file(&self, path: &str) -> Result<(), String>;
    fn move_file(&self, src: &str, dst: &str) -> Result<(), String>;
}

/// The applier's result — mirrors `PatchResult.to_dict()` field population.
#[derive(Debug, Default)]
pub struct V4aResult {
    pub success: bool,
    pub diff: String,
    pub files_modified: Vec<String>,
    pub files_created: Vec<String>,
    pub files_deleted: Vec<String>,
    pub error: Option<String>,
}

static UPDATE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\*\*\*\s*Update\s+File:\s*(.+)").unwrap());
static ADD_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\*\*\*\s*Add\s+File:\s*(.+)").unwrap());
static DELETE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\*\*\*\s*Delete\s+File:\s*(.+)").unwrap());
static MOVE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\*\*\*\s*Move\s+File:\s*(.+?)\s*->\s*(.+)").unwrap());
static HINT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^@@\s*(.+?)\s*@@").unwrap());

/// Port of `parse_v4a_patch`.
pub fn parse_v4a_patch(patch_content: &str) -> Result<Vec<PatchOperation>, String> {
    let lines: Vec<&str> = patch_content.split('\n').collect();
    let mut operations: Vec<PatchOperation> = Vec::new();

    let mut start_idx: Option<usize> = None;
    let mut end_idx: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        if line.contains("*** Begin Patch") || line.contains("***Begin Patch") {
            start_idx = Some(i);
        } else if line.contains("*** End Patch") || line.contains("***End Patch") {
            end_idx = Some(i);
            break;
        }
    }
    let start = start_idx.map(|i| i as isize).unwrap_or(-1);
    let end = end_idx.unwrap_or(lines.len());

    let mut current_op: Option<PatchOperation> = None;
    let mut current_hunk: Option<Hunk> = None;

    let flush_op = |operations: &mut Vec<PatchOperation>,
                    current_op: &mut Option<PatchOperation>,
                    current_hunk: &mut Option<Hunk>| {
        if let Some(mut op) = current_op.take() {
            if let Some(h) = current_hunk.take() {
                if !h.lines.is_empty() {
                    op.hunks.push(h);
                }
            }
            operations.push(op);
        } else {
            current_hunk.take();
        }
    };

    let mut i = (start + 1) as usize;
    while i < end {
        let line = lines[i];
        if let Some(m) = UPDATE_RE.captures(line) {
            flush_op(&mut operations, &mut current_op, &mut current_hunk);
            current_op = Some(PatchOperation {
                operation: OperationType::Update,
                file_path: m[1].trim().to_string(),
                new_path: None,
                hunks: Vec::new(),
            });
            current_hunk = None;
        } else if let Some(m) = ADD_RE.captures(line) {
            flush_op(&mut operations, &mut current_op, &mut current_hunk);
            current_op = Some(PatchOperation {
                operation: OperationType::Add,
                file_path: m[1].trim().to_string(),
                new_path: None,
                hunks: Vec::new(),
            });
            current_hunk = Some(Hunk::default());
        } else if let Some(m) = DELETE_RE.captures(line) {
            flush_op(&mut operations, &mut current_op, &mut current_hunk);
            operations.push(PatchOperation {
                operation: OperationType::Delete,
                file_path: m[1].trim().to_string(),
                new_path: None,
                hunks: Vec::new(),
            });
            current_op = None;
            current_hunk = None;
        } else if let Some(m) = MOVE_RE.captures(line) {
            flush_op(&mut operations, &mut current_op, &mut current_hunk);
            operations.push(PatchOperation {
                operation: OperationType::Move,
                file_path: m[1].trim().to_string(),
                new_path: Some(m[2].trim().to_string()),
                hunks: Vec::new(),
            });
            current_op = None;
            current_hunk = None;
        } else if line.starts_with("@@") {
            if let Some(op) = current_op.as_mut() {
                if let Some(h) = current_hunk.take() {
                    if !h.lines.is_empty() {
                        op.hunks.push(h);
                    }
                }
                let hint = HINT_RE.captures(line).map(|c| c[1].to_string());
                current_hunk = Some(Hunk { context_hint: hint, lines: Vec::new() });
            }
        } else if current_op.is_some() && !line.is_empty() {
            let hunk = current_hunk.get_or_insert_with(Hunk::default);
            if let Some(rest) = line.strip_prefix('+') {
                hunk.lines.push(HunkLine { prefix: '+', content: rest.to_string() });
            } else if let Some(rest) = line.strip_prefix('-') {
                hunk.lines.push(HunkLine { prefix: '-', content: rest.to_string() });
            } else if let Some(rest) = line.strip_prefix(' ') {
                hunk.lines.push(HunkLine { prefix: ' ', content: rest.to_string() });
            } else if line.starts_with('\\') {
                // "\ No newline at end of file" marker — skip.
            } else {
                // Treat as context line (implicit space prefix).
                hunk.lines.push(HunkLine { prefix: ' ', content: line.to_string() });
            }
        }
        i += 1;
    }
    flush_op(&mut operations, &mut current_op, &mut current_hunk);

    if operations.is_empty() {
        return Ok(operations);
    }

    let mut parse_errors: Vec<String> = Vec::new();
    for op in &operations {
        if op.file_path.is_empty() {
            parse_errors.push("Operation with empty file path".to_string());
        }
        if op.operation == OperationType::Update && op.hunks.is_empty() {
            parse_errors.push(format!("UPDATE '{}': no hunks found", op.file_path));
        }
        if op.operation == OperationType::Move && op.new_path.is_none() {
            parse_errors.push(format!(
                "MOVE '{}': missing destination path (expected 'src -> dst')",
                op.file_path
            ));
        }
    }
    if !parse_errors.is_empty() {
        return Err(format!("Parse error: {}", parse_errors.join("; ")));
    }
    Ok(operations)
}

fn count_occurrences(text: &str, pattern: &str) -> usize {
    if pattern.is_empty() {
        return 0;
    }
    let mut count = 0;
    let mut start = 0;
    while let Some(pos) = text[start..].find(pattern) {
        count += 1;
        start += pos + 1;
        // Advance one char forward like the Python version (start = pos + 1).
        start = crate::truncate::ceil_char_boundary(text, start);
    }
    count
}

/// Port of `_validate_operations` — no writes; simulates update hunks in order.
fn validate_operations(operations: &[PatchOperation], file_ops: &dyn V4aFileOps) -> Vec<String> {
    let mut errors: Vec<String> = Vec::new();
    let mut real_change_count = 0usize;

    for op in operations {
        if op.operation != OperationType::Update {
            real_change_count += 1;
        }
        match op.operation {
            OperationType::Update => {
                let mut simulated = match file_ops.read_file_raw(&op.file_path) {
                    Ok(c) => c,
                    Err(e) => {
                        errors.push(format!("{}: {}", op.file_path, e));
                        continue;
                    }
                };
                for (hunk_index, hunk) in op.hunks.iter().enumerate() {
                    let hunk_index = hunk_index + 1;
                    let search_lines: Vec<&str> = hunk
                        .lines
                        .iter()
                        .filter(|l| l.prefix == ' ' || l.prefix == '-')
                        .map(|l| l.content.as_str())
                        .collect();
                    let removed: Vec<&str> = hunk
                        .lines
                        .iter()
                        .filter(|l| l.prefix == '-')
                        .map(|l| l.content.as_str())
                        .collect();
                    let added: Vec<&str> = hunk
                        .lines
                        .iter()
                        .filter(|l| l.prefix == '+')
                        .map(|l| l.content.as_str())
                        .collect();
                    if removed.is_empty() && added.is_empty() {
                        // Inert anchor hunk — ignore without poisoning the patch.
                        continue;
                    }
                    real_change_count += 1;
                    if search_lines.is_empty() {
                        if let Some(hint) = &hunk.context_hint {
                            let occurrences = count_occurrences(&simulated, hint);
                            if occurrences == 0 {
                                errors.push(format!(
                                    "{}: addition-only hunk context hint '{}' not found",
                                    op.file_path, hint
                                ));
                            } else if occurrences > 1 {
                                errors.push(format!(
                                    "{}: addition-only hunk context hint '{}' is ambiguous ({} occurrences)",
                                    op.file_path, hint, occurrences
                                ));
                            }
                        }
                        continue;
                    }
                    let search_pattern = search_lines.join("\n");
                    let replacement: Vec<&str> = hunk
                        .lines
                        .iter()
                        .filter(|l| l.prefix == ' ' || l.prefix == '+')
                        .map(|l| l.content.as_str())
                        .collect();
                    let replacement = replacement.join("\n");
                    let outcome =
                        fuzzy_find_and_replace(&simulated, &search_pattern, &replacement, false);
                    if outcome.match_count == 0 {
                        let label = match &hunk.context_hint {
                            Some(h) => format!("'{}'", h),
                            None => "(no hint)".to_string(),
                        };
                        let mut msg = format!(
                            "{}: hunk {} {} not found{}",
                            op.file_path,
                            hunk_index,
                            label,
                            match &outcome.error {
                                Some(e) => format!(" — {}", e),
                                None => String::new(),
                            }
                        );
                        msg.push_str(&format_no_match_hint(
                            outcome.error.as_deref(),
                            outcome.match_count,
                            &search_pattern,
                            &simulated,
                        ));
                        errors.push(msg);
                    } else {
                        simulated = outcome.new_content;
                    }
                }
            }
            OperationType::Delete => {
                if file_ops.read_file_raw(&op.file_path).is_err() {
                    errors.push(format!("{}: file not found for deletion", op.file_path));
                }
            }
            OperationType::Move => {
                let Some(new_path) = &op.new_path else {
                    errors.push(format!("{}: MOVE operation missing destination path", op.file_path));
                    continue;
                };
                if file_ops.read_file_raw(&op.file_path).is_err() {
                    errors.push(format!("{}: source file not found for move", op.file_path));
                }
                if file_ops.read_file_raw(new_path).is_ok() {
                    errors.push(format!(
                        "{}: destination already exists — move would overwrite",
                        new_path
                    ));
                }
            }
            OperationType::Add => {}
        }
    }

    if errors.is_empty() && real_change_count == 0 {
        errors.push("Patch contains no changes (only context lines were provided)".to_string());
    }
    errors
}

/// Port of `apply_v4a_operations` — validate-then-apply.
pub fn apply_v4a_operations(operations: &[PatchOperation], file_ops: &dyn V4aFileOps) -> V4aResult {
    let validation_errors = validate_operations(operations, file_ops);
    if !validation_errors.is_empty() {
        return V4aResult {
            success: false,
            error: Some(format!(
                "Patch validation failed (no files were modified):\n{}",
                validation_errors
                    .iter()
                    .map(|e| format!("  • {}", e))
                    .collect::<Vec<_>>()
                    .join("\n")
            )),
            ..Default::default()
        };
    }

    let mut result = V4aResult::default();
    let mut all_diffs: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for op in operations {
        match op.operation {
            OperationType::Add => match apply_add(op, file_ops) {
                Ok(diff) => {
                    result.files_created.push(op.file_path.clone());
                    all_diffs.push(diff);
                }
                Err(e) => errors.push(format!("Failed to add {}: {}", op.file_path, e)),
            },
            OperationType::Delete => match apply_delete(op, file_ops) {
                Ok(diff) => {
                    result.files_deleted.push(op.file_path.clone());
                    all_diffs.push(diff);
                }
                Err(e) => errors.push(format!("Failed to delete {}: {}", op.file_path, e)),
            },
            OperationType::Move => match apply_move(op, file_ops) {
                Ok(diff) => {
                    result
                        .files_modified
                        .push(format!("{} -> {}", op.file_path, op.new_path.as_deref().unwrap_or("")));
                    all_diffs.push(diff);
                }
                Err(e) => errors.push(format!("Failed to move {}: {}", op.file_path, e)),
            },
            OperationType::Update => match apply_update(op, file_ops) {
                Ok(diff) => {
                    result.files_modified.push(op.file_path.clone());
                    all_diffs.push(diff);
                }
                Err(e) => errors.push(format!("Failed to update {}: {}", op.file_path, e)),
            },
        }
    }

    result.diff = all_diffs.join("\n");
    if !errors.is_empty() {
        result.success = false;
        result.error = Some(format!(
            "Apply phase failed (state may be inconsistent — run `git diff` to assess):\n{}",
            errors.iter().map(|e| format!("  • {}", e)).collect::<Vec<_>>().join("\n")
        ));
    } else {
        result.success = true;
    }
    result
}

fn apply_add(op: &PatchOperation, file_ops: &dyn V4aFileOps) -> Result<String, String> {
    let content_lines: Vec<&str> = op
        .hunks
        .iter()
        .flat_map(|h| h.lines.iter())
        .filter(|l| l.prefix == '+')
        .map(|l| l.content.as_str())
        .collect();
    let content = content_lines.join("\n");
    file_ops.write_file(&op.file_path, &content)?;
    let mut diff = format!("--- /dev/null\n+++ b/{}\n", op.file_path);
    diff.push_str(
        &content_lines.iter().map(|l| format!("+{}", l)).collect::<Vec<_>>().join("\n"),
    );
    Ok(diff)
}

fn apply_delete(op: &PatchOperation, file_ops: &dyn V4aFileOps) -> Result<String, String> {
    let content = file_ops
        .read_file_raw(&op.file_path)
        .map_err(|_| format!("Cannot delete {}: file not found", op.file_path))?;
    file_ops.delete_file(&op.file_path)?;
    let removed_lines = split_lines_keepends(&content);
    let empty: Vec<&str> = Vec::new();
    let sm = SequenceMatcher::new(&removed_lines, &empty);
    let mut diff = String::new();
    let mut started = false;
    for group in sm.get_grouped_opcodes(3) {
        if !started {
            started = true;
            diff.push_str(&format!("--- a/{}\n+++ /dev/null\n", op.file_path));
        }
        let first = group[0];
        let last = group[group.len() - 1];
        let r1 = format_range(first.1, last.2);
        let r2 = format_range(first.3, last.4);
        diff.push_str(&format!("@@ -{} +{} @@\n", r1, r2));
        for (tag, i1, i2, _j1, _j2) in group {
            if tag == Tag::Equal {
                for line in &removed_lines[i1..i2] {
                    diff.push(' ');
                    diff.push_str(line);
                }
            } else {
                for line in &removed_lines[i1..i2] {
                    diff.push('-');
                    diff.push_str(line);
                }
            }
        }
    }
    if diff.is_empty() {
        diff = format!("# Deleted: {}", op.file_path);
    }
    Ok(diff)
}

fn format_range(start: usize, stop: usize) -> String {
    let mut beginning = start + 1;
    let length = stop - start;
    if length == 1 {
        return format!("{}", beginning);
    }
    if length == 0 {
        beginning -= 1;
    }
    format!("{},{}", beginning, length)
}

fn apply_move(op: &PatchOperation, file_ops: &dyn V4aFileOps) -> Result<String, String> {
    let new_path = op.new_path.as_deref().unwrap_or("");
    file_ops.move_file(&op.file_path, new_path)?;
    Ok(format!("# Moved: {} -> {}", op.file_path, new_path))
}

fn apply_update(op: &PatchOperation, file_ops: &dyn V4aFileOps) -> Result<String, String> {
    let current_content = file_ops
        .read_file_raw(&op.file_path)
        .map_err(|e| format!("Cannot read file: {}", e))?;
    let mut new_content = current_content.clone();

    for hunk in &op.hunks {
        let mut search_lines: Vec<&str> = Vec::new();
        let mut replace_lines: Vec<&str> = Vec::new();
        for line in &hunk.lines {
            match line.prefix {
                ' ' => {
                    search_lines.push(&line.content);
                    replace_lines.push(&line.content);
                }
                '-' => search_lines.push(&line.content),
                '+' => replace_lines.push(&line.content),
                _ => {}
            }
        }
        if !search_lines.is_empty() && search_lines == replace_lines {
            continue;
        }
        if !search_lines.is_empty() {
            let search_pattern = search_lines.join("\n");
            let replacement = replace_lines.join("\n");
            let outcome = fuzzy_find_and_replace(&new_content, &search_pattern, &replacement, false);
            let mut error = outcome.error.clone();
            if outcome.match_count > 0 {
                new_content = outcome.new_content;
                error = None;
            } else if error.is_some() {
                // Try with context hint if available: search a window around it.
                if let Some(hint) = &hunk.context_hint {
                    if let Some(hint_pos) = new_content.find(hint.as_str()) {
                        let window_start = crate::truncate::floor_char_boundary(
                            &new_content,
                            hint_pos.saturating_sub(500),
                        );
                        let window_end = crate::truncate::ceil_char_boundary(
                            &new_content,
                            (hint_pos + 2000).min(new_content.len()),
                        );
                        let window = &new_content[window_start..window_end];
                        let retry = fuzzy_find_and_replace(window, &search_pattern, &replacement, false);
                        if retry.match_count > 0 {
                            new_content = format!(
                                "{}{}{}",
                                &new_content[..window_start],
                                retry.new_content,
                                &new_content[window_end..]
                            );
                            error = None;
                        }
                    }
                }
                if let Some(err) = error {
                    let mut err_msg = format!("Could not apply hunk: {}", err);
                    err_msg.push_str(&format_no_match_hint(
                        Some(&err),
                        0,
                        &search_pattern,
                        &new_content,
                    ));
                    return Err(err_msg);
                }
            }
        } else {
            // Addition-only hunk (no context or removed lines).
            let insert_text = replace_lines.join("\n");
            if let Some(hint) = &hunk.context_hint {
                let occurrences = count_occurrences(&new_content, hint);
                if occurrences == 0 {
                    new_content =
                        format!("{}\n{}\n", new_content.trim_end_matches('\n'), insert_text);
                } else if occurrences > 1 {
                    return Err(format!(
                        "Addition-only hunk: context hint '{}' is ambiguous ({} occurrences) — provide a more unique hint",
                        hint, occurrences
                    ));
                } else {
                    let hint_pos = new_content.find(hint.as_str()).unwrap();
                    match new_content[hint_pos..].find('\n') {
                        Some(rel_eol) => {
                            let eol = hint_pos + rel_eol;
                            new_content = format!(
                                "{}{}\n{}",
                                &new_content[..eol + 1],
                                insert_text,
                                &new_content[eol + 1..]
                            );
                        }
                        None => {
                            new_content = format!("{}\n{}", new_content, insert_text);
                        }
                    }
                }
            } else {
                new_content = format!("{}\n{}\n", new_content.trim_end_matches('\n'), insert_text);
            }
        }
    }

    file_ops.write_file(&op.file_path, &new_content)?;
    Ok(unified_diff(
        &current_content,
        &new_content,
        &format!("a/{}", op.file_path),
        &format!("b/{}", op.file_path),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    struct MemOps {
        files: RefCell<HashMap<String, String>>,
    }

    impl MemOps {
        fn new(files: &[(&str, &str)]) -> Self {
            Self {
                files: RefCell::new(
                    files.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
                ),
            }
        }
    }

    impl V4aFileOps for MemOps {
        fn read_file_raw(&self, path: &str) -> Result<String, String> {
            self.files
                .borrow()
                .get(path)
                .cloned()
                .ok_or_else(|| format!("File not found: {}", path))
        }
        fn write_file(&self, path: &str, content: &str) -> Result<(), String> {
            self.files.borrow_mut().insert(path.to_string(), content.to_string());
            Ok(())
        }
        fn delete_file(&self, path: &str) -> Result<(), String> {
            self.files.borrow_mut().remove(path);
            Ok(())
        }
        fn move_file(&self, src: &str, dst: &str) -> Result<(), String> {
            let content = self.read_file_raw(src)?;
            self.files.borrow_mut().remove(src);
            self.files.borrow_mut().insert(dst.to_string(), content);
            Ok(())
        }
    }

    #[test]
    fn parses_and_applies_update() {
        let patch = "*** Begin Patch\n*** Update File: a.txt\n@@ fn main @@\n context\n-old line\n+new line\n*** End Patch";
        let ops = parse_v4a_patch(patch).unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].operation, OperationType::Update);
        let mem = MemOps::new(&[("a.txt", " context\nold line\nafter\n")]);
        let result = apply_v4a_operations(&ops, &mem);
        assert!(result.success, "{:?}", result.error);
        assert!(mem.read_file_raw("a.txt").unwrap().contains("new line"));
        assert!(result.diff.contains("--- a/a.txt"));
        assert_eq!(result.files_modified, vec!["a.txt"]);
    }

    #[test]
    fn add_delete_move() {
        let patch = "*** Begin Patch\n*** Add File: new.txt\n+hello\n+world\n*** Delete File: gone.txt\n*** Move File: src.txt -> dst.txt\n*** End Patch";
        let ops = parse_v4a_patch(patch).unwrap();
        assert_eq!(ops.len(), 3);
        let mem = MemOps::new(&[("gone.txt", "x\n"), ("src.txt", "content\n")]);
        let result = apply_v4a_operations(&ops, &mem);
        assert!(result.success, "{:?}", result.error);
        assert_eq!(mem.read_file_raw("new.txt").unwrap(), "hello\nworld");
        assert!(mem.read_file_raw("gone.txt").is_err());
        assert_eq!(mem.read_file_raw("dst.txt").unwrap(), "content\n");
        assert_eq!(result.files_created, vec!["new.txt"]);
        assert_eq!(result.files_deleted, vec!["gone.txt"]);
    }

    #[test]
    fn validation_failure_leaves_files_untouched() {
        let patch = "*** Begin Patch\n*** Update File: a.txt\n-missing line entirely\n+replacement\n*** End Patch";
        let ops = parse_v4a_patch(patch).unwrap();
        let mem = MemOps::new(&[("a.txt", "unrelated content aa bb cc dd\n")]);
        let result = apply_v4a_operations(&ops, &mem);
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(err.starts_with("Patch validation failed (no files were modified):\n"), "{}", err);
        assert_eq!(mem.read_file_raw("a.txt").unwrap(), "unrelated content aa bb cc dd\n");
    }

    #[test]
    fn context_only_patch_rejected() {
        let patch = "*** Begin Patch\n*** Update File: a.txt\n unchanged\n*** End Patch";
        let ops = parse_v4a_patch(patch).unwrap();
        let mem = MemOps::new(&[("a.txt", "unchanged\n")]);
        let result = apply_v4a_operations(&ops, &mem);
        assert!(!result.success);
        assert!(result
            .error
            .unwrap()
            .contains("Patch contains no changes (only context lines were provided)"));
    }

    #[test]
    fn no_space_header_accepted() {
        let patch = "***Begin Patch\n***Update File: a.txt\n-x\n+y\n***End Patch";
        let ops = parse_v4a_patch(patch).unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].file_path, "a.txt");
    }

    #[test]
    fn move_missing_dest_is_parse_error() {
        // A Move regex requires "->"; without it, the line parses as a context
        // line under no current op and is dropped, giving an empty op list.
        let patch = "*** Begin Patch\n*** Update File: a.txt\n*** End Patch";
        let err = parse_v4a_patch(patch).unwrap_err();
        assert!(err.contains("UPDATE 'a.txt': no hunks found"), "{}", err);
    }
}
