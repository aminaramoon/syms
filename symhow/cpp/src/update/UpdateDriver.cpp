#include "symhow/update/UpdateDriver.h"

#include <algorithm>
#include <chrono>
#include <cstdio>
#include <ctime>
#include <fcntl.h>
#include <filesystem>
#include <unistd.h>
#include <sqlite3.h>
#include <unordered_map>
#include <unordered_set>

#include <nlohmann/json.hpp>

#include "symhow/common/ContentHash.h"
#include "symhow/db/Database.h"
#include "symhow/db/Meta.h"
#include "symhow/db/WritableDb.h"
#include "symhow/index/IndexDriver.h"
#include "symhow/index/Provenance.h"
#include "symhow/model/IndexBackend.h"
#include "symhow/notes/Sync.h"
#include "symhow/update/FileMatcher.h"
#include "symhow/update/Git.h"
#include "symhow/update/GitDiff.h"
#include "symhow/update/Invalidator.h"

namespace symhow::update
{

namespace
{

uint64_t millisSince(std::chrono::steady_clock::time_point start)
{
    return static_cast<uint64_t>(
        std::chrono::duration_cast<std::chrono::milliseconds>(std::chrono::steady_clock::now() - start).count());
}

std::string utcTimestampNow()
{
    const std::time_t t = std::time(nullptr);
    std::tm tm{};
    gmtime_r(&t, &tm);
    char buf[32];
    std::strftime(buf, sizeof(buf), "%Y-%m-%dT%H:%M:%SZ", &tm);
    return buf;
}

// Best-effort: returns the current dirty-paths list (mirrors
// symhow::cli::buildProvenance's `symhow index` behavior), or the previous value if
// git status can't be run.
std::vector<std::string> currentDirtyPaths(const std::string& repoRoot, const std::vector<std::string>& fallback)
{
    if (repoRoot.empty())
    {
        return fallback;
    }
    const std::optional<std::string> status = statusPorcelainZ(repoRoot);
    if (!status.has_value())
    {
        return fallback;
    }
    return parseStatusPorcelainZ(*status, repoRoot);
}

// Builds the `symhow.update-summary/1` envelope for the "error" case ("--json"
// still needs to emit SOMETHING structured on refusals/hard errors, not
// just silence -- see docs/JSON_SCHEMA.md). Never used for a successful
// run (see buildSummaryJson() below for that).
nlohmann::json buildErrorJson(const UpdateRunOptions& options, const std::string& message, int exitCode)
{
    nlohmann::json j;
    j["symhow_version"] = options.symhowVersion;
    j["schema"] = "symhow.update-summary/1";
    j["database"] = options.dbPath;
    j["error"] = message;
    j["exit_code"] = exitCode;
    return j;
}

void reportRefused(const UpdateRunOptions& options, const std::string& message)
{
    std::fprintf(stderr, "symhow update: refused: %s\n", message.c_str());
    if (options.json)
    {
        std::printf("%s\n", buildErrorJson(options, message, 2).dump().c_str());
    }
}

void reportError(const UpdateRunOptions& options, const std::string& message)
{
    std::fprintf(stderr, "symhow update: error: %s\n", message.c_str());
    if (options.json)
    {
        std::printf("%s\n", buildErrorJson(options, message, 1).dump().c_str());
    }
}

// Builds the full `symhow.update-summary/1` envelope for a successful (rc 0 or
// 5) run, dry-run included, straight from `summary`'s already-populated
// fields -- shared by the "nothing to do" (exit 5), --dry-run (exit 0), and
// real-update (exit 0) JSON output paths so all three stay in sync.
nlohmann::json buildSummaryJson(const UpdateRunOptions& options, const UpdateSummary& s)
{
    nlohmann::json j;
    j["symhow_version"] = options.symhowVersion;
    j["schema"] = "symhow.update-summary/1";
    j["database"] = s.database;
    j["dry_run"] = s.dryRun;
    j["in_place"] = s.inPlace;
    j["repo"] = {{"root", s.repoRoot}, {"from", s.fromRev}, {"to", s.toRev}};
    j["files"] = {
        {"changed", s.changedCount},
        {"deleted", s.deletedCount},
        {"renamed", s.renamedCount},
        {"flag_changed", s.flagChangedCount},
        {"unmatched", s.unmatchedCount},
        {"cleared", s.plan.clearedFileIds.size()},
        {"include_closure", s.plan.includeClosureAdded},
        {"headers_pruned", s.plan.headersPruned},
        {"cleared_paths", s.plan.clearedPaths},
        {"unmatched_paths", s.unmatchedPaths}};
    j["tus"] = {
        {"to_index", s.tusToIndex}, {"new", s.tusNew}, {"retried", s.tusRetried}, {"skipped_missing", s.skippedMissing}};
    j["gc"] = {
        {"files", s.gc.files},
        {"nodes", s.gc.nodes},
        {"edges", s.gc.edges},
        {"locations", s.gc.locations},
        {"local_symbols", s.gc.localSymbols},
        {"errors", s.gc.errors},
        {"symbols_reset", s.gc.symbolsReset},
        {"non_indexed_files", s.gc.nonIndexedFiles},
        {"orphan_elements", s.gc.orphanElements}};
    if (s.indexRan)
    {
        j["index"] = {
            {"tus_total", s.idxTusTotal},
            {"tus_attempted", s.idxTusAttempted},
            {"tus_with_content", s.idxTusWithContent},
            {"tus_failed", s.idxTusFailed},
            {"symbols", s.idxSymbols},
            {"references", s.idxReferences},
            {"files", s.idxFiles},
            {"errors", s.idxErrors},
            {"name_collisions", s.idxNameCollisions}};
    }
    else
    {
        j["index"] = nlohmann::json::object();
    }
    if (s.notesRan)
    {
        j["notes"] = {
            {"noted", s.notesNoted}, {"stale", s.notesStale}, {"newly_stale", s.notesNewlyStale}, {"problems", s.notesProblems}};
    }
    else
    {
        j["notes"] = nlohmann::json::object();
    }
    j["elapsed_ms"] = s.elapsedMs;
    j["index_elapsed_ms"] = s.indexElapsedMs;
    return j;
}

// RAII guard for `<db>.symhow-lock`, held for the whole runUpdate() call
// (including the exit-5 "nothing to do" provenance refresh) so two
// concurrent `symhow update` invocations against the same database can't race
// each other. Uses open(O_CREAT|O_EXCL) -- atomic create-if-absent, same
// trick `flock`-less tools have used forever -- rather than a real flock()
// so a stale lock from a crashed process is trivially diagnosable (it's
// just a file) and recoverable (the message below says so).
class UpdateLock
{
public:
    ~UpdateLock() { release(); }

    // Returns true (and holds the lock until this object is destroyed, or
    // release() is called) if `dbPath + ".symhow-lock"` didn't already exist;
    // false (with `*err` set) if it did.
    bool acquire(const std::string& dbPath, std::string* err)
    {
        path_ = dbPath + ".symhow-lock";
        const int fd = ::open(path_.c_str(), O_CREAT | O_EXCL | O_WRONLY, 0644);
        if (fd < 0)
        {
            if (err != nullptr)
            {
                *err = "another symhow update is running (or crashed; remove " + path_ + " if stale)";
            }
            path_.clear();
            return false;
        }
        ::close(fd);
        acquired_ = true;
        return true;
    }

    void release()
    {
        if (acquired_)
        {
            std::error_code ec;
            std::filesystem::remove(path_, ec);
            acquired_ = false;
        }
    }

private:
    std::string path_;
    bool acquired_ = false;
};

// Absolute path of `path`'s ".srctrlprj" sidecar, per the vendored writer's
// own `rfind('.')`-on-the-whole-path derivation (see SrctrldbBackend /
// IndexRunOptions comments) -- used both to build the temp copy's own
// sidecar name and to delete it afterward.
std::string projSidecarFor(const std::string& path)
{
    const std::size_t dot = path.rfind('.');
    const std::string stem = (dot == std::string::npos) ? path : path.substr(0, dot);
    return stem + ".srctrlprj";
}

struct EffectiveOptions
{
    symhow::index::CompileDbOptions dbOptions;
    symhow::index::SanitizerOptions sanitizerOptions;
    symhow::index::SystemFileRows systemFileRows = symhow::index::SystemFileRows::Direct;
};

}  // namespace

int runUpdate(const UpdateRunOptions& options, UpdateSummary* out)
{
    const auto runStart = std::chrono::steady_clock::now();
    // One cache shared by planning, matching and selection; never across runs.
    symhow::common::paths::NormalizationCache normalizePath;
    UpdateSummary summary;
    summary.database = options.dbPath;
    summary.dryRun = options.dryRun;
    summary.inPlace = options.inPlace;

    // --- 0. concurrency guard + hot-journal check -----------------------------
    // Held for the ENTIRE function (RAII -- released on every return path,
    // including every early refusal/error below and the exit-5 "nothing to
    // do" path), so two concurrent `symhow update` runs against the same
    // database can't race each other (e.g. both deciding it's safe to set
    // symhow_update_in_progress, or both copying the same working-copy name).
    UpdateLock lock;
    {
        std::string lockErr;
        if (!lock.acquire(options.dbPath, &lockErr))
        {
            reportRefused(options, lockErr);
            return 2;
        }
    }

    // A `-journal`/`-wal` file next to the original means a previous
    // process (sqlite itself, or a previous `symhow update`/`symhow index`) may
    // have crashed mid-write -- copying (or, for --in-place, writing
    // directly into) a database with a hot journal risks propagating an
    // inconsistent snapshot. Refuse rather than guess.
    if (std::filesystem::exists(options.dbPath + "-journal") || std::filesystem::exists(options.dbPath + "-wal"))
    {
        reportRefused(
            options,
            "found a hot journal/WAL file next to '" + options.dbPath +
                "' -- a previous process may have crashed mid-write; verify the database's integrity (e.g. `sqlite3 " +
                options.dbPath + " 'PRAGMA integrity_check;'`) before retrying");
        return 2;
    }

    // --- 1. open read-only, read provenance, run refusal checks ------------
    std::optional<symhow::index::Provenance> prov;
    bool resumingInterruptedIndex = false;
    try
    {
        symhow::db::Database db(options.dbPath, options.verbose);
        prov = symhow::index::readProvenance(db.handle());
        if (prov.has_value())
        {
            const std::optional<std::string> inProgress = symhow::db::getMeta(db.handle(), "symhow_update_in_progress");
            if (inProgress.has_value() && *inProgress == "1")
            {
                reportRefused(
                    options,
                    "an update is already in progress on this database (symhow_update_in_progress is set) -- if a "
                    "previous `symhow update --in-place` was interrupted, re-index from scratch to recover");
                return 2;
            }

            // Phase 8: a database left with symhow_index_in_progress="1" is an
            // INTERRUPTED `symhow index` run, not a corrupt/mid-write one (its
            // provenance and every completed checkpoint's facts are
            // durable) -- resume it instead of refusing. See the retry-set
            // computation below (step "Phase 8 resume") for how the
            // never-indexed suffix gets picked up.
            const std::optional<std::string> indexInProgress = symhow::db::getMeta(db.handle(), "symhow_index_in_progress");
            if (indexInProgress.has_value() && *indexInProgress == "1")
            {
                resumingInterruptedIndex = true;
                if (!options.json)
                {
                    std::fprintf(stderr, "symhow update: resuming interrupted index (symhow_index_in_progress was set)\n");
                }
            }
        }
    }
    catch (const std::exception& e)
    {
        reportError(options, e.what());
        return 1;
    }

    if (!prov.has_value())
    {
        reportRefused(options, "no provenance found (not written by this version of symhow, or by the real Sourcetrail indexer)");
        return 2;
    }
    if (prov->walkerVersion != symhow::index::kWalkerVersion)
    {
        reportRefused(
            options,
            "database was written by walker version " + std::to_string(prov->walkerVersion) + ", this symhow is walker version " +
            std::to_string(symhow::index::kWalkerVersion) + " -- re-index from scratch");
        return 2;
    }
    if (prov->systemFileRows == symhow::index::SystemFileRows::All)
    {
        reportRefused(options, "database was indexed with --system-file-rows=all, which `symhow update` doesn't support incrementally");
        return 2;
    }

    const std::string repoRoot = !options.repoRoot.empty() ? options.repoRoot : prov->repoRoot;
    if (repoRoot.empty() && options.explicitFiles.empty())
    {
        reportRefused(options, "no repo root available (neither --repo nor a stored symhow_repo_root) and no --files given");
        return 2;
    }

    const std::string cdbPath = !options.cdbPath.empty() ? options.cdbPath : prov->cdbPath;
    if (cdbPath.empty())
    {
        reportRefused(options, "no compile_commands.json path available (neither -c nor a stored symhow_cdb_path)");
        return 2;
    }

    // --- effective options: stored, overridden by CLI where given ----------
    EffectiveOptions eff;
    eff.dbOptions = prov->dbOptions;
    eff.sanitizerOptions = prov->sanitizerOptions;
    eff.systemFileRows = prov->systemFileRows;

    bool diverges = false;
    if (options.dbOverridesGiven)
    {
        eff.dbOptions = options.dbOverrides;
        diverges = true;
    }
    if (options.cudaModeGiven)
    {
        eff.sanitizerOptions.cudaMode = options.cudaModeOverride;
        diverges = true;
    }
    if (options.systemFileRowsGiven)
    {
        eff.systemFileRows = options.systemFileRowsOverride;
        diverges = true;
    }
    if (options.extraArgGiven)
    {
        eff.sanitizerOptions.extraArgs = options.extraArgOverride;
    }
    if (options.dropArgGiven)
    {
        eff.sanitizerOptions.dropArgPatterns = options.dropArgOverride;
    }
    if (options.stdGiven)
    {
        eff.sanitizerOptions.stdFallback = options.stdOverride;
    }
    if (options.cStdGiven)
    {
        eff.sanitizerOptions.cStdFallback = options.cStdOverride;
    }
    if (options.excludeSubmodules && !repoRoot.empty())
    {
        for (const std::string& glob : submoduleExcludeGlobs(repoRoot))
        {
            if (std::find(eff.dbOptions.excludeGlobs.begin(), eff.dbOptions.excludeGlobs.end(), glob) ==
                eff.dbOptions.excludeGlobs.end())
            {
                eff.dbOptions.excludeGlobs.push_back(glob);
                diverges = true;
            }
        }
    }
    if (diverges)
    {
        std::fprintf(
            stderr,
            "symhow update: warning: CLI overrides change include/exclude globs, cuda mode, or system-file-rows -- the "
            "resulting database will diverge from a fresh `symhow index` with these options\n");
    }

    // --- 2. compute changes --------------------------------------------------
    std::vector<ChangedFile> changes;
    std::vector<std::string> dirtyPaths = prov->dirtyPaths;
    std::string newBaseline = prov->repoCommit;

    if (!options.explicitFiles.empty())
    {
        for (const auto& f : options.explicitFiles)
        {
            ChangedFile cf;
            cf.kind = ChangeKind::Modified;
            cf.newPath = std::filesystem::absolute(f).lexically_normal().string();
            changes.push_back(std::move(cf));
        }
    }
    else
    {
        const std::string baseline = !options.fromRev.empty() ? options.fromRev : prov->repoCommit;
        if (baseline.empty())
        {
            reportRefused(options, "no baseline commit available (neither --from nor a stored symhow_repo_commit)");
            return 2;
        }
        const bool toGiven = !options.toRev.empty();
        const std::optional<std::string> toForDiff = toGiven ? std::optional<std::string>(options.toRev) : std::nullopt;

        std::string err;
        const std::optional<ChangeSet> cs = collectChanges(repoRoot, baseline, toForDiff, &err);
        if (!cs.has_value())
        {
            reportError(options, err);
            return 1;
        }
        changes = cs->changes;

        const std::optional<std::string> resolved = revParse(repoRoot, toGiven ? options.toRev : "HEAD");
        if (!resolved.has_value())
        {
            reportError(options, "couldn't resolve '" + (toGiven ? options.toRev : std::string("HEAD")) + "' to a commit");
            return 1;
        }
        newBaseline = *resolved;

        if (!toGiven)
        {
            dirtyPaths = cs->dirtyPaths;
            // Also fold in every stored-at-last-index dirty path that git
            // diff itself wouldn't re-report (e.g. a path that was already
            // dirty at the previous index/update and remains untouched
            // since): union by normalized path, added as Modified.
            std::unordered_set<std::string> seen;
            for (const auto& c : changes)
            {
                seen.insert(normalizePath(c.newPath));
            }
            for (const auto& p : prov->dirtyPaths)
            {
                if (seen.insert(normalizePath(p)).second)
                {
                    ChangedFile cf;
                    cf.kind = ChangeKind::Modified;
                    cf.newPath = p;
                    changes.push_back(cf);
                }
            }
        }
    }

    summary.repoRoot = repoRoot;
    summary.fromRev = !options.explicitFiles.empty() ? std::string() : (!options.fromRev.empty() ? options.fromRev : prov->repoCommit);
    summary.toRev = newBaseline;

    // --- load CDB, existing symhow_tu rows ---------------------------------------
    std::vector<symhow::index::CompileEntry> cdb;
    try
    {
        cdb = symhow::index::load(cdbPath, eff.dbOptions);
    }
    catch (const std::exception& e)
    {
        reportError(options, "failed to load compile database '" + cdbPath + "': " + e.what());
        return 1;
    }

    struct TuRow
    {
        std::string path;
        std::string argsHash;
        int64_t fileNodeId = 0;
        bool hasFileNodeId = false;
        std::string status = "ok";
    };
    std::vector<TuRow> tuRows;
    std::unordered_set<std::string> dbFilePaths;
    std::unordered_set<int64_t> allFileIds;
    std::unordered_map<int64_t, std::string> filePathById;

    try
    {
        symhow::db::Database db(options.dbPath, 0);
        bool haveSymhowTu = false;
        db.run("SELECT 1 FROM sqlite_master WHERE type='table' AND name='symhow_tu'", [&](sqlite3_stmt*) { haveSymhowTu = true; });
        if (haveSymhowTu)
        {
            // Phase 4: symhow_tu gained a `status` column (see Provenance.h's
            // kSymhowTuDdl) that replaces the old (Phase 3) "no outgoing
            // INCLUDE edge" retry heuristic with a precise per-TU outcome.
            // Later, symhow_tu gained an `entry_count` column (the duplicate-
            // CDB-entry fix -- args_hash became a COMBINED, per-path hash
            // instead of a single entry's, see CompileDb.h's
            // pathArgsHashes()) -- a symhow_tu table that predates either
            // column (PRAGMA table_info doesn't list it) is legacy: refuse,
            // same as any other provenance-shape mismatch -- there is no
            // way to retry-classify/flag-compare it correctly (in
            // particular, a pre-entry_count args_hash is a single entry's
            // hash, not the combined one this code now compares against).
            bool haveStatusColumn = false;
            bool haveEntryCountColumn = false;
            db.run("PRAGMA table_info(symhow_tu)", [&](sqlite3_stmt* stmt) {
                const char* colName = reinterpret_cast<const char*>(sqlite3_column_text(stmt, 1));
                if (colName == nullptr)
                {
                    return;
                }
                const std::string name = colName;
                if (name == "status")
                {
                    haveStatusColumn = true;
                }
                else if (name == "entry_count")
                {
                    haveEntryCountColumn = true;
                }
            });
            if (!haveStatusColumn)
            {
                reportRefused(
                    options,
                    "database's symhow_tu table predates per-TU status tracking (symhow_tu.status) -- re-index from scratch");
                return 2;
            }
            if (!haveEntryCountColumn)
            {
                reportRefused(
                    options,
                    "database's symhow_tu table predates duplicate-CDB-entry tracking (symhow_tu.entry_count) -- re-index "
                    "from scratch");
                return 2;
            }

            db.run("SELECT path, args_hash, file_node_id, status FROM symhow_tu", [&](sqlite3_stmt* stmt) {
                TuRow row;
                const char* p = reinterpret_cast<const char*>(sqlite3_column_text(stmt, 0));
                row.path = p != nullptr ? p : "";
                const char* h = reinterpret_cast<const char*>(sqlite3_column_text(stmt, 1));
                row.argsHash = h != nullptr ? h : "";
                if (sqlite3_column_type(stmt, 2) != SQLITE_NULL)
                {
                    row.fileNodeId = sqlite3_column_int64(stmt, 2);
                    row.hasFileNodeId = true;
                }
                const char* s = reinterpret_cast<const char*>(sqlite3_column_text(stmt, 3));
                row.status = s != nullptr ? s : "ok";
                dbFilePaths.insert(row.path);
                tuRows.push_back(std::move(row));
            });
        }
        db.run("SELECT id, path FROM file", [&](sqlite3_stmt* stmt) {
            const int64_t id = sqlite3_column_int64(stmt, 0);
            const char* p = reinterpret_cast<const char*>(sqlite3_column_text(stmt, 1));
            allFileIds.insert(id);
            filePathById[id] = p != nullptr ? p : "";
        });
    }
    catch (const std::exception& e)
    {
        reportError(options, e.what());
        return 1;
    }

    // Flag-change detection: a PATH's combined args hash (see
    // symhow::index::pathArgsHashes()'s doc comment) differs from its symhow_tu
    // row -> Modified. A symhow_tu row whose path is absent from the CDB ->
    // Deleted. Compared per PATH, not per CDB entry: a real CDB can list
    // the same source file more than once (several targets/configs with
    // different flags), and symhow_tu keeps exactly one row per path -- so
    // there is exactly one hash to compare against, computed once up front
    // from EVERY entry of that path together (not whichever entry happens
    // to be iterated), and both entries flag (or don't flag) together
    // rather than only whichever one is compared last winning.
    std::unordered_map<std::string, const TuRow*> tuByNormPath;
    for (const auto& row : tuRows)
    {
        tuByNormPath[normalizePath(row.path)] = &row;
    }
    // Suppress only content-derived changes, BEFORE adding flag changes or
    // retry seeds. --files and explicit revisions retain their force semantics.
    // A missing/unreadable file, absent/unknown fingerprint, or new TU always
    // takes the conservative path. Previous dirty paths still participate:
    // reverting one differs from its indexed hash and must be re-indexed.
    if (!changes.empty() && options.explicitFiles.empty() && options.fromRev.empty() && options.toRev.empty() && !diverges)
    {
        try
        {
            symhow::db::Database db(options.dbPath, 0);
            bool haveFingerprints = false;
            db.run("SELECT 1 FROM sqlite_master WHERE type='table' AND name='symhow_file_fingerprint'",
                [&](sqlite3_stmt*) { haveFingerprints = true; });
            if (haveFingerprints)
            {
                std::unordered_set<std::string> candidatePaths;
                for (const auto& change : changes)
                {
                    if (change.kind == ChangeKind::Modified || change.kind == ChangeKind::Added)
                    {
                        candidatePaths.insert(normalizePath(change.newPath));
                    }
                }
                std::unordered_map<std::string, std::string> indexedHashes;
                // Fetch hashes only for changed candidates, not the whole corpus.
                for (const auto& [id, path] : filePathById)
                {
                    const std::string norm = normalizePath(path);
                    if (candidatePaths.count(norm) == 0)
                    {
                        continue;
                    }
                    db.run(
                        "SELECT fp.sha256 FROM symhow_file_fingerprint fp JOIN file f ON f.id=fp.file_node_id "
                        "WHERE f.id=?1 AND f.indexed=1 AND f.complete=1",
                        [&](sqlite3_stmt* stmt) { sqlite3_bind_int64(stmt, 1, id); },
                        [&](sqlite3_stmt* stmt) {
                            const char* hash = reinterpret_cast<const char*>(sqlite3_column_text(stmt, 0));
                            if (hash != nullptr)
                            {
                                auto [it, inserted] = indexedHashes.emplace(norm, hash);
                                if (!inserted && it->second != hash)
                                {
                                    it->second.clear(); // ambiguous aliases: don't skip
                                }
                            }
                        });
                }
                std::unordered_set<std::string> newCdbPaths;
                for (const auto& entry : cdb)
                {
                    const std::string norm = normalizePath(entry.file);
                    if (tuByNormPath.find(norm) == tuByNormPath.end())
                    {
                        newCdbPaths.insert(norm);
                    }
                }
                std::unordered_map<std::string, std::optional<std::string>> diskHashes;
                changes.erase(std::remove_if(changes.begin(), changes.end(), [&](const ChangedFile& change) {
                    if (change.kind != ChangeKind::Modified && change.kind != ChangeKind::Added)
                    {
                        return false; // deletions/renames must invalidate path identity
                    }
                    const std::string norm = normalizePath(change.newPath);
                    if (newCdbPaths.count(norm) != 0)
                    {
                        return false; // file was a header, but is now also a new TU
                    }
                    const auto indexed = indexedHashes.find(norm);
                    if (indexed == indexedHashes.end() || indexed->second.empty())
                    {
                        return false;
                    }
                    auto disk = diskHashes.find(norm);
                    if (disk == diskHashes.end())
                    {
                        disk = diskHashes.emplace(norm, symhow::common::fileContentHash(change.newPath)).first;
                    }
                    return disk->second.has_value() && *disk->second == indexed->second;
                }), changes.end());
            }
        }
        catch (const std::exception& e)
        {
            reportError(options, "failed to compare indexed content: " + std::string(e.what()));
            return 1;
        }
    }

    const std::unordered_map<std::string, symhow::index::PathArgsInfo> cdbPathHashes = symhow::index::pathArgsHashes(cdb);
    std::unordered_set<std::string> cdbNormPaths;
    std::unordered_set<std::string> flaggedNormPaths;
    uint32_t flagChangedCount = 0;
    for (const auto& entry : cdb)
    {
        const std::string norm = normalizePath(entry.file);
        cdbNormPaths.insert(norm);
        const auto it = tuByNormPath.find(norm);
        if (it == tuByNormPath.end())
        {
            continue;  // brand-new path, not a flag-change candidate
        }
        if (!flaggedNormPaths.insert(norm).second)
        {
            continue;  // already flagged (or checked) this path via an earlier entry
        }
        const auto hashIt = cdbPathHashes.find(entry.file);
        const std::string combinedHash = hashIt != cdbPathHashes.end() ? hashIt->second.combinedHash : std::string();
        if (combinedHash != it->second->argsHash)
        {
            ChangedFile cf;
            cf.kind = ChangeKind::Modified;
            cf.newPath = entry.file;
            changes.push_back(cf);
            ++flagChangedCount;
        }
    }
    for (const auto& row : tuRows)
    {
        if (cdbNormPaths.find(normalizePath(row.path)) == cdbNormPaths.end())
        {
            ChangedFile cf;
            cf.kind = ChangeKind::Deleted;
            cf.newPath = row.path;
            changes.push_back(cf);
        }
    }

    // Retry set (Phase 4): CDB entries whose symhow_tu.status from the previous
    // run is anything other than "ok" -- precise per-TU tracking, replacing
    // the old (Phase 3) "no outgoing INCLUDE edge" over-approximation (see
    // docs/KNOWN_LIMITATIONS.md's Phase 4 section).
    std::vector<std::string> retryTuFiles;
    for (const auto& entry : cdb)
    {
        const std::string norm = normalizePath(entry.file);
        const auto it = tuByNormPath.find(norm);
        if (it == tuByNormPath.end())
        {
            continue;  // brand-new TU, not a retry candidate
        }
        if (it->second->status != "ok")
        {
            retryTuFiles.push_back(entry.file);
        }
    }

    // Phase 8 resume: when resuming an interrupted `symhow index`, the retry set
    // must ALSO cover every CDB entry with NO symhow_tu row at all -- not just
    // ones a `git diff` happened to flag as Added (match.newTus, below, is
    // git-diff-driven only). This is the "untracked suffix" the interrupted
    // run never got to: since checkpoints commit symhow_tu rows for a
    // contiguous CDB prefix (the parallel reorder buffer guarantees symhow_tu
    // rows land in CDB order regardless of -j), every CDB entry past the
    // last checkpoint -- and any entry whose file happens to have a `file`
    // row already (e.g. reached as a header by an earlier, completed TU)
    // but was never itself indexed as a TU -- has no symhow_tu row and lands
    // here.
    if (resumingInterruptedIndex)
    {
        for (const auto& entry : cdb)
        {
            const std::string norm = normalizePath(entry.file);
            if (tuByNormPath.find(norm) == tuByNormPath.end())
            {
                retryTuFiles.push_back(entry.file);
            }
        }
    }

    // De-duplicate `changes`: a file that's both git-diff-detected AND
    // flag/CDB-derived (e.g. a deleted TU is simultaneously a real `git
    // diff` Deleted entry AND a "symhow_tu row has no CDB entry anymore"
    // synthetic Deleted entry above; likewise a TU whose content changed
    // in the same commit its compile flags changed would otherwise get a
    // git-diff Modified entry AND a flag-changed Modified entry) would
    // otherwise be counted/cleared twice -- caught by the git-driven
    // scenario test (test/update/update_scenario.cmake step c: deleting
    // overloads.cpp reported files.deleted == 2, not 1, before this fix).
    {
        std::unordered_set<std::string> seenChangeKeys;
        std::vector<ChangedFile> deduped;
        deduped.reserve(changes.size());
        for (const auto& c : changes)
        {
            const std::string key = std::to_string(static_cast<int>(c.kind)) + "|" + normalizePath(c.oldPath) + "|" +
                                     normalizePath(c.newPath);
            if (seenChangeKeys.insert(key).second)
            {
                deduped.push_back(c);
            }
        }
        changes = std::move(deduped);
    }

    // files.renamed (symhow.update-summary/1): count of Renamed entries git diff
    // detected (GitDiff::parseNameStatusZ's `R<score>` entries) -- was
    // previously hardcoded to 0 in the JSON output below (a real gap, not
    // just cosmetic: the scenario test's `git mv` step needs a real count).
    uint32_t renamedCount = 0;
    for (const auto& c : changes)
    {
        if (c.kind == ChangeKind::Renamed)
        {
            ++renamedCount;
        }
    }

    // --- 3. classify against the DB/CDB --------------------------------------
    MatchInput matchInput;
    matchInput.changes = changes;
    matchInput.dbFilePaths = dbFilePaths;
    matchInput.cdb = cdb;
    const MatchResult match = matchChanged(matchInput, normalizePath);

    summary.changedCount = static_cast<uint32_t>(match.changedInDb.size());
    summary.deletedCount = static_cast<uint32_t>(match.deleted.size());
    summary.renamedCount = renamedCount;
    summary.flagChangedCount = flagChangedCount;
    summary.unmatchedCount = static_cast<uint32_t>(match.unmatched.size());
    summary.unmatchedPaths = match.unmatched;
    if (options.verbose > 0)
    {
        for (const auto& u : match.unmatched)
        {
            std::fprintf(stderr, "symhow update: unmatched: %s\n", u.c_str());
        }
    }

    // computeClearSet() matches its seed against `file.path` with an EXACT
    // string comparison (see Invalidator.cpp's step-0 SQL) -- so every
    // candidate below is resolved back to its precise file.path spelling
    // via a normalized-path lookup before being added. This also matters
    // for a git-changed HEADER: matchChanged() only classifies against
    // symhow_tu/CDB (TUs), so a changed header with no CDB entry of its own
    // always lands in `unmatched` -- but it's still a real `file.path` row,
    // and the whole point of the reverse-INCLUDE closure (computeClearSet
    // step 1) is to discover which TUs include it, so it MUST still be fed
    // in as a seed here.
    std::unordered_map<std::string, std::string> dbPathByNorm;
    for (const auto& [id, path] : filePathById)
    {
        dbPathByNorm[normalizePath(path)] = path;
    }
    auto resolveDbPath = [&](const std::string& p) -> std::string {
        const auto it = dbPathByNorm.find(normalizePath(p));
        return it != dbPathByNorm.end() ? it->second : p;
    };

    std::unordered_set<std::string> clearSeedSet;
    std::unordered_set<std::string> deletedSeedSet;
    for (const auto& p : match.changedInDb)
    {
        clearSeedSet.insert(resolveDbPath(p));
    }
    for (const auto& r : retryTuFiles)
    {
        clearSeedSet.insert(resolveDbPath(r));
    }
    for (const auto& p : match.deleted)
    {
        deletedSeedSet.insert(resolveDbPath(p));
    }
    for (const auto& c : changes)
    {
        const auto it = dbPathByNorm.find(normalizePath(c.newPath));
        if (it == dbPathByNorm.end())
        {
            continue;  // not a known DB file at all (brand-new file) -- nothing to clear
        }
        if (c.kind == ChangeKind::Deleted)
        {
            deletedSeedSet.insert(it->second);
        }
        else
        {
            clearSeedSet.insert(it->second);
        }
    }
    std::vector<std::string> clearSeed(clearSeedSet.begin(), clearSeedSet.end());
    std::vector<std::string> deletedSeed(deletedSeedSet.begin(), deletedSeedSet.end());

    // --- 4. nothing to do? ----------------------------------------------------
    // deletedSeed (not match.deleted!) is the right emptiness check here:
    // match.deleted only covers changed paths that already had a symhow_tu row
    // (an indexed TU), so a DELETED HEADER (never a TU itself, only ever
    // classified via the dbPathByNorm/changes loop above into
    // deletedSeedSet) would be invisible to match.deleted alone -- making
    // this branch wrongly declare "nothing to do" and skip clearing the
    // header (and its includers) entirely. deletedSeed is a strict superset
    // of match.deleted (see its construction above), so this is safe.
    if (clearSeed.empty() && deletedSeed.empty() && match.newTus.empty())
    {
        summary.nothingToDo = true;
        // Never mutate the real database on a --dry-run -- this refresh is
        // "opportunistic bookkeeping for next time", not something a
        // dry-run is allowed to have side effects for.
        if ((!repoRoot.empty() || resumingInterruptedIndex) && !options.dryRun)
        {
            try
            {
                symhow::db::WritableDb w(options.dbPath, false);
                w.begin();
                if (!repoRoot.empty())
                {
                    symhow::db::setMeta(w, "symhow_repo_commit", newBaseline);
                    symhow::db::setMeta(w, "symhow_dirty_paths", nlohmann::json(currentDirtyPaths(repoRoot, dirtyPaths)).dump());
                    symhow::db::setMeta(w, "symhow_last_update_at", utcTimestampNow());
                }
                if (resumingInterruptedIndex)
                {
                    // Every TU already has a symhow_tu row (the interrupted run
                    // must have reached the very last checkpoint before
                    // being interrupted) -- there's genuinely nothing left
                    // to index, so just clear the flag.
                    symhow::db::deleteMeta(w, "symhow_index_in_progress");
                }
                w.commit();
            }
            catch (const std::exception&)
            {
                // Best-effort only -- not a hard failure for "nothing to do".
            }
        }
        summary.elapsedMs = millisSince(runStart);
        if (!options.json)
        {
            std::fprintf(stderr, "symhow update: nothing to do\n");
        }
        else
        {
            std::printf("%s\n", buildSummaryJson(options, summary).dump().c_str());
        }
        if (out != nullptr)
        {
            *out = summary;
        }
        return 5;
    }

    // --- 5. working copy ------------------------------------------------------
    std::string work = options.dbPath;
    std::string workProj;
    bool madeTempCopy = false;
    if (!options.inPlace)
    {
        const std::filesystem::path dbp(options.dbPath);
        const std::string stem = dbp.stem().string();
        const std::string ext = dbp.extension().string();
        work = (dbp.parent_path() / (stem + ".incr-" + std::to_string(::getpid()) + ext)).string();
        std::error_code ec;
        std::filesystem::copy_file(options.dbPath, work, std::filesystem::copy_options::overwrite_existing, ec);
        if (ec)
        {
            reportError(options, "failed to create working copy '" + work + "': " + ec.message());
            return 1;
        }
        madeTempCopy = true;
        workProj = projSidecarFor(work);
    }
    else if (!options.dryRun)
    {
        // NEVER write symhow_update_in_progress on a --dry-run -- the whole
        // point of --in-place --dry-run is to be side-effect-free. Setting
        // this flag here and only clearing it after a real re-index (step
        // 9, far below, which a dry-run's early return skips entirely)
        // used to brick the database: every later `symhow update` would see
        // the stale flag and refuse forever, with no real update ever
        // having modified anything.
        try
        {
            symhow::db::WritableDb w(options.dbPath, false);
            w.begin();
            symhow::db::setMeta(w, "symhow_update_in_progress", "1");
            w.commit();
        }
        catch (const std::exception& e)
        {
            reportError(options, "failed to set in-progress flag: " + std::string(e.what()));
            return 1;
        }
    }

    auto cleanupTemp = [&]() {
        if (madeTempCopy)
        {
            std::error_code ec;
            std::filesystem::remove(work, ec);
            std::filesystem::remove(work + "-journal", ec);
            std::filesystem::remove(workProj, ec);
        }
    };

    ClearPlan plan;
    try
    {
        symhow::db::WritableDb w(work, true);
        std::vector<std::string> tuPathsForClosure;
        tuPathsForClosure.reserve(cdb.size());
        for (const auto& e : cdb)
        {
            // Resolve through the SAME normalizePath()-based mapping used
            // for the clear/deleted seeds above (dbPathByNorm/resolveDbPath)
            // rather than pushing the CDB's raw spelling -- the header-
            // pruning step (Invalidator's step 2) excludes these paths via
            // an EXACT string match against `file.path`, so a `./`-prefixed
            // or symlinked CDB entry that textually differs from its
            // file.path row would otherwise fail to match, and a header
            // that's ALSO a TU could get wrongly pruned (violating the "a
            // TU is never pruned as a header" invariant).
            tuPathsForClosure.push_back(resolveDbPath(e.file));
        }
        plan = computeClearSet(w, clearSeed, deletedSeed, tuPathsForClosure);
    }
    catch (const std::exception& e)
    {
        reportError(options, "failed to compute clear set: " + std::string(e.what()));
        cleanupTemp();
        return 1;
    }

    summary.plan = plan;

    // --- 5.5: work out what would be (or will be) re-indexed -------------------
    // Computed unconditionally (used by both --dry-run's JSON summary below
    // and the real re-index step further down) so a dry run can report
    // accurate tus.* counts instead of the caller having to guess.
    std::unordered_set<std::string> reindexNorm;
    for (const auto& p : plan.clearedPaths)
    {
        reindexNorm.insert(normalizePath(p));
    }
    for (const auto& p : match.newTus)
    {
        reindexNorm.insert(normalizePath(p));
    }
    for (const auto& p : retryTuFiles)
    {
        reindexNorm.insert(normalizePath(p));
    }

    std::vector<symhow::index::CompileEntry> presetEntries = selectEntries(cdb, reindexNorm, normalizePath);
    std::vector<symhow::index::CompileEntry> filteredEntries;
    filteredEntries.reserve(presetEntries.size());
    uint32_t skippedMissing = 0;
    for (auto& entry : presetEntries)
    {
        if (!std::filesystem::exists(entry.file))
        {
            ++skippedMissing;
            if (options.verbose > 0)
            {
                std::fprintf(stderr, "symhow update: warning: skipping missing file '%s'\n", entry.file.c_str());
            }
            continue;
        }
        filteredEntries.push_back(entry);
    }

    summary.tusToIndex = static_cast<uint32_t>(filteredEntries.size());
    summary.tusNew = static_cast<uint32_t>(match.newTus.size());
    summary.tusRetried = static_cast<uint32_t>(retryTuFiles.size());
    summary.skippedMissing = skippedMissing;

    if (options.dryRun)
    {
        std::fprintf(
            stderr,
            "symhow update: dry-run: would clear %zu files (%u seed, %u via includes, %u pruned headers); would "
            "re-index %u TUs (%u new, %u retried, %u skipped-missing)\n",
            plan.clearedFileIds.size(),
            plan.seedFiles,
            plan.includeClosureAdded,
            plan.headersPruned,
            summary.tusToIndex,
            summary.tusNew,
            summary.tusRetried,
            summary.skippedMissing);
        if (options.verbose > 0)
        {
            for (const auto& p : plan.clearedPaths)
            {
                std::fprintf(stderr, "  clear: %s\n", p.c_str());
            }
            for (const auto& t : match.newTus)
            {
                std::fprintf(stderr, "  new tu: %s\n", t.c_str());
            }
        }
        cleanupTemp();
        summary.elapsedMs = millisSince(runStart);
        if (options.json)
        {
            std::printf("%s\n", buildSummaryJson(options, summary).dump().c_str());
        }
        if (out != nullptr)
        {
            *out = summary;
        }
        return 0;
    }

    // --- 6. invalidate ----------------------------------------------------
    GcStats gc;
    try
    {
        symhow::db::WritableDb w(work, true);
        gc = invalidate(w, plan);
    }
    catch (const std::exception& e)
    {
        reportError(options, "invalidation failed: " + std::string(e.what()));
        cleanupTemp();
        return 1;
    }

    // --- 7. re-index ----------------------------------------------------------
    symhow::model::IndexSummary idxSummary;
    const auto indexStart = std::chrono::steady_clock::now();
    if (!filteredEntries.empty())
    {
        symhow::index::IndexRunOptions runOpts;
        runOpts.compileCommandsPath = cdbPath;
        runOpts.outputPath = work;
        runOpts.appendToExisting = true;
        runOpts.presetEntries = filteredEntries;
        runOpts.quiet = options.verbose <= 0;
        runOpts.jsonSummary = false;
        runOpts.noVacuum = options.noVacuum;
        runOpts.verbose = options.verbose > 0;
        runOpts.parallel = options.parallel;
        runOpts.symhowVersion = options.symhowVersion;
        runOpts.dbOptions = eff.dbOptions;
        runOpts.sanitizerOptions = eff.sanitizerOptions;
        runOpts.systemFileRows = eff.systemFileRows;

        symhow::index::Provenance updated = *prov;
        updated.symhowVersion = options.symhowVersion;
        updated.walkerVersion = symhow::index::kWalkerVersion;
        updated.cdbPath = std::filesystem::absolute(cdbPath).lexically_normal().string();
        updated.dbOptions = eff.dbOptions;
        updated.sanitizerOptions = eff.sanitizerOptions;
        updated.systemFileRows = eff.systemFileRows;
        updated.repoRoot = repoRoot;
        updated.repoCommit = newBaseline;
        updated.dirtyPaths = currentDirtyPaths(repoRoot, dirtyPaths);
        updated.lastUpdateAtUtc = utcTimestampNow();
        runOpts.provenance = updated;

        const int rc = symhow::index::runIndex(runOpts, &idxSummary);
        if (rc == 1)
        {
            reportError(options, "re-indexing failed");
            if (!options.inPlace)
            {
                cleanupTemp();
            }
            else
            {
                std::fprintf(
                    stderr,
                    "symhow update: the database was modified in place and left with symhow_update_in_progress set -- "
                    "re-index from scratch to recover\n");
            }
            return 1;
        }
        summary.indexRan = true;
        summary.idxTusTotal = idxSummary.tusTotal;
        summary.idxTusAttempted = idxSummary.tusAttempted;
        summary.idxTusWithContent = idxSummary.tusWithContent;
        summary.idxTusFailed = idxSummary.tusFailed;
        summary.idxSymbols = idxSummary.symbols;
        summary.idxReferences = idxSummary.references;
        summary.idxFiles = idxSummary.files;
        summary.idxErrors = idxSummary.errors;
        summary.idxNameCollisions = idxSummary.nameCollisions;
    }
    else
    {
        // Nothing to re-index (everything cleared was a pure deletion) --
        // still refresh provenance so symhow_repo_commit/dirty paths/
        // last-update-at stay current.
        try
        {
            symhow::db::WritableDb w(work, false);
            w.begin();
            symhow::index::Provenance updated = *prov;
            updated.symhowVersion = options.symhowVersion;
            updated.repoRoot = repoRoot;
            updated.repoCommit = newBaseline;
            updated.dirtyPaths = currentDirtyPaths(repoRoot, dirtyPaths);
            updated.lastUpdateAtUtc = utcTimestampNow();
            symhow::index::writeProvenance(w, updated);
            w.commit();
        }
        catch (const std::exception& e)
        {
            reportError(options, "failed to refresh provenance: " + std::string(e.what()));
            cleanupTemp();
            return 1;
        }
    }
    summary.indexElapsedMs = millisSince(indexStart);

    // --- 8. post-index sweep ---------------------------------------------------
    try
    {
        symhow::db::WritableDb w2(work, true);
        const GcStats sweep = sweepOrphans(w2);
        gc.edges += sweep.edges;
        gc.locations += sweep.locations;
        gc.nodes += sweep.nodes;
        gc.localSymbols += sweep.localSymbols;
        gc.errors += sweep.errors;
        gc.nonIndexedFiles += sweep.nonIndexedFiles;
        gc.orphanElements += sweep.orphanElements;
        w2.exec("ANALYZE;");
    }
    catch (const std::exception& e)
    {
        std::fprintf(stderr, "symhow update: warning: post-index sweep failed: %s\n", e.what());
    }
    summary.gc = gc;

    // --- 8.5. notes sync (Phase 3 of docs/NOTES_DESIGN.md) ---------------------
    // Best-effort: resolve a notes dir exactly like Cli.cpp's
    // resolveNotesDirDefault() (meta.symhow_notes_dir, else <repo root>/notes if
    // it exists), and if one exists on disk, re-run syncNotes() against the
    // just-re-indexed working copy so `symhow_note.stale` reflects the new code
    // before this update is handed back to the caller. Runs BEFORE the
    // symhow_index_in_progress clear/rename below so it's part of the same
    // working-copy state that becomes the final database. Never fails the
    // update -- any exception here is reported and swallowed.
    try
    {
        std::string notesDir;
        {
            symhow::db::Database rdb(work);
            const std::optional<std::string> stored = symhow::db::getMeta(rdb.handle(), "symhow_notes_dir");
            if (stored.has_value() && !stored->empty())
            {
                notesDir = *stored;
            }
            else if (!repoRoot.empty())
            {
                const std::filesystem::path candidate = std::filesystem::path(repoRoot) / "notes";
                if (std::filesystem::is_directory(candidate))
                {
                    notesDir = candidate.string();
                }
            }
        }

        if (!notesDir.empty() && std::filesystem::is_directory(notesDir))
        {
            // Pre-sync stale set, straight from whatever the previous `symhow
            // notes sync` left in `symhow_note` -- re-indexing itself never
            // touches this cache, so it's still exactly last sync's result.
            std::unordered_set<std::string> preStale;
            {
                symhow::db::Database rdb(work);
                try
                {
                    rdb.run("SELECT serialized_name FROM symhow_note WHERE stale = 1", [&](sqlite3_stmt* stmt) {
                        const unsigned char* t = sqlite3_column_text(stmt, 0);
                        if (t != nullptr)
                        {
                            preStale.insert(reinterpret_cast<const char*>(t));
                        }
                    });
                }
                catch (const std::exception&)
                {
                    // symhow_note doesn't exist yet (never synced) -- preStale stays empty.
                }
            }

            const symhow::notes::SyncResult syncResult = symhow::notes::syncNotes(work, notesDir, false);

            std::unordered_set<std::string> postStale;
            {
                symhow::db::Database rdb(work);
                rdb.run("SELECT serialized_name FROM symhow_note WHERE stale = 1", [&](sqlite3_stmt* stmt) {
                    const unsigned char* t = sqlite3_column_text(stmt, 0);
                    if (t != nullptr)
                    {
                        postStale.insert(reinterpret_cast<const char*>(t));
                    }
                });
            }
            uint32_t newlyStale = 0;
            for (const std::string& sn : postStale)
            {
                if (preStale.find(sn) == preStale.end())
                {
                    ++newlyStale;
                }
            }

            summary.notesRan = true;
            summary.notesNoted = static_cast<uint32_t>(syncResult.counts.matched);
            summary.notesStale = static_cast<uint32_t>(syncResult.counts.stale);
            summary.notesNewlyStale = newlyStale;
            summary.notesProblems = static_cast<uint32_t>(syncResult.problems.size());

            if (!options.json)
            {
                std::fprintf(
                    stderr,
                    "notes: %u noted, %u stale (%u newly stale)\n",
                    summary.notesNoted,
                    summary.notesStale,
                    summary.notesNewlyStale);
            }
        }
    }
    catch (const std::exception& e)
    {
        std::fprintf(stderr, "symhow update: warning: post-update notes sync failed: %s\n", e.what());
    }

    // Phase 8: clear symhow_index_in_progress on `work` -- before the rename/
    // in-place-commit step below, so it's gone from whichever path
    // ultimately becomes `options.dbPath`. Idempotent (no-op if absent), so
    // safe to run unconditionally rather than gating on
    // resumingInterruptedIndex (a belt-and-suspenders cleanup: e.g. a copy
    // made via `cp` of an in-progress db that `symhow index` itself never got a
    // chance to clear).
    try
    {
        symhow::db::WritableDb wflag(work, false);
        wflag.begin();
        symhow::db::deleteMeta(wflag, "symhow_index_in_progress");
        wflag.commit();
    }
    catch (const std::exception& e)
    {
        std::fprintf(stderr, "symhow update: warning: failed to clear symhow_index_in_progress: %s\n", e.what());
    }

    // --- 9. commit --------------------------------------------------------------
    if (!options.inPlace)
    {
        std::error_code ec;
        std::filesystem::rename(work, options.dbPath, ec);
        if (ec)
        {
            reportError(options, "failed to rename '" + work + "' to '" + options.dbPath + "': " + ec.message());
            cleanupTemp();
            return 1;
        }
        std::filesystem::remove(work + "-journal", ec);
        std::filesystem::remove(workProj, ec);
    }
    else
    {
        try
        {
            symhow::db::WritableDb w(options.dbPath, false);
            w.begin();
            symhow::db::deleteMeta(w, "symhow_update_in_progress");
            w.commit();
        }
        catch (const std::exception& e)
        {
            std::fprintf(
                stderr,
                "symhow update: warning: failed to clear symhow_update_in_progress after a successful update: %s\n",
                e.what());
        }
    }

    summary.elapsedMs = millisSince(runStart);

    if (!options.json)
    {
        std::fprintf(
            stderr,
            "symhow update: cleared %zu files (%u via includes, %u pruned headers); re-indexed %u TUs (%u new, %u "
            "retried, %u skipped-missing); gc: %lld nodes, %lld edges, %lld locations, %lld symbols reset; %llu "
            "ms\n",
            plan.clearedFileIds.size(),
            plan.includeClosureAdded,
            plan.headersPruned,
            summary.tusToIndex,
            summary.tusNew,
            summary.tusRetried,
            summary.skippedMissing,
            static_cast<long long>(gc.nodes),
            static_cast<long long>(gc.edges),
            static_cast<long long>(gc.locations),
            static_cast<long long>(gc.symbolsReset),
            static_cast<unsigned long long>(summary.elapsedMs));
    }
    else
    {
        std::printf("%s\n", buildSummaryJson(options, summary).dump().c_str());
    }

    if (out != nullptr)
    {
        *out = summary;
    }
    return 0;
}

}  // namespace symhow::update
