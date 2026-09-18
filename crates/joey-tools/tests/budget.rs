//! US5 — advertised tool output budgets + truncation announcement
//! (feature 034, quickstart.md US5, contracts/tool-schemas.md).

use joey_tools::ToolRegistry;

fn tool(name: &str) -> std::sync::Arc<dyn joey_tools::Tool> {
    ToolRegistry::with_builtins().get(name).unwrap_or_else(|| panic!("missing tool {name}"))
}

#[test]
fn terminal_description_advertises_output_budget() {
    let d = tool("terminal").description().to_string();
    assert!(
        d.contains("Output is truncated to the configured tool-output byte limit"),
        "{d}"
    );
}

#[test]
fn write_file_description_advertises_source_not_truncated() {
    let d = tool("write_file").description().to_string();
    assert!(d.contains("Source output is not truncated"), "{d}");
}

#[test]
fn browser_cdp_description_advertises_output_budget() {
    let d = tool("browser_cdp").description().to_string();
    assert!(
        d.contains("Results observe the configured tool-output byte limit"),
        "{d}"
    );
}

#[test]
fn truncation_announces_omitted_size() {
    let long = "a".repeat(10_000);
    let out = joey_tools::truncate::truncate_terminal_output(&long, 200);
    let head = (200_f64 * 0.4) as usize;
    let omitted = 10_000 - head - (200 - head);
    assert!(
        out.contains(&format!("... [output truncated, {omitted} chars omitted] ...")),
        "{out}"
    );
}
