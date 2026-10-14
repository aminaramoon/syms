//! Pure parsers for `git diff --name-status -z` and `git status --porcelain
//! -z` output. Mirrors `symhow::update::GitDiff`.

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    /// A path git reports as unmerged (conflict markers still in the
    /// worktree/index).
    Unmerged,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ChangedFile {
    pub kind: ChangeKind,
    pub old_path: String,
    pub new_path: String,
}

impl Default for ChangedFile {
    fn default() -> Self {
        ChangedFile { kind: ChangeKind::Modified, old_path: String::new(), new_path: String::new() }
    }
}

fn make_absolute(repo_root: &str, rel_or_abs: &str) -> String {
    let p = std::path::Path::new(rel_or_abs);
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::path::Path::new(repo_root).join(p)
    };
    crate::common::paths::lexical_normalize(&joined.to_string_lossy())
}

/// Splits `s` on NUL bytes, dropping a single trailing empty token (git
/// NUL-*terminates*, not NUL-*separates*).
fn split_nul(s: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut start = 0usize;
    while start <= s.len() {
        match s[start..].find('\0') {
            None => {
                if start < s.len() {
                    tokens.push(&s[start..]);
                }
                break;
            }
            Some(rel) => {
                tokens.push(&s[start..start + rel]);
                start = start + rel + 1;
            }
        }
    }
    tokens
}

/// Parses `git diff --name-status -z ...` output (NUL-separated records).
pub fn parse_name_status_z(nul_separated: &str, repo_root: &str) -> Vec<ChangedFile> {
    let mut result = Vec::new();
    let tokens = split_nul(nul_separated);

    let mut i = 0usize;
    while i < tokens.len() {
        let status = tokens[i];
        i += 1;
        if status.is_empty() {
            continue;
        }
        let code = status.as_bytes()[0];

        if code == b'R' || code == b'C' {
            if i + 1 >= tokens.len() {
                break; // malformed/truncated input
            }
            let old_path = make_absolute(repo_root, tokens[i]);
            i += 1;
            let new_path = make_absolute(repo_root, tokens[i]);
            i += 1;
            if code == b'R' {
                result.push(ChangedFile { kind: ChangeKind::Renamed, old_path, new_path });
            } else {
                // Copy: the source is untouched, so treat as a new TU at new_path.
                result.push(ChangedFile { kind: ChangeKind::Added, old_path: String::new(), new_path });
            }
            continue;
        }

        if i >= tokens.len() {
            break; // malformed/truncated input
        }
        let path = make_absolute(repo_root, tokens[i]);
        i += 1;

        let kind = match code {
            b'A' => ChangeKind::Added,
            b'D' => ChangeKind::Deleted,
            b'U' => ChangeKind::Unmerged,
            _ => ChangeKind::Modified, // M, T, X, B, anything else
        };
        result.push(ChangedFile { kind, old_path: String::new(), new_path: path });
    }

    result
}

/// Parses `git status --porcelain -z --untracked-files=all ...` output.
/// Returns the (absolute, lexically-normal) path of every entry; for
/// renames, only the new (current) path.
pub fn parse_status_porcelain_z(nul_separated: &str, repo_root: &str) -> Vec<String> {
    let mut result = Vec::new();
    let tokens = split_nul(nul_separated);

    let mut i = 0usize;
    while i < tokens.len() {
        let entry = tokens[i];
        i += 1;
        if entry.len() < 3 {
            continue;
        }
        let bytes = entry.as_bytes();
        let x = bytes[0];
        let y = bytes[1];
        let path = &entry[3..]; // entry[2] is the space separating "XY" from the path

        if x == b'!' && y == b'!' {
            continue; // ignored -- not a "dirty" entry
        }

        result.push(make_absolute(repo_root, path));

        if x == b'R' || x == b'C' || y == b'R' || y == b'C' {
            // Rename/copy: the next token is the old path -- consume it.
            if i < tokens.len() {
                i += 1;
            }
        }
    }

    result
}

/// Scans `git status --porcelain -z` output for any unmerged entry ('U' in
/// the X or Y status code).
pub fn status_has_unmerged_entries(nul_separated: &str) -> bool {
    let tokens = split_nul(nul_separated);
    let mut i = 0usize;
    while i < tokens.len() {
        let entry = tokens[i];
        i += 1;
        if entry.len() < 2 {
            continue;
        }
        let bytes = entry.as_bytes();
        let x = bytes[0];
        let y = bytes[1];
        if x == b'U' || y == b'U' {
            return true;
        }
        if x == b'R' || x == b'C' || y == b'R' || y == b'C' {
            if i < tokens.len() {
                i += 1;
            }
        }
    }
    false
}