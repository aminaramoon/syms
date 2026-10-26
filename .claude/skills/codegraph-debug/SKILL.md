---
name: codegraph-debug
description: Debug C/C++/CUDA code (any repo that has a symhow/symwhere index, built on demand) by walking the symhow call/reference graph and symwhere outlines from a symptom to its cause — an error/log message, a stack trace or crash line, a wrong or corrupted field value, "which implementation actually ran", "how did execution get here", a regression after a diff, or a function that seemingly is never called. Covers bridging the graph's blind spots (virtual dispatch, class-template overrides, function-pointer ops tables, lambdas/callbacks, #if-disabled code, files outside the build). Use with codegraph-explore for setup and general navigation.
---

# codegraph-debug — from symptom to cause with the code graph

Setup is the same as **codegraph-explore** (read its SKILL.md first if you haven't this session):

```bash
X=scripts/symx   # relative to THIS skill's directory; or put scripts/ on $PATH
H=symhow         # symhow CLI (Rust build has `graph`); set $SYMHOW if not on $PATH
W=symwhere       # symwhere CLI; set $SYMWHERE if not on $PATH
cd <repo>; $X status                 # STALE index => line numbers for changed files are off
```

If no index exists for the repo yet, or the binaries aren't built, see **codegraph-explore** "Setup" and "Creating the indexes" first. `symx` finds DBs under `$SYMX_DBS` (default `~/explorer`); override with `$SYMX_DBS` or `--repo NAME|PATH`.

The graph is a **static** picture of **one build configuration**. Debugging means following edges until you hit a dead end, then recognising which kind of dead end it is and bridging it (see the table at the end). Each `symx` dead end prints its own next step.

## 1. Error / log message → every path that produces it

```bash
grep -rn 'connect timed out' --include=*.cpp --include=*.h .   # 1. where the text is built
$X at folly/io/async/AsyncSocket.cpp:3959-3962                  # 2. enclosing fn + what those lines call (exact overloads)
$X tree '#203446' --callers --depth 4                           # 3. who reaches that function
```

Real result: the callers stop at `WriteTimeout::timeoutExpired <no static callers; overrides AsyncTimeout::timeoutExpired>` and `connectTimeout <overrides IoUringConnectCallback::connectTimeout>`. Follow the hint, or let `reach` do the bridging:

```bash
$X reach 'folly::AsyncTimeout::libeventCallback' 'folly::AsyncSocket::failConnect'
#  libeventCallback --calls--> AsyncTimeout::timeoutExpired ==virtual==> WriteTimeout::timeoutExpired
#  --calls--> AsyncSocket::timeoutExpired --calls--> failConnect      (each hop with its call-site line)
```

## 2. Stack trace / crash line → verified frames

For each frame `file:line` (top first): run `$X at file:line`.
- The **enclosing** entry should be the frame's function. If it isn't, the binary and the index disagree (stale index, different build, or inlining).
- The line's **CALL** references should include the next frame down. If the next frame is not a direct callee, the hop was dynamic: a virtual (`$X overriders <callee>`), a callback, or a fn-pointer (`$X dispatch`).
- "note: indexed file but nothing semantic here" means that line is in an **inactive `#if` branch** for this build config. The crashing binary was built differently; read the code via the symwhere view it prints.

## 3. "Which implementation actually runs?"

```bash
$X overriders 'folly::Executor::add'          # 22 overriders, transitive (raw overridden-by gives 19)
$X overriders 'folly::BlockingQueue::add'     # class template: 0 OVERRIDE edges -> fallback lists 7 implementations with spans
$H query references 'folly::UnboundedBlockingQueue' -d DB --json   # TYPE_USAGE sites = where the concrete type is chosen
```

The concrete type is decided where the object is **constructed**. Follow the TYPE_USAGE / constructor references back to the factory. In folly that was `CPUThreadPoolExecutor::makeLifoSemQueue` / `makeThrottledLifoSemQueue`.

## 4. Wrong / corrupted state → who writes it

```bash
$X writes 'folly::IOBufQueue::chainLength_'        # WRITE / WRITE? (address taken, exchange, swap) per site, grouped
$X tree '#<writer id>' --callers --depth 3         # how each writer is reached
$H graph slice -d DB --criterion 'ns::Cls::field_' --json   # every function that can (transitively) touch it
```

`writes` classifies each reference token from the source text (symhow itself can't tell reads from writes). `WRITE?` means a possible write through a pointer or reference. Writes through aliases elsewhere (`auto& x = obj.field_; x = …`) show up only as the `&` site.

## 5. "How did we get here?" / "can A reach B?"

```bash
$X reach 'facebook::velox::exec::Driver::runInternal' 'facebook::velox::exec::HashProbe::addInput'
#  runInternal --calls--> (anon)::addInput @Driver.cpp:603 --calls--> Operator::addInput ==virtual==> HashProbe::addInput
$X chain 'facebook::velox::SelectivityVector::countSelected' --entry 'facebook::velox::exec::Driver::runInternal'
#  waypoints EVERY static path must pass: RowVector::estimateFlatSize -> … LazyVector::loadVectorInternal -> VectorLoader::load
```

- `reach` = one cheapest path. It follows CALL edges, recorded overrides (`==virtual==>`), and a class-template heuristic (`~~virtual?~~>`, which you should verify). `--fnptr` adds calls through fn-pointer fields (C ops tables, see §6). `--usage` adds address-taken uses (callback registration).
- `chain` = dominators: what *must* have executed. "NOT statically reachable" (exit 3) from `chain` or `$H graph path` is normal across virtual calls, so use `reach` before concluding anything.
- If even `reach --usage` finds no path, the link is outside the graph: a class-template override, code outside the build, or runtime registration by name (velox function and operator registries). Grep the registration site instead.

## 6. C function-pointer ops tables (ucx style)

```bash
cd <repo>          # a C repo with function-pointer ops tables (ucx-style)
$X dispatch uct_rc_verbs_ep_put_short
#  direct static callers: 0
#  address taken at src/uct/ib/rc/verbs/rc_verbs_iface.c:465 -> stored in field .ep_put_short
#  field uct_iface_ops::ep_put_short: 1 function(s) call through it
#    dispatcher #40113 uct_ep_put_short  -> symx tree '#40113' --callers
```

So the real callers of the implementation are the callers of the dispatcher, filtered to endpoints whose iface uses that ops table.
End to end, `reach --fnptr` follows calls through ops-table fields:

```bash
$X reach ucp_put_nbx uct_rc_verbs_ep_put_short --fnptr
#  ucp_put_nbx --calls--> ucp_put_send_short @rma_send.c:218 --calls--> uct_ep_put_short @rma_send.c:181
#  --calls--> uct_iface_ops::ep_put_short ==fn-pointer==> uct_rc_verbs_ep_put_short
```

`--fnptr` is opt-in because generic callback fields (`uct_am_handler::cb`, `completed_cb`) hold dozens of functions and connect almost everything. Without the cost weighting, `ucp_tag_send_nbx → put_short` "succeeded" with a 36-hop nonsense route. Paths are cheapest-first (static hop 1, virtual 1, heuristic 3–5, `--max-hops 20`), and `LOW CONFIDENCE` is printed when heuristic hops pile up. If you suspect a detour, ask for a closer intermediate target.

## 7. Regression after a change

```bash
$X changed --tests          # vs the indexed commit: hunk -> changed function -> #callers / overriders -> test cases
$X changed HEAD~3           # vs any revision (index coordinates match only for the indexed commit)
$X tree '#ID' --callers --depth 3
$X tests SYM                # test cases that reach SYM (indexes WITH tests: sirius, ucx), e.g. sirius:
#  fold_all: 14 test function(s) call it
#    CATCH2_INTERNAL_TEST_18() [TEST_CASE "CRC32 key column order changes the hash"]  via run_gpu <- compute
$W find <SuiteOrClass> -i '**/test*/**' -t function -n 50     # folly/velox (no tests indexed): find entries by name
```

Hunks reported as "(not in index: new code / file scope / #if-disabled)" have no graph yet. Run `symhow update` (see codegraph-explore → After editing) to bring new code into the graph.

## Dead-end catalogue: why the graph went silent, and the bridge

| Symptom in output | Cause | Bridge |
|---|---|---|
| `no static callers; overrides Base::m` | called via the base class (virtual) | continue from `Base::m`, or use `$X reach` |
| caller chain ends at a pure-virtual / template base method | **class template**: no OVERRIDE edges recorded | `$X overriders` (fallback), `reach` heuristic |
| `lambda` with no callers | passed as a callback (folly::Function, std::function, executors) | its name contains the enclosing fn; trace that fn's callers |
| 0 callers, but a USAGE ref with `.field = fn` | C fn-pointer table | `$X dispatch fn`, `$X reach A fn --fnptr` |
| CALL edge whose target is a FIELD | call *through* a fn-pointer field | `$X tree '#field' --callers` = the dispatchers |
| `UNSOLVED` refs at a line | template-dependent call the indexer couldn't bind | read the template; check instantiations with `--all-matches` |
| `ROOT in this build` | public API (tests/clients aren't in the index), entry point, or registry | `grep -rn name`, `$W find` (symwhere includes tests) |
| `tests`: 0 direct, many "via base virtual" | no test calls this override directly; callers of the base *might* dispatch to it | find where the concrete type is constructed in tests (`$H query references 'ns::Cls'`), or add a test |
| `nothing semantic here` / file not in index | inactive `#if`, test, doc, other platform, failed TU | symwhere view; `grep`; check `$X status` failed TUs |
| locations off by a few lines | index older than the worktree | `$X status`, then `symhow update` |
| `(no location …)` on a node | external/system symbol or template instantiation | `$H query search` the generic name; `$W find` |

## Habits that pay off

- Resolve once, then reuse `#ID`s. Ids are stable across `symhow update` for untouched files, and they make overloads unambiguous.
- Read bodies by span (`definition`, `--spans`), not whole files. A 5,000-line `AsyncSocket.cpp` needs only the 30 lines that matter.
- Treat every graph absence (no callers, no path, no writes) as a **lead to verify**, never a conclusion. Confirm with grep before reporting "dead", "unreachable", or "never written".
- Raw SQL for anything else: see `../codegraph-explore/references/recipes.md` (schema, verified recipes, `symx name` to decode ids).
