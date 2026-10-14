//! Port of test/unit/test_content_fingerprint.cpp: content-fingerprint
//! (parsed-byte SHA-256) coverage across the full index/update pipeline.
//!
//! The C++ file's Catch2 SECTIONs are expanded into per-section iterations
//! inside each #[test] below (each iteration builds a fresh `Corpus`, which
//! is what a Catch2 section re-run does anyway). Every test is skipped when
//! `git` isn't available on PATH (the C++ Corpus needs it); the corpus root
//! lives under std::env::temp_dir(), unique per section so parallel test
//! threads never clobber each other.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use symhow::common::content_hash::{content_hash, file_content_hash};
use symhow::common::process::run;
use symhow::db::{Database, WritableDb};
use symhow::index::index_driver::{run_index, IndexRunOptions};
use symhow::index::provenance::Provenance;
use symhow::index::srctrldb_backend::SrctrldbBackend;
use symhow::model::{FileFact, IndexBackend, IndexSummary, TuFactBatch};
use symhow::update::git::{rev_parse, status_porcelain_z};
use symhow::update::git_diff::parse_status_porcelain_z;
use symhow::update::snapshot::{diff_snapshots, take_snapshot, SnapshotOptions};
use symhow::update::update_driver::{run_update, UpdateRunOptions, UpdateSummary};

const SOURCE: &str = "#include \"value.hpp\"\nint run() { return value(); }\n";
const HEADER: &str = "inline int value() { return 1; }\n";

fn git_available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| run(&["git".to_string(), "--version".to_string()], None).exit_code == 0)
}

/// std::filesystem::last_write_time(path, time) -- Rust std has no setter, so
/// mirror it with utimes(2) (libc is already a direct dependency).
fn set_file_mtime(path: &Path, mtime: SystemTime) {
    let secs = mtime.duration_since(std::time::UNIX_EPOCH).unwrap();
    let tv = libc::timeval {
        tv_sec: secs.as_secs() as libc::time_t,
        tv_usec: secs.subsec_micros() as libc::suseconds_t,
    };
    let times = [tv, tv];
    let c_path = std::ffi::CString::new(path.to_string_lossy().into_owned()).unwrap();
    assert_eq!(unsafe { libc::utimes(c_path.as_ptr(), times.as_ptr()) }, 0);
}

/// The C++ test file's `Corpus` fixture: a tiny git repository with a
/// one-TU compilation database (main.cpp including value.hpp).
struct Corpus {
    root: PathBuf,
    db: String,
    cdb: String,
}

impl Corpus {
    fn new(name: &str) -> Corpus {
        let root = std::env::temp_dir().join(format!("symhow_fingerprint_{}_{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let corpus = Corpus {
            db: root.join("index.srctrldb").to_string_lossy().into_owned(),
            cdb: root.join("compile_commands.json").to_string_lossy().into_owned(),
            root,
        };
        corpus.write("main.cpp", SOURCE);
        corpus.write("value.hpp", HEADER);
        corpus.write(".gitignore", "*.srctrldb*\ncompile_commands.json\n");
        corpus.git(&["init", "-q"]);
        corpus.commit();
        corpus.compile("-DVALUE=1", false);
        corpus
    }

    fn root_str(&self) -> String {
        self.root.to_string_lossy().into_owned()
    }

    fn write(&self, name: &str, bytes: &str) {
        std::fs::write(self.root.join(name), bytes).unwrap();
    }

    fn git(&self, args: &[&str]) {
        let mut argv: Vec<String> = vec!["git".to_string(), "-C".to_string(), self.root_str()];
        argv.extend(args.iter().map(|a| a.to_string()));
        let result = run(&argv, None);
        assert_eq!(result.exit_code, 0, "git {:?} in '{}': {}", args, self.root_str(), result.err);
    }

    fn commit(&self) {
        self.git(&["add", "-A"]);
        self.git(&["-c", "user.name=symhow", "-c", "user.email=symhow@test", "commit", "-qm", "snapshot"]);
    }

    fn compile(&self, flag: &str, header_tu: bool) {
        let root_str = self.root_str();
        let names: Vec<&str> = if header_tu { vec!["main.cpp", "value.hpp"] } else { vec!["main.cpp"] };
        let entries: Vec<serde_json::Value> = names
            .iter()
            .map(|name| {
                let file = self.root.join(name).to_string_lossy().into_owned();
                serde_json::json!({
                    "directory": root_str,
                    "file": file,
                    "arguments": ["clang++", "-x", "c++", "-std=c++20", flag, format!("-I{}", root_str), file],
                })
            })
            .collect();
        self.write("compile_commands.json", &serde_json::Value::Array(entries).to_string());
    }

    fn index(&self, output: Option<&str>, parallel: i32) {
        let mut opts = IndexRunOptions::default();
        opts.compile_commands_path = self.cdb.clone();
        opts.output_path = output.map(|o| o.to_string()).unwrap_or_else(|| self.db.clone());
        opts.force = true;
        opts.quiet = true;
        opts.super_quiet = true;
        opts.parallel = parallel;
        opts.commit_every = 1;
        let root_str = self.root_str();
        let mut prov = Provenance::default();
        prov.cdb_path = self.cdb.clone();
        prov.repo_root = root_str.clone();
        prov.repo_commit = rev_parse(&root_str, "HEAD").expect("rev-parse HEAD");
        prov.dirty_paths =
            parse_status_porcelain_z(&status_porcelain_z(&root_str).expect("git status --porcelain"), &root_str);
        opts.provenance = Some(prov);
        assert_eq!(run_index(&opts, None), 0);
    }

    fn options(&self) -> UpdateRunOptions {
        let mut opts = UpdateRunOptions::default();
        opts.db_path = self.db.clone();
        opts
    }

    fn scalar(&self, sql: &str) -> String {
        let db = Database::open(self.db.as_str(), 0).unwrap();
        let mut result = String::new();
        db.run(sql, [], |row| {
            // sqlite3_column_text() coerces whatever the column holds into
            // text (e.g. count(*) -> "2"); mirror that.
            let value: rusqlite::types::Value = row.get(0)?;
            result = match value {
                rusqlite::types::Value::Text(s) => s,
                rusqlite::types::Value::Integer(i) => i.to_string(),
                rusqlite::types::Value::Real(f) => f.to_string(),
                rusqlite::types::Value::Blob(b) => String::from_utf8_lossy(&b).into_owned(),
                rusqlite::types::Value::Null => String::new(),
            };
            Ok(())
        })
        .unwrap();
        result
    }

    fn exec(&self, sql: &str) {
        WritableDb::open(self.db.as_str(), true).unwrap().exec(sql).unwrap();
    }

    /// Re-indexes the corpus into a fresh database (with -j 2) and requires
    /// it to be logically equivalent to the current one.
    fn equivalent(&self) {
        let fresh = self.root.join("fresh.srctrldb").to_string_lossy().into_owned();
        self.index(Some(&fresh), 2);
        let a = take_snapshot(&self.db, &SnapshotOptions::default()).unwrap();
        let b = take_snapshot(&fresh, &SnapshotOptions::default()).unwrap();
        let diff = diff_snapshots(&a, &b, &[]);
        assert!(diff.only_in_a.is_empty(), "only in {}: {:?}", self.db, diff.only_in_a);
        assert!(diff.only_in_b.is_empty(), "only in {}: {:?}", fresh, diff.only_in_b);
    }
}

impl Drop for Corpus {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn fingerprint_dirty_source_header_updates_become_no_ops_but_subsequent_edits_and_reverts_still_index() {
    if !git_available() {
        eprintln!("SKIP: git not found on PATH");
        return;
    }
    for (section, file, original) in [("source", "main.cpp", SOURCE), ("header", "value.hpp", HEADER)] {
        let corpus = Corpus::new(&format!("no_op_{section}"));
        corpus.index(None, 1);
        assert_eq!(corpus.scalar("SELECT count(*) FROM symhow_file_fingerprint"), "2", "[{section}]");
        let opts = corpus.options();
        let path = corpus.root.join(file);

        // Dirty the file: one TU must be re-indexed, then a second run is a
        // no-op.
        std::fs::write(&path, format!("{}{}", original, "int extra() { return 2; }\n")).unwrap();
        let mut first = UpdateSummary::default();
        assert_eq!(run_update(&opts, Some(&mut first)), 0, "[{section}]");
        assert_eq!(first.tus_to_index, 1, "[{section}]");
        assert_eq!(run_update(&opts, None), 5, "[{section}]");
        corpus.equivalent();

        // Same size, same mtime, different bytes: mtime/size shortcuts must
        // not hide this from the content hash.
        let timestamp = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path, format!("{}{}", original, "int extra() { return 3; }\n")).unwrap();
        set_file_mtime(&path, timestamp);
        assert_eq!(run_update(&opts, None), 0, "[{section}]");
        assert_eq!(run_update(&opts, None), 5, "[{section}]");
        corpus.equivalent();

        // Revert to the committed bytes: git is now clean, but the indexed
        // content was dirty -- the revert must still be picked up.
        std::fs::write(&path, original).unwrap();
        assert_eq!(run_update(&opts, None), 0, "[{section}]");
        assert_eq!(run_update(&opts, None), 5, "[{section}]");
        corpus.equivalent();
        assert_eq!(
            corpus.scalar(
                "SELECT count(*) FROM symhow_file_fingerprint fp \
                 LEFT JOIN file f ON f.id=fp.file_node_id WHERE f.id IS NULL"
            ),
            "0",
            "[{section}]"
        );
    }
}

#[test]
fn fingerprint_full_index_of_dirty_untracked_content_already_has_usable_parsed_byte_fingerprints() {
    if !git_available() {
        eprintln!("SKIP: git not found on PATH");
        return;
    }
    for section in ["dirty source", "dirty CRLF header", "dirty header without final newline", "untracked TU"] {
        let corpus = Corpus::new(&format!("usable_{}", section.replace(' ', "_")));
        match section {
            "dirty source" => corpus.write("main.cpp", &format!("{}{}", SOURCE, "int extra;\n")),
            "dirty CRLF header" => corpus.write("value.hpp", "inline int value() { return 2; }\r\n"),
            "dirty header without final newline" => corpus.write("value.hpp", "inline int value() { return 2; }"),
            "untracked TU" => {
                corpus.git(&["rm", "--cached", "main.cpp"]);
                corpus.git(&["-c", "user.name=symhow", "-c", "user.email=symhow@test", "commit", "-qm", "untrack TU"]);
            }
            _ => unreachable!(),
        }
        corpus.index(None, 1);
        let header_path = corpus.root.join("value.hpp").to_string_lossy().into_owned();
        assert_eq!(
            corpus.scalar(
                "SELECT sha256 FROM symhow_file_fingerprint fp \
                 JOIN file f ON f.id=fp.file_node_id WHERE f.path LIKE '%value.hpp'"
            ),
            file_content_hash(&header_path).expect("hash of value.hpp"),
            "[{section}]"
        );
        let mut opts = corpus.options();

        // The dirty bytes were already indexed, so no update is needed -- and
        // a --dry-run must not touch the database file at all.
        let before = file_content_hash(&corpus.db);
        opts.dry_run = true;
        assert_eq!(run_update(&opts, None), 5, "[{section}]");
        assert_eq!(file_content_hash(&corpus.db), before, "[{section}]");
        opts.dry_run = false;
        assert_eq!(run_update(&opts, None), 5, "[{section}]");

        // Committing already indexed bytes must not trigger parsing.
        corpus.commit();
        assert_eq!(run_update(&opts, None), 5, "[{section}]");
        assert_eq!(
            corpus.scalar("SELECT value FROM meta WHERE key='symhow_repo_commit'"),
            rev_parse(&corpus.root_str(), "HEAD").expect("rev-parse HEAD"),
            "[{section}]"
        );
    }
}

#[test]
fn fingerprint_unchanged_bytes_never_suppress_forced_files_flags_retries_or_explicit_revisions() {
    if !git_available() {
        eprintln!("SKIP: git not found on PATH");
        return;
    }
    for section in [
        "forced file",
        "compiler flags",
        "partial retry",
        "failed retry",
        "explicit from revision",
        "explicit to revision",
    ] {
        let corpus = Corpus::new(&format!("unsuppressed_{}", section.replace(' ', "_")));
        let baseline = rev_parse(&corpus.root_str(), "HEAD").expect("rev-parse HEAD");
        corpus.write("main.cpp", &format!("{}{}", SOURCE, "int extra;\n"));
        corpus.index(None, 1);
        let mut opts = corpus.options();
        // The dirty bytes match the stored fingerprint: a plain update is a
        // no-op...
        assert_eq!(run_update(&opts, None), 5, "[{section}]");
        // ...but none of the cases below may be suppressed by it.
        match section {
            "forced file" => {
                opts.explicit_files = vec![corpus.root.join("main.cpp").to_string_lossy().into_owned()];
            }
            "compiler flags" => corpus.compile("-DVALUE=2", false),
            "partial retry" => corpus.exec("UPDATE symhow_tu SET status='partial'"),
            "failed retry" => corpus.exec("UPDATE symhow_tu SET status='failed'"),
            "explicit from revision" => {
                corpus.commit();
                opts.from_rev = baseline;
            }
            "explicit to revision" => {
                corpus.commit();
                opts.to_rev = "HEAD".to_string();
            }
            _ => unreachable!(),
        }
        let mut summary = UpdateSummary::default();
        assert_eq!(run_update(&opts, Some(&mut summary)), 0, "[{section}]");
        assert_eq!(summary.tus_to_index, 1, "[{section}]");
        corpus.equivalent();
    }
}

#[test]
fn fingerprint_legacy_missing_and_mismatched_fingerprints_conservatively_re_index_once() {
    if !git_available() {
        eprintln!("SKIP: git not found on PATH");
        return;
    }
    for (section, sql) in [
        ("legacy database", "DROP TABLE symhow_file_fingerprint"),
        ("missing row", "DELETE FROM symhow_file_fingerprint"),
        ("mismatched hash", "UPDATE symhow_file_fingerprint SET sha256='unknown'"),
    ] {
        let corpus = Corpus::new(&format!("conservative_{section}"));
        corpus.write("main.cpp", &format!("{}{}", SOURCE, "int extra;\n"));
        corpus.index(None, 1);
        corpus.exec(sql);
        let opts = corpus.options();
        assert_eq!(run_update(&opts, None), 0, "[{section}]");
        assert_eq!(run_update(&opts, None), 5, "[{section}]");
        corpus.equivalent();
    }
}

#[test]
fn fingerprint_a_known_header_newly_added_to_the_compilation_database_must_still_be_indexed_as_a_tu() {
    if !git_available() {
        eprintln!("SKIP: git not found on PATH");
        return;
    }
    let corpus = Corpus::new("header_tu");
    corpus.write("value.hpp", &format!("{}{}", HEADER, "int extra;\n"));
    corpus.index(None, 1);
    corpus.compile("-DVALUE=1", true);
    let opts = corpus.options();
    let mut summary = UpdateSummary::default();
    assert_eq!(run_update(&opts, Some(&mut summary)), 0);
    assert!(summary.tus_to_index >= 1);
    assert_eq!(corpus.scalar("SELECT count(*) FROM symhow_tu"), "2");
    assert_eq!(run_update(&opts, None), 5);
    corpus.equivalent();
}

#[test]
fn fingerprint_conflicting_parsed_snapshots_across_checkpoints_disable_skipping() {
    if !git_available() {
        eprintln!("SKIP: git not found on PATH");
        return;
    }
    let corpus = Corpus::new("conflicting");
    let main_cpp = corpus.root.join("main.cpp").to_string_lossy().into_owned();
    let mut backend = SrctrldbBackend::new();
    backend.begin(&corpus.db).unwrap();
    backend.start_transaction().unwrap();
    let consume = |backend: &mut SrctrldbBackend, bytes: &str| {
        let mut batch = TuFactBatch::default();
        batch.files.push(FileFact {
            path: main_cpp.clone(),
            indexed: true,
            content_hash: content_hash(bytes.as_bytes()),
            ..Default::default()
        });
        backend.consume(&batch).unwrap();
        backend.checkpoint(&[]).unwrap();
    };
    consume(&mut backend, "first parsed snapshot");
    assert_eq!(corpus.scalar("SELECT count(*) FROM symhow_file_fingerprint"), "1");
    assert_eq!(corpus.scalar("SELECT sha256 FROM symhow_file_fingerprint"), content_hash(b"first parsed snapshot"));
    // A second TU parsed the same file with different bytes: the conflict
    // permanently drops the fingerprint.
    consume(&mut backend, "second parsed snapshot");
    assert_eq!(corpus.scalar("SELECT count(*) FROM symhow_file_fingerprint"), "0");
    // A third TU must not rehabilitate mixed facts.
    consume(&mut backend, "first parsed snapshot");
    assert_eq!(corpus.scalar("SELECT count(*) FROM symhow_file_fingerprint"), "0");
    let mut summary = IndexSummary::default();
    backend.finish(&mut summary).unwrap();
}

#[test]
fn fingerprint_surviving_file_facts_cannot_be_certified_with_new_or_previously_unknown_bytes() {
    if !git_available() {
        eprintln!("SKIP: git not found on PATH");
        return;
    }
    let corpus = Corpus::new("surviving");
    let main_cpp = corpus.root.join("main.cpp").to_string_lossy().into_owned();
    let append = |bytes: &str| {
        let mut backend = SrctrldbBackend::new();
        backend.begin(&corpus.db).unwrap();
        backend.start_transaction().unwrap();
        let mut batch = TuFactBatch::default();
        batch.files.push(FileFact {
            path: main_cpp.clone(),
            indexed: true,
            content_hash: content_hash(bytes.as_bytes()),
            ..Default::default()
        });
        backend.consume(&batch).unwrap();
        backend.checkpoint(&[]).unwrap();
        let mut summary = IndexSummary::default();
        backend.finish(&mut summary).unwrap();
    };
    append("old parsed bytes");
    append("old parsed bytes");
    assert_eq!(corpus.scalar("SELECT count(*) FROM symhow_file_fingerprint"), "1");
    // Old facts survived the second identical pass; a NEW hash is not full
    // invalidation, but it must still drop the fingerprint.
    append("new parsed bytes");
    assert_eq!(corpus.scalar("SELECT count(*) FROM symhow_file_fingerprint"), "0");
    // A missing hash must not certify the old/mixed facts.
    append("new parsed bytes");
    assert_eq!(corpus.scalar("SELECT count(*) FROM symhow_file_fingerprint"), "0");
}
