#!/usr/bin/env python3
"""Real-CLI relocation regression for symhow's portable paths and --rebind.

Run by `cargo test --test scenarios portable_paths_cli`, or directly:
  python3 tests/scenarios/portable_paths.py target/debug/symhow [OTHER_BINARY...]

Each supplied binary writes a DB; every binary must be able to query it.
No external checkout, CUDA installation, or network access is needed.
"""

import json
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import tempfile


BINARIES = [Path(arg).resolve() for arg in sys.argv[1:]]


def run(binary, *args, codes=(0,)):
    result = subprocess.run(
        [str(binary), *map(str, args)], capture_output=True, text=True
    )
    assert result.returncode in codes, (
        f"{binary.name} {' '.join(map(str, args))}: exit {result.returncode}\n"
        f"stdout: {result.stdout}\nstderr: {result.stderr}"
    )
    return result


def rows(db, sql):
    with sqlite3.connect(db) as connection:
        return connection.execute(sql).fetchall()


def query(binary, db, verb, symbol, *args):
    return json.loads(run(binary, "query", verb, symbol, "-d", db, "--json", *args).stdout)


def check_backend(binary):
    with tempfile.TemporaryDirectory(prefix="symhow-portable-") as scratch:
        base = Path(scratch)
        old = base / "old checkout"
        old.mkdir()
        (old / "include").mkdir()
        (old / "src").mkdir()
        external = base / "external.hpp"
        external.write_text("inline int external_value() { return 2; }\n")
        header = old / "include" / "value.hpp"
        header.write_text("inline int value() { return 40; }\n")
        source = old / "src" / "answer.cpp"
        source.write_text(
            '#include "value.hpp"\n#include "external.hpp"\n'
            "namespace { int local_value() { int x = value(); return x; } }\n"
            "int answer() { return local_value() + external_value(); }\n"
        )
        cdb = old / "compile_commands.json"
        cdb.write_text(json.dumps([{
            "directory": str(old),
            "file": str(source),
            "arguments": ["clang++", "-std=c++17", "-I" + str(old / "include"),
                          "-I" + str(base), str(source)],
        }]))
        db = base / "portable.srctrldb"
        run(binary, "index", "-c", cdb, "-o", db, "--repo", old)
        meta = dict(rows(db, "SELECT key,value FROM meta"))
        assert meta["symhow_path_format"] == "relative-v1", meta
        assert meta["symhow_source_root"] == str(old), meta
        stored_files = {row[0] for row in rows(db, "SELECT path FROM file")}
        assert {"src/answer.cpp", "include/value.hpp", str(external)} <= stored_files, stored_files
        assert rows(db, "SELECT path FROM symhow_tu") == [("src/answer.cpp",)]
        assert not rows(db, "SELECT path FROM symhow_tu WHERE file_node_id IS NULL"), "missing TU file id"
        # File-node identities and local-symbol identities must be portable too.
        names_before = rows(db, "SELECT serialized_name FROM node ORDER BY serialized_name")
        assert all(str(old) not in name for (name,) in names_before), names_before
        assert all(str(old) not in name for (name,) in rows(db, "SELECT name FROM local_symbol"))

        run(binary, "notes", "scaffold", "-d", db, "--notes-dir", old / "notes", "--all")
        run(binary, "notes", "sync", "-d", db)
        assert rows(db, "SELECT count(*) FROM symhow_note")[0][0] > 0
        assert not rows(db, "SELECT serialized_name FROM symhow_note WHERE current_body_hash = ''"), "missing source body hashes"

        # The old checkout must actually disappear: this must not succeed by
        # accidentally following paths on the indexing machine.
        new = base / "new checkout"
        shutil.move(old, new)
        assert not old.exists()
        run(binary, "--rebind", new, "-d", db)
        assert rows(db, "SELECT serialized_name FROM node ORDER BY serialized_name") == names_before
        assert {row[0] for row in rows(db, "SELECT path FROM file")} == stored_files
        for reader in BINARIES:
            definition = query(reader, db, "definition", "answer")
            assert definition["locations"], definition
            assert all(loc["file"] == str(new / "src/answer.cpp") for loc in definition["locations"]), definition
            for spec in ("src/answer.cpp", str(new / "src/answer.cpp")):
                file_result = query(reader, db, "file", spec, "--contents")
                assert file_result["file"]["path"] == str(new / "src/answer.cpp"), file_result
                assert file_result["file"]["line_count"] > 0, file_result
                assert "int answer()" in file_result.get("contents", ""), file_result
            run(reader, "notes", "sync", "-d", db)
        assert not rows(db, "SELECT kind FROM symhow_note_problem WHERE kind LIKE '%orphan%'")

        # Invalid roots must not partially change either paths or provenance.
        metadata_before = rows(db, "SELECT key,value FROM meta ORDER BY key")
        run(binary, "--rebind", base / "missing", "-d", db, codes=(1, 2, 4))
        assert rows(db, "SELECT key,value FROM meta ORDER BY key") == metadata_before

        # Usage errors/help must not rebind a valid DB as a side effect.
        for command in (("query", "stats", "--bogus"),
                        ("query", "definition"),
                        ("query", "stats", "--limit", "bad"),
                        ("update", "--parallel", "bad"),
                        ("index",), ("notes", "lint")):
            run(binary, *command, "-d", db, "--rebind", base, codes=(1, 2, 4))
            assert rows(db, "SELECT key,value FROM meta ORDER BY key") == metadata_before
        run(binary, "query", "--help", "-d", db, "--rebind", base)
        assert rows(db, "SELECT key,value FROM meta ORDER BY key") == metadata_before

        # Rebind can accompany a query and persists for subsequent commands.
        # A new root prefixed by the old root must NOT be remapped twice.
        final = base / "new checkout-copy"
        shutil.move(new, final)
        result = query(binary, db, "definition", "answer", "--rebind", final)
        assert result["locations"][0]["file"] == str(final / "src/answer.cpp"), result
        assert dict(rows(db, "SELECT key,value FROM meta"))["symhow_source_root"] == str(final)

        # Header invalidation, CDB path/remap relocation, writer dedup, and
        # local-symbol identities must all survive an incremental reparse.
        (final / "include/value.hpp").write_text("inline int value() { return 41; }\n")
        run(binary, "update", "-d", db, "--files", final / "include/value.hpp", "--json")
        assert rows(db, "SELECT path FROM symhow_tu") == [("src/answer.cpp",)]
        assert not rows(db, "SELECT path FROM file GROUP BY path HAVING count(*) > 1")
        assert all(not path.startswith(str(old)) and not path.startswith(str(new))
                   for (path,) in rows(db, "SELECT path FROM file"))
        assert query(binary, db, "definition", "answer")["locations"][0]["file"] == str(final / "src/answer.cpp")
        run(binary, "update", "-d", db, "--files", final / "include/value.hpp", "--json")
        # Fresh-index equivalence catches changed path-dependent identities,
        # stale FILE nodes, and missed reverse-INCLUDE invalidation.
        fresh = base / "fresh.srctrldb"
        run(binary, "index", "-c", final / "compile_commands.json", "-o", fresh,
            "--repo", final, "--remap", f"{old}={final}")
        for category in ("node", "edge", "location", "symbol", "file", "local_symbol", "symbol_def", "tu"):
            run(binary, "dbdiff", db, fresh, "--only", category)

        # Regenerating the compilation database with current physical paths
        # must not apply an old-root substring remap to a prefix sibling.
        regenerated = json.loads((final / "compile_commands.json").read_text())
        for entry in regenerated:
            entry["directory"] = entry["directory"].replace(str(old), str(final))
            entry["file"] = entry["file"].replace(str(old), str(final))
            entry["arguments"] = [arg.replace(str(old), str(final)) for arg in entry["arguments"]]
        (final / "compile_commands.json").write_text(json.dumps(regenerated))
        run(binary, "update", "-d", db, "--files", final / "include/value.hpp", "--json")
        assert query(binary, db, "definition", "answer")["locations"][0]["file"] == str(final / "src/answer.cpp")

        # Returning to a previous root must not replay/cascade historical maps.
        shutil.move(final, old)
        run(binary, "--rebind", old, "-d", db)
        run(binary, "update", "-d", db, "--files", old / "src/answer.cpp", "--json")
        assert query(binary, db, "definition", "answer")["locations"][0]["file"] == str(old / "src/answer.cpp")
        for updater in BINARIES:
            run(updater, "update", "-d", db, "--files", old / "src/answer.cpp", "--json")
            assert rows(db, "SELECT path FROM symhow_tu") == [("src/answer.cpp",)]
            assert query(updater, db, "definition", "answer")["locations"][0]["file"] == str(old / "src/answer.cpp")

        # Legacy databases stay queryable; unsafe automatic conversion is
        # explicitly refused rather than silently mixing identity formats.
        legacy = base / "legacy.srctrldb"
        shutil.copyfile(db, legacy)
        with sqlite3.connect(legacy) as connection:
            connection.execute("DELETE FROM meta WHERE key IN ('symhow_path_format','symhow_source_root')")
        run(binary, "query", "stats", "-d", legacy, "--json")
        legacy_before = legacy.read_bytes()
        run(binary, "--rebind", old, "-d", legacy, codes=(1, 2, 4))
        assert legacy.read_bytes() == legacy_before
        print(f"PASS portable source paths: {binary}")


if __name__ == "__main__":
    if not BINARIES:
        sys.exit(__doc__)
    for executable in BINARIES:
        check_backend(executable)
