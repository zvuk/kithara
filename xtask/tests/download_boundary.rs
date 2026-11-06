use std::collections::HashSet;

use cargo_metadata::{DependencyKind, MetadataCommand};

#[path = "../src/testing/workspace.rs"]
mod workspace;

#[test]
fn download_dependency_closure_excludes_stream_protocols_and_facade() {
    let root = workspace::root();
    let metadata = MetadataCommand::new()
        .manifest_path(root.join("Cargo.toml"))
        .exec()
        .expect("workspace dependency graph resolves");
    let download = metadata
        .packages
        .iter()
        .find(|package| package.name == "kithara-download")
        .expect("download is an independent workspace package");
    let graph = metadata
        .resolve
        .as_ref()
        .expect("resolved dependency graph");
    let mut pending = vec![&download.id];
    let mut visited = HashSet::new();
    while let Some(id) = pending.pop() {
        if !visited.insert(id) {
            continue;
        }
        let package = &metadata[id];
        assert!(
            !matches!(
                package.name.as_str(),
                "kithara" | "kithara-stream" | "kithara-file" | "kithara-hls"
            ),
            "download must not depend on {} (including through test dependencies)",
            package.name
        );
        let node = graph
            .nodes
            .iter()
            .find(|node| &node.id == id)
            .expect("node");
        pending.extend(
            node.deps
                .iter()
                .filter(|dep| {
                    id == &download.id
                        || dep
                            .dep_kinds
                            .iter()
                            .any(|kind| kind.kind != DependencyKind::Development)
                })
                .map(|dep| &dep.pkg),
        );
    }
}
