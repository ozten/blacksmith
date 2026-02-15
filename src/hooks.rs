/// Pre/post-session hook execution.
///
/// Hooks are shell commands executed synchronously before or after each session.
/// They receive environment variables with session context (iteration counts,
/// output file paths, exit codes, etc.).
pub struct HookRunner {
    // TODO: pre-session commands, post-session commands
}

impl HookRunner {
    // TODO: pub fn run_pre_session(&self, env: &HookEnv) -> Result<(), ...>
    // TODO: pub fn run_post_session(&self, env: &HookEnv) -> Result<(), ...>
}
