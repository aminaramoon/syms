#include <catch2/catch_test_macros.hpp>

#include <algorithm>
#include <string>
#include <vector>

#include "symhow/index/ArgSanitizer.h"
#include "symhow/index/CompileDb.h"
#include "symhow/index/CudaSupport.h"

using symhow::index::applyRemaps;
using symhow::index::CompileEntry;
using symhow::index::parseRemapArg;
using symhow::index::sanitize;
using symhow::index::SanitizerOptions;

namespace
{

CompileEntry makeEntry(
    std::string file,
    std::vector<std::string> argsWithArgv0,
    std::string directory = "/work",
    std::string argv0 = "/usr/bin/c++")
{
    CompileEntry e;
    e.directory = std::move(directory);
    e.file = std::move(file);
    e.args.push_back(std::move(argv0));
    for (auto& a : argsWithArgv0)
    {
        e.args.push_back(std::move(a));
    }
    return e;
}

// Returns the portion of a fully-sanitized argv that precedes the step-5
// injection block (which always starts with "-working-directory=..."). This
// isolates steps 2-4's effect for table-driven assertions, since exhaustively
// re-deriving the exact injected suffix for every single row would obscure
// what's actually being tested.
std::vector<std::string> coreArgs(const std::vector<std::string>& args)
{
    auto it = std::find_if(
        args.begin(), args.end(), [](const std::string& t) { return t.rfind("-working-directory=", 0) == 0; });
    return std::vector<std::string>(args.begin(), it);
}

struct Case
{
    std::string name;
    std::string file;
    std::vector<std::string> input;     // argv, excluding argv0
    std::vector<std::string> expected;  // expected kept tokens (coreArgs)
    std::string argv0 = "/usr/bin/c++";
};

// `cudaMode` matters only for rows where the entry ends up going through
// step 4 CUDA synthesis (i.e. a .cu file or an nvcc argv0 -- see
// ArgSanitizer::sanitize's `doCudaSynthesis` condition, which mirrors step
// 3a's nvcc-rule gate exactly). It defaults to Auto (which is harmless for
// the plain-host-compiler rows in the universal-drop-list table, since
// those never trigger CUDA synthesis at all); the nvcc-rule table below
// overrides it to HostOnly, which deterministically appends exactly
// {"-x", "c++"} and nothing else -- see CudaSupport.h -- so its effect on
// `expected` is trivial to account for, unlike Full (which would also pull
// in an nvcc-executable-path-derived --cuda-path and a --cuda-gpu-arch
// dependent on the row's -gencode/--generate-code flags).
void runCases(const std::vector<Case>& cases, symhow::index::cuda::CudaMode cudaMode = symhow::index::cuda::CudaMode::Auto)
{
    SanitizerOptions options;
    options.resourceDir = "/opt/resource-dir";  // avoid depending on SYMHOW_DEFAULT_RESOURCE_DIR
    options.cudaMode = cudaMode;

    for (const auto& c : cases)
    {
        INFO("case: " << c.name);
        const CompileEntry entry = makeEntry(c.file, c.input, "/work", c.argv0);
        const auto result = sanitize(entry, options);
        CHECK(coreArgs(result.args) == c.expected);
    }
}

}  // namespace

TEST_CASE("arg_sanitizer: ArgSanitizer: universal drop list", "[argsanitizer]")
{
    const std::vector<Case> cases = {
        {"-c dropped", "foo.cpp", {"-DKEEP", "-c", "-O2"}, {"-DKEEP", "-O2"}},
        {"-o standalone drops flag+value", "foo.cpp", {"-DKEEP", "-o", "foo.o", "-O2"}, {"-DKEEP", "-O2"}},
        {"-o attached dropped", "foo.cpp", {"-DKEEP", "-ofoo.o", "-O2"}, {"-DKEEP", "-O2"}},
        {"-MD dropped (no-arg)", "foo.cpp", {"-DKEEP", "-MD", "-O2"}, {"-DKEEP", "-O2"}},
        {"-MMD dropped (no-arg)", "foo.cpp", {"-MMD"}, {}},
        {"-MP dropped (no-arg)", "foo.cpp", {"-MP"}, {}},
        {"-MG dropped (no-arg)", "foo.cpp", {"-MG"}, {}},
        {"-M dropped (no-arg)", "foo.cpp", {"-M"}, {}},
        {"-MM dropped (no-arg)", "foo.cpp", {"-MM"}, {}},
        {"-MF drops flag+value", "foo.cpp", {"-DKEEP", "-MF", "foo.d", "-O2"}, {"-DKEEP", "-O2"}},
        {"-MT drops flag+value", "foo.cpp", {"-MT", "foo.o"}, {}},
        {"-MQ drops flag+value", "foo.cpp", {"-MQ", "foo.o"}, {}},
        {"-fmodules-ts dropped", "foo.cpp", {"-fmodules-ts"}, {}},
        {"-fmodule-mapper= dropped (prefix)", "foo.cpp", {"-fmodule-mapper=foo.modmap"}, {}},
        {"-fdeps-format= dropped (prefix)", "foo.cpp", {"-fdeps-format=p1689r5"}, {}},
        {"-fdeps-file= dropped (prefix)", "foo.cpp", {"-fdeps-file=foo.ddi"}, {}},
        {"-fdeps-target= dropped (prefix)", "foo.cpp", {"-fdeps-target=foo.o"}, {}},
        {"-Werror dropped", "foo.cpp", {"-Werror"}, {}},
        {"-Werror= dropped (prefix)", "foo.cpp", {"-Werror=unused"}, {}},
        {"-pedantic-errors dropped", "foo.cpp", {"-pedantic-errors"}, {}},
        {"-fdiagnostics-color dropped (prefix)", "foo.cpp", {"-fdiagnostics-color=always"}, {}},
        {"-l* dropped (link-only)", "foo.cpp", {"-lfoo"}, {}},
        {"-L* dropped (link-only)", "foo.cpp", {"-L/usr/lib"}, {}},
        {"-Wl,* dropped (link-only)", "foo.cpp", {"-Wl,--as-needed"}, {}},
        {"-shared dropped (link-only)", "foo.cpp", {"-shared"}, {}},
        {"-rdynamic dropped (link-only)", "foo.cpp", {"-rdynamic"}, {}},
        {"-pie dropped (link-only)", "foo.cpp", {"-pie"}, {}},
        {"-fuse-ld= dropped (link-only, prefix)", "foo.cpp", {"-fuse-ld=lld"}, {}},
        {"-resource-dir (separate) dropped, consumes value", "foo.cpp", {"-resource-dir", "/some/dir"}, {}},
        {"-resource-dir= (joined) dropped", "foo.cpp", {"-resource-dir=/some/dir"}, {}},
        {"--resource-dir= (joined) dropped", "foo.cpp", {"--resource-dir=/some/dir"}, {}},
        {"unrecognized flags preserved verbatim",
         "foo.cpp",
         {"-DFOO=1", "-Ifoo", "-isystem", "/sys", "-O3", "-std=c++20", "-march=native", "-fPIC", "-pipe", "-pthread",
          "-Wall"},
         {"-DFOO=1", "-Ifoo", "-isystem", "/sys", "-O3", "-std=c++20", "-march=native", "-fPIC", "-pipe", "-pthread",
          "-Wall"}},
    };
    runCases(cases);
}

TEST_CASE("arg_sanitizer: ArgSanitizer: nvcc-specific rules (triggered by argv0 containing nvcc)", "[argsanitizer][cuda]")
{
    // Every row here has an nvcc argv0 and/or a .cu file, which per the
    // design ALSO triggers step 4 CUDA synthesis (the same "isNvcc ||
    // .cu file" gate as step 3a). We force CudaMode::HostOnly so that
    // synthesis deterministically appends exactly {"-x", "c++"} after
    // whatever step 3 rewriting produced, independent of --generate-code
    // arch numbers or the nvcc-path-derived --cuda-path -- see the
    // dedicated, isolated CUDA synthesis coverage in test_cuda_support.cpp.
    const std::string nvcc = "/opt/cuda/bin/nvcc";
    const std::vector<std::string> hostOnlyTail = {"-x", "c++"};
    const std::vector<Case> cases = {
        {"-forward-unknown-to-host-compiler dropped", "foo.cu", {"-forward-unknown-to-host-compiler"}, hostOnlyTail, nvcc},
        {"--generate-code= (joined) dropped", "foo.cu", {R"(--generate-code=arch=compute_75,code=[sm_75])"}, hostOnlyTail, nvcc},
        {"-gencode arch=... (separate) dropped", "foo.cu", {"-gencode", "arch=compute_80,code=sm_80"}, hostOnlyTail, nvcc},
        {"-gencode= (joined) dropped", "foo.cu", {"-gencode=arch=compute_80,code=sm_80"}, hostOnlyTail, nvcc},
        {"--gpu-architecture= dropped", "foo.cu", {"--gpu-architecture=compute_75"}, hostOnlyTail, nvcc},
        {"-Xcompiler=<flag> unwraps to bare flag", "foo.cu", {"-Xcompiler=-fPIE"}, {"-fPIE", "-x", "c++"}, nvcc},
        {"-Xcompiler <flag> (separate) unwraps to bare flag",
         "foo.cu",
         {"-Xcompiler", "-pthread"},
         {"-pthread", "-x", "c++"},
         nvcc},
        {"-Xcudafe <arg> dropped (with value)", "foo.cu", {"-Xcudafe", "--diag_suppress=x"}, hostOnlyTail, nvcc},
        {"-Xptxas <arg> dropped (with value)", "foo.cu", {"-Xptxas", "-v"}, hostOnlyTail, nvcc},
        {"-Xfatbin <arg> dropped (with value)", "foo.cu", {"-Xfatbin", "-compress-all"}, hostOnlyTail, nvcc},
        {"-Xlinker <arg> dropped (with value)", "foo.cu", {"-Xlinker", "-rpath"}, hostOnlyTail, nvcc},
        {"-ccbin <arg> dropped (with value)", "foo.cu", {"-ccbin", "g++"}, hostOnlyTail, nvcc},
        {"--compiler-options <arg> dropped (with value)", "foo.cu", {"--compiler-options", "-fPIC"}, hostOnlyTail, nvcc},
        {"-rdc= dropped (prefix)", "foo.cu", {"-rdc=true"}, hostOnlyTail, nvcc},
        {"--relocatable-device-code= dropped (prefix)", "foo.cu", {"--relocatable-device-code=true"}, hostOnlyTail, nvcc},
        {"-lineinfo dropped", "foo.cu", {"-lineinfo"}, hostOnlyTail, nvcc},
        {"-G dropped", "foo.cu", {"-G"}, hostOnlyTail, nvcc},
        {"--default-stream <x> dropped (with value)", "foo.cu", {"--default-stream", "per-thread"}, hostOnlyTail, nvcc},
        {"--expt-relaxed-constexpr dropped", "foo.cu", {"--expt-relaxed-constexpr"}, hostOnlyTail, nvcc},
        {"--expt-extended-lambda dropped", "foo.cu", {"--expt-extended-lambda"}, hostOnlyTail, nvcc},
        {"--extended-lambda dropped", "foo.cu", {"--extended-lambda"}, hostOnlyTail, nvcc},
        {"-x cu translated to -x cuda", "foo.cu", {"-x", "cu"}, {"-x", "cuda", "-x", "c++"}, nvcc},
    };
    runCases(cases, symhow::index::cuda::CudaMode::HostOnly);
}

TEST_CASE("arg_sanitizer: ArgSanitizer: nvcc rules also trigger for a .cu file even with a non-nvcc argv0", "[argsanitizer][cuda]")
{
    // isNvcc || file ends in .cu -- exercise the "file ends in .cu" half.
    // A .cu file also triggers step 4 CUDA synthesis (see the comment in the
    // nvcc-specific-rules test above); force HostOnly for a deterministic,
    // easy-to-account-for {"-x", "c++"} tail.
    const Case c{
        "-forward-unknown-to-host-compiler dropped for .cu file regardless of argv0",
        "kernel.cu",
        {"-forward-unknown-to-host-compiler", "-DKEEP"},
        {"-DKEEP", "-x", "c++"},
        "/usr/bin/c++"};
    runCases({c}, symhow::index::cuda::CudaMode::HostOnly);
}

TEST_CASE("arg_sanitizer: ArgSanitizer: universal drop list still applies inside an nvcc invocation", "[argsanitizer][cuda]")
{
    const Case c{
        "-Werror dropped even though nvcc rules are active",
        "foo.cu",
        {"-Werror", "-DKEEP"},
        {"-DKEEP", "-x", "c++"},
        "/opt/cuda/bin/nvcc"};
    runCases({c}, symhow::index::cuda::CudaMode::HostOnly);
}

TEST_CASE("arg_sanitizer: ArgSanitizer: user --drop-arg glob patterns", "[argsanitizer]")
{
    SanitizerOptions options;
    options.resourceDir = "/opt/resource-dir";
    options.dropArgPatterns = {"-DSECRET*", "-W*"};

    const CompileEntry entry = makeEntry("foo.cpp", {"-DKEEP=1", "-DSECRET_TOKEN=abc", "-Wall", "-O2"});
    const auto result = sanitize(entry, options);
    CHECK(coreArgs(result.args) == std::vector<std::string>{"-DKEEP=1", "-O2"});
}

TEST_CASE("arg_sanitizer: ArgSanitizer: injections are present and --extra-arg values are appended last", "[argsanitizer][inject]")
{
    SanitizerOptions options;
    options.resourceDir = "/opt/resource-dir";
    options.gccToolchain = "/opt/toolchain";
    options.extraArgs = {"-DEXTRA1", "-DEXTRA2"};

    const CompileEntry entry = makeEntry("foo.cpp", {"-DKEEP=1"}, "/some/work/dir");
    const auto result = sanitize(entry, options);

    REQUIRE(result.args.size() == 10);
    const auto& args = result.args;

    // Kept token first, then the fixed injection block, in the documented order.
    std::size_t idx = 0;
    CHECK(args[idx++] == "-DKEEP=1");
    CHECK(args[idx++] == "-working-directory=/some/work/dir");
    CHECK(args[idx++] == "-resource-dir=/opt/resource-dir");
    CHECK(args[idx++] == "-fsyntax-only");
    CHECK(args[idx++] == "-w");
    CHECK(args[idx++] == "-ferror-limit=0");
    CHECK(args[idx++] == "-std=c++17");  // no -std= survived rewriting -> fallback injected
    CHECK(args[idx++] == "--gcc-toolchain=/opt/toolchain");
    CHECK(args[idx++] == "-DEXTRA1");
    CHECK(args[idx++] == "-DEXTRA2");
    CHECK(idx == args.size());  // extra-args are the very last tokens
}

TEST_CASE("arg_sanitizer: ArgSanitizer: -std fallback is NOT injected when a -std= token survived rewriting", "[argsanitizer][inject]")
{
    SanitizerOptions options;
    options.resourceDir = "/opt/resource-dir";

    const CompileEntry entry = makeEntry("foo.cpp", {"-std=c++20"});
    const auto result = sanitize(entry, options);

    const int stdCount = static_cast<int>(
        std::count_if(result.args.begin(), result.args.end(), [](const std::string& t) { return t.rfind("-std=", 0) == 0; }));
    CHECK(stdCount == 1);
    CHECK(std::find(result.args.begin(), result.args.end(), "-std=c++20") != result.args.end());
}

TEST_CASE("arg_sanitizer: ArgSanitizer: --std CLI value overrides the c++17 default fallback", "[argsanitizer][inject]")
{
    SanitizerOptions options;
    options.resourceDir = "/opt/resource-dir";
    options.stdFallback = "c++23";

    const CompileEntry entry = makeEntry("foo.cpp", {"-DKEEP"});
    const auto result = sanitize(entry, options);
    CHECK(std::find(result.args.begin(), result.args.end(), "-std=c++23") != result.args.end());
}

// --- Language-aware -std fallback (bug fix: a .c TU must never get a C++
// std injected, and vice versa) ---------------------------------------------

TEST_CASE("arg_sanitizer: ArgSanitizer: a .c file with no -std= gets the C fallback (gnu17), not c++17", "[argsanitizer][inject][c]")
{
    SanitizerOptions options;
    options.resourceDir = "/opt/resource-dir";

    const CompileEntry entry = makeEntry("foo.c", {"-DKEEP"}, "/work", "/usr/bin/cc");
    const auto result = sanitize(entry, options);
    CHECK(std::find(result.args.begin(), result.args.end(), "-std=gnu17") != result.args.end());
    CHECK(std::find(result.args.begin(), result.args.end(), "-std=c++17") == result.args.end());
}

TEST_CASE("arg_sanitizer: ArgSanitizer: a .c file with an explicit -std=c11 is left untouched", "[argsanitizer][inject][c]")
{
    SanitizerOptions options;
    options.resourceDir = "/opt/resource-dir";

    const CompileEntry entry = makeEntry("foo.c", {"-std=c11"}, "/work", "/usr/bin/cc");
    const auto result = sanitize(entry, options);
    const int stdCount = static_cast<int>(
        std::count_if(result.args.begin(), result.args.end(), [](const std::string& t) { return t.rfind("-std=", 0) == 0; }));
    CHECK(stdCount == 1);
    CHECK(std::find(result.args.begin(), result.args.end(), "-std=c11") != result.args.end());
}

TEST_CASE("arg_sanitizer: ArgSanitizer: a .cpp file with no -std= still gets the C++ fallback (c++17)", "[argsanitizer][inject]")
{
    SanitizerOptions options;
    options.resourceDir = "/opt/resource-dir";

    const CompileEntry entry = makeEntry("foo.cpp", {"-DKEEP"}, "/work", "/usr/bin/c++");
    const auto result = sanitize(entry, options);
    CHECK(std::find(result.args.begin(), result.args.end(), "-std=c++17") != result.args.end());
}

TEST_CASE("arg_sanitizer: ArgSanitizer: -x c on a .cpp-named file gets the C fallback, not C++", "[argsanitizer][inject][c]")
{
    SanitizerOptions options;
    options.resourceDir = "/opt/resource-dir";

    const CompileEntry entry = makeEntry("foo.cpp", {"-x", "c", "-DKEEP"}, "/work", "/usr/bin/c++");
    const auto result = sanitize(entry, options);
    CHECK(std::find(result.args.begin(), result.args.end(), "-std=gnu17") != result.args.end());
    CHECK(std::find(result.args.begin(), result.args.end(), "-std=c++17") == result.args.end());
}

TEST_CASE("arg_sanitizer: ArgSanitizer: --c-std CLI value overrides the gnu17 default C fallback", "[argsanitizer][inject][c]")
{
    SanitizerOptions options;
    options.resourceDir = "/opt/resource-dir";
    options.cStdFallback = "c99";

    const CompileEntry entry = makeEntry("foo.c", {"-DKEEP"}, "/work", "/usr/bin/cc");
    const auto result = sanitize(entry, options);
    CHECK(std::find(result.args.begin(), result.args.end(), "-std=c99") != result.args.end());
}

// --- Remap substring replacement (step 1) -----------------------------------
// Step 1 itself lives in symhow::index::CompileDb (applyRemaps / parseRemapArg),
// tested directly here per the Phase 2 test plan.

TEST_CASE("arg_sanitizer: applyRemaps: parseRemapArg splits OLD=NEW on the first '='", "[remap]")
{
    const auto [oldStr, newStr] = parseRemapArg("/mnt/workspace=/home/aaramoon/workspace");
    CHECK(oldStr == "/mnt/workspace");
    CHECK(newStr == "/home/aaramoon/workspace");

    CHECK_THROWS_AS(parseRemapArg("no-equals-sign"), std::invalid_argument);
}

TEST_CASE("arg_sanitizer: applyRemaps: literal substring replacement, not regex", "[remap]")
{
    const std::vector<std::pair<std::string, std::string>> remaps = {{"/mnt/workspace", "/home/aaramoon/workspace"}};
    CHECK(
        applyRemaps("/mnt/workspace/oss/cucs/include", remaps) == "/home/aaramoon/workspace/oss/cucs/include");
    // Applies inside flag values too (e.g. -I/mnt/workspace/... or after -isystem).
    CHECK(
        applyRemaps("-I/mnt/workspace/oss/cucs/include", remaps) == "-I/home/aaramoon/workspace/oss/cucs/include");
    CHECK(applyRemaps("no match here", remaps) == "no match here");
}

TEST_CASE("arg_sanitizer: applyRemaps: multiple remap pairs chain in order", "[remap]")
{
    const std::vector<std::pair<std::string, std::string>> remaps = {
        {"/mnt/workspace", "/home/aaramoon/workspace"}, {"/home/aaramoon/workspace/oss", "/srv/oss"}};
    CHECK(applyRemaps("/mnt/workspace/oss/cucs", remaps) == "/srv/oss/cucs");
}

TEST_CASE(
    "arg_sanitizer: Remap applies to directory, file, and every arg token via the full sanitize pipeline",
    "[remap][argsanitizer]")
{
    CompileEntry entry;
    entry.directory = "/mnt/workspace/oss/cucs/build/release";
    entry.file = "/mnt/workspace/oss/cucs/src/foo.cpp";
    entry.args = {"/mnt/workspace/oss/cucs/.pixi/envs/default/bin/x86_64-conda-linux-gnu-c++",
                  "-I/mnt/workspace/oss/cucs/include",
                  "-isystem",
                  "/mnt/workspace/oss/cucs/.pixi/envs/default/include"};

    const std::vector<std::pair<std::string, std::string>> remaps = {{"/mnt/workspace", "/home/aaramoon/workspace"}};
    entry.directory = applyRemaps(entry.directory, remaps);
    entry.file = applyRemaps(entry.file, remaps);
    for (auto& a : entry.args)
    {
        a = applyRemaps(a, remaps);
    }

    SanitizerOptions options;
    options.resourceDir = "/opt/resource-dir";
    const auto result = sanitize(entry, options);

    for (const auto& tok : result.args)
    {
        INFO("token: " << tok);
        CHECK(tok.find("/mnt/workspace") == std::string::npos);
    }
    CHECK(
        std::find(result.args.begin(), result.args.end(), "-I/home/aaramoon/workspace/oss/cucs/include") !=
        result.args.end());
}
