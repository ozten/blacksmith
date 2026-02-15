//! Module boundary detection for Rust codebases.
//!
//! Given a file tree (list of `.rs` files), identifies logical module boundaries —
//! groups of files that form a cohesive unit (e.g., `src/adapters/` is the "adapters" module).
//! Each detected module includes its name, root path, contained files, and whether it
//! has a canonical entry point (`mod.rs` or `lib.rs`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A detected module boundary in the codebase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    /// Module name (e.g., "adapters", "db"). Top-level is "crate".
    pub name: String,
    /// Root directory of this module (e.g., `src/adapters/`). For the crate root, this is `src/`.
    pub root_path: PathBuf,
    /// All `.rs` files contained in this module (direct children only, not nested submodules).
    pub files: Vec<PathBuf>,
    /// Whether the module has a canonical entry point (`mod.rs` or `lib.rs`).
    pub has_entry_point: bool,
    /// The entry point file if one exists (`mod.rs`, `lib.rs`, or `main.rs`).
    pub entry_point: Option<PathBuf>,
    /// Names of direct child submodules.
    pub submodules: Vec<String>,
}

/// Detect module boundaries from a list of `.rs` files under a source root.
///
/// Groups files by their parent directory, treating each directory as a module.
/// The `src/` directory itself is the crate root module.
///
/// # Arguments
/// * `src_root` - The `src/` directory of the Rust project
/// * `rs_files` - All `.rs` files found under `src_root`
///
/// # Returns
/// A map from module name to `Module` struct. The crate root is keyed as `"crate"`.
pub fn detect_modules(src_root: &Path, rs_files: &[PathBuf]) -> HashMap<String, Module> {
    // Group files by their parent directory
    let mut dir_files: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();

    for file in rs_files {
        if let Some(parent) = file.parent() {
            dir_files
                .entry(parent.to_path_buf())
                .or_default()
                .push(file.clone());
        }
    }

    let mut modules: HashMap<String, Module> = HashMap::new();

    for (dir, mut files) in dir_files {
        files.sort();

        let module_name = if dir == src_root {
            "crate".to_string()
        } else {
            module_name_from_path(src_root, &dir)
        };

        let entry_point = find_entry_point(&dir, &files);
        let has_entry_point = entry_point.is_some();

        // Detect direct child submodules: subdirectories that also appear as modules
        let submodules = detect_submodules(&dir, rs_files);

        modules.insert(
            module_name.clone(),
            Module {
                name: module_name,
                root_path: dir,
                files,
                has_entry_point,
                entry_point,
                submodules,
            },
        );
    }

    modules
}

/// Derive a module name from a directory path relative to src_root.
///
/// e.g., `src/adapters/claude` → `"adapters::claude"`
fn module_name_from_path(src_root: &Path, dir: &Path) -> String {
    match dir.strip_prefix(src_root) {
        Ok(rel) => rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("::"),
        Err(_) => dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string()),
    }
}

/// Find the canonical entry point for a module directory.
///
/// Looks for `mod.rs`, `lib.rs`, or `main.rs` (in that priority order).
fn find_entry_point(dir: &Path, files: &[PathBuf]) -> Option<PathBuf> {
    let candidates = ["mod.rs", "lib.rs", "main.rs"];
    for candidate in &candidates {
        let path = dir.join(candidate);
        if files.contains(&path) {
            return Some(path);
        }
    }
    None
}

/// Detect direct child submodules of a directory.
///
/// A subdirectory is a submodule if it contains at least one `.rs` file.
fn detect_submodules(dir: &Path, rs_files: &[PathBuf]) -> Vec<String> {
    let mut submodule_names: Vec<String> = Vec::new();

    for file in rs_files {
        if let Some(parent) = file.parent() {
            // Check if this file's parent is a direct child directory of `dir`
            if parent != dir {
                if let Some(grandparent) = parent.parent() {
                    if grandparent == dir {
                        if let Some(name) = parent.file_name() {
                            let name_str = name.to_string_lossy().to_string();
                            if !submodule_names.contains(&name_str) {
                                submodule_names.push(name_str);
                            }
                        }
                    }
                }
            }
        }
    }

    submodule_names.sort();
    submodule_names
}

/// Build a complete module tree from a repo root.
///
/// Convenience function that collects `.rs` files and detects modules in one call.
pub fn detect_modules_from_repo(repo_root: &Path) -> HashMap<String, Module> {
    let src_root = repo_root.join("src");
    let rs_files = collect_rs_files_for_modules(&src_root);
    detect_modules(&src_root, &rs_files)
}

/// Recursively collect all `.rs` files under a directory.
fn collect_rs_files_for_modules(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_recursive(dir, &mut files);
    files.sort();
    files
}

fn collect_recursive(dir: &Path, files: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_recursive(&path, files);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            files.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Helper: create a temp Rust project with given files.
    fn setup_project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        for (path, content) in files {
            let full = src.join(path);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&full, content).unwrap();
        }
        tmp
    }

    #[test]
    fn empty_project_no_modules() {
        let tmp = setup_project(&[]);
        let modules = detect_modules_from_repo(tmp.path());
        assert!(modules.is_empty());
    }

    #[test]
    fn single_main_file() {
        let tmp = setup_project(&[("main.rs", "fn main() {}")]);
        let modules = detect_modules_from_repo(tmp.path());

        assert_eq!(modules.len(), 1);
        let crate_mod = &modules["crate"];
        assert_eq!(crate_mod.name, "crate");
        assert!(crate_mod.has_entry_point);
        assert!(crate_mod.entry_point.as_ref().unwrap().ends_with("main.rs"));
        assert_eq!(crate_mod.files.len(), 1);
    }

    #[test]
    fn crate_with_sibling_modules() {
        let tmp = setup_project(&[
            ("main.rs", "mod config;\nmod db;\nfn main() {}"),
            ("config.rs", "pub struct Config;"),
            ("db.rs", "pub struct Database;"),
        ]);
        let modules = detect_modules_from_repo(tmp.path());

        assert_eq!(modules.len(), 1); // all in src/, so one module
        let crate_mod = &modules["crate"];
        assert_eq!(crate_mod.files.len(), 3);
        assert!(crate_mod.has_entry_point);
    }

    #[test]
    fn subdir_module_with_mod_rs() {
        let tmp = setup_project(&[
            ("main.rs", "mod adapters;\nfn main() {}"),
            ("adapters/mod.rs", "pub mod claude;"),
            ("adapters/claude.rs", "pub struct ClaudeAdapter;"),
        ]);
        let modules = detect_modules_from_repo(tmp.path());

        assert_eq!(modules.len(), 2); // crate + adapters
        let adapters = &modules["adapters"];
        assert_eq!(adapters.name, "adapters");
        assert!(adapters.has_entry_point);
        assert!(adapters.entry_point.as_ref().unwrap().ends_with("mod.rs"));
        assert_eq!(adapters.files.len(), 2); // mod.rs + claude.rs
    }

    #[test]
    fn nested_submodules() {
        let tmp = setup_project(&[
            ("main.rs", "mod adapters;\nfn main() {}"),
            ("adapters/mod.rs", "pub mod claude;"),
            ("adapters/claude/mod.rs", "pub mod client;"),
            ("adapters/claude/client.rs", "pub struct Client;"),
        ]);
        let modules = detect_modules_from_repo(tmp.path());

        assert_eq!(modules.len(), 3); // crate, adapters, adapters::claude
        assert!(modules.contains_key("adapters::claude"));

        let claude_mod = &modules["adapters::claude"];
        assert_eq!(claude_mod.files.len(), 2);
        assert!(claude_mod.has_entry_point);
    }

    #[test]
    fn submodule_detection() {
        let tmp = setup_project(&[
            ("main.rs", "mod adapters;\nmod db;\nfn main() {}"),
            ("adapters/mod.rs", "pub mod claude;"),
            ("adapters/claude.rs", "pub struct ClaudeAdapter;"),
            ("db.rs", "pub struct Database;"),
        ]);
        let modules = detect_modules_from_repo(tmp.path());

        let crate_mod = &modules["crate"];
        assert!(crate_mod.submodules.contains(&"adapters".to_string()));
    }

    #[test]
    fn module_without_entry_point() {
        let tmp = setup_project(&[
            ("main.rs", "fn main() {}"),
            ("utils/helper.rs", "pub fn help() {}"),
            (
                "utils/math.rs",
                "pub fn add(a: i32, b: i32) -> i32 { a + b }",
            ),
        ]);
        let modules = detect_modules_from_repo(tmp.path());

        assert_eq!(modules.len(), 2);
        let utils = &modules["utils"];
        assert!(!utils.has_entry_point);
        assert!(utils.entry_point.is_none());
        assert_eq!(utils.files.len(), 2);
    }

    #[test]
    fn lib_rs_as_entry_point() {
        let tmp = setup_project(&[
            ("lib.rs", "pub mod config;"),
            ("config.rs", "pub struct Config;"),
        ]);
        let modules = detect_modules_from_repo(tmp.path());

        let crate_mod = &modules["crate"];
        assert!(crate_mod.has_entry_point);
        assert!(crate_mod.entry_point.as_ref().unwrap().ends_with("lib.rs"));
    }

    #[test]
    fn module_name_derivation() {
        let src = Path::new("/project/src");
        assert_eq!(
            module_name_from_path(src, Path::new("/project/src/adapters")),
            "adapters"
        );
        assert_eq!(
            module_name_from_path(src, Path::new("/project/src/adapters/claude")),
            "adapters::claude"
        );
        assert_eq!(
            module_name_from_path(src, Path::new("/project/src/a/b/c")),
            "a::b::c"
        );
    }

    #[test]
    fn detect_modules_returns_sorted_files() {
        let tmp = setup_project(&[
            ("main.rs", "fn main() {}"),
            ("z_module.rs", "pub fn z() {}"),
            ("a_module.rs", "pub fn a() {}"),
        ]);
        let modules = detect_modules_from_repo(tmp.path());

        let crate_mod = &modules["crate"];
        let file_names: Vec<_> = crate_mod
            .files
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(file_names, vec!["a_module.rs", "main.rs", "z_module.rs"]);
    }

    #[test]
    fn detect_modules_with_import_graph_input() {
        // Verify detect_modules works with files obtained from import_graph's collect
        let tmp = setup_project(&[
            ("main.rs", "mod config;\nfn main() {}"),
            ("config.rs", "pub struct Config;"),
            ("adapters/mod.rs", "pub mod claude;"),
            ("adapters/claude.rs", "pub struct Claude;"),
        ]);
        let src_root = tmp.path().join("src");
        let rs_files = collect_rs_files_for_modules(&src_root);
        let modules = detect_modules(&src_root, &rs_files);

        assert_eq!(modules.len(), 2); // crate + adapters
        assert!(modules.contains_key("crate"));
        assert!(modules.contains_key("adapters"));
    }
}
