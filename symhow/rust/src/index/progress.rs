//! Pure formatting helpers for `symhow index`'s stderr progress output.
//! Mirrors `symhow::index::progress`.

fn digit_count(mut v: u32) -> u32 {
    let mut n = 1u32;
    while v >= 10 {
        v /= 10;
        n += 1;
    }
    n
}

/// Right-aligned "[  12/975]" counter: `index` is 1-based, width is derived
/// from the decimal digit count of `total`.
pub fn format_counter(index: u32, total: u32) -> String {
    let width = digit_count(total) as usize;
    format!("[{:>width$}/{}]", index, total, width = width)
}

/// Per-TU wall time, e.g. "1.3s" / "0.1s" / "42.0s" -- always one decimal.
pub fn format_tu_seconds(seconds: f64) -> String {
    let seconds = if seconds > 0.0 { seconds } else { 0.0 };
    format!("{:.1}s", seconds)
}

/// A human duration used for both "elapsed" and "eta", e.g. "41s", "9m41s",
/// "1h08m20s". Never negative; a negative or non-finite input is clamped to
/// "0s".
pub fn format_duration_hms(seconds: f64) -> String {
    let seconds = if seconds.is_finite() && seconds >= 0.0 {
        seconds
    } else {
        0.0
    };
    let total = (seconds + 0.5) as u64; // round to nearest second
    let h = total / 3600;
    let total = total % 3600;
    let m = total / 60;
    let s = total % 60;

    if h > 0 {
        format!("{}h{:02}m{:02}s", h, m, s)
    } else if m > 0 {
        format!("{}m{:02}s", m, s)
    } else {
        format!("{}s", s)
    }
}

/// Full per-TU progress line (without trailing newline).
pub fn format_progress_line(
    index: u32,
    total: u32,
    seconds: f64,
    status: &str,
    path: &str,
) -> String {
    format!(
        "{} {}  {}  {}",
        format_counter(index, total),
        format_tu_seconds(seconds),
        status,
        path
    )
}

/// Periodic/final one-line summary.
pub fn format_running_summary(
    done: u32,
    total: u32,
    tus_per_min: f64,
    elapsed_seconds: f64,
    eta_seconds: f64,
) -> String {
    let rate = format!("{:.1}", tus_per_min);

    let mut line = format!(
        "symhow index: {}/{} done, {} TU/min, elapsed {}",
        done,
        total,
        rate,
        format_duration_hms(elapsed_seconds)
    );
    if done < total {
        line.push_str(&format!(", eta ~{}", format_duration_hms(eta_seconds)));
    }
    line
}

/// Returns `abs_path` relative to `repo_root` (POSIX-style, "/"-joined) if
/// `repo_root` is non-empty and `abs_path` lexically falls inside it; else
/// returns `abs_path` unchanged. Purely lexical/textual.
pub fn relative_to_root(abs_path: &str, repo_root: &str) -> String {
    if repo_root.is_empty() {
        return abs_path.to_string();
    }

    let root = repo_root.trim_end_matches('/');
    if abs_path.len() > root.len() + 1
        && abs_path.starts_with(root)
        && abs_path.as_bytes()[root.len()] == b'/'
    {
        return abs_path[root.len() + 1..].to_string();
    }
    if abs_path == root {
        return ".".to_string();
    }
    abs_path.to_string()
}