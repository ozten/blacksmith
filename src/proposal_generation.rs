//! Automatic proposal generation from refactoring candidates.
//!
//! Bridges the gap between the signal correlator (which identifies *what* needs
//! refactoring) and proposal validation (which checks *whether* a specific plan
//! is safe). For each `RefactorCandidate`, maps its structural smell flags to
//! one or more `RefactorProposal`s with concrete file lists and module names.
//!
//! This is step 3.5 of the architecture analysis pipeline.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::module_detect::Module;
use crate::proposal_validation::{ProposalKind, RefactorProposal};
use crate::signal_correlator::RefactorCandidate;
use crate::structural_metrics::StructuralReport;

/// Generate refactoring proposals from a list of candidates.
///
/// Each candidate may produce multiple proposals if it has several smell flags.
/// Proposals are ordered: SplitModule first (highest impact), then BreakCycle,
/// MoveFiles, and ExtractInterface.
pub fn generate_proposals(
    candidates: &[RefactorCandidate],
    report: &StructuralReport,
    modules: &HashMap<String, Module>,
) -> Vec<RefactorProposal> {
    let mut proposals = Vec::new();

    for candidate in candidates {
        let module = match modules.get(&candidate.module) {
            Some(m) => m,
            None => continue,
        };

        // SplitModule: triggered by god files, large module, or wide API
        if candidate.smells.has_god_files
            || candidate.smells.large_module
            || candidate.smells.wide_api
        {
            if let Some(p) = make_split_proposal(candidate, module, report) {
                proposals.push(p);
            }
        }

        // BreakCycle: triggered by cycle participation
        if candidate.smells.in_cycle {
            proposals.push(make_break_cycle_proposal(candidate, module));
        }

        // MoveFiles: triggered by boundary violations
        if candidate.smells.has_violations {
            if let Some(p) = make_move_files_proposal(candidate, module, report) {
                proposals.push(p);
            }
        }

        // ExtractInterface: triggered by high fan-in
        if candidate.smells.high_fan_in {
            proposals.push(make_extract_interface_proposal(candidate, module));
        }
    }

    proposals
}

/// Build a SplitModule proposal based on god file clusters or file size.
///
/// Uses god file detection results to suggest concrete split boundaries. If god
/// files are present, splits along cluster boundaries. Otherwise, splits into
/// a `_core` module (entry point + small files) and a `_ext` module (large files).
fn make_split_proposal(
    candidate: &RefactorCandidate,
    module: &Module,
    report: &StructuralReport,
) -> Option<RefactorProposal> {
    if module.files.len() < 2 {
        return None;
    }

    let core_name = format!("{}_core", candidate.module);
    let ext_name = format!("{}_ext", candidate.module);

    // Partition files: entry point stays in core, god files / large files go to ext
    let mut affected = Vec::new();
    for file in &module.files {
        // Keep the entry point in the original module
        if Some(file) == module.entry_point.as_ref() {
            continue;
        }
        let is_god = report
            .files
            .get(file)
            .map(|f| f.is_god_file)
            .unwrap_or(false);
        let is_large = report
            .files
            .get(file)
            .map(|f| f.line_count > 300)
            .unwrap_or(false);
        if is_god || is_large {
            affected.push(file.clone());
        }
    }

    // If no files qualify by size/god-file, pick the larger half of non-entry files
    if affected.is_empty() {
        let mut non_entry: Vec<_> = module
            .files
            .iter()
            .filter(|f| Some(*f) != module.entry_point.as_ref())
            .collect();
        non_entry.sort_by_key(|f| {
            std::cmp::Reverse(
                report
                    .files
                    .get(*f)
                    .map(|m| m.line_count)
                    .unwrap_or(0),
            )
        });
        let half = (non_entry.len() + 1) / 2;
        affected = non_entry.into_iter().take(half).cloned().collect();
    }

    if affected.is_empty() {
        return None;
    }

    Some(RefactorProposal {
        kind: ProposalKind::SplitModule,
        target_module: candidate.module.clone(),
        candidate: candidate.clone(),
        proposed_modules: vec![core_name, ext_name],
        affected_files: affected,
    })
}

/// Build a BreakCycle proposal listing all files in the cyclic module.
fn make_break_cycle_proposal(candidate: &RefactorCandidate, module: &Module) -> RefactorProposal {
    RefactorProposal {
        kind: ProposalKind::BreakCycle,
        target_module: candidate.module.clone(),
        candidate: candidate.clone(),
        proposed_modules: vec![],
        affected_files: module.files.clone(),
    }
}

/// Build a MoveFiles proposal for files involved in boundary violations.
///
/// Identifies which files in this module are referenced by violations (as the
/// target of non-public imports) and suggests moving them to the source module
/// that most frequently accesses them.
fn make_move_files_proposal(
    candidate: &RefactorCandidate,
    module: &Module,
    report: &StructuralReport,
) -> Option<RefactorProposal> {
    // Find violations where this module is the target (its internals are accessed)
    let violations: Vec<_> = report
        .boundary_violations
        .iter()
        .filter(|v| v.target_module == candidate.module)
        .collect();

    if violations.is_empty() {
        return None;
    }

    // Count which source modules access this module's internals most
    let mut source_counts: HashMap<&str, usize> = HashMap::new();
    for v in &violations {
        *source_counts.entry(&v.source_module).or_default() += 1;
    }

    // The primary destination is the module that accesses us most
    let destination = source_counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(module, _)| module.to_string())?;

    // Affected files: files in our module that contain the violated symbols
    let violated_files: Vec<PathBuf> = module
        .files
        .iter()
        .filter(|f| {
            let file_name = f
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            violations.iter().any(|_| {
                // Violations tell us the *symbol* being accessed, not the exact
                // file. Include non-entry-point files as candidates for moving.
                file_name != "mod.rs" && file_name != "lib.rs"
            })
        })
        .cloned()
        .collect();

    if violated_files.is_empty() {
        return None;
    }

    Some(RefactorProposal {
        kind: ProposalKind::MoveFiles,
        target_module: candidate.module.clone(),
        candidate: candidate.clone(),
        proposed_modules: vec![destination],
        affected_files: violated_files,
    })
}

/// Build an ExtractInterface proposal for high fan-in modules.
///
/// Suggests extracting a trait/interface from the module to reduce coupling.
/// Affected files are the high fan-in files within the module.
fn make_extract_interface_proposal(
    candidate: &RefactorCandidate,
    module: &Module,
) -> RefactorProposal {
    // Include all files from the module — the interface extraction affects
    // the module's public API surface.
    RefactorProposal {
        kind: ProposalKind::ExtractInterface,
        target_module: candidate.module.clone(),
        candidate: candidate.clone(),
        proposed_modules: vec![],
        affected_files: module.files.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boundary_violation::BoundaryViolation;
    use crate::signal_correlator::{ModuleSignals, StructuralSmells};
    use crate::structural_metrics::{FileMetrics, ModuleMetrics};
    use std::path::Path;

    fn make_candidate(module: &str, smells: StructuralSmells) -> RefactorCandidate {
        RefactorCandidate {
            module: module.to_string(),
            smells,
            signals: ModuleSignals {
                module: module.to_string(),
                expansion_score: 2.0,
                integration_score: 1.0,
                drift_count: 1,
                historical_score: 3.0,
            },
            combined_score: 5.0,
            confidence: 0.5,
        }
    }

    fn default_smells() -> StructuralSmells {
        StructuralSmells {
            high_fan_in: false,
            large_module: false,
            in_cycle: false,
            has_violations: false,
            has_god_files: false,
            wide_api: false,
            structural_score: 0.0,
        }
    }

    fn make_module(name: &str, files: &[&str]) -> Module {
        let file_paths: Vec<PathBuf> = files.iter().map(|f| PathBuf::from(f)).collect();
        let root = file_paths
            .first()
            .and_then(|f| f.parent())
            .unwrap_or(Path::new("src"))
            .to_path_buf();
        let entry_point = file_paths
            .iter()
            .find(|f| {
                f.file_name()
                    .map(|n| n == "mod.rs" || n == "lib.rs")
                    .unwrap_or(false)
            })
            .cloned();
        Module {
            name: name.to_string(),
            root_path: root,
            files: file_paths,
            has_entry_point: entry_point.is_some(),
            entry_point,
            submodules: vec![],
        }
    }

    fn make_report(
        module_specs: &[(&str, &[(&str, usize, bool)])],
        violations: Vec<BoundaryViolation>,
    ) -> StructuralReport {
        let mut modules = HashMap::new();
        let mut files = HashMap::new();

        for (mod_name, file_specs) in module_specs {
            let mut total_lines = 0;
            let mut god_count = 0;
            for (path, lines, is_god) in *file_specs {
                let p = PathBuf::from(path);
                total_lines += lines;
                if *is_god {
                    god_count += 1;
                }
                files.insert(
                    p.clone(),
                    FileMetrics {
                        path: p,
                        line_count: *lines,
                        fan_in_score: 0.0,
                        fan_in_importers: 0,
                        is_god_file: *is_god,
                        cluster_count: if *is_god { 4 } else { 1 },
                    },
                );
            }
            modules.insert(
                mod_name.to_string(),
                ModuleMetrics {
                    name: mod_name.to_string(),
                    file_count: file_specs.len(),
                    total_lines,
                    api_surface_width: 5,
                    in_cycle: false,
                    violations_as_source: 0,
                    violations_as_target: violations
                        .iter()
                        .filter(|v| v.target_module == *mod_name)
                        .count(),
                    god_file_count: god_count,
                },
            );
        }

        StructuralReport {
            modules,
            files,
            cycles: vec![],
            boundary_violations: violations,
            total_modules: module_specs.len(),
            total_files: module_specs
                .iter()
                .map(|(_, specs)| specs.len())
                .sum(),
        }
    }

    #[test]
    fn split_module_from_god_files() {
        let smells = StructuralSmells {
            has_god_files: true,
            structural_score: 1.0,
            ..default_smells()
        };
        let candidate = make_candidate("auth", smells);
        let module = make_module(
            "auth",
            &["src/auth/mod.rs", "src/auth/session.rs", "src/auth/oauth.rs"],
        );
        let report = make_report(
            &[(
                "auth",
                &[
                    ("src/auth/mod.rs", 50, false),
                    ("src/auth/session.rs", 400, true), // god file
                    ("src/auth/oauth.rs", 100, false),
                ],
            )],
            vec![],
        );
        let modules: HashMap<String, Module> =
            [("auth".to_string(), module)].into_iter().collect();

        let proposals = generate_proposals(&[candidate], &report, &modules);

        assert!(!proposals.is_empty());
        let split = proposals
            .iter()
            .find(|p| p.kind == ProposalKind::SplitModule)
            .expect("Should have a SplitModule proposal");
        assert_eq!(split.target_module, "auth");
        assert_eq!(split.proposed_modules.len(), 2);
        assert!(split.proposed_modules.contains(&"auth_core".to_string()));
        assert!(split.proposed_modules.contains(&"auth_ext".to_string()));
        // God file should be in affected
        assert!(split
            .affected_files
            .iter()
            .any(|f| f.ends_with("session.rs")));
    }

    #[test]
    fn split_module_from_large_files() {
        let smells = StructuralSmells {
            large_module: true,
            structural_score: 1.0,
            ..default_smells()
        };
        let candidate = make_candidate("db", smells);
        let module = make_module("db", &["src/db/mod.rs", "src/db/queries.rs"]);
        let report = make_report(
            &[(
                "db",
                &[
                    ("src/db/mod.rs", 50, false),
                    ("src/db/queries.rs", 500, false), // large
                ],
            )],
            vec![],
        );
        let modules: HashMap<String, Module> =
            [("db".to_string(), module)].into_iter().collect();

        let proposals = generate_proposals(&[candidate], &report, &modules);
        let split = proposals
            .iter()
            .find(|p| p.kind == ProposalKind::SplitModule);
        assert!(split.is_some());
        assert!(split
            .unwrap()
            .affected_files
            .iter()
            .any(|f| f.ends_with("queries.rs")));
    }

    #[test]
    fn break_cycle_proposal() {
        let smells = StructuralSmells {
            in_cycle: true,
            structural_score: 1.0,
            ..default_smells()
        };
        let candidate = make_candidate("auth", smells);
        let module = make_module("auth", &["src/auth/mod.rs", "src/auth/login.rs"]);
        let report = make_report(
            &[(
                "auth",
                &[
                    ("src/auth/mod.rs", 50, false),
                    ("src/auth/login.rs", 100, false),
                ],
            )],
            vec![],
        );
        let modules: HashMap<String, Module> =
            [("auth".to_string(), module)].into_iter().collect();

        let proposals = generate_proposals(&[candidate], &report, &modules);
        let cycle = proposals
            .iter()
            .find(|p| p.kind == ProposalKind::BreakCycle)
            .expect("Should have a BreakCycle proposal");
        assert_eq!(cycle.target_module, "auth");
        assert_eq!(cycle.affected_files.len(), 2);
    }

    #[test]
    fn move_files_proposal() {
        let smells = StructuralSmells {
            has_violations: true,
            structural_score: 1.0,
            ..default_smells()
        };
        let candidate = make_candidate("utils", smells);
        let module = make_module(
            "utils",
            &["src/utils/mod.rs", "src/utils/helpers.rs"],
        );
        let violations = vec![BoundaryViolation {
            source_module: "auth".to_string(),
            target_module: "utils".to_string(),
            symbol: "internal_helper".to_string(),
            source_file: "src/auth/mod.rs".to_string(),
            import_line: "use crate::utils::internal_helper;".to_string(),
        }];
        let report = make_report(
            &[(
                "utils",
                &[
                    ("src/utils/mod.rs", 50, false),
                    ("src/utils/helpers.rs", 100, false),
                ],
            )],
            violations,
        );
        let modules: HashMap<String, Module> =
            [("utils".to_string(), module)].into_iter().collect();

        let proposals = generate_proposals(&[candidate], &report, &modules);
        let mv = proposals
            .iter()
            .find(|p| p.kind == ProposalKind::MoveFiles)
            .expect("Should have a MoveFiles proposal");
        assert_eq!(mv.target_module, "utils");
        assert_eq!(mv.proposed_modules, vec!["auth".to_string()]);
        // helpers.rs should be in affected (non-entry-point file)
        assert!(mv
            .affected_files
            .iter()
            .any(|f| f.ends_with("helpers.rs")));
    }

    #[test]
    fn extract_interface_proposal() {
        let smells = StructuralSmells {
            high_fan_in: true,
            structural_score: 1.0,
            ..default_smells()
        };
        let candidate = make_candidate("db", smells);
        let module = make_module("db", &["src/db/mod.rs", "src/db/pool.rs"]);
        let report = make_report(
            &[(
                "db",
                &[
                    ("src/db/mod.rs", 80, false),
                    ("src/db/pool.rs", 120, false),
                ],
            )],
            vec![],
        );
        let modules: HashMap<String, Module> =
            [("db".to_string(), module)].into_iter().collect();

        let proposals = generate_proposals(&[candidate], &report, &modules);
        let iface = proposals
            .iter()
            .find(|p| p.kind == ProposalKind::ExtractInterface)
            .expect("Should have an ExtractInterface proposal");
        assert_eq!(iface.target_module, "db");
        assert_eq!(iface.affected_files.len(), 2);
    }

    #[test]
    fn multiple_smells_produce_multiple_proposals() {
        let smells = StructuralSmells {
            large_module: true,
            in_cycle: true,
            high_fan_in: true,
            structural_score: 3.0,
            ..default_smells()
        };
        let candidate = make_candidate("core", smells);
        let module = make_module(
            "core",
            &["src/core/mod.rs", "src/core/engine.rs", "src/core/types.rs"],
        );
        let report = make_report(
            &[(
                "core",
                &[
                    ("src/core/mod.rs", 50, false),
                    ("src/core/engine.rs", 400, false),
                    ("src/core/types.rs", 200, false),
                ],
            )],
            vec![],
        );
        let modules: HashMap<String, Module> =
            [("core".to_string(), module)].into_iter().collect();

        let proposals = generate_proposals(&[candidate], &report, &modules);

        // Should have SplitModule, BreakCycle, and ExtractInterface
        assert!(proposals.iter().any(|p| p.kind == ProposalKind::SplitModule));
        assert!(proposals.iter().any(|p| p.kind == ProposalKind::BreakCycle));
        assert!(proposals
            .iter()
            .any(|p| p.kind == ProposalKind::ExtractInterface));
        assert!(proposals.len() >= 3);
    }

    #[test]
    fn no_candidates_no_proposals() {
        let report = make_report(&[], vec![]);
        let modules: HashMap<String, Module> = HashMap::new();
        let proposals = generate_proposals(&[], &report, &modules);
        assert!(proposals.is_empty());
    }

    #[test]
    fn unknown_module_skipped() {
        let smells = StructuralSmells {
            large_module: true,
            structural_score: 1.0,
            ..default_smells()
        };
        let candidate = make_candidate("nonexistent", smells);
        let report = make_report(&[], vec![]);
        let modules: HashMap<String, Module> = HashMap::new();

        let proposals = generate_proposals(&[candidate], &report, &modules);
        assert!(proposals.is_empty());
    }

    #[test]
    fn single_file_module_no_split() {
        let smells = StructuralSmells {
            large_module: true,
            structural_score: 1.0,
            ..default_smells()
        };
        let candidate = make_candidate("tiny", smells);
        let module = make_module("tiny", &["src/tiny.rs"]);
        let report = make_report(
            &[("tiny", &[("src/tiny.rs", 600, false)])],
            vec![],
        );
        let modules: HashMap<String, Module> =
            [("tiny".to_string(), module)].into_iter().collect();

        let proposals = generate_proposals(&[candidate], &report, &modules);
        // Cannot split a single-file module
        assert!(proposals
            .iter()
            .all(|p| p.kind != ProposalKind::SplitModule));
    }

    #[test]
    fn proposals_include_candidate_data() {
        let smells = StructuralSmells {
            in_cycle: true,
            structural_score: 1.0,
            ..default_smells()
        };
        let candidate = make_candidate("auth", smells);
        let module = make_module("auth", &["src/auth/mod.rs"]);
        let report = make_report(
            &[("auth", &[("src/auth/mod.rs", 50, false)])],
            vec![],
        );
        let modules: HashMap<String, Module> =
            [("auth".to_string(), module)].into_iter().collect();

        let proposals = generate_proposals(&[candidate.clone()], &report, &modules);
        assert!(!proposals.is_empty());
        // Every proposal should carry the original candidate
        for p in &proposals {
            assert_eq!(p.candidate.module, "auth");
            assert_eq!(p.candidate.combined_score, candidate.combined_score);
        }
    }

    #[test]
    fn split_prefers_god_files_over_size() {
        let smells = StructuralSmells {
            has_god_files: true,
            large_module: true,
            structural_score: 2.0,
            ..default_smells()
        };
        let candidate = make_candidate("big", smells);
        let module = make_module(
            "big",
            &[
                "src/big/mod.rs",
                "src/big/god.rs",
                "src/big/small.rs",
            ],
        );
        let report = make_report(
            &[(
                "big",
                &[
                    ("src/big/mod.rs", 50, false),
                    ("src/big/god.rs", 500, true),  // god file + large
                    ("src/big/small.rs", 30, false), // small, not god
                ],
            )],
            vec![],
        );
        let modules: HashMap<String, Module> =
            [("big".to_string(), module)].into_iter().collect();

        let proposals = generate_proposals(&[candidate], &report, &modules);
        let split = proposals
            .iter()
            .find(|p| p.kind == ProposalKind::SplitModule)
            .expect("Should have SplitModule");
        // god.rs should be in affected (god + large), small.rs should not
        assert!(split.affected_files.iter().any(|f| f.ends_with("god.rs")));
        assert!(!split
            .affected_files
            .iter()
            .any(|f| f.ends_with("small.rs")));
    }
}
