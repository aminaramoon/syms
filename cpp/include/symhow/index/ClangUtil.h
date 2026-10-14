#pragma once

#include <cstdint>
#include <string>

#include <clang-c/Index.h>

// Small libclang RAII/utility helpers shared by TuParser/NameBuilder/
// AstWalker. libclang is leak-prone by design (almost everything that
// returns a CXString/CXIndex/CXTranslationUnit/CXCursor* array must be
// disposed explicitly); centralizing the disposal here makes it much easier
// to audit.
namespace symhow::index::clangutil
{

// Converts and disposes a CXString in one step.
inline std::string toString(CXString s)
{
    const char* c = clang_getCString(s);
    std::string result = c ? c : "";
    clang_disposeString(s);
    return result;
}

// Owns a CXIndex, disposing it on destruction. Move-only.
class ScopedIndex
{
public:
    ScopedIndex() = default;
    explicit ScopedIndex(CXIndex idx) : m_idx(idx) {}
    ~ScopedIndex() { reset(); }

    ScopedIndex(const ScopedIndex&) = delete;
    ScopedIndex& operator=(const ScopedIndex&) = delete;

    ScopedIndex(ScopedIndex&& other) noexcept : m_idx(other.m_idx) { other.m_idx = nullptr; }
    ScopedIndex& operator=(ScopedIndex&& other) noexcept
    {
        if (this != &other)
        {
            reset();
            m_idx = other.m_idx;
            other.m_idx = nullptr;
        }
        return *this;
    }

    CXIndex get() const { return m_idx; }

    void reset()
    {
        if (m_idx != nullptr)
        {
            clang_disposeIndex(m_idx);
            m_idx = nullptr;
        }
    }

private:
    CXIndex m_idx = nullptr;
};

// Owns a CXTranslationUnit, disposing it on destruction. Move-only.
class ScopedTU
{
public:
    ScopedTU() = default;
    explicit ScopedTU(CXTranslationUnit tu) : m_tu(tu) {}
    ~ScopedTU() { reset(); }

    ScopedTU(const ScopedTU&) = delete;
    ScopedTU& operator=(const ScopedTU&) = delete;

    ScopedTU(ScopedTU&& other) noexcept : m_tu(other.m_tu) { other.m_tu = nullptr; }
    ScopedTU& operator=(ScopedTU&& other) noexcept
    {
        if (this != &other)
        {
            reset();
            m_tu = other.m_tu;
            other.m_tu = nullptr;
        }
        return *this;
    }

    CXTranslationUnit get() const { return m_tu; }

    void reset()
    {
        if (m_tu != nullptr)
        {
            clang_disposeTranslationUnit(m_tu);
            m_tu = nullptr;
        }
    }

private:
    CXTranslationUnit m_tu = nullptr;
};

// Absolute-ish file path for a CXFile, exactly as libclang/THIS TU reports
// it (no realpath/normalization) -- i.e. the as-#include'd spelling, which
// can differ textually between two TUs that reach the same physical file
// via different include paths. Callers that embed this in an identity/
// disambiguation string that must be STABLE across TUs (anything in
// NameBuilder.cpp building a symbol's Name/SymbolKey) need to normalize it
// first -- see NameBuilder.cpp's normalizedFileName().
inline std::string fileName(CXFile file)
{
    if (file == nullptr)
    {
        return {};
    }
    return toString(clang_getFileName(file));
}

// FNV-1a 64-bit hash (well-known, public-domain algorithm).
inline uint64_t fnv1a64(const std::string& s)
{
    uint64_t h = 14695981039346656037ull;  // offset basis
    for (unsigned char c : s)
    {
        h ^= c;
        h *= 1099511628211ull;  // FNV prime
    }
    return h;
}

// Lower 8 hex digits (32 bits) of `v`, e.g. "3f9a12c7".
inline std::string hex8(uint64_t v)
{
    static const char* digits = "0123456789abcdef";
    std::string out(8, '0');
    auto low = static_cast<uint32_t>(v);
    for (int i = 7; i >= 0; --i)
    {
        out[static_cast<std::size_t>(i)] = digits[low & 0xFu];
        low >>= 4;
    }
    return out;
}

}  // namespace symhow::index::clangutil
