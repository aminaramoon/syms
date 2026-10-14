#include <catch2/catch_test_macros.hpp>

#include <algorithm>
#include <filesystem>
#include <fstream>
#include <set>
#include <sstream>
#include <string>

#include <nlohmann/json.hpp>

#include "symhow/common/Process.h"
#include "symhow/notes/Markdown.h"
#include "symhow/notes/NoteModel.h"

using namespace symhow::notes;

namespace
{

bool hasProblem(const std::vector<Problem>& problems, const std::string& kind, int line)
{
    return std::any_of(problems.begin(), problems.end(), [&](const Problem& p) { return p.kind == kind && p.line == line; });
}

int countProblems(const std::vector<Problem>& problems, const std::string& kind)
{
    return static_cast<int>(std::count_if(problems.begin(), problems.end(), [&](const Problem& p) { return p.kind == kind; }));
}

std::string readFile(const std::string& path)
{
    std::ifstream in(path, std::ios::binary);
    std::ostringstream ss;
    ss << in.rdbuf();
    return ss.str();
}

}  // namespace

// -----------------------------------------------------------------------
// Golden example (docs/NOTES_DESIGN.md's "Markdown format" section).
// -----------------------------------------------------------------------

static const char* kGolden =
    "# src/planner/query.cpp\n"
    "\n"
    "## sirius::planner::query::get_pipeline\n"
    "One-line brief. (free text until the first `###`)\n"
    "### what ?\n"
    "_TODO_\n"
    "### how ?\n"
    "_TODO_\n"
    "### signature\n"
    "```cpp\n"
    "sirius::pipeline::sirius_pipeline *get_pipeline(op::sirius_physical_operator *)\n"
    "```\n"
    "### condition\n"
    "#### pre-condition\n"
    "1. _TODO_\n"
    "#### post-condition\n"
    "1. _TODO_\n"
    "#### bugs\n"
    "1. _TODO_\n"
    "#### suggestions\n"
    "1. _TODO_\n";

TEST_CASE("parseNotesFile: golden example from NOTES_DESIGN.md", "[notes]")
{
    NotesFile f = parseNotesFile(kGolden, "src/planner/query.cpp.md");
    CHECK(f.problems.empty());
    REQUIRE(f.fileHeading == "src/planner/query.cpp");
    REQUIRE(f.blocks.size() == 1);

    const NoteBlock& b = f.blocks[0];
    CHECK(b.heading == "sirius::planner::query::get_pipeline");
    CHECK(b.headingLine == 3);
    CHECK(b.brief == "One-line brief. (free text until the first `###`)");
    CHECK(b.what.present);
    CHECK(b.what.text.empty());
    CHECK(b.how.present);
    CHECK(b.how.text.empty());
    CHECK(b.signature.present);
    CHECK(b.signature.text == "sirius::pipeline::sirius_pipeline *get_pipeline(op::sirius_physical_operator *)");
    CHECK(b.pre.present);
    CHECK(b.pre.items.empty());
    CHECK(b.post.present);
    CHECK(b.post.items.empty());
    CHECK(b.bugs.present);
    CHECK(b.bugs.items.empty());
    CHECK(b.suggestions.present);
    CHECK(b.suggestions.items.empty());
    CHECK(b.filled());  // brief is non-empty
    CHECK(b.bodyHash.empty());
    CHECK(b.commit.empty());
    CHECK(b.metaLine == 0);
}

TEST_CASE("render/parse fixed point over the golden example", "[notes]")
{
    NotesFile f = parseNotesFile(kGolden, "p.md");
    const std::string rendered = renderFile(f);
    NotesFile f2 = parseNotesFile(rendered, "p.md");
    CHECK(f2.problems.empty());
    REQUIRE(f2.blocks.size() == f.blocks.size());
    CHECK(f2.fileHeading == f.fileHeading);
    CHECK(f2.blocks[0].heading == f.blocks[0].heading);
    CHECK(f2.blocks[0].brief == f.blocks[0].brief);
    CHECK(f2.blocks[0].what.text == f.blocks[0].what.text);
    CHECK(f2.blocks[0].how.text == f.blocks[0].how.text);
    CHECK(f2.blocks[0].signature.text == f.blocks[0].signature.text);
    CHECK(f2.blocks[0].pre.items == f.blocks[0].pre.items);
    CHECK(f2.blocks[0].post.items == f.blocks[0].post.items);
    CHECK(f2.blocks[0].bugs.items == f.blocks[0].bugs.items);
    CHECK(f2.blocks[0].suggestions.items == f.blocks[0].suggestions.items);

    // Idempotence: re-rendering f2 reproduces exactly `rendered`.
    CHECK(renderFile(f2) == rendered);
}

// -----------------------------------------------------------------------
// Overloads
// -----------------------------------------------------------------------

TEST_CASE("overloads: two blocks, same heading, different signatures", "[notes]")
{
    const std::string content =
        "# f.cpp\n"
        "\n"
        "## ns::foo\n"
        "brief a\n"
        "### signature\n"
        "```cpp\n"
        "int foo(int)\n"
        "```\n"
        "\n"
        "## ns::foo\n"
        "brief b\n"
        "### signature\n"
        "```cpp\n"
        "int foo(double)\n"
        "```\n";
    NotesFile f = parseNotesFile(content, "f.md");
    CHECK(f.problems.empty());
    REQUIRE(f.blocks.size() == 2);
    CHECK(f.blocks[0].heading == "ns::foo");
    CHECK(f.blocks[1].heading == "ns::foo");
    CHECK(f.blocks[0].signature.text == "int foo(int)");
    CHECK(f.blocks[1].signature.text == "int foo(double)");
    CHECK(countProblems(f.problems, "duplicate-block") == 0);
}

TEST_CASE("#include inside a signature fence is not treated as a heading", "[notes]")
{
    const std::string content =
        "# f.cpp\n"
        "\n"
        "## ns::foo\n"
        "brief\n"
        "### signature\n"
        "```cpp\n"
        "#include <vector>\n"
        "int foo()\n"
        "```\n";
    NotesFile f = parseNotesFile(content, "f.md");
    CHECK(f.problems.empty());
    REQUIRE(f.blocks.size() == 1);
    CHECK(f.blocks[0].signature.text == "#include <vector> int foo()");
}

TEST_CASE("CRLF input parses identically to LF", "[notes]")
{
    std::string crlf = kGolden;
    std::string out;
    for (char c : crlf)
    {
        if (c == '\n')
        {
            out += "\r\n";
        }
        else
        {
            out += c;
        }
    }
    NotesFile f = parseNotesFile(out, "p.md");
    NotesFile fLf = parseNotesFile(kGolden, "p.md");
    CHECK(f.problems.empty());
    REQUIRE(f.blocks.size() == fLf.blocks.size());
    CHECK(f.blocks[0].heading == fLf.blocks[0].heading);
    CHECK(f.blocks[0].brief == fLf.blocks[0].brief);
    CHECK(f.blocks[0].signature.text == fLf.blocks[0].signature.text);
}

// -----------------------------------------------------------------------
// _TODO_ handling / list continuation
// -----------------------------------------------------------------------

TEST_CASE("_TODO_-only block is unfilled; _TODO_ list items are dropped", "[notes]")
{
    const std::string content =
        "# f.cpp\n"
        "\n"
        "## ns::foo\n"
        "_TODO_\n"
        "### what ?\n"
        "_TODO_\n"
        "### condition\n"
        "#### pre-condition\n"
        "1. _TODO_\n"
        "2. real item\n"
        "3. _TODO_\n";
    NotesFile f = parseNotesFile(content, "f.md");
    REQUIRE(f.blocks.size() == 1);
    const NoteBlock& b = f.blocks[0];
    CHECK(b.brief.empty());
    CHECK(b.what.text.empty());
    REQUIRE(b.pre.items.size() == 1);
    CHECK(b.pre.items[0] == "real item");
    CHECK(b.filled());  // the surviving "real item" makes the block filled
}

TEST_CASE("list continuation lines are appended to the previous item", "[notes]")
{
    const std::string content =
        "# f.cpp\n"
        "\n"
        "## ns::foo\n"
        "brief\n"
        "### condition\n"
        "#### bugs\n"
        "1. first line\n"
        "   continues here\n"
        "2. second item\n";
    NotesFile f = parseNotesFile(content, "f.md");
    REQUIRE(f.blocks.size() == 1);
    REQUIRE(f.blocks[0].bugs.items.size() == 2);
    CHECK(f.blocks[0].bugs.items[0] == "first line continues here");
    CHECK(f.blocks[0].bugs.items[1] == "second item");
}

// -----------------------------------------------------------------------
// Every Problem kind
// -----------------------------------------------------------------------

TEST_CASE("problem: unknown-heading (bad ### name)", "[notes]")
{
    const std::string c = "# f.cpp\n\n## ns::foo\nbrief\n### bogus\ntext\n";
    NotesFile f = parseNotesFile(c, "f.md");
    CHECK(hasProblem(f.problems, "unknown-heading", 5));
}

TEST_CASE("problem: unknown-heading (#### outside ### condition)", "[notes]")
{
    const std::string c = "# f.cpp\n\n## ns::foo\nbrief\n### what ?\nx\n#### pre-condition\n1. y\n";
    NotesFile f = parseNotesFile(c, "f.md");
    CHECK(hasProblem(f.problems, "unknown-heading", 7));
}

TEST_CASE("problem: duplicate-section", "[notes]")
{
    const std::string c = "# f.cpp\n\n## ns::foo\nbrief\n### what ?\none\n### what ?\ntwo\n";
    NotesFile f = parseNotesFile(c, "f.md");
    CHECK(hasProblem(f.problems, "duplicate-section", 7));
    REQUIRE(f.blocks.size() == 1);
    CHECK(f.blocks[0].what.text == "one");  // first occurrence wins
}

TEST_CASE("problem: duplicate-block (same heading, same signature)", "[notes]")
{
    const std::string c =
        "# f.cpp\n"
        "\n"
        "## ns::foo\n"
        "brief\n"
        "### signature\n"
        "```cpp\n"
        "int foo()\n"
        "```\n"
        "\n"
        "## ns::foo\n"
        "brief2\n"
        "### signature\n"
        "```cpp\n"
        "int foo()\n"
        "```\n";
    NotesFile f = parseNotesFile(c, "f.md");
    CHECK(hasProblem(f.problems, "duplicate-block", 10));
}

TEST_CASE("no duplicate-block when both signatures are empty (ambiguous, not duplicate)", "[notes]")
{
    const std::string c = "# f.cpp\n\n## ns::foo\nbrief\n\n## ns::foo\nbrief2\n";
    NotesFile f = parseNotesFile(c, "f.md");
    CHECK(countProblems(f.problems, "duplicate-block") == 0);
}

TEST_CASE("problem: unclosed-fence", "[notes]")
{
    const std::string c = "# f.cpp\n\n## ns::foo\nbrief\n### signature\n```cpp\nint foo()\n";
    NotesFile f = parseNotesFile(c, "f.md");
    CHECK(hasProblem(f.problems, "unclosed-fence", 6));
}

TEST_CASE("problem: missing-signature-fence", "[notes]")
{
    const std::string c = "# f.cpp\n\n## ns::foo\nbrief\n### signature\n_TODO_\n";
    NotesFile f = parseNotesFile(c, "f.md");
    CHECK(hasProblem(f.problems, "missing-signature-fence", 5));
}

TEST_CASE("problem: empty-heading", "[notes]")
{
    std::string c = "# f.cpp\n\n";
    c += "##";
    c += " \n";  // "## " with a trailing space and nothing else
    c += "brief\n";
    NotesFile f = parseNotesFile(c, "f.md");
    CHECK(hasProblem(f.problems, "empty-heading", 3));
    REQUIRE(f.blocks.size() == 1);
    CHECK(f.blocks[0].heading.empty());
}

TEST_CASE("problem: text-outside-block", "[notes]")
{
    const std::string c = "stray text\n# f.cpp\n\n## ns::foo\nbrief\n";
    NotesFile f = parseNotesFile(c, "f.md");
    CHECK(hasProblem(f.problems, "text-outside-block", 1));
}

TEST_CASE("problem: missing-section only reported in strict mode", "[notes]")
{
    const std::string c = "# f.cpp\n\n## ns::foo\nbrief\n";
    NotesFile lax = parseNotesFile(c, "f.md", false);
    CHECK(countProblems(lax.problems, "missing-section") == 0);

    NotesFile strict = parseNotesFile(c, "f.md", true);
    CHECK(countProblems(strict.problems, "missing-section") == 7);
    CHECK(hasProblem(strict.problems, "missing-section", 3));
}

TEST_CASE("problem: stray-heading-level (##### and a second #)", "[notes]")
{
    const std::string c = "# f.cpp\n# again\n\n## ns::foo\nbrief\n##### deep\n";
    NotesFile f = parseNotesFile(c, "f.md");
    CHECK(hasProblem(f.problems, "stray-heading-level", 2));
    CHECK(hasProblem(f.problems, "stray-heading-level", 6));
}

TEST_CASE("problem: misplaced-meta", "[notes]")
{
    const std::string c = "# f.cpp\n\n## ns::foo\nbrief text here\n<!-- symhow-hash: abc123 -->\n";
    NotesFile f = parseNotesFile(c, "f.md");
    CHECK(hasProblem(f.problems, "misplaced-meta", 5));
    REQUIRE(f.blocks.size() == 1);
    CHECK(f.blocks[0].bodyHash.empty());
}

// -----------------------------------------------------------------------
// symhow-hash / symhow-commit staleness anchor comment
// -----------------------------------------------------------------------

TEST_CASE("meta comment immediately after heading is parsed into bodyHash/commit", "[notes]")
{
    const std::string c = "# f.cpp\n\n## ns::foo\n<!-- symhow-hash: abc123  symhow-commit: deadbeef -->\nbrief text\n";
    NotesFile f = parseNotesFile(c, "f.md");
    CHECK(f.problems.empty());
    REQUIRE(f.blocks.size() == 1);
    CHECK(f.blocks[0].bodyHash == "abc123");
    CHECK(f.blocks[0].commit == "deadbeef");
    CHECK(f.blocks[0].metaLine == 4);
    CHECK(f.blocks[0].brief == "brief text");
}

TEST_CASE("meta comment absent leaves bodyHash/commit empty", "[notes]")
{
    const std::string c = "# f.cpp\n\n## ns::foo\nbrief text\n";
    NotesFile f = parseNotesFile(c, "f.md");
    REQUIRE(f.blocks.size() == 1);
    CHECK(f.blocks[0].bodyHash.empty());
    CHECK(f.blocks[0].commit.empty());
    CHECK(f.blocks[0].metaLine == 0);
}

TEST_CASE("meta comment round-trips through render/parse", "[notes]")
{
    NoteBlock b;
    b.heading = "ns::foo";
    b.bodyHash = "0123456789abcdef";
    b.commit = "cafefeed";
    b.signature.text = "int foo()";
    const std::string rendered = renderBlock(b);
    CHECK(rendered.substr(0, rendered.find('\n')) == "## ns::foo");
    NotesFile f = parseNotesFile(rendered, "f.md");
    CHECK(f.problems.empty());
    REQUIRE(f.blocks.size() == 1);
    CHECK(f.blocks[0].bodyHash == b.bodyHash);
    CHECK(f.blocks[0].commit == b.commit);
    CHECK(f.blocks[0].metaLine == 2);
}

// -----------------------------------------------------------------------
// normalizeSignature
// -----------------------------------------------------------------------

TEST_CASE("normalizeSignature", "[notes]")
{
    CHECK(normalizeSignature("foo()") == "foo()");
    CHECK(normalizeSignature("foo();") == "foo()");
    CHECK(normalizeSignature("  int   f( int x ) ;  ") == "int f( int x )");
    CHECK(normalizeSignature("a\n b\tc") == "a b c");
    CHECK(normalizeSignature("") == "");
    CHECK(normalizeSignature(";") == "");
}

// -----------------------------------------------------------------------
// placeholderBlockMarkdown
// -----------------------------------------------------------------------

TEST_CASE("placeholderBlockMarkdown parses to an unfilled block with the given heading/signature", "[notes]")
{
    const std::string block = placeholderBlockMarkdown("ns::Foo::bar", "int bar ( int ) const ;");
    NotesFile f = parseNotesFile(block, "f.md");
    CHECK(f.problems.empty());
    REQUIRE(f.blocks.size() == 1);
    CHECK(f.blocks[0].heading == "ns::Foo::bar");
    CHECK_FALSE(f.blocks[0].filled());
    CHECK(f.blocks[0].signature.text == normalizeSignature("int bar ( int ) const ;"));
    CHECK(f.blocks[0].bodyHash.empty());
}

TEST_CASE("placeholderBlockMarkdown with bodyHash/commit emits and round-trips the meta comment", "[notes]")
{
    const std::string block = placeholderBlockMarkdown("ns::Foo::bar", "int bar()", "deadbeef01234567", "abc123");
    NotesFile f = parseNotesFile(block, "f.md");
    CHECK(f.problems.empty());
    REQUIRE(f.blocks.size() == 1);
    CHECK(f.blocks[0].bodyHash == "deadbeef01234567");
    CHECK(f.blocks[0].commit == "abc123");
}

// -----------------------------------------------------------------------
// Fake generator: run it, then lint the result (Phase 1 verification).
// -----------------------------------------------------------------------

TEST_CASE("fake generator output lints as expected and round-trips", "[notes][notes_fake]")
{
    const std::string sourceDir = SYMHOW_TEST_SOURCE_DIR;
    const std::string scriptPath = sourceDir + "/../scripts/symhow-notes-fake.py";
    const std::string fixturePath = sourceDir + "/notes/symbols.fixture.json";
    const std::string outDir = std::string(SYMHOW_TEST_BINARY_DIR) + "/notes_fake_out";

    std::filesystem::remove_all(outDir);
    std::filesystem::create_directories(outDir);

    symhow::common::process::Result gen = symhow::common::process::run(
        {"python3", scriptPath, "--symbols-json", fixturePath, "--notes-dir", outDir, "--seed", "7", "--fill-ratio", "0.6", "--empty-ratio", "0.1",
         "--overloads"});
    if (gen.exitCode == -1)
    {
        WARN("python3 not available -- skipping fake generator test");
        return;
    }
    REQUIRE(gen.exitCode == 0);

    // Without --strict: no lint problems (the injected ambiguous
    // empty-signature pair is not something lint -- which has no DB -- can
    // detect; it's a sync-time/DB-aware concern).
    symhow::common::process::Result lintPlain = symhow::common::process::run({SYMHOW_BINARY, "notes", "lint", "--notes-dir", outDir});
    CHECK(lintPlain.exitCode == 0);

    // With --strict: --empty-ratio guarantees some sections are entirely
    // absent, so missing-section problems are reported.
    symhow::common::process::Result lintStrict = symhow::common::process::run({SYMHOW_BINARY, "notes", "lint", "--notes-dir", outDir, "--strict", "--json"});
    CHECK(lintStrict.exitCode == 3);

    nlohmann::json j = nlohmann::json::parse(lintStrict.out);
    CHECK(j["schema"] == "symhow.notes-lint/1");
    const std::size_t reportedBlocks = j["blocks"].get<std::size_t>();

    // Cross-check block count and fixed point (parse -> render -> byte
    // identical) directly against the files on disk.
    std::size_t actualBlocks = 0;
    for (const std::filesystem::directory_entry& e : std::filesystem::recursive_directory_iterator(outDir))
    {
        if (!e.is_regular_file() || e.path().extension() != ".md")
        {
            continue;
        }
        const std::string content = readFile(e.path().string());
        NotesFile parsed = parseNotesFile(content, e.path().string());
        actualBlocks += parsed.blocks.size();

        const std::string rerendered = renderFile(parsed);
        CHECK(rerendered == content);
    }
    CHECK(actualBlocks == reportedBlocks);
    CHECK(actualBlocks >= 12);  // 12 fixture symbols + injected overloads/ambiguous pair
}
