//! E2E regression test: ingest a mixed-language project through the full
//! pipeline (walk → registry dispatch → per-language extractors → graph
//! upserts) and assert that every tree-sitter-supported language added in
//! the multi-language expansion produces the expected structural nodes.

use joey_neurocode::graph::DependencyGraph;
use joey_neurocode::parse::ingest_project;

fn setup_project(root: &std::path::Path) {
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();

    std::fs::write(
        src.join("user_service.rb"),
        r#"
require "json"

class UserService < BaseService
  def find(id)
    @store[id]
  end
end
"#,
    )
    .unwrap();

    std::fs::write(
        src.join("engine.php"),
        r#"
<?php
namespace App;
use App\Repositories\UserRepository;

class Engine implements EngineInterface {
  private UserRepository $repo;
  public function run(): void { }
}
"#,
    )
    .unwrap();

    std::fs::write(
        src.join("widget.cs"),
        r#"
namespace UI;
public class Widget : IWidget {
    public void Render() { }
}
"#,
    )
    .unwrap();

    std::fs::write(
        src.join("core.cpp"),
        r#"
#include "base.h"
namespace core {
class Core : public Base {
public:
    void Start();
};
}
"#,
    )
    .unwrap();

    std::fs::write(
        src.join("deploy.sh"),
        "#!/bin/bash\nfunction deploy() {\n  echo hi\n}\n",
    )
    .unwrap();

    std::fs::write(
        src.join("point.c"),
        "#include \"math.h\"\nstruct Point { int x; };\nint compute(int a) { return a; }\n",
    )
    .unwrap();
}

fn find_node(graph: &DependencyGraph, term: &str, fq_suffix: &str) -> bool {
    graph
        .query_fts(term, 10)
        .unwrap()
        .iter()
        .any(|n| n.fqcn == fq_suffix || n.fqcn.ends_with(fq_suffix))
}

#[test]
fn ingest_mixed_language_project() {
    let tmp = tempfile::tempdir().unwrap();
    setup_project(tmp.path());

    let db = tmp.path().join("graph.db");
    let graph = DependencyGraph::open(&db).unwrap();
    let result = ingest_project(&graph, &tmp.path().join("src").parent().unwrap());

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.files_scanned, 6);

    // Ruby: class + method.
    assert!(find_node(&graph, "UserService", "UserService"));
    assert!(find_node(&graph, "UserService", "UserService.find()"));
    // PHP: namespaced class + field + method.
    assert!(find_node(&graph, "Engine", "App.Engine"));
    assert!(find_node(&graph, "Engine", "App.Engine.run()"));
    assert!(find_node(&graph, "Engine", "App.Engine.repo"));
    // C#: namespaced class + method.
    assert!(find_node(&graph, "Widget", "UI.Widget"));
    assert!(find_node(&graph, "Widget", "UI.Widget.Render()"));
    // C++: namespaced class + method.
    assert!(find_node(&graph, "Core", "core.Core"));
    assert!(find_node(&graph, "Core", "core.Core.Start()"));
    // C: struct + field + free function.
    assert!(find_node(&graph, "Point", "Point"));
    assert!(find_node(&graph, "Point", "Point.x"));
    assert!(find_node(&graph, "compute", "compute"));
    // Bash: function.
    assert!(find_node(&graph, "deploy", "deploy"));
}
