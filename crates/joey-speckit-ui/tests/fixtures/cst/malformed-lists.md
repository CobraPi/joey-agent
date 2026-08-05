# Malformed: Lists

This file exercises list edge cases that the CST must preserve verbatim.

   - indented bullet (3 spaces, unusual)

* star bullet

- nested
  - child
    - grandchild with   extra   spaces

- [ ] unchecked task with [brackets] in text
- [x] checked task
- [X] uppercase-X checked

- list
  item spanning
  multiple lines

-- not really a bullet

   -mixed marker (no space after dash)

> - blockquoted bullet
> - another

1. ordered item (not a bullet list in CommonMark sense, but preserved)
2. second ordered

- trailing tab	inside text
