#!/usr/bin/env python3
"""symhow-notes-fake.py -- deterministic fake `symhow notes` markdown generator.

Phase 1 test-data generator for docs/NOTES_DESIGN.md (see the Phase 1 brief
in that doc's "Phases" section). Consumes a JSON array of symbols
(`{"source_path", "qualified_name", "signature", "body_hash"?, "commit"?}`,
paths relative to a repo root) and writes one
`<notes-dir>/<source_path>.md` file per distinct source path, one `##` block
per symbol (in input order), in EXACTLY the canonical format
`symhow::notes::renderBlock` (include/symhow/notes/Markdown.h,
src/notes/Markdown.cpp) produces -- the two must be kept in sync by hand;
see test_notes_markdown.cpp's "fake generator output round-trips" case,
which parses this script's own output with the real C++ parser.

No third-party dependencies (stdlib only), so it runs with a bare
`python3 scripts/symhow-notes-fake.py` (no venv/pip needed).
"""
import argparse
import os
import random
import sys

PLACEHOLDER = "_TODO_"

_LOREM_WORDS = (
    "pipeline node scope symbol graph edge cache walker token span buffer "
    "arena cursor visitor scheduler batch offset stride handle registry "
    "provenance snapshot delta cursor frame shard partition"
).split()


def _lorem_sentence(rng, min_words=6, max_words=14):
    n = rng.randint(min_words, max_words)
    words = [rng.choice(_LOREM_WORDS) for _ in range(n)]
    s = " ".join(words)
    return s[0].upper() + s[1:] + "."


def _lorem_items(rng):
    n = rng.randint(1, 3)
    return [_lorem_sentence(rng, 3, 8) for _ in range(n)]


def normalize_signature(sig):
    # Mirror symhow::notes::normalizeSignature: collapse whitespace runs to a
    # single space, trim, strip one trailing ';'.
    out = []
    prev_space = False
    for c in sig:
        if c.isspace():
            if out and not prev_space:
                out.append(" ")
                prev_space = True
        else:
            out.append(c)
            prev_space = False
    s = "".join(out).rstrip(" ")
    if s.endswith(";"):
        s = s[:-1].rstrip(" ")
    return s


def render_list_section(name, items):
    lines = ["#### " + name]
    if not items:
        lines.append("1. " + PLACEHOLDER)
    else:
        for i, item in enumerate(items, start=1):
            lines.append(f"{i}. {item}")
    return "\n".join(lines) + "\n"


def render_block(symbol, rng, fill_ratio, empty_ratio):
    heading = symbol["qualified_name"]
    signature = normalize_signature(symbol.get("signature", ""))
    body_hash = symbol.get("body_hash", "") or ""
    commit = symbol.get("commit", "") or ""

    filled = rng.random() < fill_ratio
    present = {
        name: rng.random() >= empty_ratio
        for name in ("what", "how", "signature", "pre", "post", "bugs", "suggestions")
    }

    out = ["## " + heading]

    if body_hash or commit:
        parts = []
        if body_hash:
            parts.append("symhow-hash: " + body_hash)
        if commit:
            parts.append("symhow-commit: " + commit)
        out.append("<!-- " + "  ".join(parts) + " -->")

    brief = _lorem_sentence(rng) if filled else PLACEHOLDER
    out.append(brief)

    if present["what"]:
        out.append("### what ?")
        out.append(_lorem_sentence(rng, 8, 20) if filled else PLACEHOLDER)
    if present["how"]:
        out.append("### how ?")
        out.append(_lorem_sentence(rng, 8, 20) if filled else PLACEHOLDER)
    if present["signature"]:
        out.append("### signature")
        out.append("```cpp")
        out.append(signature if signature else PLACEHOLDER)
        out.append("```")

    have_condition = present["pre"] or present["post"] or present["bugs"] or present["suggestions"]
    if have_condition:
        out.append("### condition")
        if present["pre"]:
            out.append(render_list_section("pre-condition", _lorem_items(rng) if filled else []).rstrip("\n"))
        if present["post"]:
            out.append(render_list_section("post-condition", _lorem_items(rng) if filled else []).rstrip("\n"))
        if present["bugs"]:
            out.append(render_list_section("bugs", _lorem_items(rng) if filled else []).rstrip("\n"))
        if present["suggestions"]:
            out.append(render_list_section("suggestions", _lorem_items(rng) if filled else []).rstrip("\n"))

    return "\n".join(out)


def render_file(source_path, blocks):
    return "# " + source_path + "\n\n" + "\n\n".join(blocks) + "\n"


def inject_overloads(symbols, rng, log):
    out = list(symbols)
    candidates = [s for s in symbols if s.get("signature")]
    n_extra = max(0, round(len(candidates) * 0.10))
    chosen = rng.sample(candidates, k=min(n_extra, len(candidates))) if candidates else []
    for s in chosen:
        twin = dict(s)
        # Simple, deterministic alternate-overload signature: swap an `int`
        # parameter for `double`, or append a distinguishing marker if the
        # signature has no bare `int` to swap.
        if "int" in s["signature"]:
            twin["signature"] = s["signature"].replace("int", "double", 1)
        else:
            twin["signature"] = s["signature"] + " /* overload */"
        twin.pop("body_hash", None)
        twin.pop("commit", None)
        out.append(twin)
        log.append(f"overload: {s['qualified_name']} -- injected twin signature '{twin['signature']}'")

    if symbols:
        ambiguous_base = rng.choice(symbols)
        pair_a = dict(ambiguous_base)
        pair_a["signature"] = ""
        pair_a.pop("body_hash", None)
        pair_a.pop("commit", None)
        pair_b = dict(pair_a)
        out.append(pair_a)
        out.append(pair_b)
        log.append(
            f"ambiguous pair: {ambiguous_base['qualified_name']} -- two entries with empty signature (not resolvable by lint alone)"
        )

    return out


def corrupt_and_drop_hashes(symbols, stale_ratio, drop_ratio, rng, log):
    """Phase 2: --stale-ratio replaces body_hash with a wrong (but well-
    formed) value for a deterministic fraction of hashed blocks, so `symhow
    notes sync` reports them stale; --drop-hash-ratio strips body_hash
    entirely from a (disjoint, independently-drawn-after) fraction of the
    REMAINING hashed blocks, so sync reports them unanchored instead. A
    block is never both (drop only ever considers blocks stale-corruption
    left alone), so the two counts partition cleanly for callers checking
    them (see test/unit/test_notes_sync.cpp)."""
    out = []
    stale_names = []
    dropped_names = []
    for s in symbols:
        s = dict(s)
        if s.get("body_hash") and stale_ratio > 0 and rng.random() < stale_ratio:
            original = s["body_hash"]
            wrong = f"{(int(original, 16) ^ 0xFFFFFFFFFFFFFFFF) & 0xFFFFFFFFFFFFFFFF:016x}"
            s["body_hash"] = wrong
            stale_names.append(s["qualified_name"])
        elif s.get("body_hash") and drop_ratio > 0 and rng.random() < drop_ratio:
            s.pop("body_hash", None)
            dropped_names.append(s["qualified_name"])
        out.append(s)
    for name in stale_names:
        log.append(f"stale: {name}")
    for name in dropped_names:
        log.append(f"drop-hash: {name}")
    return out


def fill_block(block_text, rng, fill_ratio):
    """Phase 3's `--fill-existing`: replaces PLACEHOLDER occurrences in one
    already-canonical (symhow::notes::renderBlock()-shaped) `## ...` block's
    brief/### what?/### how?/list-section items with deterministic lorem
    text, each independently at `fill_ratio`. Headings, the `### signature`
    fence's contents, and the `<!-- symhow-hash: ... -->` comment are copied
    through completely untouched -- this is what makes the result byte-for-
    byte identical to the input except for the placeholders it actually
    replaced. Returns (new_text, filled) -- `filled` mirrors
    symhow::notes::NoteBlock::filled() (true if brief/what/how/any list section
    ended up non-empty; the signature section deliberately doesn't count).
    """
    lines = block_text.split("\n")
    out = []
    i = 0
    filled = False

    out.append(lines[i])  # "## heading"
    i += 1
    if i < len(lines) and lines[i].startswith("<!--"):
        out.append(lines[i])
        i += 1
    if i < len(lines):
        brief = lines[i]
        if brief == PLACEHOLDER and rng.random() < fill_ratio:
            brief = _lorem_sentence(rng)
            filled = True
        out.append(brief)
        i += 1

    item_re = __import__("re").compile(r"^(\d+\. )(.*)$")
    while i < len(lines):
        line = lines[i]
        if line in ("### what ?", "### how ?"):
            out.append(line)
            i += 1
            if i < len(lines):
                text = lines[i]
                if text == PLACEHOLDER and rng.random() < fill_ratio:
                    text = _lorem_sentence(rng, 8, 20)
                    filled = True
                out.append(text)
                i += 1
        elif line == "### signature":
            # Copy the heading + fence open + signature text + fence close
            # verbatim -- scaffold's signature is never a placeholder to
            # begin with (it's the real, already-known signature string).
            for _ in range(4):
                if i < len(lines):
                    out.append(lines[i])
                    i += 1
        elif line in ("#### pre-condition", "#### post-condition", "#### bugs", "#### suggestions"):
            out.append(line)
            i += 1
            while i < len(lines) and not lines[i].startswith("#"):
                item_line = lines[i]
                m = item_re.match(item_line)
                if m and m.group(2) == PLACEHOLDER and rng.random() < fill_ratio:
                    item_line = m.group(1) + _lorem_sentence(rng, 3, 8)
                    filled = True
                out.append(item_line)
                i += 1
        else:
            out.append(line)
            i += 1

    return "\n".join(out), filled


def fill_existing_file(content, rng, fill_ratio):
    """Splits an already-canonical notes file (as symhow::notes::renderFile()
    produces) into its "# heading" preamble and `## ...` blocks (blocks
    never contain a blank line internally -- only the "\\n\\n" separators
    between them do -- so a plain split on "\\n\\n" round-trips exactly),
    fills each block via fill_block(), and rejoins with the exact same
    separators. Returns (new_content, blocks_filled)."""
    parts = content.split("\n\n")
    if len(parts) < 2:
        return content, 0
    heading_part = parts[0]
    block_parts = parts[1:]

    trailing = ""
    if block_parts and block_parts[-1].endswith("\n"):
        trailing = "\n"
        block_parts[-1] = block_parts[-1][:-1]

    blocks_filled = 0
    new_blocks = []
    for b in block_parts:
        new_b, filled = fill_block(b, rng, fill_ratio)
        if filled:
            blocks_filled += 1
        new_blocks.append(new_b)

    return heading_part + "\n\n" + "\n\n".join(new_blocks) + trailing, blocks_filled


def run_fill_existing(notes_dir, seed, fill_ratio):
    rng = random.Random(seed)
    md_files = []
    for root, _dirs, files in os.walk(notes_dir):
        for name in sorted(files):
            if name.endswith(".md"):
                md_files.append(os.path.join(root, name))
    md_files.sort()

    total_blocks_filled = 0
    for path in md_files:
        with open(path) as f:
            content = f.read()
        new_content, filled = fill_existing_file(content, rng, fill_ratio)
        total_blocks_filled += filled
        if new_content != content:
            with open(path, "w") as f:
                f.write(new_content)

    print(f"symhow-notes-fake: --fill-existing filled {total_blocks_filled} blocks across {len(md_files)} files", file=sys.stderr)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--symbols-json")
    ap.add_argument("--fill-existing", metavar="DIR", help="fill _TODO_ placeholders in an already-scaffolded notes tree in place")
    ap.add_argument("--notes-dir")
    ap.add_argument("--seed", type=int, required=True)
    ap.add_argument("--fill-ratio", type=float, default=0.6)
    ap.add_argument("--empty-ratio", type=float, default=0.1)
    ap.add_argument("--overloads", action="store_true")
    ap.add_argument("--stale-ratio", type=float, default=0.0,
                     help="fraction of hashed blocks to corrupt so `symhow notes sync` reports them stale")
    ap.add_argument("--drop-hash-ratio", type=float, default=0.0,
                     help="fraction of hashed blocks to strip body_hash from (unanchored)")
    args = ap.parse_args()

    if args.fill_existing:
        run_fill_existing(args.fill_existing, args.seed, args.fill_ratio)
        return

    if not args.symbols_json or not args.notes_dir:
        ap.error("--symbols-json and --notes-dir are required unless --fill-existing is given")

    with open(args.symbols_json) as f:
        import json

        loaded = json.load(f)
        # Phase 2: accept `symhow notes symbols --json`'s own envelope
        # ({"schema": "symhow.notes-symbols/1", "symbols": [...], ...}) directly,
        # as well as the original bare JSON array this script always took.
        symbols = loaded["symbols"] if isinstance(loaded, dict) else loaded

    rng = random.Random(args.seed)
    injected_log = []
    if args.overloads:
        symbols = inject_overloads(symbols, rng, injected_log)
    symbols = corrupt_and_drop_hashes(symbols, args.stale_ratio, args.drop_hash_ratio, rng, injected_log)

    by_path = {}
    for s in symbols:
        by_path.setdefault(s["source_path"], []).append(s)

    os.makedirs(args.notes_dir, exist_ok=True)
    total_blocks = 0
    for source_path, syms in by_path.items():
        blocks = [render_block(s, rng, args.fill_ratio, args.empty_ratio) for s in syms]
        total_blocks += len(blocks)
        out_path = os.path.join(args.notes_dir, source_path + ".md")
        os.makedirs(os.path.dirname(out_path), exist_ok=True)
        with open(out_path, "w") as f:
            f.write(render_file(source_path, blocks))

    for entry in injected_log:
        print(entry, file=sys.stderr)
    print(f"symhow-notes-fake: wrote {total_blocks} blocks across {len(by_path)} files", file=sys.stderr)


if __name__ == "__main__":
    main()
