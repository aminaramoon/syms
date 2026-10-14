//! Update layer (mirrors `symhow::update`): git-driven incremental updates.

pub mod file_matcher;
pub mod git;
pub mod git_diff;
pub mod invalidator;
pub mod snapshot;
pub mod update_driver;

pub use file_matcher::{match_changed, select_entries, MatchInput, MatchResult};
pub use git::{collect_changes, rev_parse, top_level, ChangeSet};
pub use git_diff::{ChangeKind, ChangedFile};
pub use invalidator::{invalidate, sweep_orphans, ClearPlan, GcStats};
pub use snapshot::{diff_snapshots, take_snapshot, Snapshot, SnapshotDiff, SnapshotOptions};
pub use update_driver::{run_update, UpdateRunOptions, UpdateSummary};