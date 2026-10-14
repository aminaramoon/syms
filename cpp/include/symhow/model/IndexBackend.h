#pragma once

#include <cstdint>
#include <string>
#include <vector>

#include "symhow/model/Facts.h"

// The extraction<->persistence seam: an IndexBackend consumes the
// TuFactBatch produced per translation unit and is responsible for
// committing it to some concrete storage. symhow::index::SrctrldbBackend (the
// only backend implemented so far, writing to a Sourcetrail-schema SQLite
// database via the vendored SourcetrailDB writer) lives in symhow_index, not
// here -- symhow_model must not depend on third_party/sourcetraildb. This split
// is deliberately kept clean so a future non-libclang backend (e.g.
// CodeQL) could plug in without touching extraction code.
namespace symhow::model
{

struct IndexSummary
{
    uint32_t tusTotal{};
    uint32_t tusAttempted{};
    uint32_t tusWithContent{};
    uint32_t tusFailed{};
    uint32_t symbols{};
    uint32_t references{};
    uint32_t files{};
    uint32_t errors{};
    uint32_t nameCollisions{};
    uint64_t elapsedMs{};
    std::vector<std::string> failedTus;
};

class IndexBackend
{
public:
    virtual ~IndexBackend() = default;

    virtual bool begin(const std::string& dbPath, std::string* err) = 0;

    // Called once per translation unit, single-threaded, in TU order.
    virtual bool consume(const TuFactBatch& batch, std::string* err) = 0;

    virtual bool finish(IndexSummary& summary, std::string* err) = 0;
};

}  // namespace symhow::model
