#pragma once

#include <string>
#include <vector>

#include "symhow/index/ClangUtil.h"

// Parses a single translation unit with libclang. Deliberately independent
// of symhow::model::FactSink: diagnostics are returned as plain DiagInfo
// values (a file *path*, not a FactSink LocalId) so TuParser doesn't need
// to know about FileRegistry/FactSink at all; AstWalker/IndexDriver turn
// them into FileFact + ErrorFact after the fact.
namespace symhow::index
{

struct DiagInfo
{
    std::string message;
    bool fatal = false;
    bool hasLocation = false;
    std::string file;
    unsigned line = 1;
    unsigned col = 1;
};

struct ParsedTU
{
    clangutil::ScopedIndex index;
    clangutil::ScopedTU tu;
    bool ok = false;  // true iff clang_parseTranslationUnit2 succeeded AND produced a non-null TU
    std::vector<DiagInfo> diagnostics;  // severity >= CXDiagnostic_Error only
    uint32_t diagErrors = 0;            // count of diagnostics with severity == Error
    uint32_t diagFatal = 0;             // count of diagnostics with severity == Fatal
};

// Parses one TU. `args` is the fully-sanitized argv EXCLUDING argv[0]
// (clang_parseTranslationUnit2, unlike the FullArgv variant, prepends its
// own synthetic "clang" argv[0] -- matches symhow::index::sanitize(), which
// never emits one). The source file path is expected to already be part of
// `args` (as injected by the compile database / sanitizer pipeline), not
// passed separately.
//
// "Failure" here means no usable TU at all (ec != CXError_Success or a
// null TU): libclang can and does return a perfectly usable, partial TU
// even in the presence of many diagnostic errors, and this function keeps
// that TU (ok=true) rather than discarding it -- callers (IndexDriver's
// CUDA rung retry loop, AstWalker's hasContent() check) decide what to do
// with a TU that parsed but produced diagnostics.
ParsedTU parseTU(const std::vector<std::string>& args);

}  // namespace symhow::index
