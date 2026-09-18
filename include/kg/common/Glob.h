#pragma once

#include <string>

// Small glob matcher shared across kg (compile-database --include/--exclude
// filtering in Phase 2; reused by later phases for other path-matching
// needs).
//
// Supported syntax:
//   *      any run of characters (possibly empty), EXCLUDING '/'
//   **     any run of characters (possibly empty), INCLUDING '/'
//   ?      exactly one character (any character, including '/')
//   [...]  a character class; a leading '!' or '^' negates it, and 'a-z'
//          style ranges are supported (e.g. [a-zA-Z0-9_])
//
// Everything else matches itself literally. There is no escape character;
// to match a literal '*', '?', or '[' there is currently no way to do so
// (not needed for kg's own use cases -- path/flag globs).
namespace kg::common::glob
{

// Returns true if `path` matches `pattern` in full (the whole string, not a
// substring/prefix match).
bool match(const std::string& pattern, const std::string& path);

}  // namespace kg::common::glob
