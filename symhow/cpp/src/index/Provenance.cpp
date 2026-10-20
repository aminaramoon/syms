#include "symhow/index/Provenance.h"

#include <sqlite3.h>

#include <nlohmann/json.hpp>

#include "symhow/db/Meta.h"
#include "symhow/db/WritableDb.h"

namespace symhow::index
{

namespace
{

nlohmann::json optionsToJson(const Provenance& prov)
{
    nlohmann::json remaps = nlohmann::json::array();
    for (const auto& [from, to] : prov.dbOptions.remaps)
    {
        remaps.push_back({{"from", from}, {"to", to}});
    }

    const char* systemFileRowsName = "direct";
    switch (prov.systemFileRows)
    {
        case SystemFileRows::None:
            systemFileRowsName = "none";
            break;
        case SystemFileRows::Direct:
            systemFileRowsName = "direct";
            break;
        case SystemFileRows::All:
            systemFileRowsName = "all";
            break;
    }

    nlohmann::json j;
    j["remaps"] = remaps;
    j["include"] = prov.dbOptions.includeGlobs;
    j["exclude"] = prov.dbOptions.excludeGlobs;
    j["system_file_rows"] = systemFileRowsName;
    j["drop_args"] = prov.sanitizerOptions.dropArgPatterns;
    j["extra_args"] = prov.sanitizerOptions.extraArgs;
    j["resource_dir"] = prov.sanitizerOptions.resourceDir;
    j["gcc_toolchain"] = prov.sanitizerOptions.gccToolchain;
    j["std"] = prov.sanitizerOptions.stdFallback;
    j["c_std"] = prov.sanitizerOptions.cStdFallback;
    j["cuda_path"] = prov.sanitizerOptions.cudaPath;
    j["cuda_mode"] = cuda::cudaModeName(prov.sanitizerOptions.cudaMode);
    j["limit"] = prov.limit;
    return j;
}

void optionsFromJson(const nlohmann::json& j, Provenance& prov)
{
    if (j.contains("remaps"))
    {
        for (const auto& r : j.at("remaps"))
        {
            prov.dbOptions.remaps.emplace_back(r.value("from", ""), r.value("to", ""));
        }
    }
    if (j.contains("include"))
    {
        prov.dbOptions.includeGlobs = j.at("include").get<std::vector<std::string>>();
    }
    if (j.contains("exclude"))
    {
        prov.dbOptions.excludeGlobs = j.at("exclude").get<std::vector<std::string>>();
    }
    if (j.contains("system_file_rows"))
    {
        const std::string v = j.at("system_file_rows").get<std::string>();
        prov.systemFileRows = v == "none" ? SystemFileRows::None : (v == "all" ? SystemFileRows::All : SystemFileRows::Direct);
    }
    if (j.contains("drop_args"))
    {
        prov.sanitizerOptions.dropArgPatterns = j.at("drop_args").get<std::vector<std::string>>();
    }
    if (j.contains("extra_args"))
    {
        prov.sanitizerOptions.extraArgs = j.at("extra_args").get<std::vector<std::string>>();
    }
    if (j.contains("resource_dir"))
    {
        prov.sanitizerOptions.resourceDir = j.at("resource_dir").get<std::string>();
    }
    if (j.contains("gcc_toolchain"))
    {
        prov.sanitizerOptions.gccToolchain = j.at("gcc_toolchain").get<std::string>();
    }
    if (j.contains("std"))
    {
        prov.sanitizerOptions.stdFallback = j.at("std").get<std::string>();
    }
    if (j.contains("c_std"))
    {
        prov.sanitizerOptions.cStdFallback = j.at("c_std").get<std::string>();
    }
    if (j.contains("cuda_path"))
    {
        prov.sanitizerOptions.cudaPath = j.at("cuda_path").get<std::string>();
    }
    if (j.contains("cuda_mode"))
    {
        try
        {
            prov.sanitizerOptions.cudaMode = cuda::parseCudaMode(j.at("cuda_mode").get<std::string>());
        }
        catch (const std::exception&)
        {
            // Leave at default; a malformed/unknown value here shouldn't
            // prevent reading the rest of provenance.
        }
    }
    if (j.contains("limit"))
    {
        prov.limit = j.at("limit").get<long>();
    }
}

}  // namespace

void writeProvenance(symhow::db::WritableDb& db, const Provenance& prov)
{
    symhow::db::setMeta(db, "symhow_provenance_version", std::to_string(kProvenanceVersion));
    symhow::db::setMeta(db, "symhow_version", prov.symhowVersion);
    symhow::db::setMeta(db, "symhow_walker_version", std::to_string(prov.walkerVersion));
    symhow::db::setMeta(db, "symhow_cdb_path", prov.cdbPath);
    symhow::db::setMeta(db, "symhow_options_json", optionsToJson(prov).dump());
    symhow::db::setMeta(db, "symhow_repo_root", prov.repoRoot);
    symhow::db::setMeta(db, "symhow_repo_commit", prov.repoCommit);
    symhow::db::setMeta(db, "symhow_dirty_paths", nlohmann::json(prov.dirtyPaths).dump());
    symhow::db::setMeta(db, "symhow_indexed_at", prov.indexedAtUtc);
    symhow::db::setMeta(db, "symhow_last_update_at", prov.lastUpdateAtUtc);
    // Own meta key, NOT part of symhow_options_json -- see Provenance::notesDir's
    // doc comment. Only written when non-empty, so a `symhow index` run without
    // --notes-dir never clobbers a notes dir set by a later `symhow notes sync
    // --notes-dir` / `symhow update`.
    if (!prov.notesDir.empty())
    {
        symhow::db::setMeta(db, "symhow_notes_dir", prov.notesDir);
    }
}

std::optional<Provenance> readProvenance(sqlite3* handle)
{
    const std::optional<std::string> version = symhow::db::getMeta(handle, "symhow_provenance_version");
    if (!version.has_value())
    {
        return std::nullopt;
    }

    Provenance prov;
    prov.symhowVersion = symhow::db::getMeta(handle, "symhow_version").value_or("");
    try
    {
        prov.walkerVersion = std::stoi(symhow::db::getMeta(handle, "symhow_walker_version").value_or("0"));
    }
    catch (const std::exception&)
    {
        prov.walkerVersion = 0;
    }
    prov.cdbPath = symhow::db::getMeta(handle, "symhow_cdb_path").value_or("");

    const std::string optionsJson = symhow::db::getMeta(handle, "symhow_options_json").value_or("");
    if (!optionsJson.empty())
    {
        try
        {
            optionsFromJson(nlohmann::json::parse(optionsJson), prov);
        }
        catch (const std::exception&)
        {
            // Corrupt/foreign JSON: leave options at defaults rather than
            // failing the whole readProvenance() call.
        }
    }

    prov.repoRoot = symhow::db::getMeta(handle, "symhow_repo_root").value_or("");
    prov.repoCommit = symhow::db::getMeta(handle, "symhow_repo_commit").value_or("");

    const std::string dirtyJson = symhow::db::getMeta(handle, "symhow_dirty_paths").value_or("");
    if (!dirtyJson.empty())
    {
        try
        {
            prov.dirtyPaths = nlohmann::json::parse(dirtyJson).get<std::vector<std::string>>();
        }
        catch (const std::exception&)
        {
        }
    }

    prov.indexedAtUtc = symhow::db::getMeta(handle, "symhow_indexed_at").value_or("");
    prov.lastUpdateAtUtc = symhow::db::getMeta(handle, "symhow_last_update_at").value_or("");
    prov.notesDir = symhow::db::getMeta(handle, "symhow_notes_dir").value_or("");

    return prov;
}

}  // namespace symhow::index
