#include "kg/index/TuParser.h"

#include <vector>

namespace kg::index
{

namespace
{

void collectDiagnostics(CXTranslationUnit tu, ParsedTU& result)
{
    const std::string mainFile = clangutil::toString(clang_getTranslationUnitSpelling(tu));

    const unsigned numDiags = clang_getNumDiagnostics(tu);
    for (unsigned i = 0; i < numDiags; ++i)
    {
        CXDiagnostic diag = clang_getDiagnostic(tu, i);
        const CXDiagnosticSeverity severity = clang_getDiagnosticSeverity(diag);
        if (severity < CXDiagnostic_Error)
        {
            clang_disposeDiagnostic(diag);
            continue;
        }

        DiagInfo info;
        info.fatal = (severity == CXDiagnostic_Fatal);
        info.message = clangutil::toString(clang_formatDiagnostic(diag, clang_defaultDiagnosticDisplayOptions()));

        CXSourceLocation loc = clang_getDiagnosticLocation(diag);
        CXFile file = nullptr;
        unsigned line = 0, col = 0, offset = 0;
        clang_getExpansionLocation(loc, &file, &line, &col, &offset);

        if (file != nullptr)
        {
            info.hasLocation = true;
            info.file = clangutil::fileName(file);
            info.line = line;
            info.col = col;
        }
        else
        {
            info.hasLocation = true;
            info.file = mainFile;
            info.line = 1;
            info.col = 1;
        }

        if (severity == CXDiagnostic_Fatal)
        {
            ++result.diagFatal;
        }
        else
        {
            ++result.diagErrors;
        }

        result.diagnostics.push_back(std::move(info));
        clang_disposeDiagnostic(diag);
    }
}

}  // namespace

ParsedTU parseTU(const std::vector<std::string>& args)
{
    ParsedTU result;

    result.index = clangutil::ScopedIndex(clang_createIndex(/*excludeDeclarationsFromPCH=*/0, /*displayDiagnostics=*/0));

    std::vector<const char*> argv;
    argv.reserve(args.size());
    for (const auto& a : args)
    {
        argv.push_back(a.c_str());
    }

    const unsigned opts = CXTranslationUnit_DetailedPreprocessingRecord | CXTranslationUnit_KeepGoing |
                           CXTranslationUnit_IgnoreNonErrorsFromIncludedFiles;

    CXTranslationUnit tu = nullptr;
    const CXErrorCode ec = clang_parseTranslationUnit2(
        result.index.get(),
        /*source_filename=*/nullptr,
        argv.data(),
        static_cast<int>(argv.size()),
        /*unsaved_files=*/nullptr,
        0,
        opts,
        &tu);

    result.tu = clangutil::ScopedTU(tu);
    result.ok = (ec == CXError_Success) && (tu != nullptr);

    if (tu != nullptr)
    {
        collectDiagnostics(tu, result);
    }

    return result;
}

}  // namespace kg::index
