//! Language registry: grammars + compiled symbol queries per language.
//! CONTRACT: see SPEC.md `lang.rs`. Owner: queries agent.
//!
//! Query capture convention (see src/queries/<lang>/symbols.scm):
//! - `@symbol.<kind>` is captured on the *name node* of every definition,
//!   where `<kind>` is one of the `SymbolKind` names.
//! - `@scope` is captured on the name of enclosing scope nodes (namespaces,
//!   classes/structs/enums/unions, `impl` items, `mod` items, traits, …) so
//!   the indexer can build qualified container paths by tracking ancestors.
//!
//! Duplicate-capture note: a few definitions match two patterns on purpose
//! (e.g. a C++ constructor defined inline matches the generic
//! `@symbol.function` pattern *and* the `@symbol.constructor` pattern; a Rust
//! `fn` inside an `impl`/`trait` matches both `@symbol.function` and
//! `@symbol.method`). Consumers should treat the most specific kind
//! (constructor/method) as authoritative when both land on the same node.
use crate::types::{Lang, SymbolKind};
use anyhow::{Context, Result};
use std::path::Path;
use std::str::FromStr;

const C_SYMBOLS: &str = include_str!("queries/c/symbols.scm");
const CPP_SYMBOLS: &str = include_str!("queries/cpp/symbols.scm");
const RUST_SYMBOLS: &str = include_str!("queries/rust/symbols.scm");

pub struct LangRegistry;

impl LangRegistry {
    pub fn new() -> Self {
        LangRegistry
    }

    /// Map a CLI language name (`c`, `cpp`, `cuda`, `rust`, …) to a `Lang`.
    /// Part of the public registry API; currently consumed by tests only.
    #[allow(dead_code)]
    pub fn lang_by_name(name: &str) -> Option<Lang> {
        Lang::from_str(name).ok()
    }

    /// Detect the language of `path`, with `forced` taking precedence.
    #[allow(dead_code)]
    pub fn lang_for_path(&self, path: &Path, forced: Option<Lang>) -> Option<Lang> {
        forced.or_else(|| Lang::detect(path))
    }

    /// Tree-sitter grammar for a language (CUDA reuses the C++ grammar).
    pub fn grammar(&self, lang: Lang) -> tree_sitter::Language {
        match lang {
            Lang::C => tree_sitter::Language::from(tree_sitter_c::LANGUAGE),
            Lang::Cpp | Lang::Cuda => tree_sitter::Language::from(tree_sitter_cpp::LANGUAGE),
            Lang::Rust => tree_sitter::Language::from(tree_sitter_rust::LANGUAGE),
        }
    }

    /// Compile the per-language symbol query embedded from
    /// `src/queries/<lang>/symbols.scm`.
    pub fn symbols_query(&self, lang: Lang) -> Result<tree_sitter::Query> {
        let source = match lang {
            Lang::C => C_SYMBOLS,
            Lang::Cpp | Lang::Cuda => CPP_SYMBOLS,
            Lang::Rust => RUST_SYMBOLS,
        };
        tree_sitter::Query::new(&self.grammar(lang), source).with_context(|| {
            format!(
                "failed to compile symbols query for language `{}`",
                lang.name()
            )
        })
    }

    /// Map a capture name like `symbol.function` to a `SymbolKind`.
    /// Returns `None` for anything else (e.g. `@scope`).
    #[allow(dead_code)]
    pub fn capture_kind(name: &str) -> Option<SymbolKind> {
        let name = name.strip_prefix('@').unwrap_or(name);
        let kind = name.strip_prefix("symbol.")?;
        SymbolKind::from_str(kind).ok()
    }
}

impl Default for LangRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SymbolKind;

    #[test]
    fn all_language_queries_compile() {
        let registry = LangRegistry::new();
        for lang in Lang::ALL {
            let err = registry.symbols_query(lang).err().map(|e| format!("{e:#}"));
            assert!(
                err.is_none(),
                "symbols query for {} failed: {:?}",
                lang.name(),
                err
            );
        }
    }

    #[test]
    fn capture_kind_maps_symbol_names() {
        for kind in SymbolKind::ALL {
            assert_eq!(
                LangRegistry::capture_kind(&format!("symbol.{}", kind.name())),
                Some(kind),
                "capture `symbol.{}` should map to SymbolKind::{kind:?}",
                kind.name()
            );
        }
        assert_eq!(LangRegistry::capture_kind("scope"), None);
        assert_eq!(LangRegistry::capture_kind("symbol.nonexistent"), None);
    }

    /// End-to-end check: run each language's query over a representative
    /// snippet and assert the expected kinds are captured.
    #[test]
    fn queries_capture_expected_kinds() {
        use tree_sitter::StreamingIterator;
        let registry = LangRegistry::new();
        let cases: &[(Lang, &str, &[&str])] = &[
            (
                Lang::C,
                C_SNIPPET,
                &[
                    "function",
                    "struct",
                    "field",
                    "enum",
                    "enum-member",
                    "type-alias",
                    "macro",
                ],
            ),
            (
                Lang::Cpp,
                CPP_SNIPPET,
                &[
                    "function",
                    "method",
                    "constructor",
                    "operator",
                    "class",
                    "struct",
                    "enum-member",
                    "namespace",
                    "type-alias",
                    "macro",
                    "interface",
                ],
            ),
            (
                Lang::Rust,
                RUST_SNIPPET,
                &[
                    "function",
                    "method",
                    "struct",
                    "enum",
                    "enum-member",
                    "field",
                    "trait",
                    "impl",
                    "module",
                    "constant",
                    "static",
                    "macro",
                    "type-alias",
                    "union",
                ],
            ),
        ];
        for (lang, src, expected) in cases {
            let query = registry.symbols_query(*lang).expect("query compiles");
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&registry.grammar(*lang))
                .expect("set language");
            let tree = parser.parse(src, None).expect("parse");
            let mut kinds = std::collections::BTreeSet::new();
            let mut cursor = tree_sitter::QueryCursor::new();
            let mut matches = cursor.matches(&query, tree.root_node(), src.as_bytes());
            while let Some(m) = matches.next() {
                for c in m.captures {
                    if let Some(kind) =
                        LangRegistry::capture_kind(query.capture_names()[c.index as usize])
                    {
                        kinds.insert(kind.name());
                    }
                }
            }
            for kind in *expected {
                assert!(
                    kinds.contains(*kind),
                    "{} query should capture `{kind}` kinds; got {kinds:?}",
                    lang.name()
                );
            }
        }
    }

    /// Ground-truth probe: dump CST s-expressions for tricky snippets so the
    /// query patterns can be checked against the real grammar shapes.
    #[test]
    #[ignore = "manual grammar probe; run with --ignored --nocapture"]
    fn probe_tree_shapes() {
        let registry = LangRegistry::new();
        let cases: &[(Lang, &str)] = &[
            (Lang::Cpp, CPP_SNIPPET),
            (Lang::C, C_SNIPPET),
            (Lang::Rust, RUST_SNIPPET),
        ];
        for (lang, src) in cases {
            let mut parser = tree_sitter::Parser::new();
            parser.set_language(&registry.grammar(*lang)).unwrap();
            let tree = parser.parse(src, None).unwrap();
            println!("===== {:?} =====\n{}", lang, tree.root_node().to_sexp());
        }
    }

    const CPP_SNIPPET: &str = r#"
namespace a::b { void f() {} }
namespace N { void g() {} }
class Foo {
public:
    Foo(int x);
    void bar() {}
    ~Foo();
    int& operator*();
    operator bool() const;
    static int s;
    int field;
};
void Foo::bar() {}
Foo::~Foo() {}
void ns::fn() {}
template <typename T> T tf(T x) { return x; }
int *ptr_fn(void) { return 0; }
int (*fpdef(void))(int) { return 0; }
using A = int;
concept Addable = requires(int x) { x + x; };
#define FUNC(x) ((x) + 1)
#define CONST 1
enum class Color { Red, Green };
enum E { EA, EB };
struct S { int y; };
union U { int z; };
typedef int MyInt;
typedef int (*cb)(int);
void proto(int);
struct Fwd;
struct WithBody { int m; };
int *member_ptr;
"#;

    const C_SNIPPET: &str = r#"
int plain(void) {}
int *ptr_fn(void) { return 0; }
int (*fpdef(void))(int) { return 0; }
struct S { int y; int *p; int (*cb)(int); int arr[3]; };
union U { int z; };
enum E { EA, EB };
typedef int MyInt;
typedef int (*cb)(int);
typedef struct Node { int v; } Node;
#define FUNC(x) ((x) + 1)
#define CONST 1
void proto(int);
static int g_var = 1;
"#;

    const RUST_SNIPPET: &str = r#"
mod m { pub fn inmod() {} }
pub fn free() {}
struct S { f: u32 }
enum E { V1, V2(u32) }
trait Tr { fn tm(&self); fn td(&self) {} }
impl S { fn new() -> Self { todo!() } fn m(&self) {} }
trait Disp {}
impl Disp for S { fn d(&self) {} }
impl<T> G<T> where T: Clone { fn gm(&self) {} }
type Alias = u32;
const C: u32 = 1;
static ST: u32 = 1;
macro_rules! mac { () => {} }
union U { u: u32 }
extern "C" { fn ext_fn(x: i32); }
"#;
}
