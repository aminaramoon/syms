//! Minimal subprocess runner. Mirrors `symhow::common::process`.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessResult {
    pub exit_code: i32,
    pub out: String,
    pub err: String,
}

/// Runs `argv[0]` with the remaining elements as arguments (looked up on
/// PATH). If `cwd` is `Some`, the child chdir()s there first. Captures
/// stdout/stderr fully. Returns `exit_code == -1` with `err` set if the
/// binary can't be spawned at all.
pub fn run(argv: &[String], cwd: Option<&str>) -> ProcessResult {
    if argv.is_empty() {
        return ProcessResult {
            exit_code: -1,
            out: String::new(),
            err: "symhow::common::process::run: empty argv".to_string(),
        };
    }

    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }

    match cmd.output() {
        Ok(output) => ProcessResult {
            exit_code: output.status.code().unwrap_or(-1),
            out: String::from_utf8_lossy(&output.stdout).into_owned(),
            err: String::from_utf8_lossy(&output.stderr).into_owned(),
        },
        Err(e) => ProcessResult {
            exit_code: -1,
            out: String::new(),
            err: format!("failed to spawn '{}': {}", argv[0], e),
        },
    }
}

/// Convenience: run a command built from an iterator of `&str`.
pub fn run_str<I, S>(argv: I, cwd: Option<&str>) -> ProcessResult
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let owned: Vec<String> = argv.into_iter().map(|s| s.as_ref().to_string()).collect();
    run(&owned, cwd)
}