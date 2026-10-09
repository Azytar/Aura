//! Guards release metadata against drift between manifests, the changelog,
//! and the `--version` output (which shares `CARGO_PKG_VERSION`).

use std::path::PathBuf;

fn workspace_file(name: &str) -> String {
    // This test target lives at `<root>/crates/aura-app/tests`, so two parents
    // up is the workspace root.
    let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    root.pop();
    root.pop();
    std::fs::read_to_string(root.join(name))
        .unwrap_or_else(|_| panic!("workspace file {name} must be readable"))
}

fn manifest_version(path: &str) -> String {
    workspace_file(path)
        .lines()
        .find_map(|line| {
            let stripped = line.trim();
            stripped
                .strip_prefix("version = \"")
                .and_then(|rest| rest.strip_suffix('"'))
                .map(str::to_owned)
        })
        .unwrap_or_else(|| panic!("{path} must declare a version"))
}

#[test]
fn workspace_versions_changelog_and_binary_agree() {
    let version = env!("CARGO_PKG_VERSION");
    for member in [
        "crates/aura-app/Cargo.toml",
        "crates/aura-config/Cargo.toml",
        "crates/aura-core/Cargo.toml",
        "crates/aura-pty/Cargo.toml",
        "crates/aura-render/Cargo.toml",
        "crates/aura-text/Cargo.toml",
    ] {
        assert_eq!(
            manifest_version(member),
            version,
            "{member} drifted from {version}"
        );
    }
    let changelog = workspace_file("CHANGELOG.md");
    assert!(
        changelog.contains(&format!("## [{version}]")),
        "CHANGELOG must contain an entry for version {version}"
    );
    let readme = workspace_file("README.md");
    assert!(
        readme.contains(version),
        "README must reference the current version {version}"
    );
}
