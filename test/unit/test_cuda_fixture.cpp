// Exercises the hermetic, toolkit-free Phase 6 CUDA fixture database
// (build/cuda_fixture.srctrldb, produced by the `cuda_fixture_index` CTest
// test -- see test/CMakeLists.txt's FIXTURES_SETUP FixtureCudaDb wiring and
// test/fixtures/cuda/{fake_cuda.h,kernels.cu,compile_commands.json.in} for
// the fixture itself). Tagged "[fixture][cuda_fixture]" so it gets its own
// FIXTURES_REQUIRED FixtureCudaDb registration ("cuda_fixture_query." in
// test/CMakeLists.txt), distinct from the synthetic C++ fixture's
// FixtureDb -- this test must NOT run against build/fixture.srctrldb.

#include <catch2/catch_test_macros.hpp>

#include <algorithm>

#include "kg/db/Database.h"
#include "kg/db/Enums.h"
#include "kg/query/QueryEngine.h"
#include "kg/query/SymbolResolver.h"

using kg::query::QueryEngine;
using kg::query::ResolveOutcome;

namespace
{

std::string cudaFixtureDbPath()
{
#ifdef KG_CUDA_FIXTURE_DB
    return KG_CUDA_FIXTURE_DB;
#else
    return "cuda_fixture.srctrldb";
#endif
}

int64_t resolveOne(kg::db::Database& db, const std::string& spec, const std::vector<int>* kindFilter = nullptr)
{
    const kg::query::ResolveResult rr = kg::query::resolveSymbol(db, spec, kindFilter, nullptr);
    INFO("resolving '" << spec << "'");
    REQUIRE(rr.outcome == ResolveOutcome::Unique);
    return rr.candidates.front().id;
}

bool containsNode(const kg::query::BfsResult& r, int64_t id)
{
    return std::any_of(r.nodes.begin(), r.nodes.end(), [id](const kg::query::BfsNode& n) { return n.id == id; });
}

}  // namespace

TEST_CASE(
    "cuda_fixture: kernel, device helper, and host launcher all exist as FUNCTION nodes",
    "[fixture][cuda_fixture]")
{
    kg::db::Database db(cudaFixtureDbPath());
    const std::vector<int> functionKind = {kg::db::NodeKind::FUNCTION};

    const int64_t kernel = resolveOne(db, "kg_cuda_fixture_kernel", &functionKind);
    const int64_t helper = resolveOne(db, "kg_cuda_fixture_double_value", &functionKind);
    const int64_t launcher = resolveOne(db, "kg_cuda_fixture_launch", &functionKind);

    // Just resolving each uniquely as a FUNCTION-kind node (above) already
    // proves they exist with that kind; REQUIRE something on the ids too so
    // this TEST_CASE isn't vacuous if resolveOne's own REQUIRE were ever
    // weakened.
    REQUIRE(kernel != 0);
    REQUIRE(helper != 0);
    REQUIRE(launcher != 0);
}

TEST_CASE("cuda_fixture: launcher -> kernel is a real CALL edge (nowrapper rung, no CUDA toolkit installed)", "[fixture][cuda_fixture]")
{
    kg::db::Database db(cudaFixtureDbPath());
    QueryEngine engine(db);

    const int64_t kernel = resolveOne(db, "kg_cuda_fixture_kernel");
    const int64_t launcher = resolveOne(db, "kg_cuda_fixture_launch");

    // The `kernel<<<grid,block>>>(args)` launch syntax lowers (via clang's
    // Sema, see fake_cuda.h's cudaConfigureCall doc comment) into a
    // CUDAKernelCallExpr whose libclang cursor kind is plain CXCursor_CallExpr
    // with clang_getCursorReferenced() resolving DIRECTLY to the kernel's
    // FunctionDecl -- empirically verified (see docs/KNOWN_LIMITATIONS.md's
    // Phase 6 section) via a standalone libclang cursor dump before writing
    // this assertion, so kg::index::AstWalker::handleCallExpr() needs no
    // CUDA-specific special-casing to record this edge.
    const kg::query::BfsResult callees = engine.callees({launcher}, /*depth=*/1, /*maxNodes=*/0);
    CHECK(containsNode(callees, kernel));

    bool foundCallEdge = false;
    for (const auto& e : callees.edges)
    {
        if (e.kind == kg::db::EdgeKind::CALL && e.source == launcher && e.target == kernel)
        {
            foundCallEdge = true;
        }
    }
    CHECK(foundCallEdge);
}

TEST_CASE("cuda_fixture: kernel -> device helper is a real CALL edge", "[fixture][cuda_fixture]")
{
    kg::db::Database db(cudaFixtureDbPath());
    QueryEngine engine(db);

    const int64_t kernel = resolveOne(db, "kg_cuda_fixture_kernel");
    const int64_t helper = resolveOne(db, "kg_cuda_fixture_double_value");

    const kg::query::BfsResult callees = engine.callees({kernel}, /*depth=*/1, /*maxNodes=*/0);
    CHECK(containsNode(callees, helper));

    bool foundCallEdge = false;
    for (const auto& e : callees.edges)
    {
        if (e.kind == kg::db::EdgeKind::CALL && e.source == kernel && e.target == helper)
        {
            foundCallEdge = true;
        }
    }
    CHECK(foundCallEdge);
}
