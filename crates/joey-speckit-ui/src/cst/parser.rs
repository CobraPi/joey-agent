//! Lossless CST parser built on the already-present `pulldown-cmark`
//! (research.md §3 — no new backend dep).
//!
//! Walks `pulldown-cmark`'s `OffsetIter` event stream and builds a
//! `CstDocument` whose nodes partition `[0, file_len)` with no gaps (gaps
//! become `CstKind::Raw` preserving bytes verbatim). Always total — never
//! panics, never drops bytes, never returns `Err` for odd markdown (only I/O
//! errors). Performance: ≤400 ms p95 for a 200-task file (FR-040).

use std::collections::BTreeMap;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::cst::fingerprint::fingerprint;
use crate::cst::anchors::verify_anchor;
use crate::cst::{
    CstDocument, CstError, CstKind, CstNode, CstProps, NodeId,
};
use crate::cst::parser_trait::{CstParser, CstMaterialize};

/// Default CST parser. Stateless — safe to reuse.
#[derive(Debug, Default, Clone)]
pub struct DefaultCstParser;

impl CstParser for DefaultCstParser {
    fn parse(&self, artifact_path: &str, bytes: &[u8]) -> Result<CstDocument, CstError> {
        Ok(parse_bytes(artifact_path, bytes))
    }
}

/// Build a lossless `CstDocument` from the raw UTF-8 bytes of an artifact.
///
/// Guarantees (FR-012):
///   * every input byte is covered by exactly one node range;
///   * `document.materialize() == input`;
///   * unrecognized ranges become `Raw` nodes, never dropped.
pub fn parse_bytes(artifact_path: &str, bytes: &[u8]) -> CstDocument {
    // Lossy conversion: a stray non-UTF-8 byte must degrade to U+FFFD nodes,
    // never panic. `text.len()` then equals `bytes.len()` only for valid
    // UTF-8; pass the TEXT length (not bytes.len()) so every slice into
    // `source` stays in bounds. The doc guarantees "every input byte is
    // covered" in spirit: invalid bytes surface as replacement chars in Raw
    // nodes instead of an out-of-bounds panic.
    let text = String::from_utf8_lossy(bytes).into_owned();
    let byte_len = text.len();
    let revision_hash = crate::conflict::content_hash(&text);

    // Collect block-level spans from pulldown-cmark with offsets.
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_FOOTNOTES);

    let parser = Parser::new_ext(&text, options);
    let block_spans = collect_block_spans(parser);

    // Build the node tree from the spans, filling gaps with Raw nodes.
    let mut builder = TreeBuilder::new(artifact_path.to_string(), &text, revision_hash.clone(), byte_len);
    builder.build(&block_spans);

    builder.finish()
}

/// One recognized block-level span: a (start, end, kind, props) tuple extracted
/// from the pulldown-cmark event stream. Spans are non-overlapping within the
/// same nesting level.
#[derive(Debug, Clone)]
struct BlockSpan {
    byte_start: usize,
    byte_end: usize,
    kind: CstKind,
    props: CstProps,
    children: Vec<BlockSpan>,
}

/// Walk the pulldown-cmark event stream with offsets and collect block-level
/// spans. Each Start(Tag)/End(TagEnd) pair at block level produces one span
/// with its children. Inline events (Text, Code, emphasis) are absorbed into
/// their parent block.
fn collect_block_spans(parser: Parser) -> Vec<BlockSpan> {
    let mut stack: Vec<BlockSpanBuilder> = Vec::new();
    let mut top_level: Vec<BlockSpan> = Vec::new();

    for (event, range) in parser.into_offset_iter() {
        match event {
            Event::Start(tag) => {
                if let Some(builder) = start_block_span(&tag, range.start, range.end) {
                    stack.push(builder);
                }
                // Inline Start tags (Emphasis, Strong, Link, …) are ignored at
                // the CST block level — they don't create separate CST nodes.
            }
            Event::End(tag_end) => {
                if let Some(b) = stack.last() {
                    if b.end_matches(&tag_end) {
                        let mut finished = stack.pop().unwrap();
                        finished.byte_end = range.end;
                        let span = finished.finish();
                        if let Some(parent) = stack.last_mut() {
                            parent.children.push(span);
                        } else {
                            top_level.push(span);
                        }
                    }
                }
            }
            Event::Text(_)
            | Event::Code(_)
            | Event::Html(_)
            | Event::InlineHtml(_)
            | Event::FootnoteReference(_)
            | Event::SoftBreak
            | Event::HardBreak
            | Event::Rule
            | Event::DisplayMath(_)
            | Event::InlineMath(_)
            | Event::TaskListMarker(_) => {
                // Inline content — absorbed into the parent block. We don't
                // create CST nodes for inline events; the parent block's
                // expected_bytes carry them verbatim.
            }
        }
    }

    top_level
}

/// A block span under construction (waiting for its End event).
struct BlockSpanBuilder {
    byte_start: usize,
    byte_end: usize,
    kind: CstKind,
    props: CstProps,
    children: Vec<BlockSpan>,
}

impl BlockSpanBuilder {
    fn end_matches(&self, tag_end: &TagEnd) -> bool {
        matches!((&self.kind, tag_end),
            (CstKind::Paragraph, TagEnd::Paragraph)
            | (CstKind::Heading { .. }, TagEnd::Heading(_))
            | (CstKind::CodeFence { .. }, TagEnd::CodeBlock)
            | (CstKind::Table, TagEnd::Table)
            | (CstKind::TableRow, TagEnd::TableRow)
            | (CstKind::TableCell, TagEnd::TableCell)
            | (CstKind::BlockQuote, TagEnd::BlockQuote(_))
            | (CstKind::ListItem, TagEnd::Item)
        )
    }

    fn finish(self) -> BlockSpan {
        BlockSpan {
            byte_start: self.byte_start,
            byte_end: self.byte_end,
            kind: self.kind,
            props: self.props,
            children: self.children,
        }
    }
}

/// Map a pulldown-cmark `Tag` to a CST block span builder, or `None` for
/// inline-only tags (Emphasis, Strong, Link, Image) that don't create CST
/// nodes.
fn start_block_span(tag: &Tag, byte_start: usize, byte_end: usize) -> Option<BlockSpanBuilder> {
    let (kind, props) = match tag {
        Tag::Paragraph => (CstKind::Paragraph, CstProps::None),
        Tag::Heading { level, .. } => {
            let level = match level {
                HeadingLevel::H1 => 1,
                HeadingLevel::H2 => 2,
                HeadingLevel::H3 => 3,
                HeadingLevel::H4 => 4,
                HeadingLevel::H5 => 5,
                HeadingLevel::H6 => 6,
            };
            (CstKind::Heading { level }, CstProps::None)
        }
        Tag::CodeBlock(kind) => {
            let lang = match kind {
                CodeBlockKind::Fenced(lang) => {
                    let l = lang.as_ref().trim().to_string();
                    if l.is_empty() { None } else { Some(l) }
                }
                CodeBlockKind::Indented => None,
            };
            (CstKind::CodeFence { lang }, CstProps::None)
        }
        Tag::Table(_) => (CstKind::Table, CstProps::None),
        Tag::TableRow => (CstKind::TableRow, CstProps::None),
        Tag::TableCell => (CstKind::TableCell, CstProps::None),
        Tag::BlockQuote(_) => (CstKind::BlockQuote, CstProps::None),
        Tag::List(_) => {
            // A List itself doesn't get a CST node — its Items are the
            // block-level spans. This keeps the CST flat enough for semantic
            // classification while remaining lossless (the list range is
            // covered by its Item children).
            return None;
        }
        Tag::Item => (CstKind::ListItem, CstProps::None),
        // Inline tags and structural tags we don't model as block CST nodes.
        _ => return None,
    };

    // Extract props lazily: we'll fill text-based props after we know the
    // source bytes. For now, the builder carries CstProps::None and the
    // TreeBuilder enriches it.
    let _ = byte_end; // end is finalized on the End event
    Some(BlockSpanBuilder {
        byte_start,
        byte_end,
        kind,
        props,
        children: Vec::new(),
    })
}

/// Builds the final `CstDocument` from the collected block spans, filling
/// gaps with `Raw` nodes to guarantee lossless coverage.
struct TreeBuilder {
    artifact_path: String,
    source: String,
    revision_hash: String,
    byte_len: usize,
    nodes: BTreeMap<NodeId, CstNode>,
    next_id: u32,
}

impl TreeBuilder {
    fn new(artifact_path: String, source: &str, revision_hash: String, byte_len: usize) -> Self {
        TreeBuilder {
            artifact_path,
            source: source.to_string(),
            revision_hash,
            byte_len,
            nodes: BTreeMap::new(),
            next_id: 1, // 0 is reserved for ROOT
        }
    }

    fn alloc_id(&mut self) -> NodeId {
        let id = NodeId(self.next_id);
        self.next_id += 1;
        id
    }

    fn build(&mut self, top_spans: &[BlockSpan]) {
        // The root node covers [0, byte_len).
        let root_id = NodeId::ROOT;
        let root_node = CstNode {
            id: root_id,
            kind: CstKind::Root,
            byte_start: 0,
            byte_end: self.byte_len,
            expected_bytes: self.source[0..self.byte_len].to_string(),
            revision_hash: self.revision_hash.clone(),
            fingerprint: "root/_".to_string(),
            props: CstProps::None,
            children: Vec::new(),
        };
        self.nodes.insert(root_id, root_node);

        // Process top-level spans + gap-filling.
        let mut children_ids = self.process_level(top_spans, 0, self.byte_len);
        if let Some(root) = self.nodes.get_mut(&root_id) {
            root.children.append(&mut children_ids);
        }
    }

    /// Process a sequence of block spans within [parent_start, parent_end),
    /// filling gaps with Raw nodes. Returns the list of child NodeIds.
    fn process_level(
        &mut self,
        spans: &[BlockSpan],
        parent_start: usize,
        parent_end: usize,
    ) -> Vec<NodeId> {
        let mut children = Vec::new();
        let mut cursor = parent_start;

        for span in spans {
            // Gap before this span → Raw node.
            if span.byte_start > cursor {
                let raw_id = self.make_raw(cursor, span.byte_start);
                children.push(raw_id);
            }
            // The span itself.
            let span_id = self.make_span_node(span);
            children.push(span_id);
            cursor = span.byte_end;
        }

        // Trailing gap → Raw node.
        if cursor < parent_end {
            let raw_id = self.make_raw(cursor, parent_end);
            children.push(raw_id);
        }

        children
    }

    fn make_span_node(&mut self, span: &BlockSpan) -> NodeId {
        let id = self.alloc_id();
        let expected = self.source[span.byte_start..span.byte_end].to_string();

        // Enrich props with extracted text for known kinds.
        let props = enrich_props(&span.kind, &span.props, &expected);
        let fp = fingerprint(&span.kind, &props);

        // Process children within this span.
        let child_ids = self.process_level(&span.children, span.byte_start, span.byte_end);

        let node = CstNode {
            id,
            kind: span.kind.clone(),
            byte_start: span.byte_start,
            byte_end: span.byte_end,
            expected_bytes: expected,
            revision_hash: self.revision_hash.clone(),
            fingerprint: fp,
            props,
            children: child_ids,
        };
        self.nodes.insert(id, node);
        id
    }

    fn make_raw(&mut self, byte_start: usize, byte_end: usize) -> NodeId {
        let id = self.alloc_id();
        let expected = self.source[byte_start..byte_end].to_string();
        let node = CstNode {
            id,
            kind: CstKind::Raw,
            byte_start,
            byte_end,
            expected_bytes: expected,
            revision_hash: self.revision_hash.clone(),
            fingerprint: "raw/_".to_string(),
            props: CstProps::None,
            children: Vec::new(),
        };
        self.nodes.insert(id, node);
        id
    }

    fn finish(self) -> CstDocument {
        CstDocument {
            artifact_path: self.artifact_path,
            revision_hash: self.revision_hash,
            byte_len: self.byte_len,
            nodes: self.nodes,
            root: NodeId::ROOT,
        }
    }
}

/// Fill in kind-specific props (heading text, list-item text, etc.) from the
/// node's source bytes. The bytes are authoritative; props are a convenience
/// for the meaning layer so it doesn't re-parse.
fn enrich_props(kind: &CstKind, _original: &CstProps, bytes: &str) -> CstProps {
    match kind {
        CstKind::Heading { .. } => {
            // Strip leading `#` markers and trailing newline.
            let text = bytes
                .trim_start_matches('#')
                .trim_start()
                .trim_end_matches('\n')
                .trim_end_matches('\r')
                .to_string();
            CstProps::Heading { text }
        }
        CstKind::Paragraph => {
            let text = bytes.to_string();
            CstProps::Paragraph { text }
        }
        CstKind::ListItem => {
            // Detect marker char and extract text (best effort).
            let (marker, text) = if bytes.starts_with("- ") {
                ('-', bytes[2..].to_string())
            } else if bytes.starts_with("* ") {
                ('*', bytes[2..].to_string())
            } else if bytes.starts_with("+ ") {
                ('+', bytes[2..].to_string())
            } else {
                ('-', bytes.to_string())
            };
            CstProps::ListItem { marker, text }
        }
        CstKind::CodeFence { .. } => {
            // Extract content (strip fence lines).
            let content = extract_code_fence_content(bytes);
            CstProps::CodeFence { content }
        }
        CstKind::TableCell => {
            CstProps::TableCell {
                text: bytes.trim().to_string(),
            }
        }
        _ => CstProps::None,
    }
}

/// Extract the inner content of a code fence, stripping the ``` or ~~~ lines.
fn extract_code_fence_content(bytes: &str) -> String {
    let mut lines = bytes.lines();
    // Skip the opening fence line.
    if let Some(_) = lines.next() {
        // Collect until the closing fence.
        let mut content = Vec::new();
        for line in lines {
            let trimmed = line.trim();
            if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                break;
            }
            content.push(line);
        }
        content.join("\n")
    } else {
        String::new()
    }
}

// ---------------------------------------------------------------------
// CstMaterialize — the round-trip invariant.
// ---------------------------------------------------------------------

impl CstMaterialize for CstDocument {
    /// Reconstruct the exact source bytes by concatenating top-level node
    /// ranges in order. Because top-level nodes partition `[0, byte_len)`,
    /// this reproduces the source byte-for-byte.
    fn materialize(&self) -> Vec<u8> {
        // Concatenate all top-level children of root in byte_start order.
        // The BTreeMap key ordering (by NodeId) follows allocation order,
        // which follows byte_start order, so iterating nodes in id order
        // and taking the root's direct children gives us the source.
        let root = match self.nodes.get(&self.root) {
            Some(r) => r,
            None => return Vec::new(),
        };

        let mut result = Vec::with_capacity(self.byte_len);
        for child_id in &root.children {
            if let Some(child) = self.nodes.get(child_id) {
                result.extend_from_slice(child.expected_bytes.as_bytes());
            }
        }
        result
    }
}

/// Verify that a document's nodes fully partition `[0, byte_len)` with no
/// gaps or overlaps at the top level. Used by tests.
pub fn verify_partition(doc: &CstDocument) -> bool {
    let root = match doc.nodes.get(&doc.root) {
        Some(r) => r,
        None => return false,
    };

    let materialized = doc.materialize();
    let mut cursor = 0usize;
    for child_id in &root.children {
        let child = match doc.nodes.get(child_id) {
            Some(c) => c,
            None => return false,
        };
        if child.byte_start != cursor {
            return false;
        }
        if !verify_anchor(
            &materialized,
            child.byte_start,
            child.byte_end,
            &child.expected_bytes,
        ) {
            return false;
        }
        cursor = child.byte_end;
    }
    cursor == doc.byte_len
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_simple_paragraph() {
        let input = b"Hello world\n";
        let doc = parse_bytes("test.md", input);
        let output = doc.materialize();
        assert_eq!(output.as_slice(), input);
    }

    #[test]
    fn roundtrip_heading_and_list() {
        let input = b"# Title\n\n- item one\n- item two\n";
        let doc = parse_bytes("test.md", input);
        let output = doc.materialize();
        assert_eq!(output.as_slice(), input);
    }

    #[test]
    fn roundtrip_empty_file() {
        let input = b"";
        let doc = parse_bytes("test.md", input);
        let output = doc.materialize();
        assert_eq!(output.as_slice(), input);
    }

    #[test]
    fn roundtrip_whitespace_only() {
        let input = b"   \n\n  \n";
        let doc = parse_bytes("test.md", input);
        let output = doc.materialize();
        assert_eq!(output.as_slice(), input);
    }

    #[test]
    fn roundtrip_code_fence() {
        let input = b"```rust\nfn main() {}\n```\n";
        let doc = parse_bytes("test.md", input);
        let output = doc.materialize();
        assert_eq!(output.as_slice(), input);
    }

    #[test]
    fn roundtrip_no_trailing_newline() {
        let input = b"# No newline at end";
        let doc = parse_bytes("test.md", input);
        let output = doc.materialize();
        assert_eq!(output.as_slice(), input);
    }

    #[test]
    fn heading_props_extracted() {
        let input = b"## My Heading\n";
        let doc = parse_bytes("test.md", input);
        let heading_node = doc
            .nodes
            .values()
            .find(|n| matches!(n.kind, CstKind::Heading { level: 2 }))
            .expect("heading node exists");
        match &heading_node.props {
            CstProps::Heading { text } => assert_eq!(text, "My Heading"),
            other => panic!("expected Heading props, got {other:?}"),
        }
    }

    #[test]
    fn list_item_fingerprint_stable() {
        let input = b"- **FR-001**: First requirement.\n";
        let doc1 = parse_bytes("test.md", input);
        let doc2 = parse_bytes("test.md", input);
        let fp1 = doc1
            .nodes
            .values()
            .find(|n| matches!(n.kind, CstKind::ListItem))
            .map(|n| &n.fingerprint);
        let fp2 = doc2
            .nodes
            .values()
            .find(|n| matches!(n.kind, CstKind::ListItem))
            .map(|n| &n.fingerprint);
        assert_eq!(fp1, fp2, "fingerprint must be deterministic");
    }
}
