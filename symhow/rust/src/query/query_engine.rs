//! One method per `symhow query` verb. Mirrors `symhow::query::QueryEngine`.

use std::collections::{HashMap, HashSet};

use rusqlite::types::Value;

use crate::common::Result;
use crate::db::enums::{edge_kind, location_kind};
use crate::db::name_decode::decode_serialized_name;
use crate::db::Database;

use super::bfs::{run_bfs, BfsEdge, BfsNode, BfsParams, BfsResult, CHUNK_SIZE};
use super::fuzzy_matcher::fuzzy_match;
use super::results::{
    DefinitionResult, FileLocation, FileOccurrence, FileResult, KindCount, ReferenceHit,
    ReferencesResult, SearchHit, StatsErrorRow, StatsResult,
};

fn sort_nodes(nodes: &mut [BfsNode]) {
    nodes.sort_by(|a, b| {
        a.depth
            .cmp(&b.depth)
            .then(a.kind.cmp(&b.kind))
            .then(a.serialized_name.cmp(&b.serialized_name))
            .then(a.id.cmp(&b.id))
    });
}

fn sort_edges(edges: &mut [BfsEdge]) {
    edges.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then(a.source.cmp(&b.source))
            .then(a.target.cmp(&b.target))
            .then(a.id.cmp(&b.id))
    });
}

fn sort_locations(locs: &mut [FileLocation]) {
    locs.sort_by(|a, b| {
        a.file_path
            .cmp(&b.file_path)
            .then(a.start_line.cmp(&b.start_line))
            .then(a.start_column.cmp(&b.start_column))
            .then(a.file_id.cmp(&b.file_id))
    });
}

pub struct QueryEngine<'a> {
    db: &'a Database,
    file_by_id: HashMap<i64, String>,
}

impl<'a> QueryEngine<'a> {
    pub fn new(db: &'a Database) -> Result<Self> {
        let mut engine = QueryEngine {
            db,
            file_by_id: HashMap::new(),
        };
        engine.load_file_map()?;
        Ok(engine)
    }

    fn load_file_map(&mut self) -> Result<()> {
        let db = self.db;
        db.run("SELECT id, path FROM file", [], |row| {
            self.file_by_id.insert(
                row.get::<_, i64>(0)?,
                row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            );
            Ok(())
        })
    }

    fn resolve_file_path(&self, file_id: i64) -> String {
        self.file_by_id.get(&file_id).cloned().unwrap_or_default()
    }

    fn sorted_bfs(&self, params: BfsParams) -> Result<BfsResult> {
        let mut result = run_bfs(self.db, &params)?;
        sort_nodes(&mut result.nodes);
        sort_edges(&mut result.edges);
        Ok(result)
    }

    pub fn callers(
        &self,
        symbol_ids: &[i64],
        depth: i32,
        max_nodes: i32,
        node_kind_mask: i32,
    ) -> Result<BfsResult> {
        self.sorted_bfs(BfsParams {
            seeds: symbol_ids.to_vec(),
            edge_kind_mask: edge_kind::CALL,
            forward: false,
            depth,
            max_nodes,
            node_kind_mask,
        })
    }

    pub fn callees(
        &self,
        symbol_ids: &[i64],
        depth: i32,
        max_nodes: i32,
        node_kind_mask: i32,
    ) -> Result<BfsResult> {
        self.sorted_bfs(BfsParams {
            seeds: symbol_ids.to_vec(),
            edge_kind_mask: edge_kind::CALL,
            forward: true,
            depth,
            max_nodes,
            node_kind_mask,
        })
    }

    pub fn bases(
        &self,
        symbol_ids: &[i64],
        depth: i32,
        max_nodes: i32,
        node_kind_mask: i32,
    ) -> Result<BfsResult> {
        // INHERITANCE edges are recorded source = derived, target = base, so
        // walking forward (source_node_id IN frontier) walks derived -> base.
        self.sorted_bfs(BfsParams {
            seeds: symbol_ids.to_vec(),
            edge_kind_mask: edge_kind::INHERITANCE,
            forward: true,
            depth,
            max_nodes,
            node_kind_mask,
        })
    }

    pub fn derived(
        &self,
        symbol_ids: &[i64],
        depth: i32,
        max_nodes: i32,
        node_kind_mask: i32,
    ) -> Result<BfsResult> {
        self.sorted_bfs(BfsParams {
            seeds: symbol_ids.to_vec(),
            edge_kind_mask: edge_kind::INHERITANCE,
            forward: false,
            depth,
            max_nodes,
            node_kind_mask,
        })
    }

    pub fn overrides(&self, symbol_ids: &[i64], node_kind_mask: i32) -> Result<BfsResult> {
        self.sorted_bfs(BfsParams {
            seeds: symbol_ids.to_vec(),
            edge_kind_mask: edge_kind::OVERRIDE,
            forward: true,
            depth: 1,
            max_nodes: 0,
            node_kind_mask,
        })
    }

    pub fn overridden_by(&self, symbol_ids: &[i64], node_kind_mask: i32) -> Result<BfsResult> {
        self.sorted_bfs(BfsParams {
            seeds: symbol_ids.to_vec(),
            edge_kind_mask: edge_kind::OVERRIDE,
            forward: false,
            depth: 1,
            max_nodes: 0,
            node_kind_mask,
        })
    }

    pub fn members(&self, symbol_ids: &[i64], node_kind_mask: i32) -> Result<BfsResult> {
        self.sorted_bfs(BfsParams {
            seeds: symbol_ids.to_vec(),
            edge_kind_mask: edge_kind::MEMBER,
            forward: true,
            depth: 1,
            max_nodes: 0,
            node_kind_mask,
        })
    }

    pub fn neighbors(&self, symbol_ids: &[i64], edge_kind_mask: i32) -> Result<BfsResult> {
        let mut result = BfsResult::default();
        let seed_set: HashSet<i64> = symbol_ids.iter().copied().collect();
        let mut node_ids: HashSet<i64> = seed_set.clone();
        let mut seen_edge_ids: HashSet<i64> = HashSet::new();

        for &seed in symbol_ids {
            self.db.run(
                "SELECT id,type,source_node_id,target_node_id FROM edge WHERE (source_node_id = ? OR target_node_id = ?) AND (type & ?) != 0",
                rusqlite::params![seed, seed, edge_kind_mask],
                |row| {
                    let edge_id = row.get::<_, i64>(0)?;
                    if seen_edge_ids.contains(&edge_id) {
                        return Ok(());
                    }
                    seen_edge_ids.insert(edge_id);
                    let kind = row.get::<_, i32>(1)?;
                    let source = row.get::<_, i64>(2)?;
                    let target = row.get::<_, i64>(3)?;
                    result.edges.push(BfsEdge {
                        id: edge_id,
                        kind,
                        source,
                        target,
                    });
                    node_ids.insert(source);
                    node_ids.insert(target);
                    Ok(())
                },
            )?;
        }

        let ids_to_hydrate: Vec<i64> = node_ids.into_iter().collect();
        self.db.run_chunked_in(
            "SELECT id,type,serialized_name FROM node WHERE id IN ({IN})",
            &ids_to_hydrate,
            &[],
            |row| {
                let id = row.get::<_, i64>(0)?;
                result.nodes.push(BfsNode {
                    id,
                    kind: row.get::<_, i32>(1)?,
                    serialized_name: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    depth: if seed_set.contains(&id) { 0 } else { 1 },
                });
                Ok(())
            },
            CHUNK_SIZE,
        )?;

        sort_nodes(&mut result.nodes);
        sort_edges(&mut result.edges);
        Ok(result)
    }

    pub fn references(&self, symbol_ids: &[i64]) -> Result<ReferencesResult> {
        let mut result = ReferencesResult::default();

        for &symbol_id in symbol_ids {
            // edge_id -> (kind, source)
            let mut edge_by_id: HashMap<i64, (i32, i64)> = HashMap::new();
            self.db.run(
                "SELECT id, type, source_node_id FROM edge WHERE target_node_id = ?",
                [symbol_id],
                |row| {
                    edge_by_id.insert(
                        row.get::<_, i64>(0)?,
                        (row.get::<_, i32>(1)?, row.get::<_, i64>(2)?),
                    );
                    Ok(())
                },
            )?;

            let mut ids_to_query: Vec<i64> = Vec::new();
            ids_to_query.push(symbol_id);
            for &edge_id in edge_by_id.keys() {
                ids_to_query.push(edge_id);
            }

            // `l.type IN (0,1,3,9)` is TOKEN, SCOPE, LOCAL_SYMBOL, UNSOLVED.
            self.db.run_chunked_in(
                "SELECT o.element_id, l.id, l.file_node_id, l.start_line, l.start_column, l.end_line, l.end_column, l.type FROM occurrence o JOIN source_location l ON l.id = o.source_location_id WHERE o.element_id IN ({IN}) AND l.type IN (0,1,3,9)",
                &ids_to_query,
                &[],
                |row| {
                    let element_id = row.get::<_, i64>(0)?;

                    let mut hit = ReferenceHit::default();
                    hit.location.file_id = row.get::<_, i64>(2)?;
                    hit.location.file_path = self.resolve_file_path(hit.location.file_id);
                    hit.location.start_line = row.get::<_, i32>(3)?;
                    hit.location.start_column = row.get::<_, i32>(4)?;
                    hit.location.end_line = row.get::<_, i32>(5)?;
                    hit.location.end_column = row.get::<_, i32>(6)?;
                    hit.location.location_kind = row.get::<_, i32>(7)?;
                    hit.target_node_id = symbol_id;

                    if let Some(&(kind, source)) = edge_by_id.get(&element_id) {
                        hit.has_edge = true;
                        hit.edge_id = element_id;
                        hit.edge_kind = kind;
                        hit.context_node_id = source;
                    } else {
                        hit.has_edge = false;
                    }
                    result.hits.push(hit);
                    Ok(())
                },
                CHUNK_SIZE,
            )?;
        }

        result.hits.sort_by(|a, b| {
            a.location
                .file_path
                .cmp(&b.location.file_path)
                .then(a.location.start_line.cmp(&b.location.start_line))
                .then(a.location.start_column.cmp(&b.location.start_column))
                .then(a.edge_id.cmp(&b.edge_id))
        });
        Ok(result)
    }

    pub fn definition(&self, symbol_ids: &[i64]) -> Result<DefinitionResult> {
        let mut result = DefinitionResult::default();

        for &symbol_id in symbol_ids {
            let mut scope_locs: Vec<FileLocation> = Vec::new();
            let mut token_locs: Vec<FileLocation> = Vec::new();

            self.db.run(
                "SELECT l.id, l.file_node_id, l.start_line, l.start_column, l.end_line, l.end_column, l.type FROM occurrence o JOIN source_location l ON l.id = o.source_location_id WHERE o.element_id = ? AND l.type IN (0,1)",
                [symbol_id],
                |row| {
                    let mut loc = FileLocation::default();
                    loc.file_id = row.get::<_, i64>(1)?;
                    loc.file_path = self.resolve_file_path(loc.file_id);
                    loc.start_line = row.get::<_, i32>(2)?;
                    loc.start_column = row.get::<_, i32>(3)?;
                    loc.end_line = row.get::<_, i32>(4)?;
                    loc.end_column = row.get::<_, i32>(5)?;
                    loc.location_kind = row.get::<_, i32>(6)?;
                    if loc.location_kind == location_kind::SCOPE {
                        scope_locs.push(loc);
                    } else {
                        token_locs.push(loc);
                    }
                    Ok(())
                },
            )?;

            // Prefer SCOPE (full defining body) over TOKEN (bare name span).
            let chosen = if !scope_locs.is_empty() {
                &scope_locs
            } else {
                &token_locs
            };
            for loc in chosen {
                result.locations.push(loc.clone());
            }
        }

        sort_locations(&mut result.locations);
        Ok(result)
    }

    pub fn search(
        &self,
        text: &str,
        kind_filter: Option<&[i32]>,
        limit: i32,
    ) -> Result<Vec<SearchHit>> {
        let mut mask = 0;
        if let Some(kf) = kind_filter {
            for &k in kf {
                mask |= k;
            }
        }

        struct Meta {
            kind: i32,
            qualified_name: String,
            display_name: String,
            indexed: bool,
        }
        let mut meta: HashMap<i64, Meta> = HashMap::new();
        let mut candidates: Vec<(i64, String)> = Vec::new();

        let has_filter = kind_filter.map_or(false, |kf| !kf.is_empty());
        let sql = if has_filter {
            "SELECT n.id, n.type, n.serialized_name, s.definition_kind FROM node n LEFT JOIN symbol s ON s.id = n.id WHERE (n.type & ?) != 0"
        } else {
            "SELECT n.id, n.type, n.serialized_name, s.definition_kind FROM node n LEFT JOIN symbol s ON s.id = n.id"
        };

        let params: Vec<Value> = if has_filter {
            vec![Value::Integer(mask as i64)]
        } else {
            Vec::new()
        };

        self.db.run(sql, rusqlite::params_from_iter(params), |row| {
            let id = row.get::<_, i64>(0)?;
            let kind = row.get::<_, i32>(1)?;
            let sname = row.get::<_, Option<String>>(2)?.unwrap_or_default();
            let indexed = row.get::<_, Option<i64>>(3)?.is_some();

            let decoded = decode_serialized_name(&sname);
            let m = Meta {
                kind,
                qualified_name: decoded.qualified_name(),
                display_name: decoded.display_name(),
                indexed,
            };
            candidates.push((id, m.display_name.clone()));
            meta.insert(id, m);
            Ok(())
        })?;

        let fuzzy_limit = if limit > 0 { limit * 3 } else { 0 };
        let hits = fuzzy_match(text, &candidates, fuzzy_limit);

        let mut out: Vec<SearchHit> = Vec::with_capacity(hits.len());
        for h in &hits {
            if let Some(m) = meta.get(&h.node_id) {
                out.push(SearchHit {
                    node_id: h.node_id,
                    kind: m.kind,
                    display_name: m.display_name.clone(),
                    qualified_name: m.qualified_name.clone(),
                    score: h.score,
                    indexed: m.indexed,
                    matched_indices: h.matched_indices.clone(),
                });
            }
        }

        out.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then(a.display_name.len().cmp(&b.display_name.len()))
                .then(a.display_name.cmp(&b.display_name))
                .then(a.node_id.cmp(&b.node_id))
        });

        if limit > 0 && out.len() > limit as usize {
            out.truncate(limit as usize);
        }
        Ok(out)
    }

    pub fn file_info(
        &self,
        file_id: i64,
        want_contents: bool,
        want_occurrences: bool,
        line_range: Option<(i32, i32)>,
        location_kind_filter: Option<&[i32]>,
    ) -> Result<FileResult> {
        let mut result = FileResult::default();
        result.file_id = file_id;

        self.db.run(
            "SELECT path, language, indexed, complete, line_count FROM file WHERE id = ?",
            [file_id],
            |row| {
                result.path = row.get::<_, Option<String>>(0)?.unwrap_or_default();
                result.language = row.get::<_, Option<String>>(1)?.unwrap_or_default();
                result.indexed = row.get::<_, i32>(2)? != 0;
                result.complete = row.get::<_, i32>(3)? != 0;
                result.line_count = row.get::<_, i32>(4)?;
                Ok(())
            },
        )?;

        if want_contents {
            self.db.run(
                "SELECT content FROM filecontent WHERE id = ?",
                [file_id],
                |row| {
                    result.contents = row.get::<_, Option<String>>(0)?.unwrap_or_default();
                    result.has_contents = true;
                    Ok(())
                },
            )?;
        }

        if want_occurrences {
            let mut sql = String::from(
                "SELECT id, start_line, start_column, end_line, end_column, type FROM source_location WHERE file_node_id = ?",
            );
            if line_range.is_some() {
                sql.push_str(" AND start_line BETWEEN ? AND ?");
            }
            if let Some(kf) = location_kind_filter {
                if !kf.is_empty() {
                    sql.push_str(" AND type IN (");
                    for (i, k) in kf.iter().enumerate() {
                        if i != 0 {
                            sql.push(',');
                        }
                        sql.push_str(&k.to_string());
                    }
                    sql.push(')');
                }
            }

            struct LocRow {
                id: i64,
                loc: FileLocation,
            }
            let mut loc_rows: Vec<LocRow> = Vec::new();
            let params: Vec<Value> = match line_range {
                Some((first, second)) => vec![
                    Value::Integer(file_id),
                    Value::Integer(first as i64),
                    Value::Integer(second as i64),
                ],
                None => vec![Value::Integer(file_id)],
            };
            self.db.run(&sql, rusqlite::params_from_iter(params), |row| {
                let mut r = LocRow {
                    id: row.get::<_, i64>(0)?,
                    loc: FileLocation::default(),
                };
                r.loc.file_id = file_id;
                r.loc.file_path = result.path.clone();
                r.loc.start_line = row.get::<_, i32>(1)?;
                r.loc.start_column = row.get::<_, i32>(2)?;
                r.loc.end_line = row.get::<_, i32>(3)?;
                r.loc.end_column = row.get::<_, i32>(4)?;
                r.loc.location_kind = row.get::<_, i32>(5)?;
                loc_rows.push(r);
                Ok(())
            })?;

            if !loc_rows.is_empty() {
                let loc_ids: Vec<i64> = loc_rows.iter().map(|r| r.id).collect();

                let mut elements_by_loc: HashMap<i64, Vec<i64>> = HashMap::new();
                self.db.run_chunked_in(
                    "SELECT element_id, source_location_id FROM occurrence WHERE source_location_id IN ({IN})",
                    &loc_ids,
                    &[],
                    |row| {
                        let element_id = row.get::<_, i64>(0)?;
                        let loc_id = row.get::<_, i64>(1)?;
                        elements_by_loc.entry(loc_id).or_default().push(element_id);
                        Ok(())
                    },
                    CHUNK_SIZE,
                )?;

                let mut all_element_ids: HashSet<i64> = HashSet::new();
                for elems in elements_by_loc.values() {
                    for &e in elems {
                        all_element_ids.insert(e);
                    }
                }

                struct ElemInfo {
                    kind: i32,
                    display_name: String,
                }
                let mut elem_info: HashMap<i64, ElemInfo> = HashMap::new();
                let elem_ids_vec: Vec<i64> = all_element_ids.into_iter().collect();
                self.db.run_chunked_in(
                    "SELECT id,type,serialized_name FROM node WHERE id IN ({IN})",
                    &elem_ids_vec,
                    &[],
                    |row| {
                        let id = row.get::<_, i64>(0)?;
                        let kind = row.get::<_, i32>(1)?;
                        let sname = row.get::<_, Option<String>>(2)?.unwrap_or_default();
                        elem_info.insert(
                            id,
                            ElemInfo {
                                kind,
                                display_name: decode_serialized_name(&sname).display_name(),
                            },
                        );
                        Ok(())
                    },
                    CHUNK_SIZE,
                )?;

                for r in &loc_rows {
                    let Some(elems) = elements_by_loc.get(&r.id) else {
                        continue;
                    };
                    for &element_id in elems {
                        let mut occ = FileOccurrence::default();
                        occ.element_id = element_id;
                        occ.location = r.loc.clone();
                        if let Some(ei) = elem_info.get(&element_id) {
                            occ.element_kind = ei.kind;
                            occ.element_name = ei.display_name.clone();
                        } else {
                            // element_id refers to an edge or local_symbol row
                            // (not itself a `node`) -- leave kind/name unset.
                            occ.element_kind = 0;
                        }
                        result.occurrences.push(occ);
                    }
                }

                result.occurrences.sort_by(|a, b| {
                    a.location
                        .start_line
                        .cmp(&b.location.start_line)
                        .then(a.location.start_column.cmp(&b.location.start_column))
                        .then(a.element_id.cmp(&b.element_id))
                });
            }
        }

        Ok(result)
    }

    pub fn stats(&self, error_limit: i32) -> Result<StatsResult> {
        let mut result = StatsResult::default();
        result.storage_version = self.db.storage_version();

        let count_of = |table: &str| -> Result<i64> {
            let mut count = 0i64;
            let sql = format!("SELECT COUNT(*) FROM {}", table);
            self.db.run(&sql, [], |row| {
                count = row.get::<_, i64>(0)?;
                Ok(())
            })?;
            Ok(count)
        };

        result.node_count = count_of("node")?;
        result.edge_count = count_of("edge")?;
        result.file_count = count_of("file")?;
        result.symbol_count = count_of("symbol")?;
        result.occurrence_count = count_of("occurrence")?;
        result.source_location_count = count_of("source_location")?;
        result.local_symbol_count = count_of("local_symbol")?;
        result.error_count = count_of("error")?;

        self.db.run(
            "SELECT type, COUNT(*) FROM node GROUP BY type ORDER BY type",
            [],
            |row| {
                result.nodes_by_kind.push(KindCount {
                    kind: row.get::<_, i32>(0)?,
                    count: row.get::<_, i64>(1)?,
                });
                Ok(())
            },
        )?;
        self.db.run(
            "SELECT type, COUNT(*) FROM edge GROUP BY type ORDER BY type",
            [],
            |row| {
                result.edges_by_kind.push(KindCount {
                    kind: row.get::<_, i32>(0)?,
                    count: row.get::<_, i64>(1)?,
                });
                Ok(())
            },
        )?;

        let limit = if error_limit > 0 { error_limit } else { 100 };
        self.db.run(
            "SELECT id, message, fatal, translation_unit FROM error ORDER BY id LIMIT ?",
            [limit],
            |row| {
                let mut r = StatsErrorRow::default();
                r.id = row.get::<_, i64>(0)?;
                r.message = row.get::<_, Option<String>>(1)?.unwrap_or_default();
                r.fatal = row.get::<_, i32>(2)? != 0;
                r.translation_unit = row.get::<_, Option<String>>(3)?.unwrap_or_default();
                result.errors.push(r);
                Ok(())
            },
        )?;

        Ok(result)
    }

    pub fn primary_locations(&self, node_ids: &[i64]) -> Result<HashMap<i64, String>> {
        struct Best {
            file_id: i64,
            line: i32,
        }
        let mut best: HashMap<i64, Best> = HashMap::new();

        self.db.run_chunked_in(
            "SELECT o.element_id, l.file_node_id, l.start_line FROM occurrence o JOIN source_location l ON l.id = o.source_location_id WHERE o.element_id IN ({IN}) AND l.type = 0",
            node_ids,
            &[],
            |row| {
                let id = row.get::<_, i64>(0)?;
                let file_id = row.get::<_, i64>(1)?;
                let line = row.get::<_, i32>(2)?;
                let replace = match best.get(&id) {
                    None => true,
                    Some(b) => file_id < b.file_id || (file_id == b.file_id && line < b.line),
                };
                if replace {
                    best.insert(id, Best { file_id, line });
                }
                Ok(())
            },
            CHUNK_SIZE,
        )?;

        let mut out = HashMap::new();
        for (id, b) in &best {
            out.insert(*id, format!("{}:{}", self.resolve_file_path(b.file_id), b.line));
        }
        Ok(out)
    }

    pub fn file_map(&self) -> &HashMap<i64, String> {
        &self.file_by_id
    }
}