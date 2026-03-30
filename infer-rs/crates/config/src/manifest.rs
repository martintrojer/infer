// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Manifest for exported textual SIL files.
//!
//! Produced by `infer debug --export-textual <dir>`. Maps original source files
//! to their exported `.sil` files and procedure lists.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// A single entry in the export manifest.
#[derive(Clone, Debug, Deserialize)]
pub struct ManifestEntry {
    /// Original source file path (e.g., "src/foo.c").
    pub source: String,
    /// Exported .sil filename (e.g., "foo.sil").
    pub sil: String,
    /// Procedure names defined in this file.
    pub procedures: Vec<String>,
}

/// Parse a `manifest.json` file and return entries with resolved .sil paths.
///
/// The `.sil` paths in the manifest are relative to the manifest's directory.
pub fn read_manifest(manifest_path: &Path) -> Result<Vec<ManifestEntry>, String> {
    let content = std::fs::read_to_string(manifest_path)
        .map_err(|e| format!("failed to read {}: {e}", manifest_path.display()))?;
    parse_manifest(&content)
}

/// Parse manifest JSON content.
pub fn parse_manifest(json: &str) -> Result<Vec<ManifestEntry>, String> {
    serde_json::from_str(json).map_err(|e| format!("failed to parse manifest: {e}"))
}

/// Resolve .sil paths relative to a base directory.
pub fn resolve_sil_paths(entries: &[ManifestEntry], base_dir: &Path) -> Vec<PathBuf> {
    entries.iter().map(|e| base_dir.join(&e.sil)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_manifest() {
        let json = r#"[
            {"source": "src/foo.c", "sil": "foo.sil", "procedures": ["main", "helper"]},
            {"source": "lib/bar.c", "sil": "bar.sil", "procedures": ["bar_init"]}
        ]"#;
        let entries = parse_manifest(json).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].source, "src/foo.c");
        assert_eq!(entries[0].sil, "foo.sil");
        assert_eq!(entries[0].procedures, vec!["main", "helper"]);
        assert_eq!(entries[1].source, "lib/bar.c");
    }

    #[test]
    fn test_resolve_sil_paths() {
        let entries = vec![
            ManifestEntry {
                source: "a.c".into(),
                sil: "a.sil".into(),
                procedures: vec![],
            },
            ManifestEntry {
                source: "b.c".into(),
                sil: "b.sil".into(),
                procedures: vec![],
            },
        ];
        let paths = resolve_sil_paths(&entries, Path::new("/tmp/out"));
        assert_eq!(paths[0], PathBuf::from("/tmp/out/a.sil"));
        assert_eq!(paths[1], PathBuf::from("/tmp/out/b.sil"));
    }
}
