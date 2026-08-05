# Comment-Style Prose

This file has prose paragraphs interspersed with semantic constructs. The CST
must classify the semantic ones and preserve the prose as Raw/Paragraph nodes.

<!-- This is an HTML comment that must round-trip verbatim. -->

## Narrative

The system under test has several moving parts. This paragraph is pure prose
and carries no Spec Kit semantic marker. It should be a CST Paragraph node
with no corresponding SemanticNode (the meaning layer does not classify prose
quality — contracts/semantic-graph.md "Non-goals").

- **FR-100**: A requirement embedded in prose-heavy context.

Another prose paragraph here. The CST must preserve it even though it matches
no semantic pattern. This is the lossless guarantee (FR-012).

<!-- trailing comment -->
