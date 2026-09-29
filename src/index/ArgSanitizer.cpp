#include "kg/index/ArgSanitizer.h"

#include <algorithm>
#include <filesystem>
#include <regex>
#include <unordered_set>

#include "kg/common/Glob.h"

#ifndef KG_DEFAULT_RESOURCE_DIR
#define KG_DEFAULT_RESOURCE_DIR ""
#endif

namespace kg::index
{

namespace
{

bool startsWith(const std::string& s, const std::string& prefix)
{
    return s.size() >= prefix.size() && s.compare(0, prefix.size(), prefix) == 0;
}

bool endsWith(const std::string& s, const std::string& suffix)
{
    return s.size() >= suffix.size() && s.compare(s.size() - suffix.size(), suffix.size(), suffix) == 0;
}

std::string basenameOf(const std::string& path)
{
    return std::filesystem::path(path).filename().string();
}

// --- Language detection for the step-5 -std fallback ------------------------

enum class TuLang
{
    Cxx,
    C,
    Cuda,
    ObjC,     // .m/.mm or -x objective-c[++]: no -std fallback is ever injected
    Unknown,  // couldn't tell from -x or extension; argv0 is consulted as a tie-breaker
};

// Maps a clang `-x <lang>` value to a TuLang bucket; Unknown for anything
// this sanitizer doesn't specifically special-case (e.g. asm, fortran).
TuLang langFromDashX(const std::string& lang)
{
    if (lang == "c" || lang == "c-header" || lang == "cpp-output")
    {
        return TuLang::C;
    }
    if (lang == "c++" || lang == "c++-header" || lang == "c++-cpp-output")
    {
        return TuLang::Cxx;
    }
    if (lang == "cuda")
    {
        return TuLang::Cuda;
    }
    if (lang == "objective-c" || lang == "objective-c++" || lang == "objective-c-header" || lang == "objective-c++-header")
    {
        return TuLang::ObjC;
    }
    return TuLang::Unknown;
}

// Maps `file`'s extension to a TuLang bucket; Unknown if unrecognized.
TuLang langFromExtension(const std::string& file)
{
    if (endsWith(file, ".c"))
    {
        return TuLang::C;
    }
    if (endsWith(file, ".cu"))
    {
        return TuLang::Cuda;
    }
    if (endsWith(file, ".cpp") || endsWith(file, ".cc") || endsWith(file, ".cxx") || endsWith(file, ".C") ||
        endsWith(file, ".c++"))
    {
        return TuLang::Cxx;
    }
    if (endsWith(file, ".m") || endsWith(file, ".mm"))
    {
        return TuLang::ObjC;
    }
    return TuLang::Unknown;
}

// Determines the TU's language for the step-5 -std fallback: an explicit
// `-x <lang>` surviving the rewrite pass (step 3) wins outright; otherwise
// `entry.file`'s extension; otherwise (only when the extension itself is
// unrecognized) argv0's basename is consulted as a tie-breaker -- a bare
// "cc"/"gcc"/"clang"-style name (no "++", not nvcc) hints C, anything else
// falls back to C++ (matching this function's pre-existing default).
TuLang detectTuLang(const std::vector<std::string>& out, const std::string& file, const std::string& argv0Basename)
{
    for (std::size_t k = 0; k + 1 < out.size(); ++k)
    {
        if (out[k] == "-x")
        {
            const TuLang fromX = langFromDashX(out[k + 1]);
            if (fromX != TuLang::Unknown)
            {
                return fromX;
            }
        }
    }

    const TuLang fromExt = langFromExtension(file);
    if (fromExt != TuLang::Unknown)
    {
        return fromExt;
    }

    // Ambiguous/unknown extension: use argv0 as a tie-breaker only.
    const bool argv0HintsC =
        argv0Basename.find("++") == std::string::npos && argv0Basename.find("nvcc") == std::string::npos;
    return argv0HintsC ? TuLang::C : TuLang::Cxx;
}

// nvcc lives at <toolkit>/bin/nvcc, so the toolkit root is two directories up.
std::string deriveCudaPathFromNvcc(const std::string& nvccPath)
{
    if (nvccPath.empty())
    {
        return {};
    }
    std::filesystem::path p(nvccPath);
    return p.parent_path().parent_path().string();
}

// Scans `text` for compute_N / sm_N occurrences (N may have a trailing
// letter suffix like the 'a' in compute_90a or the 'f' in compute_100f;
// only the leading digit run is parsed) and updates `maxArch` with the
// numerically highest N found, if any.
void updateMaxArch(int& maxArch, const std::string& text)
{
    static const std::regex re(R"((?:compute|sm)_([0-9]+))");
    auto begin = std::sregex_iterator(text.begin(), text.end(), re);
    auto end = std::sregex_iterator();
    for (auto it = begin; it != end; ++it)
    {
        const int n = std::stoi((*it)[1].str());
        if (n > maxArch)
        {
            maxArch = n;
        }
    }
}

// --- Step 3b: universal drop list ------------------------------------------
//
// Returns the number of tokens (starting at args[i]) to drop, or 0 if this
// token isn't matched by the universal drop list at all (caller then tries
// step 3c).
std::size_t universalDropCount(const std::vector<std::string>& args, std::size_t i)
{
    const std::string& tok = args[i];
    const bool hasNext = (i + 1 < args.size());

    if (tok == "-c")
    {
        return 1;
    }
    if (tok == "-o")
    {
        return hasNext ? 2 : 1;
    }
    if (startsWith(tok, "-o") && tok.size() > 2)
    {
        return 1;  // -o<x> attached form
    }

    if (tok == "-MD" || tok == "-MMD" || tok == "-MP" || tok == "-MG" || tok == "-M" || tok == "-MM")
    {
        return 1;
    }
    if (tok == "-MF" || tok == "-MT" || tok == "-MQ")
    {
        return hasNext ? 2 : 1;
    }

    if (tok == "-fmodules-ts")
    {
        return 1;
    }
    if (startsWith(tok, "-fmodule-mapper="))
    {
        return 1;
    }
    if (startsWith(tok, "-fdeps-format=") || startsWith(tok, "-fdeps-file=") || startsWith(tok, "-fdeps-target="))
    {
        return 1;
    }

    if (tok == "-Werror" || startsWith(tok, "-Werror="))
    {
        return 1;
    }
    if (tok == "-pedantic-errors")
    {
        return 1;
    }

    if (startsWith(tok, "-fdiagnostics-color"))
    {
        return 1;
    }

    // Link-only flags.
    if (startsWith(tok, "-l") || startsWith(tok, "-L"))
    {
        return 1;
    }
    if (startsWith(tok, "-Wl,"))
    {
        return 1;
    }
    if (tok == "-shared" || tok == "-rdynamic" || tok == "-pie")
    {
        return 1;
    }
    if (startsWith(tok, "-fuse-ld="))
    {
        return 1;
    }

    // We always inject our own -resource-dir in step 5, so any CDB-supplied
    // one is always dropped. clang treats "-resource-dir <dir>" (separate)
    // and "-resource-dir=<dir>" / "--resource-dir=<dir>" (joined) as
    // equivalent spellings; only the separate form needs to consume a
    // following token.
    if (tok == "-resource-dir" || tok == "--resource-dir")
    {
        return hasNext ? 2 : 1;
    }
    if (startsWith(tok, "-resource-dir") || startsWith(tok, "--resource-dir"))
    {
        return 1;
    }

    return 0;
}

// --- Step 3a: nvcc-specific rules -------------------------------------------
//
// Attempts to handle args[i] (and possibly args[i+1]) as an nvcc-specific
// rule. On a match, appends any translated output tokens to `out`, advances
// `i` past the consumed tokens, updates `maxArch` as needed, and returns
// true. Returns false (leaving `i` untouched) if no nvcc rule matches, so
// the caller can fall through to steps 3b/3c.
bool tryNvccRule(const std::vector<std::string>& args, std::size_t& i, std::vector<std::string>& out, int& maxArch)
{
    static const std::unordered_set<std::string> dropWithFollowingArg = {
        "-Xcudafe", "-Xptxas", "-Xfatbin", "-Xlinker", "-ccbin", "--compiler-options"};

    const std::string& tok = args[i];
    const bool hasNext = (i + 1 < args.size());

    if (tok == "-forward-unknown-to-host-compiler")
    {
        i += 1;
        return true;
    }

    // --generate-code / -gencode, separate-argument form.
    if (tok == "-gencode" || tok == "--generate-code")
    {
        if (hasNext)
        {
            updateMaxArch(maxArch, args[i + 1]);
            i += 2;
        }
        else
        {
            i += 1;
        }
        return true;
    }
    // --generate-code=... / -gencode=... , joined form.
    if (startsWith(tok, "-gencode=") || startsWith(tok, "--generate-code="))
    {
        updateMaxArch(maxArch, tok);
        i += 1;
        return true;
    }
    // --gpu-architecture=compute_N, joined form only.
    if (startsWith(tok, "--gpu-architecture="))
    {
        updateMaxArch(maxArch, tok);
        i += 1;
        return true;
    }

    // -Xcompiler=<flag> (joined) unwraps to bare <flag>.
    if (startsWith(tok, "-Xcompiler="))
    {
        out.push_back(tok.substr(std::string("-Xcompiler=").size()));
        i += 1;
        return true;
    }
    // -Xcompiler <flag> (separate, two-token) unwraps to bare <flag>.
    if (tok == "-Xcompiler")
    {
        if (hasNext)
        {
            out.push_back(args[i + 1]);
            i += 2;
        }
        else
        {
            i += 1;
        }
        return true;
    }

    if (dropWithFollowingArg.count(tok) != 0)
    {
        i += hasNext ? 2 : 1;
        return true;
    }

    if (startsWith(tok, "-rdc=") || startsWith(tok, "--relocatable-device-code="))
    {
        i += 1;
        return true;
    }
    if (tok == "-lineinfo" || tok == "-G")
    {
        i += 1;
        return true;
    }
    if (tok == "--default-stream")
    {
        i += hasNext ? 2 : 1;
        return true;
    }
    if (tok == "--expt-relaxed-constexpr" || tok == "--expt-extended-lambda" || tok == "--extended-lambda")
    {
        i += 1;
        return true;
    }

    // -x cu -> -x cuda (two-token form).
    if (tok == "-x" && hasNext && args[i + 1] == "cu")
    {
        out.push_back("-x");
        out.push_back("cuda");
        i += 2;
        return true;
    }

    return false;
}

struct RewriteResult
{
    std::vector<std::string> args;
    int maxArch = -1;  // -1 = "none seen"; caller applies the 90 default.
};

// Step 3: single left-to-right rewrite pass. `nvccRulesApply` gates step 3a;
// `dropArgPatterns` are the user --drop-arg glob patterns (step 3c).
RewriteResult rewrite(
    const std::vector<std::string>& args, bool nvccRulesApply, const std::vector<std::string>& dropArgPatterns)
{
    RewriteResult result;

    std::size_t i = 0;
    while (i < args.size())
    {
        if (nvccRulesApply && tryNvccRule(args, i, result.args, result.maxArch))
        {
            continue;
        }

        const std::size_t dropCount = universalDropCount(args, i);
        if (dropCount > 0)
        {
            i += dropCount;
            continue;
        }

        bool userDropped = false;
        for (const auto& pattern : dropArgPatterns)
        {
            if (kg::common::glob::match(pattern, args[i]))
            {
                userDropped = true;
                break;
            }
        }
        if (userDropped)
        {
            i += 1;
            continue;
        }

        result.args.push_back(args[i]);
        i += 1;
    }

    return result;
}

}  // namespace

SanitizeResult sanitize(const CompileEntry& entry, const SanitizerOptions& options)
{
    SanitizeResult result;

    // Step 2: split argv0.
    std::vector<std::string> remaining = entry.args;
    std::string argv0;
    if (!remaining.empty())
    {
        argv0 = remaining.front();
        remaining.erase(remaining.begin());
    }
    result.basename = basenameOf(argv0);
    result.isNvcc = result.basename.find("nvcc") != std::string::npos;
    result.isHostCxx = !result.isNvcc && result.basename.find("++") != std::string::npos;

    const bool isCuFile = endsWith(entry.file, ".cu");
    const bool nvccRulesApply = result.isNvcc || isCuFile;

    // Step 3: rewrite pass.
    RewriteResult rewritten = rewrite(remaining, nvccRulesApply, options.dropArgPatterns);
    std::vector<std::string> out = std::move(rewritten.args);

    // Step 4: CUDA synthesis (only for .cu files or an nvcc invocation).
    const bool doCudaSynthesis = isCuFile || result.isNvcc;
    if (doCudaSynthesis)
    {
        cuda::CudaSynthesisInput cudaInput;
        cudaInput.maxArch = (rewritten.maxArch >= 0) ? rewritten.maxArch : 90;
        cudaInput.cudaPath = options.cudaPath;
        if (cudaInput.cudaPath.empty() && result.isNvcc)
        {
            cudaInput.cudaPath = deriveCudaPathFromNvcc(argv0);
        }

        const cuda::CudaMode resolvedMode = cuda::resolveAutoMode(options.cudaMode);
        result.cudaModeUsed = resolvedMode;

        if (resolvedMode == cuda::CudaMode::Skip)
        {
            result.skipped = true;
            return result;  // no argv at all for a skipped file
        }

        const std::vector<std::string> cudaFlags = cuda::synthesize(resolvedMode, cudaInput);
        out.insert(out.end(), cudaFlags.begin(), cudaFlags.end());
    }

    // Step 5: injections.
    out.push_back("-working-directory=" + entry.directory);

    std::string resourceDir = options.resourceDir;
    if (resourceDir.empty())
    {
        resourceDir = KG_DEFAULT_RESOURCE_DIR;
    }
    out.push_back("-resource-dir=" + resourceDir);

    out.push_back("-fsyntax-only");
    out.push_back("-w");
    out.push_back("-ferror-limit=0");

    const bool hasStd =
        std::any_of(out.begin(), out.end(), [](const std::string& tok) { return startsWith(tok, "-std="); });
    if (!hasStd)
    {
        const TuLang tuLang = detectTuLang(out, entry.file, result.basename);
        if (tuLang == TuLang::C)
        {
            out.push_back("-std=" + options.cStdFallback);
        }
        else if (tuLang != TuLang::ObjC)
        {
            // C++ and CUDA both use the C++/CUDA fallback (detectTuLang()
            // never returns Unknown -- it ties C/C++ by argv0). ObjC/ObjC++
            // gets no fallback at all.
            out.push_back("-std=" + options.stdFallback);
        }
    }

    if (!options.gccToolchain.empty())
    {
        out.push_back("--gcc-toolchain=" + options.gccToolchain);
    }

    for (const auto& extra : options.extraArgs)
    {
        out.push_back(extra);
    }

    result.args = std::move(out);
    return result;
}

}  // namespace kg::index
