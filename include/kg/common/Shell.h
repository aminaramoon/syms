#pragma once

#include <string>
#include <vector>

// POSIX-ish shell word splitter.
//
// Used to tokenize compile_commands.json entries that only provide a
// `command` string (rather than an already-tokenized `arguments` array) --
// this is the form the real cucs compile database uses. Deliberately not a
// full POSIX shell grammar (no variable expansion, globbing, command
// substitution, `$'...'` etc.) -- just enough quoting/escaping support to
// correctly reconstruct compiler argv tokens:
//
//   - Unquoted runs of non-whitespace characters are one token each.
//   - Single quotes ('...') take everything literally (no escapes) until
//     the matching close quote.
//   - Double quotes ("...") take everything literally except that a
//     backslash followed by ", \, $, or ` escapes that character (matching
//     bash's double-quote escaping rules); other backslashes are kept
//     as-is.
//   - Outside of quotes, a backslash escapes the following character
//     (the backslash is dropped, the next character is taken literally,
//     even if it's whitespace).
//   - Quoted and unquoted segments can be concatenated within a single
//     token, e.g. foo"bar baz"qux is one token: foobar bazqux.
namespace kg::common::shell
{

// Splits `command` into argv-style tokens using the rules above.
std::vector<std::string> split(const std::string& command);

}  // namespace kg::common::shell
