//! Resolve fixture symbols independently of indexer-assigned node IDs.

use std::sync::OnceLock;

use symhow::db::name_decode::decode_serialized_name;
use symhow::db::Database;
use symhow::query::resolve_symbol;

pub fn fixture_db_path() -> String {
    super::fixtures::fixture_db()
}

// Each integration-test binary uses only part of this shared fixture catalog.
#[allow(dead_code)]
#[derive(Clone, Copy)]
pub struct FixtureIds {
    pub unsolved_hub: i64,
    pub square_area: i64,
    pub square_perimeter: i64,
    pub g_global_counter: i64,
    pub use_everything: i64,
    pub level4: i64,
    pub level3: i64,
    pub level2: i64,
    pub level1: i64,
    pub level0: i64,
    pub factorial: i64,
    pub pong: i64,
    pub ping: i64,
    pub run_chain: i64,
    pub box_touch: i64,
    pub helper_tu_a: i64,
    pub main_tu_a: i64,
    pub helper_tu_b: i64,
    pub main_tu_b: i64,
    pub accessor_value: i64,
    pub accessor_value_const: i64,
}

/// Restrict name matches by an occurrence in the fixture source file and,
/// for overloads, the exact signature postfix. Do not rely on TU hash suffixes.
fn resolve_one(db: &Database, name: &str, file: Option<&str>, postfix: Option<&str>) -> i64 {
    let resolved = resolve_symbol(db, name, None, None).unwrap();
    let mut ids = Vec::new();
    for candidate in resolved.candidates {
        if let Some(postfix) = postfix {
            let decoded = decode_serialized_name(&candidate.serialized_name);
            if decoded.elements.last().unwrap().postfix != postfix {
                continue;
            }
        }
        if let Some(file) = file {
            let mut matches_file = false;
            db.run(
                "SELECT f.path FROM occurrence o
                 JOIN source_location l ON l.id = o.source_location_id
                 JOIN file f ON f.id = l.file_node_id
                 WHERE o.element_id = ?",
                [candidate.id],
                |row| {
                    let path: String = row.get(0)?;
                    matches_file |= path.replace('\\', "/").ends_with(file);
                    Ok(())
                },
            )
            .unwrap();
            if !matches_file {
                continue;
            }
        }
        ids.push(candidate.id);
    }
    assert_eq!(
        ids.len(),
        1,
        "expected one fixture symbol for {name:?} in {file:?} with postfix {postfix:?}, got {ids:?}"
    );
    ids[0]
}

pub fn fixture_ids() -> &'static FixtureIds {
    static IDS: OnceLock<FixtureIds> = OnceLock::new();
    IDS.get_or_init(|| {
        let db = Database::open(fixture_db_path(), 0).unwrap();
        let in_file = |name, file| resolve_one(&db, name, Some(file), None);
        FixtureIds {
            unsolved_hub: resolve_one(&db, "unsolved symbol", None, None),
            square_area: in_file("fixture::Square::area", "/include/shapes.hpp"),
            square_perimeter: in_file("fixture::squarePerimeter", "/src/ns.cpp"),
            g_global_counter: in_file("gGlobalCounter", "/src/ns.cpp"),
            use_everything: in_file("useEverything", "/src/ns.cpp"),
            level4: in_file("level4", "/src/chain.cpp"),
            level3: in_file("level3", "/src/chain.cpp"),
            level2: in_file("level2", "/src/chain.cpp"),
            level1: in_file("level1", "/src/chain.cpp"),
            level0: in_file("level0", "/src/chain.cpp"),
            factorial: in_file("factorial", "/src/chain.cpp"),
            pong: in_file("pong", "/src/chain.cpp"),
            ping: in_file("ping", "/src/chain.cpp"),
            run_chain: in_file("runChain", "/src/chain.cpp"),
            box_touch: in_file("Box::touch", "/src/templates.cpp"),
            helper_tu_a: in_file("helper", "/src/tu_a.cpp"),
            main_tu_a: in_file("main", "/src/tu_a.cpp"),
            helper_tu_b: in_file("helper", "/src/tu_b.cpp"),
            main_tu_b: in_file("main", "/src/tu_b.cpp"),
            accessor_value: resolve_one(
                &db,
                "Accessor::value",
                Some("/src/overloads.cpp"),
                Some("()"),
            ),
            accessor_value_const: resolve_one(
                &db,
                "Accessor::value",
                Some("/src/overloads.cpp"),
                Some("() const"),
            ),
        }
    })
}

pub fn sorted_ids<const N: usize>(mut ids: [i64; N]) -> Vec<i64> {
    ids.sort_unstable();
    ids.to_vec()
}
