#!/usr/bin/env python3
"""Re-parse every `.h` file in a symwhere index with the C++ grammar.

Why: symwhere's Lang::detect maps `.h` -> C (src/types.rs). In C++ repos
(folly, velox, ...) the C grammar silently drops ~60% of header symbols
(classes, methods, templates, namespaces). `index --lang` only filters, it
cannot remap, so this script repairs an existing index in place using
symwhere's own C++ parser (`symwhere symbols FILE -l cpp --json`).

Usage:  symwhere-h-as-cpp.py DB_PATH REPO_ROOT [-j N]
Re-run after any `symwhere update` that touched .h files (update re-parses
them as C again).
"""
import argparse, json, os, sqlite3, subprocess, sys, time
from concurrent.futures import ThreadPoolExecutor

SYMWHERE = os.environ.get("SYMWHERE", "symwhere")


def parse(root, rel):
    p = subprocess.run([SYMWHERE, "symbols", os.path.join(root, rel), "-l", "cpp", "--json"],
                       capture_output=True, text=True)
    if p.returncode != 0:
        return rel, None, p.stderr.strip()
    return rel, [json.loads(l) for l in p.stdout.splitlines() if l.strip()], None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("db"); ap.add_argument("root")
    ap.add_argument("-j", type=int, default=os.cpu_count())
    a = ap.parse_args()
    con = sqlite3.connect(a.db)
    rows = con.execute("SELECT id, path FROM files WHERE lang='c' AND path LIKE '%.h'").fetchall()
    ids = {path: fid for fid, path in rows}
    t0, before, after, failed = time.time(), 0, 0, []
    with ThreadPoolExecutor(a.j) as ex:
        results = list(ex.map(lambda r: parse(a.root, r[1]), rows))
    with con:
        for rel, syms, err in results:
            if syms is None:
                failed.append((rel, err)); continue
            fid = ids[rel]
            before += con.execute("SELECT count(*) FROM symbols WHERE file_id=?", (fid,)).fetchone()[0]
            con.execute("DELETE FROM symbols WHERE file_id=?", (fid,))
            con.executemany(
                "INSERT INTO symbols(file_id,name,kind,container,start_line,start_col,end_line,end_col,signature)"
                " VALUES (?,?,?,?,?,?,?,?,?)",
                [(fid, s["name"], s["kind"], s.get("container"), s["start_line"], s["start_col"],
                  s["end_line"], s["end_col"], s.get("signature")) for s in syms])
            con.execute("UPDATE files SET lang='cpp' WHERE id=?", (fid,))
            after += len(syms)
        con.execute("INSERT OR REPLACE INTO meta(key,value) VALUES('h_as_cpp_patched_at', ?)",
                    (str(int(time.time())),))
    print(f"re-parsed {len(rows) - len(failed)} .h files as C++: symbols {before} -> {after} "
          f"({time.time() - t0:.1f}s)")
    for rel, err in failed:
        print(f"  FAILED {rel}: {err}", file=sys.stderr)


if __name__ == "__main__":
    main()
