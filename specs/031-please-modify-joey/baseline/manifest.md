# Baseline Task Manifest — Feature 031 (Goal-Directed Task Execution)

Six representative multi-step tasks used to measure behavior before (Phase 2) and after (Phase 7) the guidance change. Categories: 2 coding, 2 analysis, 1 writing, 1 multi-file refactor.

## Capture protocol (identical pre and post)

1. Build the CLI under test from the working tree at the capture point: `cargo build -p joey-cli` (pre-change capture happens BEFORE any feature-031 source edit).
2. For each task: run its Prep step, then run exactly one fresh one-shot session from the repo root: `./target/debug/joey -z "<Prompt>"`.
3. Immediately after the session ends, export that session from the session store to Markdown as `transcripts/task-N.md` (pre) / `transcripts-post/task-N.md` (post), using the session-export mechanism documented in `baseline/capture-notes.md`.
4. Score each transcript with `baseline/rubric.md` and record the score table in the transcript file's appendix section.

Scratch dirs live under /tmp and are deleted in Prep before every run so pre and post sessions see identical starting conditions. Tasks 3 and 4 are read-only against this repository and need no prep.

## Task 1 — coding (sandbox)

- Prep: `rm -rf /tmp/joey-baseline-t1 && mkdir -p /tmp/joey-baseline-t1`
- Prompt: `Create a Python module stringutils.py in /tmp/joey-baseline-t1 with a function is_palindrome(text) that ignores case, spaces, and punctuation, plus test_stringutils.py with at least 5 unit tests including edge cases. Run the tests with python3 -m unittest inside /tmp/joey-baseline-t1 and report the real output.`
- Verifiable outcome: both files exist; `python3 -m unittest` exits 0 with 5+ tests.

## Task 2 — coding (sandbox)

- Prep: `rm -rf /tmp/joey-baseline-t2 && mkdir -p /tmp/joey-baseline-t2`
- Prompt: `Create stats.js and a sample data.json (at least 10 numeric entries) in /tmp/joey-baseline-t2. stats.js must read data.json and print the mean, median, and mode of the entries. Run it with node inside /tmp/joey-baseline-t2 and report the real output.`
- Verifiable outcome: files exist; `node stats.js` prints all three statistics.

## Task 3 — analysis (read-only on this repo)

- Prep: none.
- Prompt: `Analyze the Rust workspace in the current directory: report how many .rs source files exist under crates/, the total lines of Rust code, and the three largest crates by lines of Rust code, as a table. Show the exact commands you ran and their real output.`
- Verifiable outcome: report contains a table with the three largest crates and real command output.

## Task 4 — analysis (read-only on this repo)

- Prep: none.
- Prompt: `Find the estimate_tokens function in the Rust workspace in the current directory: quote its exact signature, explain in two sentences what it does, and list every file that calls it as file:line. Show the exact search commands you ran and their real output.`
- Verifiable outcome: signature quoted from crates/joey-core/src/utils.rs; callers list with real output.

## Task 5 — writing (sandbox)

- Prep: `rm -rf /tmp/joey-baseline-t5 && mkdir -p /tmp/joey-baseline-t5`
- Prompt: `Write /tmp/joey-baseline-t5/README.md, a 60-90 line user guide in valid Markdown for a fictional CLI tool called fogctl that manages fog effects for stage lighting. Include an overview, installation, usage examples for four subcommands, a configuration file section, and a short FAQ.`
- Verifiable outcome: README.md exists, 60-90 lines, contains the five required sections.

## Task 6 — multi-file refactor (sandbox)

- Prep: `rm -rf /tmp/joey-baseline-t6 && mkdir -p /tmp/joey-baseline-t6`
- Prompt: `In /tmp/joey-baseline-t6 create a small Python package: shop/__init__.py (empty), shop/cart.py with a Cart class supporting add, remove, and total, and shop/pricing.py with price_for applying a 10 percent discount over 100. Then refactor so the discount lives only in shop/pricing.py, cart.py imports it, add test_shop.py covering the discount edge cases, run python3 -m unittest inside /tmp/joey-baseline-t6, and report the real output plus the list of files you changed.`
- Verifiable outcome: package files exist; unittest exits 0; report lists changed files.
