#include "symhow/notes/Markdown.h"

#include <algorithm>
#include <cctype>
#include <regex>
#include <sstream>
#include <unordered_set>

namespace symhow::notes
{

namespace
{

std::string trim(std::string_view s)
{
    std::size_t b = 0;
    std::size_t e = s.size();
    while (b < e && std::isspace(static_cast<unsigned char>(s[b])))
    {
        ++b;
    }
    while (e > b && std::isspace(static_cast<unsigned char>(s[e - 1])))
    {
        --e;
    }
    return std::string(s.substr(b, e - b));
}

bool isBlank(const std::string& s)
{
    return trim(s).empty();
}

// Splits `content` into lines (no trailing newline kept per line). CRLF and
// lone-CR line endings are normalized to '\n' first.
std::vector<std::string> splitLines(std::string_view content)
{
    std::string normalized;
    normalized.reserve(content.size());
    for (std::size_t i = 0; i < content.size(); ++i)
    {
        const char c = content[i];
        if (c == '\r')
        {
            normalized.push_back('\n');
            if (i + 1 < content.size() && content[i + 1] == '\n')
            {
                ++i;
            }
        }
        else
        {
            normalized.push_back(c);
        }
    }
    std::vector<std::string> lines;
    std::stringstream ss(normalized);
    std::string line;
    while (std::getline(ss, line, '\n'))
    {
        lines.push_back(line);
    }
    return lines;
}

enum class FenceRole
{
    None,
    Open,
    Body,
    Close
};

// Pass A: fence-aware scan. Determines, for every line, whether it's inside
// a ``` / ~~~ fence (Open/Body/Close) so the heading scanner below can skip
// it entirely -- this is what makes `#include` inside a `### signature`
// fence safe. Also collects "unclosed-fence" problems.
std::vector<FenceRole> scanFences(const std::vector<std::string>& lines, std::vector<Problem>& problems)
{
    std::vector<FenceRole> roles(lines.size(), FenceRole::None);
    bool inFence = false;
    char fenceChar = '\0';
    std::size_t fenceLen = 0;
    int fenceOpenLine = -1;

    for (std::size_t i = 0; i < lines.size(); ++i)
    {
        const std::string t = trim(lines[i]);
        if (!inFence)
        {
            if (t.size() >= 3 && (t[0] == '`' || t[0] == '~'))
            {
                const char c = t[0];
                std::size_t runLen = 0;
                while (runLen < t.size() && t[runLen] == c)
                {
                    ++runLen;
                }
                if (runLen >= 3)
                {
                    inFence = true;
                    fenceChar = c;
                    fenceLen = runLen;
                    fenceOpenLine = static_cast<int>(i);
                    roles[i] = FenceRole::Open;
                    continue;
                }
            }
            roles[i] = FenceRole::None;
        }
        else
        {
            // Closing fence: a run of >= fenceLen of the same fence char and
            // nothing else (besides surrounding whitespace, already trimmed).
            bool isClose = !t.empty();
            std::size_t runLen = 0;
            while (isClose && runLen < t.size() && t[runLen] == fenceChar)
            {
                ++runLen;
            }
            isClose = isClose && runLen == t.size() && runLen >= fenceLen;
            if (isClose)
            {
                roles[i] = FenceRole::Close;
                inFence = false;
                fenceOpenLine = -1;
            }
            else
            {
                roles[i] = FenceRole::Body;
            }
        }
    }

    if (inFence && fenceOpenLine >= 0)
    {
        problems.push_back(Problem{fenceOpenLine + 1, "unclosed-fence", "fence opened here is never closed"});
    }
    return roles;
}

// Turns raw lines collected under a `####` list section into items:
// numbering markers stripped, continuation lines appended to the previous
// item, `_TODO_` items dropped.
std::vector<std::string> parseListItems(const std::vector<std::string>& raw)
{
    static const std::regex kItemRe(R"(^\s*(?:\d+[.)]|[-*+])\s+(.*)$)");
    std::vector<std::string> rawItems;
    bool haveItem = false;
    for (const std::string& line : raw)
    {
        if (isBlank(line))
        {
            continue;
        }
        std::smatch m;
        if (std::regex_match(line, m, kItemRe))
        {
            rawItems.push_back(trim(m[1].str()));
            haveItem = true;
        }
        else if (haveItem)
        {
            rawItems.back() += " " + trim(line);
        }
        else
        {
            rawItems.push_back(trim(line));
            haveItem = true;
        }
    }
    std::vector<std::string> items;
    items.reserve(rawItems.size());
    for (std::string& it : rawItems)
    {
        if (it != kPlaceholder)
        {
            items.push_back(std::move(it));
        }
    }
    return items;
}

std::string joinTrim(const std::vector<std::string>& raw)
{
    std::string joined;
    for (std::size_t i = 0; i < raw.size(); ++i)
    {
        if (i)
        {
            joined += "\n";
        }
        joined += raw[i];
    }
    std::string t = trim(joined);
    if (t == kPlaceholder)
    {
        return "";
    }
    return t;
}

enum class Section
{
    None,
    Brief,
    What,
    How,
    Signature,
    Condition,
    Pre,
    Post,
    Bugs,
    Suggestions,
    Discard
};

const std::regex& symhowCommentRe()
{
    static const std::regex re(R"(^\s*<!--.*\bsymhow-(?:hash|commit)\b.*-->\s*$)");
    return re;
}

void parseSymhowComment(const std::string& line, NoteBlock& block, int lineNo)
{
    static const std::regex kvRe(R"(([A-Za-z][A-Za-z0-9_-]*)\s*:\s*(\S+))");
    auto begin = std::sregex_iterator(line.begin(), line.end(), kvRe);
    auto end = std::sregex_iterator();
    for (auto it = begin; it != end; ++it)
    {
        const std::string key = (*it)[1].str();
        const std::string value = (*it)[2].str();
        if (key == "symhow-hash")
        {
            block.bodyHash = value;
        }
        else if (key == "symhow-commit")
        {
            block.commit = value;
        }
    }
    block.metaLine = lineNo;
}

}  // namespace

std::string normalizeSignature(std::string_view signature)
{
    std::string out;
    bool prevSpace = false;
    for (char c : signature)
    {
        if (std::isspace(static_cast<unsigned char>(c)))
        {
            if (!out.empty() && !prevSpace)
            {
                out.push_back(' ');
                prevSpace = true;
            }
        }
        else
        {
            out.push_back(c);
            prevSpace = false;
        }
    }
    while (!out.empty() && out.back() == ' ')
    {
        out.pop_back();
    }
    if (!out.empty() && out.back() == ';')
    {
        out.pop_back();
    }
    while (!out.empty() && out.back() == ' ')
    {
        out.pop_back();
    }
    return out;
}

NotesFile parseNotesFile(std::string_view content, std::string path, bool strict)
{
    NotesFile file;
    file.path = std::move(path);

    const std::vector<std::string> lines = splitLines(content);
    std::vector<FenceRole> fenceRoles = scanFences(lines, file.problems);

    static const std::regex headingRe(R"(^(#+)(.*)$)");

    bool sawFileHeading = false;
    int curBlockIdx = -1;
    Section section = Section::None;
    bool insideCondition = false;
    std::unordered_set<std::string> seenSections;  // per-block section-name dedupe key
    std::vector<std::pair<int, std::string>> buf;   // (0-based line idx, raw text) for the active section

    auto curBlock = [&]() -> NoteBlock& { return file.blocks[static_cast<std::size_t>(curBlockIdx)]; };

    auto flushSection = [&]() {
        if (curBlockIdx < 0)
        {
            buf.clear();
            return;
        }
        NoteBlock& b = curBlock();
        switch (section)
        {
            case Section::Brief:
            {
                std::vector<std::string> raw;
                for (auto& p : buf) raw.push_back(p.second);
                b.brief = joinTrim(raw);
                break;
            }
            case Section::What:
            {
                std::vector<std::string> raw;
                for (auto& p : buf) raw.push_back(p.second);
                b.what.text = joinTrim(raw);
                break;
            }
            case Section::How:
            {
                std::vector<std::string> raw;
                for (auto& p : buf) raw.push_back(p.second);
                b.how.text = joinTrim(raw);
                break;
            }
            case Section::Signature:
            {
                // Buffer holds every raw line between the heading and the
                // next heading, including the fence delimiters themselves
                // (fence-interior lines are never mistaken for headings, so
                // they always survive into this buffer intact).
                std::vector<std::string> bodyLines;
                bool foundFence = false;
                for (auto& p : buf)
                {
                    const FenceRole role = fenceRoles[static_cast<std::size_t>(p.first)];
                    if (role == FenceRole::Open)
                    {
                        foundFence = true;
                    }
                    else if (role == FenceRole::Body)
                    {
                        bodyLines.push_back(p.second);
                    }
                }
                if (!foundFence)
                {
                    file.problems.push_back(Problem{b.signature.line, "missing-signature-fence", "'### signature' has no ```cpp fence"});
                    b.signature.text = "";
                }
                else
                {
                    std::string joined;
                    for (std::size_t i = 0; i < bodyLines.size(); ++i)
                    {
                        if (i) joined += "\n";
                        joined += bodyLines[i];
                    }
                    b.signature.text = normalizeSignature(joined);
                    if (b.signature.text == kPlaceholder)
                    {
                        b.signature.text = "";
                    }
                }
                break;
            }
            case Section::Pre:
            {
                std::vector<std::string> raw;
                for (auto& p : buf) raw.push_back(p.second);
                b.pre.items = parseListItems(raw);
                break;
            }
            case Section::Post:
            {
                std::vector<std::string> raw;
                for (auto& p : buf) raw.push_back(p.second);
                b.post.items = parseListItems(raw);
                break;
            }
            case Section::Bugs:
            {
                std::vector<std::string> raw;
                for (auto& p : buf) raw.push_back(p.second);
                b.bugs.items = parseListItems(raw);
                break;
            }
            case Section::Suggestions:
            {
                std::vector<std::string> raw;
                for (auto& p : buf) raw.push_back(p.second);
                b.suggestions.items = parseListItems(raw);
                break;
            }
            case Section::Condition:
            case Section::None:
            case Section::Discard:
                break;
        }
        buf.clear();
    };

    for (std::size_t i = 0; i < lines.size(); ++i)
    {
        const int lineNo = static_cast<int>(i) + 1;
        const std::string& line = lines[i];

        if (fenceRoles[i] != FenceRole::None)
        {
            // Inside (or delimiting) a fence: never a heading. Belongs to
            // whatever section is active (only meaningful for Signature,
            // but harmless to collect elsewhere too).
            if (curBlockIdx < 0)
            {
                if (!isBlank(line))
                {
                    file.problems.push_back(Problem{lineNo, "text-outside-block", "text before the first '##' block"});
                }
            }
            else
            {
                buf.emplace_back(static_cast<int>(i), line);
            }
            continue;
        }

        std::smatch m;
        if (std::regex_match(line, m, headingRe))
        {
            const std::string hashes = m[1].str();
            const std::string rest = m[2].str();
            const std::size_t hashCount = hashes.size();

            if (hashCount >= 5)
            {
                file.problems.push_back(Problem{lineNo, "stray-heading-level", "heading level too deep ('" + hashes + "')"});
                continue;
            }
            if (rest.empty() || rest[0] != ' ')
            {
                // "##NoSpace" etc -- not a real heading, treat as content.
                if (curBlockIdx < 0)
                {
                    if (!isBlank(line))
                    {
                        file.problems.push_back(Problem{lineNo, "text-outside-block", "text before the first '##' block"});
                    }
                }
                else
                {
                    buf.emplace_back(static_cast<int>(i), line);
                }
                continue;
            }

            const std::string headingText = trim(rest);

            if (hashCount == 1)
            {
                flushSection();
                section = Section::None;
                insideCondition = false;
                if (!sawFileHeading)
                {
                    file.fileHeading = headingText;
                    sawFileHeading = true;
                }
                else
                {
                    file.problems.push_back(Problem{lineNo, "stray-heading-level", "a second top-level '#' heading"});
                }
                continue;
            }

            if (hashCount == 2)
            {
                flushSection();
                if (headingText.empty())
                {
                    file.problems.push_back(Problem{lineNo, "empty-heading", "'##' heading has no text"});
                }
                NoteBlock block;
                block.heading = headingText;
                block.headingLine = lineNo;
                file.blocks.push_back(std::move(block));
                curBlockIdx = static_cast<int>(file.blocks.size()) - 1;
                seenSections.clear();
                section = Section::Brief;
                insideCondition = false;
                continue;
            }

            if (hashCount == 3)
            {
                flushSection();
                if (curBlockIdx < 0)
                {
                    // Stray section heading before any block -- fold into
                    // text-outside-block noise rather than inventing a new
                    // problem kind for an even rarer corner case.
                    file.problems.push_back(Problem{lineNo, "text-outside-block", "'###' heading before the first '##' block"});
                    section = Section::Discard;
                    insideCondition = false;
                    continue;
                }
                NoteBlock& b = curBlock();
                if (headingText == "what ?")
                {
                    if (seenSections.count("what")) { file.problems.push_back(Problem{lineNo, "duplicate-section", "duplicate '### what ?'"}); section = Section::Discard; }
                    else { seenSections.insert("what"); b.what.present = true; b.what.line = lineNo; section = Section::What; }
                    insideCondition = false;
                }
                else if (headingText == "how ?")
                {
                    if (seenSections.count("how")) { file.problems.push_back(Problem{lineNo, "duplicate-section", "duplicate '### how ?'"}); section = Section::Discard; }
                    else { seenSections.insert("how"); b.how.present = true; b.how.line = lineNo; section = Section::How; }
                    insideCondition = false;
                }
                else if (headingText == "signature")
                {
                    if (seenSections.count("signature")) { file.problems.push_back(Problem{lineNo, "duplicate-section", "duplicate '### signature'"}); section = Section::Discard; }
                    else { seenSections.insert("signature"); b.signature.present = true; b.signature.line = lineNo; section = Section::Signature; }
                    insideCondition = false;
                }
                else if (headingText == "condition")
                {
                    if (seenSections.count("condition")) { file.problems.push_back(Problem{lineNo, "duplicate-section", "duplicate '### condition'"}); section = Section::Discard; insideCondition = false; }
                    else { seenSections.insert("condition"); section = Section::Condition; insideCondition = true; }
                }
                else
                {
                    file.problems.push_back(Problem{lineNo, "unknown-heading", "unknown '### " + headingText + "'"});
                    section = Section::Discard;
                    insideCondition = false;
                }
                continue;
            }

            // hashCount == 4
            flushSection();
            if (curBlockIdx < 0 || !insideCondition)
            {
                file.problems.push_back(Problem{lineNo, "unknown-heading", "'#### " + headingText + "' outside '### condition'"});
                section = Section::Discard;
                continue;
            }
            NoteBlock& b = curBlock();
            if (headingText == "pre-condition")
            {
                if (seenSections.count("pre-condition")) { file.problems.push_back(Problem{lineNo, "duplicate-section", "duplicate '#### pre-condition'"}); section = Section::Discard; }
                else { seenSections.insert("pre-condition"); b.pre.present = true; b.pre.line = lineNo; section = Section::Pre; }
            }
            else if (headingText == "post-condition")
            {
                if (seenSections.count("post-condition")) { file.problems.push_back(Problem{lineNo, "duplicate-section", "duplicate '#### post-condition'"}); section = Section::Discard; }
                else { seenSections.insert("post-condition"); b.post.present = true; b.post.line = lineNo; section = Section::Post; }
            }
            else if (headingText == "bugs")
            {
                if (seenSections.count("bugs")) { file.problems.push_back(Problem{lineNo, "duplicate-section", "duplicate '#### bugs'"}); section = Section::Discard; }
                else { seenSections.insert("bugs"); b.bugs.present = true; b.bugs.line = lineNo; section = Section::Bugs; }
            }
            else if (headingText == "suggestions")
            {
                if (seenSections.count("suggestions")) { file.problems.push_back(Problem{lineNo, "duplicate-section", "duplicate '#### suggestions'"}); section = Section::Discard; }
                else { seenSections.insert("suggestions"); b.suggestions.present = true; b.suggestions.line = lineNo; section = Section::Suggestions; }
            }
            else
            {
                file.problems.push_back(Problem{lineNo, "unknown-heading", "unknown '#### " + headingText + "'"});
                section = Section::Discard;
            }
            continue;
        }

        // Not a heading line.
        if (std::regex_match(line, symhowCommentRe()))
        {
            const bool briefEmptySoFar = std::all_of(buf.begin(), buf.end(), [](const auto& p) { return isBlank(p.second); });
            if (curBlockIdx >= 0 && section == Section::Brief && briefEmptySoFar && curBlock().metaLine == 0)
            {
                parseSymhowComment(line, curBlock(), lineNo);
            }
            else
            {
                file.problems.push_back(Problem{lineNo, "misplaced-meta", "symhow-hash/symhow-commit comment must immediately follow the '##' heading"});
            }
            continue;
        }

        if (curBlockIdx < 0)
        {
            if (!isBlank(line))
            {
                file.problems.push_back(Problem{lineNo, "text-outside-block", "text before the first '##' block"});
            }
            continue;
        }
        buf.emplace_back(static_cast<int>(i), line);
    }
    flushSection();

    // duplicate-block: same heading AND same non-empty normalized
    // signature. Two blocks that share a heading but BOTH have an empty
    // (missing/placeholder) signature are "ambiguous", not "duplicate" --
    // lint has no DB, so it can't tell whether they're accidental repeats
    // or legitimately distinct overloads someone forgot to disambiguate;
    // that call is left to Phase 2's DB-aware `sync`.
    {
        std::unordered_set<std::string> seenBlocks;
        for (const NoteBlock& b : file.blocks)
        {
            if (b.signature.text.empty())
            {
                continue;
            }
            const std::string key = b.heading + "\x1f" + b.signature.text;
            if (!seenBlocks.insert(key).second)
            {
                file.problems.push_back(Problem{b.headingLine, "duplicate-block", "duplicate block '" + b.heading + "' with the same signature"});
            }
        }
    }

    if (strict)
    {
        for (const NoteBlock& b : file.blocks)
        {
            if (!b.what.present) file.problems.push_back(Problem{b.headingLine, "missing-section", "missing '### what ?'"});
            if (!b.how.present) file.problems.push_back(Problem{b.headingLine, "missing-section", "missing '### how ?'"});
            if (!b.signature.present) file.problems.push_back(Problem{b.headingLine, "missing-section", "missing '### signature'"});
            if (!b.pre.present) file.problems.push_back(Problem{b.headingLine, "missing-section", "missing '#### pre-condition'"});
            if (!b.post.present) file.problems.push_back(Problem{b.headingLine, "missing-section", "missing '#### post-condition'"});
            if (!b.bugs.present) file.problems.push_back(Problem{b.headingLine, "missing-section", "missing '#### bugs'"});
            if (!b.suggestions.present) file.problems.push_back(Problem{b.headingLine, "missing-section", "missing '#### suggestions'"});
        }
    }

    std::stable_sort(file.problems.begin(), file.problems.end(), [](const Problem& a, const Problem& b) { return a.line < b.line; });

    return file;
}

namespace
{

std::string renderListSection(const std::string& name, const ListSection& section)
{
    std::string out = "#### " + name + "\n";
    if (section.items.empty())
    {
        out += "1. " + std::string(kPlaceholder) + "\n";
    }
    else
    {
        for (std::size_t i = 0; i < section.items.size(); ++i)
        {
            out += std::to_string(i + 1) + ". " + section.items[i] + "\n";
        }
    }
    return out;
}

}  // namespace

std::string renderBlock(const NoteBlock& block)
{
    std::string s;
    s += "## " + block.heading + "\n";
    if (!block.bodyHash.empty() || !block.commit.empty())
    {
        std::vector<std::string> parts;
        if (!block.bodyHash.empty())
        {
            parts.push_back("symhow-hash: " + block.bodyHash);
        }
        if (!block.commit.empty())
        {
            parts.push_back("symhow-commit: " + block.commit);
        }
        s += "<!-- ";
        for (std::size_t i = 0; i < parts.size(); ++i)
        {
            if (i)
            {
                s += "  ";
            }
            s += parts[i];
        }
        s += " -->\n";
    }
    s += (block.brief.empty() ? std::string(kPlaceholder) : block.brief) + "\n";
    // Sections that were never present in the source (Phase 1's
    // --empty-ratio-style "missing-section" case, checked only under
    // `lint --strict`) are omitted here too, so parseNotesFile(renderFile(f))
    // == f holds even for files with genuinely absent sections, not just
    // fully-populated ones -- see test_notes_markdown.cpp's
    // "fake generator output ... round-trips" case.
    if (block.what.present)
    {
        s += "### what ?\n";
        s += (block.what.text.empty() ? std::string(kPlaceholder) : block.what.text) + "\n";
    }
    if (block.how.present)
    {
        s += "### how ?\n";
        s += (block.how.text.empty() ? std::string(kPlaceholder) : block.how.text) + "\n";
    }
    if (block.signature.present)
    {
        s += "### signature\n```cpp\n";
        s += (block.signature.text.empty() ? std::string(kPlaceholder) : block.signature.text) + "\n";
        s += "```\n";
    }
    if (block.pre.present || block.post.present || block.bugs.present || block.suggestions.present)
    {
        s += "### condition\n";
        if (block.pre.present) s += renderListSection("pre-condition", block.pre);
        if (block.post.present) s += renderListSection("post-condition", block.post);
        if (block.bugs.present) s += renderListSection("bugs", block.bugs);
        if (block.suggestions.present) s += renderListSection("suggestions", block.suggestions);
    }
    if (!s.empty() && s.back() == '\n')
    {
        s.pop_back();
    }
    return s;
}

std::string renderFile(const NotesFile& file)
{
    std::string s = "# " + file.fileHeading + "\n\n";
    for (std::size_t i = 0; i < file.blocks.size(); ++i)
    {
        if (i)
        {
            s += "\n\n";
        }
        s += renderBlock(file.blocks[i]);
    }
    s += "\n";
    return s;
}

std::string placeholderBlockMarkdown(const std::string& qualifiedName, const std::string& signature, const std::string& bodyHash, const std::string& commit)
{
    NoteBlock b;
    b.heading = qualifiedName;
    b.bodyHash = bodyHash;
    b.commit = commit;
    b.what.present = true;
    b.how.present = true;
    b.signature.present = true;
    b.signature.text = normalizeSignature(signature);
    b.pre.present = true;
    b.post.present = true;
    b.bugs.present = true;
    b.suggestions.present = true;
    return renderBlock(b);
}

}  // namespace symhow::notes
