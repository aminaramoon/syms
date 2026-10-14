//! POSIX-ish shell word splitter. Mirrors `symhow::common::shell`.

fn is_escapable_in_double_quotes(c: char) -> bool {
    c == '"' || c == '\\' || c == '$' || c == '`'
}

/// Splits `command` into argv-style tokens (see the C++ header for the exact
/// quoting/escaping grammar).
pub fn split(command: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_token = false;

    let bytes: Vec<char> = command.chars().collect();
    let n = bytes.len();
    let mut i = 0;

    while i < n {
        let c = bytes[i];

        if c.is_whitespace() {
            if in_token {
                tokens.push(std::mem::take(&mut current));
                in_token = false;
            }
            i += 1;
            continue;
        }

        in_token = true;

        match c {
            '\'' => {
                i += 1;
                while i < n && bytes[i] != '\'' {
                    current.push(bytes[i]);
                    i += 1;
                }
                if i < n {
                    i += 1; // skip closing quote
                }
            }
            '"' => {
                i += 1;
                while i < n && bytes[i] != '"' {
                    if bytes[i] == '\\' && i + 1 < n && is_escapable_in_double_quotes(bytes[i + 1]) {
                        current.push(bytes[i + 1]);
                        i += 2;
                    } else {
                        current.push(bytes[i]);
                        i += 1;
                    }
                }
                if i < n {
                    i += 1; // skip closing quote
                }
            }
            '\\' => {
                if i + 1 < n {
                    current.push(bytes[i + 1]);
                    i += 2;
                } else {
                    i += 1; // trailing unmatched backslash: drop it
                }
            }
            _ => {
                current.push(c);
                i += 1;
            }
        }
    }

    if in_token {
        tokens.push(current);
    }

    tokens
}