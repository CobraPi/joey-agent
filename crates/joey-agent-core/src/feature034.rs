//! Feature 034 (context assembly improvements) — story tests.
//!
//! Mirrors compression/loop_tests.rs: a scripted Transport drives run_turn
//! and captures outgoing ProviderRequests; assertions run on captured wire
//! payloads. One submodule per story so quickstart.md filters work:
//! `cargo test -p joey-agent-core gauge` etc.

#![allow(clippy::await_holding_lock)]

mod support;

mod gauge;

mod reasoning_prune;

mod notice;

mod compaction_framing;

mod prefix;
