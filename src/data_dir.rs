use std::path::{Path, PathBuf};
use std::process::Command;

/// Manages the `.blacksmith/` directory layout.
///
/// All blacksmith artifacts live under a single data directory (default `.blacksmith/`).
/// This struct provides accessors for each well-known path and handles initialization.
#[derive(Debug, Clone)]
pub struct DataDir {
    root: PathBuf,
}

impl DataDir {
    /// Create a new DataDir referencing the given root path.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The root directory (e.g. `.blacksmith/`).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Path to the SQLite database.
    pub fn db(&self) -> PathBuf {
        self.root.join("blacksmith.db")
    }

    /// Path to the harness status file.
    pub fn status(&self) -> PathBuf {
        self.root.join("status")
    }

    /// Path to the global iteration counter file.
    pub fn counter(&self) -> PathBuf {
        self.root.join("counter")
    }

    /// Path to the sessions directory.
    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    /// Path to the worktrees directory.
    pub fn worktrees_dir(&self) -> PathBuf {
        self.root.join("worktrees")
    }

    /// Path to the singleton lock file.
    pub fn lock(&self) -> PathBuf {
        self.root.join("lock")
    }

    /// Path to the config file (e.g. `.blacksmith/config.toml`).
    pub fn config(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    /// Path to a specific session file (e.g. `sessions/42.jsonl`).
    pub fn session_file(&self, iteration: u32) -> PathBuf {
        self.sessions_dir().join(format!("{iteration}.jsonl"))
    }

    /// Default content written to `config.toml` when initializing a new data directory.
    const DEFAULT_CONFIG: &str = "\
# Blacksmith configuration
# See documentation for all available options.

[agent]
command = \"claude\"
args = [\"-p\", \"{prompt}\", \"--verbose\", \"--output-format\", \"stream-json\"]

[session]
max_iterations = 100

[storage]
compress_after = 5
retention = \"last-50\"
";

    /// Initialize the full directory structure.
    /// Creates root, sessions/, and worktrees/ directories.
    /// Also writes a default config.toml if one doesn't already exist.
    /// Returns Ok(true) if directories were created, Ok(false) if they already existed.
    pub fn init(&self) -> std::io::Result<bool> {
        let created = !self.root.exists();
        std::fs::create_dir_all(&self.root)?;
        std::fs::create_dir_all(self.sessions_dir())?;
        std::fs::create_dir_all(self.worktrees_dir())?;

        // Write default config.toml if it doesn't exist
        let config_path = self.config();
        if !config_path.exists() {
            std::fs::write(&config_path, Self::DEFAULT_CONFIG)?;
        }

        Ok(created)
    }

    /// Initialize the directory structure with custom config content.
    /// Creates root, sessions/, and worktrees/ directories.
    /// Writes `config_content` to config.toml only if one doesn't already exist.
    /// Returns Ok(true) if directories were created, Ok(false) if they already existed.
    pub fn init_with_config(&self, config_content: &str) -> std::io::Result<bool> {
        let created = !self.root.exists();
        std::fs::create_dir_all(&self.root)?;
        std::fs::create_dir_all(self.sessions_dir())?;
        std::fs::create_dir_all(self.worktrees_dir())?;

        let config_path = self.config();
        if !config_path.exists() {
            std::fs::write(&config_path, config_content)?;
        }

        Ok(created)
    }

    /// Ensure the data directory is initialized, creating it if missing.
    /// Also appends the data_dir to .gitignore if a .gitignore exists
    /// and doesn't already contain the entry.
    pub fn ensure_initialized(&self) -> std::io::Result<()> {
        self.init()?;
        self.update_gitignore()?;
        Ok(())
    }

    /// Append the data directory path to .gitignore if:
    /// 1. A .gitignore file exists in the current directory or parent of data_dir
    /// 2. It doesn't already contain the entry
    pub fn update_gitignore(&self) -> std::io::Result<()> {
        // Determine where .gitignore should be (parent of data_dir, or cwd)
        let gitignore_dir = self.root.parent().unwrap_or_else(|| Path::new("."));
        let gitignore_path = gitignore_dir.join(".gitignore");

        // Get the directory name to add to .gitignore
        let dir_name = self
            .root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.root.to_string_lossy().to_string());
        let entry = format!("{dir_name}/");

        if gitignore_path.exists() {
            let contents = std::fs::read_to_string(&gitignore_path)?;
            // Check if already present (exact line match)
            let already_present = contents.lines().any(|line| {
                let trimmed = line.trim();
                trimmed == entry || trimmed == dir_name
            });
            if !already_present {
                // Append with a newline separator if file doesn't end with one
                let prefix = if contents.ends_with('\n') || contents.is_empty() {
                    ""
                } else {
                    "\n"
                };
                let mut file = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&gitignore_path)?;
                use std::io::Write;
                writeln!(file, "{prefix}{entry}")?;
            }
        }
        // If no .gitignore exists, don't create one
        Ok(())
    }
}

/// Resolve `.blacksmith/...` paths against the repository root shared by git worktrees.
///
/// If `path` is relative and starts with `.blacksmith`, it is rebased to
/// `<git-common-root>/.blacksmith/...` when inside a git repository.
/// Other paths are returned unchanged.
pub fn resolve_repo_relative_blacksmith_path(path: &Path) -> PathBuf {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(_) => return path.to_path_buf(),
    };
    resolve_repo_relative_blacksmith_path_from(path, &cwd)
}

pub(crate) fn resolve_repo_relative_blacksmith_path_from(path: &Path, cwd: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }

    let mut components = path.components();
    let first = components.next();
    let is_blacksmith_relative =
        matches!(first, Some(std::path::Component::Normal(name)) if name == ".blacksmith");
    if !is_blacksmith_relative {
        return path.to_path_buf();
    }

    let Some(repo_root) = git_common_repo_root(cwd) else {
        return path.to_path_buf();
    };
    repo_root.join(path)
}

fn git_common_repo_root(cwd: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(cwd)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let common_dir = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string());
    if common_dir.as_os_str().is_empty() {
        return None;
    }

    if common_dir.file_name().is_some_and(|n| n == ".git") {
        return common_dir.parent().map(Path::to_path_buf);
    }
    Some(common_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn test_data_dir_paths() {
        let dd = DataDir::new(".blacksmith");
        assert_eq!(dd.root(), Path::new(".blacksmith"));
        assert_eq!(dd.db(), PathBuf::from(".blacksmith/blacksmith.db"));
        assert_eq!(dd.status(), PathBuf::from(".blacksmith/status"));
        assert_eq!(dd.counter(), PathBuf::from(".blacksmith/counter"));
        assert_eq!(dd.sessions_dir(), PathBuf::from(".blacksmith/sessions"));
        assert_eq!(dd.worktrees_dir(), PathBuf::from(".blacksmith/worktrees"));
        assert_eq!(
            dd.session_file(42),
            PathBuf::from(".blacksmith/sessions/42.jsonl")
        );
    }

    #[test]
    fn test_config_path() {
        let dd = DataDir::new(".blacksmith");
        assert_eq!(dd.config(), PathBuf::from(".blacksmith/config.toml"));
    }

    #[test]
    fn test_init_creates_directories_and_config() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".blacksmith");
        let dd = DataDir::new(&root);

        assert!(!root.exists());
        let created = dd.init().unwrap();
        assert!(created);
        assert!(root.exists());
        assert!(dd.sessions_dir().exists());
        assert!(dd.worktrees_dir().exists());
        assert!(dd.config().exists());

        // Verify config is valid TOML that parses
        let contents = std::fs::read_to_string(dd.config()).unwrap();
        assert!(contents.contains("[agent]"));
        assert!(contents.contains("[session]"));
        assert!(contents.contains("[storage]"));
    }

    #[test]
    fn test_init_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".blacksmith");
        let dd = DataDir::new(&root);

        let created1 = dd.init().unwrap();
        assert!(created1);
        let created2 = dd.init().unwrap();
        assert!(!created2);
    }

    #[test]
    fn test_init_does_not_overwrite_existing_config() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".blacksmith");
        let dd = DataDir::new(&root);

        // First init creates config
        dd.init().unwrap();

        // Overwrite with custom content
        let custom = "[agent]\ncommand = \"my-agent\"\n";
        std::fs::write(dd.config(), custom).unwrap();

        // Second init should NOT clobber custom config
        dd.init().unwrap();
        let contents = std::fs::read_to_string(dd.config()).unwrap();
        assert_eq!(contents, custom);
    }

    #[test]
    fn test_ensure_initialized_creates_and_updates_gitignore() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".blacksmith");
        let gitignore = tmp.path().join(".gitignore");

        // Create a .gitignore with existing content
        std::fs::write(&gitignore, "node_modules/\n").unwrap();

        let dd = DataDir::new(&root);
        dd.ensure_initialized().unwrap();

        assert!(root.exists());
        let contents = std::fs::read_to_string(&gitignore).unwrap();
        assert!(contents.contains(".blacksmith/"));
        assert!(contents.contains("node_modules/"));
    }

    #[test]
    fn test_gitignore_not_duplicated() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".blacksmith");
        let gitignore = tmp.path().join(".gitignore");

        std::fs::write(&gitignore, ".blacksmith/\n").unwrap();

        let dd = DataDir::new(&root);
        dd.ensure_initialized().unwrap();

        let contents = std::fs::read_to_string(&gitignore).unwrap();
        assert_eq!(contents.matches(".blacksmith/").count(), 1);
    }

    #[test]
    fn test_gitignore_not_created_if_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".blacksmith");
        let gitignore = tmp.path().join(".gitignore");

        let dd = DataDir::new(&root);
        dd.ensure_initialized().unwrap();

        assert!(!gitignore.exists());
    }

    #[test]
    fn test_init_with_config_writes_custom_content() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".blacksmith");
        let dd = DataDir::new(&root);

        let custom_config = "[agent]\ncommand = \"custom-agent\"\nargs = [\"-p\", \"{prompt}\"]\n";

        // First call writes the custom config
        let created = dd.init_with_config(custom_config).unwrap();
        assert!(created);
        assert!(dd.config().exists());
        assert!(dd.sessions_dir().exists());
        assert!(dd.worktrees_dir().exists());

        let contents = std::fs::read_to_string(dd.config()).unwrap();
        assert_eq!(contents, custom_config);

        // Second call should NOT overwrite existing config
        let created2 = dd.init_with_config("overwritten").unwrap();
        assert!(!created2);
        let contents2 = std::fs::read_to_string(dd.config()).unwrap();
        assert_eq!(contents2, custom_config);
    }

    #[test]
    fn test_gitignore_append_no_trailing_newline() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join(".blacksmith");
        let gitignore = tmp.path().join(".gitignore");

        // Write without trailing newline
        std::fs::write(&gitignore, "node_modules/").unwrap();

        let dd = DataDir::new(&root);
        dd.ensure_initialized().unwrap();

        let contents = std::fs::read_to_string(&gitignore).unwrap();
        assert_eq!(contents, "node_modules/\n.blacksmith/\n");
    }

    #[test]
    fn test_resolve_repo_relative_blacksmith_path_main_and_worktree_share_root() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();

        run_git(&repo, &["init"]);
        std::fs::write(repo.join("README.md"), "hello\n").unwrap();
        run_git(&repo, &["add", "README.md"]);
        run_git(
            &repo,
            &[
                "-c",
                "user.email=test@example.com",
                "-c",
                "user.name=Test",
                "commit",
                "-m",
                "init",
            ],
        );

        let worktree = repo.join(".blacksmith/worktrees/worker-0-beads-test");
        let worktree_parent = worktree.parent().unwrap();
        std::fs::create_dir_all(worktree_parent).unwrap();
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                "--detach",
                worktree.to_string_lossy().as_ref(),
                "HEAD",
            ],
        );

        let rel = Path::new(".blacksmith/blacksmith.db");
        let main_resolved = resolve_repo_relative_blacksmith_path_from(rel, &repo);
        let worktree_resolved = resolve_repo_relative_blacksmith_path_from(rel, &worktree);

        let expected = repo.join(".blacksmith/blacksmith.db");
        assert_eq!(main_resolved, expected);
        assert_eq!(worktree_resolved, expected);
    }

    #[test]
    fn test_resolve_repo_relative_blacksmith_path_keeps_non_blacksmith_relative_path() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init"]);

        let rel = Path::new("custom-dir/blacksmith.db");
        let resolved = resolve_repo_relative_blacksmith_path_from(rel, &repo);
        assert_eq!(resolved, rel);
    }

    fn run_git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(
            status.success(),
            "git command failed: git {}",
            args.join(" ")
        );
    }
}
