---
name: rust-review
description: Review Rust code for correctness, idiom, and error handling before finishing a change.
version: 1.0.0
license: MIT
platforms: [linux, macos, windows]
metadata:
  joey:
    tags: [rust, code-review, quality]
---

# Rust review

When you have finished editing Rust code, run this checklist before reporting done.

## Steps

1. **Build and test.** Run `cargo build` then `cargo test` in the affected crate.
   Do not report success until both pass. Paste the failing output if they don't.
2. **Error handling.** Every `Result` is either propagated with `?`, matched, or
   deliberately `unwrap`/`expect`-ed with a comment saying why it cannot fail. No
   silent `let _ = ...` on a fallible call whose failure matters.
3. **No stray `unwrap()` on external input.** File reads, parses, and network calls
   must handle the error path.
4. **Idiom.** Prefer iterators over index loops, `&str` over `String` in signatures,
   and match the surrounding code's naming and module conventions.
5. **Clippy.** If `cargo clippy` is available, run it and address warnings on the
   changed lines.

## When to use

Load this whenever you have written or modified `.rs` files and are about to finish
the task.
