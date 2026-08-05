# Code Fences

## Spec Kit project tree

```text
crates/foo/
├── Cargo.toml
└── src/
    ├── lib.rs
    └── main.rs
```

## Rust code block

```rust
fn main() {
    let x = 42;
    println!("{x}");
}
```

## GWT block

**Acceptance Scenarios:**

- **Given** a feature directory
  **When** the CST parser runs
  **Then** the file round-trips byte-for-byte.

## Fenced block with no language

```
plain
code
```

## Nested fences (tilde)

~~~text
outer
~~~

## Empty fence

```
```

## Table

| Col A | Col B |
|-------|-------|
| 1     | 2     |
| 3     | 4     |

## Inline code and edge content

Use `cargo build` to compile. The `Vec<u8>` returns.

Trailing content with   irregular spacing.
