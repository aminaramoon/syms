//! Parallel repo indexer.
//! CONTRACT: see SPEC.md `index.rs`. Owner: indexer agent.
use crate::lang::LangRegistry;
use crate::store::{db_path, Store};
use crate::types::{FileRecord, Lang, SymbolKind, SymbolRecord};
use crate::walk::{self, Filter};
use anyhow::{Context, Result};
use rayon::prelude::*;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::Instant;
use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

/// Options for a full (re)index, mirroring the `symwhere index` CLI.
pub struct IndexOptions {
    pub path: PathBuf,
    pub langs: BTreeSet<Lang>,
    pub excludes: Vec<String>,
    pub includes: Vec<String>,
    pub force: bool,
    pub db: Option<PathBuf>,
    pub jobs: Option<usize>,
}

/// Result of parsing one file: its record, its symbols, and a per-file error
/// (warnings keep the index going; only db/walk failures are fatal).
struct FileOutcome {
    rec: FileRecord,
    syms: Vec<SymbolRecord>,
    error: Option<String>,
}

/// Build (or rebuild) the symbol index for `opts.path`.
pub fn run_index(opts: IndexOptions) -> Result<()> {
    let started = Instant::now();
    let root = opts
        .path
        .canonicalize()
        .with_context(|| format!("cannot access repo root {}", opts.path.display()))?;

    let db_file = db_path(&root, &opts.db);
    if opts.force {
        // Remove the database (and WAL sidecars) before opening fresh.
        for suffix in ["", "-wal", "-shm"] {
            let target = PathBuf::from(format!("{}{suffix}", db_file.display()));
            let _ = fs::remove_file(&target);
        }
    }
    if let Some(parent) = db_file.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("cannot create db directory {}", parent.display()))?;
        }
    }

    let filter = Filter {
        excludes: opts.excludes.clone(),
        includes: opts.includes.clone(),
        langs: opts.langs.clone(),
    };
    let files = walk::walk(&root, &filter);
    let total_files = files.len();

    // Compile one symbols query per language up front.
    let registry = LangRegistry::new();
    let mut queries: HashMap<Lang, Query> = HashMap::new();
    for lang in &opts.langs {
        match registry.symbols_query(*lang) {
            Ok(query) => {
                queries.insert(*lang, query);
            }
            Err(err) => {
                eprintln!("symwhere: warning: no symbols query for {lang}: {err:#}");
            }
        }
    }

    // Single writer thread owns the Store; rayon workers stream results to it.
    let store = Store::open(&db_file, true)
        .with_context(|| format!("cannot open database {}", db_file.display()))?;
    let (tx, rx) = mpsc::channel::<FileOutcome>();
    let tx = Mutex::new(tx);
    let writer = std::thread::spawn(move || {
        let mut store = store;
        let mut files_done = 0usize;
        let mut syms_total = 0usize;
        let mut first_error: Option<anyhow::Error> = None;
        for outcome in rx {
            files_done += 1;
            if let Some(err) = &outcome.error {
                eprintln!("symwhere: warning: {}: {err:#}", outcome.rec.path);
            }
            let write = store.upsert_file_with_symbols(&outcome.rec, &outcome.syms);
            if let Err(err) = write {
                if first_error.is_none() {
                    first_error = Some(
                        err.context(format!("writing {} to the index failed", outcome.rec.path)),
                    );
                }
            }
            syms_total += outcome.syms.len();
            if files_done.is_multiple_of(500) {
                eprintln!(
                    "symwhere: {files_done}/{total_files} files, {syms_total} symbols, {:.0}s",
                    started.elapsed().as_secs_f32()
                );
            }
        }
        (store, files_done, syms_total, first_error)
    });

    let thread_count = opts.jobs.unwrap_or(0); // 0 = rayon default (all cores)
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(thread_count)
        .build()
        .context("failed to start worker thread pool")?;

    pool.install(|| {
        files.par_iter().for_each_init(
            || (LangRegistry::new(), Parser::new()),
            |(registry, parser), path| {
                let outcome = process_file(path, &root, &queries, registry, parser);
                if let Ok(guard) = tx.lock() {
                    let _ = guard.send(outcome);
                }
            },
        );
    });
    drop(tx); // close the channel so the writer can finish

    let (mut store, files_done, syms_total, first_error) = writer
        .join()
        .map_err(|_| anyhow::anyhow!("index writer thread panicked"))?;

    store
        .set_meta("root", &root.to_string_lossy())
        .context("storing meta root")?;
    if let Some(head) = git_head(&root) {
        store.set_meta("head", &head).context("storing meta head")?;
    }
    store
        .set_meta("schema_version", "1")
        .context("storing meta schema_version")?;

    if let Some(err) = first_error {
        return Err(err);
    }

    println!(
        "indexed {files_done} files, {syms_total} symbols in {:.1}s (db: {})",
        started.elapsed().as_secs_f32(),
        db_file.display()
    );
    Ok(())
}

/// Read and hash one file, parse it, and package the outcome. Errors are
/// reported per file (warning + continue), never abort the run.
fn process_file(
    path: &Path,
    root: &Path,
    queries: &HashMap<Lang, Query>,
    registry: &LangRegistry,
    parser: &mut Parser,
) -> FileOutcome {
    let rel = rel_path(path, root);
    let outcome_for = |rec: FileRecord, syms: Vec<SymbolRecord>, error: Option<String>| {
        FileOutcome { rec, syms, error }
    };
    let rec = |hash: String, size: u64| FileRecord {
        path: rel.clone(),
        lang: Lang::detect(path).unwrap_or(Lang::C),
        hash,
        size,
    };

    let src = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) => {
            return outcome_for(
                rec(String::new(), 0),
                Vec::new(),
                Some(format!("read failed: {err}")),
            );
        }
    };
    let rec = rec(blake3::hash(&src).to_string(), src.len() as u64);

    let Some(lang) = Lang::detect(path) else {
        return outcome_for(rec, Vec::new(), Some("unknown language".into()));
    };
    let Some(query) = queries.get(&lang) else {
        return outcome_for(
            rec,
            Vec::new(),
            Some(format!("no symbols query for {lang}")),
        );
    };

    match parse_with_query(&src, lang, parser, query, registry) {
        Ok(syms) => outcome_for(rec, syms, None),
        Err(err) => outcome_for(rec, Vec::new(), Some(format!("{err:#}"))),
    }
}

/// Repo-relative, `/`-separated path (best effort: falls back to the file name).
fn rel_path(path: &Path, root: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) => {
            let s = rel.to_string_lossy();
            if std::path::MAIN_SEPARATOR == '/' {
                s.into_owned()
            } else {
                s.replace(std::path::MAIN_SEPARATOR, "/")
            }
        }
        Err(_) => path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

/// `git -C root rev-parse HEAD`, if the repo has one.
fn git_head(root: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let head = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if head.is_empty() {
        None
    } else {
        Some(head)
    }
}

/// Stateless single-file parse used by `index` and `symwhere symbols`.
pub fn parse_file(
    path: &Path,
    lang: Lang,
    src: &[u8],
    registry: &LangRegistry,
) -> Result<Vec<SymbolRecord>> {
    let mut parser = Parser::new();
    let query = registry
        .symbols_query(lang)
        .with_context(|| format!("compiling symbols query for {lang}"))?;
    parse_with_query(src, lang, &mut parser, &query, registry)
        .with_context(|| format!("parsing {}", path.display()))
}

/// Parse `src` with a prebuilt `query` (so a batch run compiles each
/// language's query once). Extracts symbol records:
/// - `@symbol.<kind>` captures name the *name node* of each definition.
/// - `@scope` captures mark enclosing scopes used to build `container`.
pub fn parse_with_query(
    src: &[u8],
    lang: Lang,
    parser: &mut Parser,
    query: &Query,
    registry: &LangRegistry,
) -> Result<Vec<SymbolRecord>> {
    parser
        .set_language(&registry.grammar(lang))
        .with_context(|| format!("loading {lang} grammar"))?;
    let tree = parser
        .parse(src, None)
        .context("tree-sitter produced no syntax tree")?;
    let root = tree.root_node();

    // Resolve capture indices once: `@symbol.<kind>` → SymbolKind, `@scope`.
    let mut kind_by_index: HashMap<u32, SymbolKind> = HashMap::new();
    for kind in SymbolKind::ALL {
        let name = format!("symbol.{}", kind.name());
        if let Some(index) = query.capture_index_for_name(&name) {
            kind_by_index.entry(index).or_insert(kind);
        }
    }
    let scope_index = query.capture_index_for_name("scope");

    let mut cursor = QueryCursor::new();
    let mut seen_scopes: HashSet<usize> = HashSet::new();
    let mut symbol_slot: HashMap<usize, usize> = HashMap::new();
    // (name node, kinds captured on it — one node can match several patterns,
    // e.g. a Rust `fn` in an `impl` is both `@symbol.function` and
    // `@symbol.method`; the most specific kind wins, see `choose_kind`)
    let mut symbols: Vec<(Node, Vec<SymbolKind>)> = Vec::new();
    // (scope node spanning the scope, displayed scope name)
    let mut scopes: Vec<(Node, String)> = Vec::new();

    let mut matches = cursor.matches(query, root, src);
    while let Some(m) = matches.next() {
        for capture in m.captures {
            let node = capture.node;
            if let Some(kind) = kind_by_index.get(&capture.index) {
                match symbol_slot.get(&node.id()) {
                    Some(&slot) => {
                        if !symbols[slot].1.contains(kind) {
                            symbols[slot].1.push(*kind);
                        }
                    }
                    None => {
                        symbol_slot.insert(node.id(), symbols.len());
                        symbols.push((node, vec![*kind]));
                    }
                }
            } else if Some(capture.index) == scope_index && seen_scopes.insert(node.id()) {
                if let Some(entry) = scope_entry(node, src) {
                    scopes.push(entry);
                }
            }
        }
    }

    // Outermost scope first: earlier start, then the wider node wins.
    scopes.sort_by(|a, b| {
        a.0.start_byte()
            .cmp(&b.0.start_byte())
            .then(b.0.end_byte().cmp(&a.0.end_byte()))
    });

    symbols.sort_by_key(|(node, _)| node.start_byte());

    let mut out = Vec::with_capacity(symbols.len());
    for (name_node, kinds) in symbols {
        let name = name_node.utf8_text(src).unwrap_or_default().trim();
        if name.is_empty() {
            continue;
        }
        let Some(kind) = choose_kind(&kinds) else {
            continue;
        };
        // Span: start points at the name node (precise jump target), the end
        // covers the enclosing definition so `--within` line filtering works.
        let start = name_node.start_position();
        let container = container_for(name_node, &scopes);
        let outer = outer_definition(name_node, root);
        let end = outer.end_position();
        let signature = build_signature(outer.utf8_text(src).unwrap_or_default());
        out.push(SymbolRecord {
            name: name.to_string(),
            kind,
            container,
            start_line: (start.row + 1) as u32,
            start_col: start.column as u32,
            end_line: (end.row + 1) as u32,
            end_col: end.column as u32,
            signature,
        });
    }
    Ok(out)
}

/// Pick the most specific kind when a definition node carries several
/// `@symbol.*` captures (see lang.rs duplicate-capture note): a constructor
/// or method beats the generic `function` capture; ties keep first-seen.
fn choose_kind(kinds: &[SymbolKind]) -> Option<SymbolKind> {
    kinds.iter().copied().min_by_key(|kind| match kind {
        SymbolKind::Constructor => 0u8,
        SymbolKind::Method => 1,
        SymbolKind::Operator => 2,
        SymbolKind::Function => 3,
        _ => 4,
    })
}

/// A `@scope` capture may sit on the scope's *name node* (a leaf) or on the
/// scope node itself. Normalize to (spanning scope node, display name).
///
/// Leaves inside a `qualified_identifier` (out-of-class C++ definitions like
/// `RingBuffer<T>::push`, `Foo::count`) use the qualified identifier's parent
/// as the span: the scope segment applies to the identifier that follows it,
/// and using the QI itself would collide with the direct-parent exclusion in
/// [`container_for`]. The queries only emit these captures on gated member
/// patterns, so namespace-qualified free functions never gain a bogus segment.
fn scope_entry<'a>(node: Node<'a>, src: &'a [u8]) -> Option<(Node<'a>, String)> {
    if node.child_count() == 0 {
        // Bare name node: the parent is the scope that name introduces.
        let parent = node.parent()?;
        let text = node.utf8_text(src).unwrap_or_default().trim();
        if text.is_empty() {
            return None;
        }
        if parent.kind() == "qualified_identifier" {
            let span = parent.parent()?;
            return Some((span, text.to_string()));
        }
        Some((parent, text.to_string()))
    } else {
        Some((node, container_node_name(node, src)))
    }
}

/// Best-effort display name for a scope node: its `name` field child if it
/// has one, else the header text before the first `{`/`;` (e.g. Rust
/// `impl<T> Foo<T>` blocks have no `name` field).
fn container_node_name(node: Node, src: &[u8]) -> String {
    if let Some(name) = node.child_by_field_name("name") {
        let text = name.utf8_text(src).unwrap_or_default().trim();
        if !text.is_empty() {
            return text.to_string();
        }
    }
    build_signature(node.utf8_text(src).unwrap_or_default()).unwrap_or_default()
}

/// Names of the scopes that enclose `name_node`, ordered outermost →
/// innermost and joined with `::`. The node a scope capture sits on is the
/// symbol's own definition when it is the name node's direct parent, so that
/// entry is excluded (a struct is not inside itself).
fn container_for(name_node: Node, scopes: &[(Node, String)]) -> Option<String> {
    let parent = name_node.parent();
    let parent_id = parent.map(|p| p.id());
    let start = name_node.start_byte();
    let end = name_node.end_byte();
    // Out-of-class C++ definitions (`RingBuffer<T>::push`): the scope segment
    // of the qualified identifier precedes the name instead of enclosing it,
    // so accept scope entries that sit inside the QI's `scope` field.
    let qi_scope = parent
        .filter(|p| p.kind() == "qualified_identifier")
        .and_then(|p| p.child_by_field_name("scope"));
    let mut names: Vec<&str> = Vec::new();
    for (node, text) in scopes {
        let encloses = node.start_byte() <= start && end <= node.end_byte();
        let in_qi_scope = qi_scope.is_some_and(|s| {
            s.start_byte() <= node.start_byte() && node.end_byte() <= s.end_byte()
        });
        if (encloses || in_qi_scope)
            && Some(node.id()) != parent_id
            && names.last() != Some(&text.as_str())
        {
            names.push(text);
        }
    }
    if names.is_empty() {
        None
    } else {
        Some(names.join("::"))
    }
}

/// The "outer" definition node for a signature: climb from the name node
/// through ancestors that start on the same line as the name (staying within
/// the definition, never reaching the root).
fn outer_definition<'a>(name_node: Node<'a>, root: Node<'a>) -> Node<'a> {
    let name_row = name_node.start_position().row;
    let mut best = name_node;
    let mut current = name_node;
    while let Some(parent) = current.parent() {
        if parent.id() == root.id() || parent.start_position().row != name_row {
            break;
        }
        best = parent;
        current = parent;
    }
    best
}

/// One-line signature from a definition node's text: text up to the first
/// `{` or `;`, whitespace-collapsed, leading Rust `#[...]` attributes
/// stripped, truncated to ~120 chars with " …". Total: never panics.
fn build_signature(text: &str) -> Option<String> {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut = collapsed.find(['{', ';']).unwrap_or(collapsed.len());
    let mut sig = strip_rust_attributes(collapsed[..cut].trim());
    if sig.is_empty() {
        // Header began with `{`/`;`: fall back to the first non-empty line.
        sig = text
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("")
            .trim_end_matches('{')
            .trim()
            .to_string();
    }
    if sig.is_empty() {
        return None;
    }
    const MAX: usize = 120;
    if sig.chars().count() <= MAX {
        Some(sig)
    } else {
        let prefix: String = sig.chars().take(MAX - 2).collect();
        Some(format!("{prefix} …"))
    }
}

/// Remove leading `#[attr]` / `#![...]` groups (Rust attributes).
fn strip_rust_attributes(text: &str) -> String {
    let mut rest = text.trim_start();
    while rest.starts_with("#[") || rest.starts_with("#![") {
        let open = rest.find('[').unwrap_or(0);
        let Some(close_rel) = matching_bracket(&rest[open..]) else {
            break;
        };
        rest = rest[open + close_rel + 1..].trim_start();
    }
    rest.trim().to_string()
}

/// Index of the `]` matching a `[` at position 0 of `s`, or None.
fn matching_bracket(s: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (i, ch) in s.char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::LangRegistry;
    use std::panic::{self, AssertUnwindSafe};

    /// Build a registry, tolerating a still-stubbed lang.rs (its methods
    /// `todo!()`): tests skip with a note until the queries agent lands.
    fn registry_ready() -> Option<LangRegistry> {
        let prev_hook = panic::take_hook();
        panic::set_hook(Box::new(|_| {}));
        let result = panic::catch_unwind(|| {
            let registry = LangRegistry::new();
            let _ = registry.grammar(Lang::Rust);
            registry
        });
        panic::set_hook(prev_hook);
        result.ok()
    }

    #[test]
    fn signature_short_and_truncated() {
        assert_eq!(
            build_signature("void f() { return; }"),
            Some("void f()".into())
        );
        assert_eq!(
            build_signature("pub fn new() -> Self {"),
            Some("pub fn new() -> Self".into())
        );
        assert_eq!(
            build_signature("type Foo = Bar;"),
            Some("type Foo = Bar".into())
        );
        assert_eq!(build_signature("{\n    junk\n}"), None); // falls to empty
        assert_eq!(build_signature(""), None);

        let long = format!("void f({}) {{ }}", "x".repeat(300));
        let sig = build_signature(&long).expect("non-empty");
        assert!(sig.ends_with(" …"));
        assert_eq!(sig.chars().count(), 120);

        // Multi-line header is collapsed onto one line.
        let multi = "pub fn big(\n    a: u32,\n    b: u32,\n) -> u32 {\n    0\n}";
        assert_eq!(
            build_signature(multi),
            Some("pub fn big( a: u32, b: u32, ) -> u32".into())
        );

        // Leading Rust attribute is stripped.
        assert_eq!(
            build_signature("#[inline]\npub fn f() {"),
            Some("pub fn f()".into())
        );
    }

    #[test]
    fn parses_rust_symbols_with_container_and_signature() {
        let Some(registry) = registry_ready() else {
            eprintln!("note: lang.rs still stubbed; skipping rust parse test");
            return;
        };
        let grammar = registry.grammar(Lang::Rust);
        let query = Query::new(
            &grammar,
            "(struct_item name: (type_identifier) @symbol.struct)
             (function_item name: (identifier) @symbol.function)
             (impl_item body: (declaration_list
                (function_item name: (identifier) @symbol.method)))
             (impl_item type: (type_identifier) @scope)",
        )
        .expect("test query compiles");

        let src: &[u8] = b"struct Widget {\n    name: String,\n}\n\nimpl Widget {\n    pub fn new(name: &str) -> Self {\n        Widget { name: name.to_string() }\n    }\n}\n";
        let mut parser = Parser::new();
        let syms = parse_with_query(src, Lang::Rust, &mut parser, &query, &registry)
            .expect("parse succeeds");

        let widget = syms
            .iter()
            .find(|s| s.name == "Widget" && s.kind == SymbolKind::Struct)
            .expect("struct Widget found");
        assert_eq!(widget.start_line, 1);
        assert_eq!(widget.start_col, 7);
        assert_eq!(widget.end_line, 3); // whole `struct_item` spans to `}`
        assert_eq!(widget.container, None);
        assert_eq!(widget.signature.as_deref(), Some("struct Widget"));

        let new_fn = syms.iter().find(|s| s.name == "new").expect("fn new found");
        // `fn` inside an impl matches both @symbol.function and
        // @symbol.method; the more specific kind must win.
        assert_eq!(new_fn.kind, SymbolKind::Method);
        assert_eq!(new_fn.container.as_deref(), Some("Widget"));
        assert_eq!(new_fn.start_line, 6);
        assert_eq!(new_fn.end_line, 8); // whole `function_item`
        assert_eq!(
            new_fn.signature.as_deref(),
            Some("pub fn new(name: &str) -> Self")
        );

        // Deterministic, source order.
        let starts: Vec<u32> = syms.iter().map(|s| s.start_line).collect();
        assert!(starts.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn parses_cpp_symbols_with_nested_containers() {
        let Some(registry) = registry_ready() else {
            eprintln!("note: lang.rs still stubbed; skipping cpp parse test");
            return;
        };
        let grammar = registry.grammar(Lang::Cpp);
        let query = Query::new(
            &grammar,
            "(function_definition
                declarator: (function_declarator
                    declarator: (field_identifier) @symbol.method))
             (class_specifier name: (type_identifier) @symbol.class)
             (class_specifier name: (type_identifier) @scope)
             (namespace_definition name: (namespace_identifier) @scope)",
        )
        .expect("test query compiles");

        let src: &[u8] = b"namespace util {\nclass Buffer {\npublic:\n    int size() const { return size_; }\nprivate:\n    int size_;\n};\n}\n";
        let mut parser = Parser::new();
        let syms = parse_with_query(src, Lang::Cpp, &mut parser, &query, &registry)
            .expect("parse succeeds");

        let buffer = syms
            .iter()
            .find(|s| s.name == "Buffer" && s.kind == SymbolKind::Class)
            .expect("class Buffer found");
        assert_eq!(buffer.start_line, 2);
        assert_eq!(buffer.container.as_deref(), Some("util"));

        let size = syms.iter().find(|s| s.name == "size").expect("size found");
        assert_eq!(size.container.as_deref(), Some("util::Buffer"));
        assert_eq!(size.start_line, 4);
        assert_eq!(size.start_col, 8);
        assert_eq!(size.signature.as_deref(), Some("int size() const"));
    }

    #[test]
    fn parse_file_uses_registry_query() {
        let Some(registry) = registry_ready() else {
            eprintln!("note: lang.rs still stubbed; skipping parse_file test");
            return;
        };
        // symbols_query comes from src/queries/<lang>/symbols.scm; skip until
        // the queries agent provides them.
        let query_check =
            panic::catch_unwind(AssertUnwindSafe(|| registry.symbols_query(Lang::Rust)));
        let Ok(Ok(_)) = query_check else {
            eprintln!("note: queries/<lang>/symbols.scm missing; skipping parse_file test");
            return;
        };
        let src = b"fn standalone() {}\n".as_slice();
        let syms = parse_file(Path::new("lib.rs"), Lang::Rust, src, &registry)
            .expect("parse_file succeeds");
        assert!(syms.iter().any(|s| s.name == "standalone"));
    }
}
