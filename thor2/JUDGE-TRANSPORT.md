# Judge transport for the capture guard (2026-08-05)

Scope: replace the deterministic substring rulebook as the DECIDER for Lane
C's capture guard (`serve/src/capture.rs`) with a cheap external judge model,
per the measurement in `AB-JUDGE-VS-RULEBOOK.md` and `FRESH-HOLDOUT-RESULTS.md`
(both at the repo root, not re-run here - taken as given). Working tree:
`thor2`. Nothing was deployed - see "Not done / not verified here" at the end.

## Why (one paragraph, the numbers are not re-derived here)

On one frozen, blind hold-out (`serve/eval/capture-eval-fresh-decisions.json`,
60 prompts that all state a durable decision; `serve/eval/capture-eval-fresh-nearmiss.json`,
70 that do not), a cheap judge model (Claude Haiku 4.5, given the prompt
below) caught 60/60 genuine decisions with 0/70 false blocks, against the
deterministic rulebook's 33/60 catch and 8/70 false-block rate on the SAME
set - worst on the hypothetical class (rulebook 6/12 wrong, judge 0/12). The
rulebook's own any-tier catch is only 42/60 (70%), so using it as a prefilter
in front of the judge would cap the catch rate at 70% and was explicitly
rejected. This is why the design below has the judge decide outright, with
the rulebook demoted to a warn-only fallback for when the judge cannot be
reached at all.

## Design implemented

1. **The judge decides, not the rulebook.** No rulebook prefilter sits in
   front of it.
2. **The judge runs at Stop (turn end), never at UserPromptSubmit.** C1
   (`serve/src/bin/serve.rs::capture_flag`) is now "cheap and dumb": it
   records the raw prompt text and the store's current max event seq
   (`seq_at_flag`) for the session, unconditionally, on every non-empty
   prompt - no rulebook match, no judge call, no classification of any kind.
   Classification happens once, at that same turn's Stop
   (`capture_stop_check`), where the multi-second judge call overlaps with
   the owner reading the assistant's reply instead of sitting in front of it.
3. **The rulebook is the fallback, and the fallback never blocks.**
   `capture::fallback_decision` runs the exact same matcher as before
   (`decide_capture`, unchanged, still fully unit-tested on its own) but
   forces the result to `Tier::Warn` no matter what tier the rulebook's own
   JSON assigns the rule that fired. Any error on the judge side - no config
   file, an unreadable/malformed config, a command that cannot be spawned, a
   run that exceeds its timeout, or an answer that does not parse into
   BLOCK/WARN/ALLOW - falls through to this fallback. This is the existing
   fail-open doctrine (SPEC-ENFORCEMENT.md 1.1), made explicit for the judge.
4. **The whole capture guard (C1/C2/C3) is gated off for a subagent, added
   2026-08-05 as a correction after this document's first draft.** Claude
   Code's own hooks documentation, verified after that draft: `SessionStart`
   fires ONLY in the main session, but `UserPromptSubmit`, `Stop` and
   `PreToolUse` all DO fire inside a Task-tool subagent, carrying
   `agent_id`/`agent_type` only in that case. A subagent's own task prompt is
   written by an orchestrating agent, not the owner, and is routinely full of
   the very words ("always", "never", "from now on") this guard watches for -
   unguarded, a subagent's Stop could be blocked over a "decision" the owner
   never made, deadlocking the exact delegated-task workflow this project
   runs on. See "Subagent gating" below.
5. **C3 was restored, added 2026-08-05 as a second correction.** Moving
   classification to Stop (point 2 above) had an undisclosed-until-caught
   side effect: C3 lost the ability to act within the same turn a decision
   was stated, because the verdict genuinely did not exist yet at
   `PreToolUse` time. It is restored WITHOUT reintroducing the rulebook's
   measured false-block rate into any blocking path - see "C3 restored"
   below.

## Architecture: where each piece lives

- `serve/src/judge.rs` (new) - the whole judge transport: `JUDGE_PROMPT`
  (verbatim, see below), `JudgeConfig`/`parse_judge_config`, `JudgeVerdict`,
  `parse_judge_output` (pure), `run_judge_command` (I/O: spawn, write stdin,
  read stdout, enforce timeout), `run_judge` (the two combined).
- `serve/src/capture.rs` (rewritten) - `decide_capture`/`parse_rulebook`
  unchanged (still the matcher, still its own tests, now feeding TWO places -
  see point 5 below); new `fallback_decision` (forces Warn); new
  `from_judge_verdict` (turns a `JudgeVerdict` into the same `FlagDecision`
  shape the fallback produces); `Marker` gained a `prompt: String` field
  (what C1 records) and its `tier`/`quote`/`rule_id`/`house_style` fields are
  now the AUTHORITATIVE, Stop-decided verdict (`tier: Option<Tier>`, `None` =
  not yet classified this turn) - plus THREE more fields,
  `provisional_tier`/`provisional_quote`/`provisional_rule_id`, C1's own
  immediate rulebook signal (see "C3 restored" below); `decide_stop` gained a
  `classification: Option<&FlagDecision>` parameter and its own first check
  is still `debt_paid` - a paid debt wins over ANY classification, including
  a judge `Block` (this is a required, explicitly tested case, not an
  incidental one - see "Tests" below); `sink_is_blocked` was replaced by
  `sink_verdict` returning a three-way `SinkVerdict { Allow, Warn(String),
  Block(String) }` (see "C3 restored").
- `serve/src/bin/serve.rs` - `capture_flag` (C1) never calls the judge (only
  C2 does), but now DOES read the fallback rulebook again to compute the
  provisional signal it passes into `flag_marker_text`; `capture_stop_check`
  (C2) computes `debt_paid` first and ONLY calls `classify_capture` (which
  tries the judge, then the fallback) when the debt is still unpaid - saving
  a real judge round trip whenever the model already did the right thing
  this same turn; `capture_sink_check` (C3) now returns `capture::SinkVerdict`
  instead of `Option<String>`, and the `PreToolUse` arm folds a `Warn` into
  whatever else that call would render as additional context (never a
  block), while a `Block` still short-circuits exactly as before. All three
  (`capture_flag`/`capture_stop_check`/`capture_sink_check`) are now gated on
  `!payload_is_from_a_subagent(&payload)` - see "Subagent gating" below - and
  the dead `SessionStart` subagent gate that could never execute was removed
  (see "SessionStart gate removed" below).
- `serve/src/bin/fake_judge.rs` (new) - TEST FIXTURE ONLY, never a real judge.
  A tiny external command (`fake_judge <mode> [args...]`: `block`/`warn`/
  `allow`/`garbage`/`sleep <ms> <mode...>`/`sentinel <path> <mode...>`) used
  by the integration tests to drive `run_judge_command` against a REAL
  process - the one thing a pure unit test cannot exercise (a genuine
  timeout, a genuine spawn, genuinely unparseable output).
- `guard-judge-config.example.json` (new, repo root) - worked example, the
  Claude CLI (a hosted model), UNVERIFIED end to end (see "What could not be
  verified here" below).
- `guard-judge-config-local-model.example.json` (new, repo root, 2026-08-05
  addendum) - the second worked example: a fully local model (Ollama or LM
  Studio), also UNVERIFIED end to end. See "A second worked example" under
  "Config format" below.
- `guard-judge-local-model-wrapper.ps1` (new, repo root, 2026-08-05
  addendum) - the actual runnable PowerShell wrapper the local-model example
  config points at (an example config alone is not enough for a local model:
  something has to actually speak its HTTP API on the stdin/stdout shape
  this transport expects). Also documented in the same "Config format"
  section.
- `serve/examples/judge_latency.rs` (new) - the latency harness (see its own
  section below).
- `serve/tests/capture_guard_subagent_gate.rs` (new) - the subagent-gating
  tests (see "Subagent gating" below).
- `serve/tests/subagent_session_start_suppression.rs` -> renamed to
  `serve/tests/subagent_hook_behavior.rs`, dropping the one test that
  exercised the now-removed dead `SessionStart` gate and keeping the two that
  describe behavior still real (see "SessionStart gate removed" below).
- `serve/tests/judge_parse_fixtures.rs` (new, 2026-08-05 addendum) - the
  29-item labeled fixture set proving `parse_judge_output`'s tolerance (see
  "Tolerant judge-output parsing" below).

## Subagent gating (2026-08-05 correction)

Claude Code's own hooks documentation, verified after this document's first
draft: `SessionStart` fires ONLY in the main session - never inside a
Task-tool subagent. `PreToolUse`, `UserPromptSubmit` and `Stop` DO fire
inside a subagent, and `agent_id`/`agent_type` are present on the payload
ONLY in that case - a reliable, documented gate on exactly those three
events.

The consequence for Lane C: C1 (`UserPromptSubmit`), C2 (`Stop`) and C3
(`PreToolUse`) all sit on surfaces that fire inside a subagent. A subagent's
own task prompt is written by an orchestrating agent, not the owner, and is
routinely full of the words ("always", "never", "from now on") this guard
watches for. Left ungated, a subagent would get a capture debt flagged
against its own task prompt, and its Stop would then be blocked until it
records a "decision" the owner never made - a deadlock in the exact
delegated-task workflow this project runs on.

**Fix:** `serve/src/bin/serve.rs` already had
`payload_is_from_a_subagent(&payload)` (a non-empty `agent_id` string - see
`INJECTION-FRAMING.md` step 3 for how this predicate was established). It now
gates all three capture-guard call sites in `hook_once`:

- `UserPromptSubmit` arm: `capture_flag` is only called when
  `!payload_is_from_a_subagent(&payload)` - a subagent's own prompt never
  gets recorded, so no marker, no provisional signal, nothing for C2 or C3 to
  ever act on for that session.
- `Stop` arm: when `payload_is_from_a_subagent(&payload)` is true, the whole
  Lane-C branch returns `None` (Allow) before `capture_stop_check` is even
  called - this holds regardless of whether a marker somehow exists for that
  session id (defense in depth, proven directly in the tests below).
- `PreToolUse` default arm: `capture_sink_check` is skipped entirely
  (`capture::SinkVerdict::Allow` substituted directly) for a subagent
  payload - a subagent's own tool calls are never touched by C3 either.

The Response Guard (a separate surface, watching the assistant's reply, not
the memory) is UNCHANGED and still runs on every `Stop` regardless of
subagent status - this fix is scoped to Lane C only.

**Tests** (`serve/tests/capture_guard_subagent_gate.rs`, against the real
compiled `serve hook` binary):
- `a_subagent_prompt_never_creates_a_capture_debt` - a subagent's own
  decision-shaped prompt leaves no marker for that session at all.
- `a_subagent_turn_is_never_blocked` - a subagent's Stop is never blocked,
  proven even against a marker crafted directly with an owed, unpaid Block
  for that session id (bypassing C1 to test C2's gate in isolation).
- `a_subagent_sink_write_is_never_touched_by_capture` - C3's half of the same
  proof, against a real judge-decided Block marker.
- `the_owners_own_main_session_is_still_flagged_and_blocked_normally` - the
  scoping check: the SAME store, a plain payload with no `agent_id`, still
  gets flagged and blocked exactly as before. This fix narrows one thing, it
  is not a kill switch.

## C3 restored (2026-08-05 correction)

Moving classification to Stop (point 2 in "Design implemented") had a real,
disclosed side effect this document's first draft named honestly but did not
fix: C3 could no longer act WITHIN the same turn a decision was stated,
because the judge only runs at Stop, so the verdict genuinely did not exist
yet at `PreToolUse` time, before that turn's Stop had run.

**Restored without reintroducing the rulebook's measured 11.4% false-block
rate into any blocking path:**

- C1 now ALSO runs the cheap, free, deterministic rulebook match
  (`capture::decide_capture` - the exact same fallback matcher, unchanged)
  and stores its result in the marker as `provisional_tier`/
  `provisional_quote`/`provisional_rule_id` - available immediately, well
  before this turn's Stop has judged anything. This NEVER reaches the
  AUTHORITATIVE `tier` field, which stays `None` at C1 regardless and is
  Stop's alone to fill in via the judge.
- C3's decision (`capture::sink_verdict`) now returns one of three outcomes,
  not two:
  - `Block` - ONLY when the AUTHORITATIVE `tier` is `Some(Tier::Block)` AND
    it came from an ACTUAL judge verdict (`rule_id == JUDGE_RULE_ID`). By
    construction this is the only way `tier` can ever be `Some(Block)` at
    all (`fallback_decision` can only ever produce `Warn`), so the `rule_id`
    check is a second line of defense, not the only one.
  - `Warn` - when `provisional_tier == Some(Tier::Block)` (C1's own cheap
    match, possibly all that exists yet) - a visible, non-blocking nudge
    folded into the tool call's own `additionalContext`, never a block.
  - `Allow` - neither signal present, or the debt is already paid.
- `serve/src/bin/serve.rs`'s `PreToolUse` arm: a `Warn` is prepended to
  whatever else that call would already render (or stands alone if nothing
  else would); a `Block` still short-circuits the whole arm exactly as
  before.

**What this buys back:** within the SAME turn the decision was stated, a
tool call at a mirror sink now gets an immediate, visible warning - the
single most useful moment for this guard to say something, restored without
ever letting the rulebook's own false-block rate reach an actual block.
**What is still true, and disclosed:** a REAL block still requires the judge
to have run (at Stop) and said BLOCK - so within that same turn, before Stop,
the strongest signal available is a warning, never a hard stop; and C1's
blind per-turn marker overwrite (unchanged by this fix) still means an
unpaid, judge-decided Block from a PREVIOUS turn is discarded the instant the
next user prompt arrives, exactly as this document's first draft already
disclosed (see "Known limitations").

**Test:** `capture::tests::c3_only_warns_when_the_tier_came_from_the_rulebook`
(pure) plus `capture_guard_hook.rs::c3_warns_provisionally_then_blocks_once_stop_confirms_it`
(end to end: a mirror-sink write warns right after C1, before Stop; the
identical write blocks once Stop has judged it via the real `fake_judge`).

## SessionStart gate removed (2026-08-05 correction)

Unrelated to the judge, but directed alongside it, on the same documentation
evidence: `INJECTION-FRAMING.md` step 3 had added a subagent gate on the
`SessionStart` arm of `hook_once`. Since `SessionStart` never fires inside a
subagent at all (confirmed above), that gate could never execute - dead code
that reads as protection is worse than no code, so it was removed from
`serve/src/bin/serve.rs`. `render::FRAMING_LINE` (the fix that DOES cover a
surface subagents actually reach, `PreToolUse`) is untouched.
`serve/tests/subagent_session_start_suppression.rs` was renamed to
`serve/tests/subagent_hook_behavior.rs`, dropping the test that exercised the
removed gate and keeping the two that describe behavior still real. Full
account: `INJECTION-FRAMING.md`'s own "Addendum (2026-08-05)" section.

### The judge prompt (verbatim, unchanged)

`serve/src/judge.rs`'s `JUDGE_PROMPT` constant is copied byte-for-byte from
`AB-JUDGE-VS-RULEBOOK.md`'s "The judge prompt (verbatim, used unchanged for
all 130 items and for all latency tests)" section - the exact prompt that
produced the 100%/0% result. It was not edited for phrasing, tone, or
"improvement": a changed prompt invalidates the measurement it carries. If
this prompt is ever revised, the A/B measurement no longer describes this
code and must be re-run before the new wording is trusted.

One thing that IS new, and was never itself measured: the exact framing used
to combine that fixed prompt with ONE owner prompt for a single external-
command call. `AB-JUDGE-VS-RULEBOOK.md`'s own measurement invoked the model
through the Agent tool's separate system/user-message split, batched 10
prompts per call; this implementation instead writes one flat string to the
command's stdin (`judge::build_judge_stdin`):

```
<JUDGE_PROMPT, verbatim>

PROMPT:
<the one owner prompt to judge>
```

This specific stdin framing was never itself run against the frozen
hold-out. It is a reasonable, documented construction, not a re-verified one
- see "What could not be verified here" below.

## Config format

A judge config sits beside the store, same stance as the two existing
rulebook files (`guard-response-rulebook.json`, `guard-capture-rulebook.json`
- a project directory could otherwise plant one and redirect every judge
call): `<store's parent dir>/guard-judge-config.json`
(`judge::default_judge_config_path`).

```json
{
  "command": "the executable to run",
  "args": ["each", "argument", "as", "its", "own", "string"],
  "timeout_ms": 8000
}
```

- `command` (required, non-empty after trimming) - anything `std::process::Command::new`
  can spawn. No shell is invoked; `args` are passed directly, so no shell
  quoting is needed or possible.
- `args` (optional, defaults to `[]`).
- `timeout_ms` (optional, defaults to 8000 - see "The 8-second default" below).

No config file present, or a config file that fails to parse (bad JSON, no
`command`, or an empty/whitespace-only `command`), is the quiet, ordinary
case: no judge, fall through to the rulebook fallback. This is not an error
and never logged as one (`judge::parse_judge_config` returns `None`, and
`serve.rs`'s `classify_capture` just moves on to the fallback).

### Worked example (UNVERIFIED - read before using)

`guard-judge-config.example.json` (repo root):

```json
{
  "command": "claude",
  "args": ["-p", "--model", "claude-haiku-4-5"],
  "timeout_ms": 8000
}
```

This assumes the `claude` CLI, in print mode (`-p`), reads its prompt from
stdin and prints a plain-text answer to stdout when given no positional
prompt argument - a common pattern for this kind of CLI, but **this could
not be verified in this environment**: there is no `ANTHROPIC_API_KEY` and no
`claude` CLI installed on this machine. Running `judge_latency` (below)
against this exact file, on this machine, confirms the honest failure mode
(the command cannot even be spawned) rather than confirming the invocation
itself works.

Before pointing a real session at this: confirm the invocation actually
works on your own machine first, e.g. `echo hello | claude -p --model
claude-haiku-4-5` from a plain shell, and adjust `command`/`args` if it does
not behave as assumed. Copy the file to `guard-judge-config.json` in the same
folder as `thor.db` to activate it - the `.example.json` name is never read
by the hook itself, exactly like the two existing rulebook examples.

A wrapper script (a small `.cmd`/`.ps1`/Python file that itself calls
whatever API or CLI is actually available, reading stdin and writing stdout
in the shape this module expects) is very likely the more robust integration
path in practice, since "any external command" is the whole point of this
transport - it does not have to be the `claude` CLI specifically.

### A second worked example: a fully local model (added 2026-08-05)

The owner's own stated requirement: BOTH options must be genuinely open, not
just the Claude CLI above - a **local model** running entirely on his own
machine (Ollama, LM Studio, or similar) is a first-class choice, not an
afterthought. Two new files at the repo root implement this:

- `guard-judge-local-model-wrapper.ps1` - the actual runnable wrapper (the
  transport is "write to stdin, read from stdout," so an example config
  alone is not enough here; this script IS the thing THOR would spawn).
  Reads the whole judge prompt from stdin (`[Console]::In.ReadToEnd()`),
  POSTs it to a local model server, and prints the model's plain-text answer
  to stdout. Defaults to talking to **Ollama** (`-Backend ollama`, the
  default); pass `-Backend lmstudio` to talk to **LM Studio**'s
  OpenAI-compatible endpoint instead. Written in PowerShell specifically
  because it ships with every supported version of Windows and
  `Invoke-RestMethod` is a built-in cmdlet (`Microsoft.PowerShell.Utility`,
  confirmed present on this machine) - no new software to install for the
  wrapper script itself, only for whichever local runtime you choose to run
  behind it.
- `guard-judge-config-local-model.example.json` - the config file that points
  at the wrapper script.

**What you need to install first (for someone who has never done this
before) - pick ONE:**

1. **Ollama** (the wrapper's default) - <https://ollama.com/download>.
   Install it, then from any ordinary terminal (PowerShell, cmd, whatever you
   already have open) run once:
   ```
   ollama pull llama3.2
   ```
   This downloads a small model. Ollama runs as a background service once
   installed, listening on `http://localhost:11434` automatically - there is
   no separate "start the server" step.
2. **LM Studio** - <https://lmstudio.ai>. Install it, open it, use its own
   "Discover" tab to download a small model, then open its "Developer" tab
   and click "Start Server". LM Studio listens on `http://localhost:1234`
   while that server toggle is on (turn it off when you are done - it stays
   off across restarts by default).

**Where the files go and what to change:** copy both
`guard-judge-local-model-wrapper.ps1` and
`guard-judge-config-local-model.example.json` to wherever you like on disk
(there is no requirement they sit next to the store); then edit the
`"-File"` argument inside the copied config's `"args"` array to the wrapper
script's actual absolute path on your machine. Finally, copy that edited
config to `guard-judge-config.json` **in the same folder as `thor.db`** -
exactly like the Claude CLI example above, the `.example.json` name itself
is never read by the hook.

**How to tell whether it works**, before pointing a real THOR session at it:
from a plain terminal, with your chosen local runtime actually running,
```
echo hello | powershell -NoProfile -File guard-judge-local-model-wrapper.ps1
```
A few lines of model-generated text printed back means it works. Nothing
printed, or a PowerShell error, means the local server is not reachable yet
(server not started, wrong port, model name not pulled/loaded) - fix that
before activating the config.

**What WAS verified on this machine, and what was not:** neither Ollama nor
LM Studio is installed here, so the actual HTTP call this script makes to a
local model has never been run end to end - this is stated plainly, not
glossed over. What WAS verified directly: that `[Console]::In.ReadToEnd()`,
invoked exactly the way THOR's own transport invokes an external command
(piped stdin, closed after the write, via `powershell.exe -File script.ps1`),
correctly captures the full piped text on this machine. Only the
model-calling half is unverified; the stdin-plumbing half is not.

### The Claude CLI and the local model are equally supported, by design

Neither example above is the "real" one with the other as a fallback - the
owner asked for both to be genuinely open, and nothing in `serve/src/judge.rs`
or this document treats one as more official than the other. Swapping
between them is a one-file copy (`guard-judge-config.json` beside the
store), nothing else changes.

### The 8-second default

`judge::DEFAULT_JUDGE_TIMEOUT_MS` is 8000. This is a JUDGMENT CALL, not a
measured number: `AB-JUDGE-VS-RULEBOOK.md`'s own `duration_ms` band (the
model's own self-reported generation time, still measured through an
agent-harness) was 3.9-6.9 seconds; 8000ms sits comfortably above that with
some margin for a real, lean API call plus process-spawn overhead. It is
fully overridable per deployment via `timeout_ms` in the config file, and
this repository has no measured direct-API number to justify a tighter or
looser default (see "Latency" below - nothing was measured here at all).
`guard-judge-config-local-model.example.json` overrides it to 15000ms - also
a judgment call, not a measured one: a CPU-bound local model is plausibly
slower than a hosted API call, so this leaves more headroom before THOR's
own transport gives up and kills the process.

## What data leaves the machine, per call

Every judge call sends, via the configured external command's stdin, the
COMPLETE, UNREDACTED text of:

1. The fixed `JUDGE_PROMPT` (public - already in this file and in
   `AB-JUDGE-VS-RULEBOOK.md`, carries no private data).
2. The owner's own ONE prompt for that turn, exactly as typed, in full (not
   summarized, not truncated, not filtered for secrets or personal
   information).

THOR performs NO redaction, filtering, or truncation of the prompt text
before handing it to the configured command. If that command itself calls a
hosted API (the `claude` CLI, a `curl` call to `/v1/messages`, or anything
similar), this raw prompt text leaves the machine to that API's servers on
EVERY turn where the debt is not already paid (see the paid-debt skip below)
- once per turn, not once per prompt-with-a-decision, since C1 records every
prompt and C2 classifies whichever one is currently recorded whenever the
debt is outstanding. If the configured command is a fully local model
instead, nothing leaves the machine at all - the privacy cost is entirely a
function of what the OWNER points `guard-judge-config.json` at, not of
anything this module hard-codes.

One mitigation already in place: when the debt for this session is already
paid (a `fact_created`/`fact_revised` event landed since the prompt was
recorded), `capture_stop_check` skips calling the judge entirely - so a turn
where the model already called `remember`/`revise` never sends that prompt
to the judge at all (proven by `serve/tests/judge_transport.rs`'s
`a_paid_debt_skips_the_judge_call_entirely`).

## The fallback ladder (fail-open, all the way down)

1. Debt already paid (a `fact_created`/`fact_revised` landed since the
   prompt was recorded) -> ALLOW, unconditionally, judge never even called.
2. Debt unpaid, judge configured and reachable, answer parses -> use the
   judge's verdict as-is: `Block` blocks turn end once (naming the judge's
   own citation); `Warn` and `Allow` never block.
3. Debt unpaid, but the judge is unreachable for ANY reason (no config file,
   malformed config, empty `command`, the command cannot be spawned, the
   command exceeds `timeout_ms`, or its output does not parse into
   BLOCK/WARN/ALLOW) -> fall back to the deterministic rulebook
   (`fallback_decision`). A fallback match is ALWAYS `Tier::Warn`, regardless
   of what tier the rulebook's own JSON assigns that rule - this fallback can
   never block turn end, by construction.
4. No fallback rulebook either, or nothing in it matches -> silence (ALLOW).
   On real uncertainty, this guard says nothing rather than guessing.

C3 (the mirror-sink `PreToolUse` check) layers on top of this same ladder,
restored 2026-08-05 (see "C3 restored" above): it may WARN (never block) on
C1's own cheap, immediate rulebook signal alone, and may BLOCK only once
Stop has actually run and the judge itself said `Block`, still unpaid.

## The latency harness

`serve/examples/judge_latency.rs`. Run:

```
cargo run -p serve --example judge_latency
```

Overridable via `JUDGE_LATENCY_CONFIG` (path to a judge config; default the
repo-root `guard-judge-config.example.json`), `JUDGE_LATENCY_PROMPT`
(default a generic synthetic sentence), `JUDGE_LATENCY_CALLS` (default and
minimum 20, per this task's own brief). It makes that many real calls
through `judge::run_judge_command`, timing each with `Instant`, and reports
min/median/p95/max wall-clock milliseconds plus min/avg/max response size in
characters (all only over calls that actually produced a response) - or, if
every call failed to produce one (including "no config file" and "config
does not parse"), it says so plainly and reports latency as UNMEASURED. It
never estimates or simulates a number.

### Result, run on this machine, 2026-08-05

```
Judge transport latency harness (JUDGE-TRANSPORT.md)
config   : .../guard-judge-config.example.json
prompt   : 99 char(s) (measured)
calls    : 20 (minimum 20 per the design brief)

command  : claude ["-p", "--model", "claude-haiku-4-5"] (timeout_ms 8000)

  call  1/20: no response (failed to spawn, timed out, or non-UTF8 output)
  ... (20/20, all the same)

Every one of the 20 call(s) failed to produce a response (could not spawn the
command, or it never answered before its own timeout - 20/20 failure(s)).

LATENCY: UNMEASURED. A config file exists and parses, but the configured command
itself is not reachable/working on this machine right now, so there is no real
round trip to report a number for.
```

**Latency remains UNMEASURED.** This machine has no `claude` CLI installed
and no `ANTHROPIC_API_KEY`, so `Command::new("claude")` fails to spawn on
every one of the 20 attempts (near-instant failures, not real round trips -
the whole run above completed in well under a second). The harness is fully
working and ready to produce a real min/median/p95/max the moment a working
judge command is configured on a machine that has one - no new tool needs to
be written at that time. No number above was estimated, guessed, or carried
over from `AB-JUDGE-VS-RULEBOOK.md`'s own agent-harness measurement (3.9-6.9s
`duration_ms` band) - that number describes a different call path (the Agent
tool, batched) and is not restated here as if it were this transport's own
measured latency.

A sanity check WAS run against the `fake_judge` test fixture (not a real
model, not reported as a latency number here) purely to confirm the harness
itself has no bugs: 20/20 calls succeeded, reporting a plumbing-only
wall-clock (process spawn plus stdin/stdout round trip with no real model
work) of about 22-26ms. This proves the harness's mechanics work; it is NOT
a judge latency number and is deliberately not presented as one anywhere in
this document's "Result" section above.

## Tolerant judge-output parsing for a local model (2026-08-05 addendum)

`judge::parse_judge_output` was originally written assuming a well-behaved
hosted model (Claude Haiku, in the measured A/B): a clean `VERDICT: <token>`
/ `JUSTIFICATION: <text>` answer and nothing else. A small local model very
often does not answer that cleanly - it wraps the verdict in reasoning
prose, inside a markdown code fence, inside a JSON object, or restates the
question first. `parse_judge_output` was extended to read all of these,
while keeping one hard boundary absolute: **an ambiguous answer must yield
no verdict, never a guess.**

**The one-sentence resolution rule** (also written directly into
`parse_judge_output`'s own doc comment in `serve/src/judge.rs`, so it never
drifts out of sync with the code):

> Every recognized verdict signal in the answer - a `VERDICT:` marker
> anywhere on a line, a JSON object's `verdict` field, or a line that is
> just one bare token - is reduced to its BLOCK/WARN/ALLOW value, and the
> result is that value ONLY when every signal found agrees on exactly one
> distinct value; zero signals or two-or-more DIFFERENT values both yield no
> verdict, ever.

Concretely: free prose merely mentioning a token ("this could be BLOCK or
ALLOW") is not itself a recognized signal, so it yields no verdict (nothing
was recognized at all, not "recognized but ambiguous" - the outcome is the
same either way: `None`). Two contradicting `VERDICT:` lines, or a JSON
verdict that disagrees with a `VERDICT:` line elsewhere in the same answer,
ARE two recognized signals that disagree - also `None`. A `None` degrades to
the existing rulebook fallback (`capture::fallback_decision`, untouched by
this task), which is forced to `Tier::Warn` regardless of what tier its own
rulebook JSON assigns - so nothing on this path can ever cause a block, by
construction, all the way down.

One extra, deliberate hardening beyond the task's own list: `JUDGE_PROMPT`'s
own instructed answer format is the literal string `VERDICT: <BLOCK|WARN|ALLOW>`
- a weak local model that echoes the instructions back verbatim, instead of
substituting a real choice, can reproduce that literal placeholder. Since
`JUDGE_PROMPT` is frozen, this exact string is known in advance and is
stripped before parsing (`ECHOED_VERDICT_PLACEHOLDER` /
`ECHOED_JUSTIFICATION_PLACEHOLDER` in `serve/src/judge.rs`), so an echoed
placeholder is never mistaken for a real `BLOCK` answer or allowed to create
a false second signal that would otherwise make a genuinely unambiguous
answer look ambiguous.

### Fixture set and parse rates (Task 3)

`serve/tests/judge_parse_fixtures.rs` - 29 realistic messy judge outputs,
labeled by class, run through `parse_judge_output` directly (pure, no
process spawn). Run `cargo test -p serve --test judge_parse_fixtures --
--nocapture` to reproduce this table:

```
Judge output parser: fixture parse rate per class
----------------------------------------------------------------------
ambiguous                  4/4   (100.0%)
clean                      4/4   (100.0%)
fenced                     4/4   (100.0%)
garbage-and-empty          6/6   (100.0%)
json                       4/4   (100.0%)
prose-wrapped              4/4   (100.0%)
restates-question          3/3   (100.0%)
----------------------------------------------------------------------
TOTAL                     29/29
```

- **clean** (4/4): the original hosted-model shape, kept as the baseline.
- **prose-wrapped** (4/4): reasoning before the verdict, after it, both, and
  the verdict stated mid-sentence rather than on its own line.
- **fenced** (4/4): a plain fence, a language-tagged fence, bolded
  `**VERDICT: ...**`/`**JUSTIFICATION:**` lines, and an inline-backtick
  verdict.
- **json** (4/4): a clean `{"verdict": ..., "reason": ...}` object, a
  lowercase value with a `justification` key instead, JSON wrapped in prose,
  JSON inside a fenced code block, plus one bonus case inside the same test
  (not counted in this table, see `a_json_shaped_verdict_is_still_read` in
  `serve/src/judge.rs`): even a TRUNCATED json blob missing its closing
  brace still gets its verdict read, because the plain-text marker scan and
  the JSON extractor are two independent signal sources, and the plain-text
  one alone is enough here.
- **restates-question** (3/3): restating the owner's prompt before
  answering, restating the judging task itself, and echoing
  `JUDGE_PROMPT`'s own literal answer-format placeholder before giving the
  real answer (the hardening described above).
- **ambiguous** (4/4, ALL correctly yield no verdict): two contradicting
  `VERDICT:` lines, a hedge naming two verdicts with no structured signal, a
  JSON verdict disagreeing with a `VERDICT:` line elsewhere, and a hedge
  naming all three verdicts at once.
- **garbage-and-empty** (6/6, ALL correctly yield no verdict): empty output,
  whitespace only, a plain refusal, a truncated single word, a JSON blob
  with no `verdict` field at all, and the common English word "block"
  appearing in ordinary code (`block_on(fut)`) with no `VERDICT` marker
  anywhere near it - proving the parser does not fire on the bare word
  alone.

**The safety property, checked directly as its own test:**
`every_ambiguous_and_garbage_fixture_yields_no_verdict_and_never_blocks` (same
file) asserts, over every fixture in the `ambiguous` and `garbage-and-empty`
classes, both that the result is `None` AND that it is specifically never
`Some(JudgeVerdict::Block(_))` - the same property `judge.rs`'s own
`no_verdict_can_never_produce_a_block` unit test checks over a second,
overlapping set of inputs colocated with `parse_judge_output` itself.

## Tests

Every test named in the task brief, plus the pure parsing/config tests that
support them. Located in `serve/src/judge.rs` (pure, transport-config
parsing), `serve/src/capture.rs` (pure, decision logic), and two integration
files driving the real compiled `serve hook` binary,
`serve/tests/capture_guard_hook.rs` and `serve/tests/judge_transport.rs`.

| Required test | Where |
|---|---|
| a missing judge config never blocks and never errors | `judge::tests::a_missing_or_malformed_config_yields_no_judge` (pure); `judge_transport.rs::a_missing_judge_config_never_blocks_and_never_errors` (end to end, real hook binary) |
| a judge command that times out never blocks | `judge_transport.rs::a_judge_command_that_times_out_never_blocks` (real `fake_judge sleep`, real timeout) |
| a judge answer that does not parse never blocks | `judge::tests::an_unparseable_answer_is_no_verdict` (pure); `judge_transport.rs::a_judge_answer_that_does_not_parse_never_blocks` (real `fake_judge garbage`) |
| a BLOCK verdict from the judge does block, and its reason carries the judge's citation | `capture::tests::a_judge_block_verdict_carries_its_citation_into_the_block_reason` (pure); `capture_guard_hook.rs::c1_records_and_c2_blocks_on_a_judge_block_verdict` (real `fake_judge block`) |
| the fallback path can WARN but can never BLOCK | `capture::tests::the_fallback_path_can_warn_but_never_block` (pure); `capture_guard_hook.rs::with_no_judge_configured_the_fallback_rulebook_never_blocks` (end to end, no judge config at all) |
| the judge is never invoked at UserPromptSubmit | `judge_transport.rs::the_judge_is_never_invoked_at_user_prompt_submit` (sentinel-file proof: the judge writes a marker file the instant it runs; the marker must not exist after C1 and must exist after C2) |
| an already-recorded decision does not block even when the judge says BLOCK | `capture::tests::an_already_recorded_decision_does_not_block_even_when_the_judge_says_block` (pure); `capture_guard_hook.rs::recording_the_decision_before_stop_pays_the_debt_even_with_a_judge_configured_to_block` (real `fake_judge block`, a real `fact_created` event, real Stop) |
| a subagent prompt never creates a capture debt | `capture_guard_subagent_gate.rs::a_subagent_prompt_never_creates_a_capture_debt` |
| a subagent turn is never blocked | `capture_guard_subagent_gate.rs::a_subagent_turn_is_never_blocked` (plus `a_subagent_sink_write_is_never_touched_by_capture` for C3's half, and `the_owners_own_main_session_is_still_flagged_and_blocked_normally` proving the fix is scoped) |
| C3 only warns when the tier came from the rulebook | `capture::tests::c3_only_warns_when_the_tier_came_from_the_rulebook` (pure); `capture_guard_hook.rs::c3_warns_provisionally_then_blocks_once_stop_confirms_it` (end to end: warns pre-Stop, blocks post-Stop) |
| a verdict wrapped in prose is still read | `judge::tests::a_verdict_wrapped_in_prose_is_still_read` |
| a fenced verdict is still read | `judge::tests::a_fenced_verdict_is_still_read` |
| a json-shaped verdict is still read | `judge::tests::a_json_shaped_verdict_is_still_read` |
| an ambiguous answer yields no verdict | `judge::tests::an_ambiguous_answer_yields_no_verdict` |
| garbage yields no verdict and never blocks | `judge::tests::garbage_yields_no_verdict_and_never_blocks` |
| no verdict can never produce a block | `judge::tests::no_verdict_can_never_produce_a_block` (plus `judge_parse_fixtures.rs::every_ambiguous_and_garbage_fixture_yields_no_verdict_and_never_blocks`, the same property over the full labeled fixture set) |

Plus, earning their keep beyond the required set (this task, 2026-08-05):
`judge::tests::an_echoed_format_placeholder_does_not_shadow_the_real_answer`
(the `JUDGE_PROMPT`-echo hardening) and
`judge_parse_fixtures.rs::fixture_set_parses_exactly_as_labeled` (the 29-item
labeled fixture set and its per-class parse-rate table - see "Tolerant
judge-output parsing" above).

Plus, earning their keep beyond the required set: `no_classification_at_all_never_blocks`,
`the_same_flag_never_blocks_twice`, `a_marker_with_no_signal_at_all_never_touches_a_sink_write`,
`an_owed_judge_decided_capture_blocks_only_the_mirror_sink_not_any_file`,
`a_warn_tier_marker_never_blocks_a_sink_write`, `a_paid_debt_silences_even_a_provisional_sink_warning`,
`a_house_style_fallback_capture_names_the_pointer_not_the_value`,
`every_block_reason_names_the_rule_and_the_trigger`,
`c1_stores_a_provisional_rulebook_tier_but_never_an_authoritative_one`,
`a_missing_rulebook_at_c1_yields_no_provisional_signal` (all in `capture.rs`);
`a_paid_debt_skips_the_judge_call_entirely` (the latency-saving skip, proven
with a sentinel, in `judge_transport.rs`); every existing pre-redesign test
that still applies (`a_fact_whose_currency_cannot_be_proven_never_blocks`) was
kept and updated for the new architecture rather than deleted.

### Test run status

Figures below are the sum across every test binary `cargo test -p serve`
runs (the `serve` lib plus every `serve/tests/*.rs` integration file plus
doctests), since that command reports each binary separately rather than
one grand total:

```
cargo test -p serve
  -> 262 passed, 0 failed (was 253 before this task's tolerant-parsing work;
     174 before the judge-transport task before that)

cargo test --release --features semantic -p serve
  -> 285 passed, 0 failed (was 276 before this task's tolerant-parsing work;
     197 before the judge-transport task before that)
```

The lib-only count (`cargo test -p serve --lib`, the number this task's own
brief quoted as its starting baseline) went 194 -> 201 (default features) and
217 -> 224 (release + semantic); the +7 in each case is the six required
named tests plus `an_echoed_format_placeholder_does_not_shadow_the_real_answer`,
all in `serve/src/judge.rs`. The remaining +2 in each total above is
`serve/tests/judge_parse_fixtures.rs`'s two tests (new file, this task).

No pre-existing test outside `capture.rs`/`judge.rs`/`serve.rs`/
`capture_guard_hook.rs` was touched by the judge-transport work itself. The
subagent-gating and C3-restoration corrections additionally touched
`serve/tests/subagent_session_start_suppression.rs` (renamed to
`serve/tests/subagent_hook_behavior.rs`, per direction - see "SessionStart
gate removed" above) - `render::FRAMING_LINE` in `serve/src/render.rs` and
`serve/src/session_start.rs` was left exactly as found and still passes.
This task (tolerant parsing, 2026-08-05) touched only `serve/src/judge.rs`
(the parser itself), added `serve/tests/judge_parse_fixtures.rs` (new file),
and added the two new example config files plus the wrapper script (repo
root) - `capture.rs`, `JUDGE_PROMPT`, every rulebook, and every eval set were
left untouched, per this task's own hard constraints.

## Known limitations (honest, not hidden)

- **Nothing was deployed.** No file under `C:\Users\dev\thor2\` was
  touched, no `settings.json` anywhere was touched, no hook was wired into a
  live Claude Code session, and no copy of the live store was even needed for
  this task (every test here uses its own fresh `tempfile::tempdir()` store,
  same pattern the pre-existing capture-guard tests already used).
- **No real judge transport exists on this machine.** No `ANTHROPIC_API_KEY`,
  no `claude` CLI. The example config is documentation, explicitly labeled
  unverified; the latency harness genuinely ran and genuinely found nothing
  to measure (see above) - this is not a gap that was worked around, it is
  the honest state of this environment.
- **Neither Ollama nor LM Studio is installed on this machine either
  (2026-08-05 addendum).** `guard-judge-local-model-wrapper.ps1`'s actual
  HTTP call to a local model has never been run end to end - only the
  stdin-reading mechanic it depends on (`[Console]::In.ReadToEnd()` against
  a genuinely piped, genuinely closed stdin) was independently verified on
  this machine, in isolation from the rest of the script. The `-Backend
  lmstudio` branch is additionally unverified even at the shape level - it
  was written to LM Studio's documented OpenAI-compatible response shape,
  not exercised against a real LM Studio server.
- **The tolerant judge-output parser (2026-08-05 addendum) has one
  acknowledged residual gap:** if a judge's own citation/reason TEXT happens
  to contain the literal word "verdict" immediately followed by one of
  BLOCK/WARN/ALLOW (e.g. a citation that itself says "...a real verdict is
  ALLOW here..."), the plain-text marker scan can pick that up as a second,
  independent signal - correctly demoted to "no verdict" if it disagrees
  with the true verdict elsewhere, but this means a sufficiently adversarial
  or coincidental citation could turn an otherwise-clean answer into "no
  verdict" rather than the intended one. This was judged an acceptable,
  disclosed trade-off (fails toward caution, never toward a wrong guess) and
  was not engineered away, per the "keep the resolution rule simple enough
  to state in one sentence" instruction for this task.
- **The exact stdin framing (`build_judge_stdin`) was never itself measured.**
  The 100%/0% result was produced through the Agent tool's system/user-message
  split, batched 10-at-a-time; this implementation's single flat stdin blob,
  per single-item call, is a reasonable but DIFFERENT framing that was never
  run against the frozen hold-out. If a real judge is ever wired up, a small
  re-check (a handful of prompts through the ACTUAL configured command) before
  trusting the 100%/0% number for THIS exact transport would be prudent.
- **C3's within-the-same-turn window was restored (see "C3 restored" above),
  but only ever to a WARN, never a real block, within that turn.** A REAL
  block still requires the judge to have run (at Stop) and said `Block` -
  before that, the strongest signal C3 can ever act on is C1's own cheap
  rulebook match, capped to `Warn` by construction. So within the SAME turn
  the decision was stated, the guard can nudge but never stop a mirror-sink
  write; only on a LATER `PreToolUse` call (after that turn's Stop has judged
  it) can a real block happen.
  - **Cross-turn survival is still narrowed, unchanged by the C3 restoration.**
    C1 overwrites the WHOLE marker on EVERY prompt (there is no rulebook
    match gating whether it fires at all any more), so a judge-decided Block
    that survived a Stop block still unpaid is discarded the instant the
    user sends their next message, rather than surviving until paid. "One
    block per flag, ever" (the `blocked_once` mechanism) is kept in the code
    for defensiveness and is still unit-tested, but under this architecture
    `decide_stop` runs at most once per turn in practice (the second Stop
    call in a blocked-and-retried turn is skipped by `stop_hook_active`,
    same as before), so it rarely has the chance to matter across more than
    one call any more. This trade-off follows directly from "the judge
    decides, not the rulebook, and it only runs at Stop" - reintroducing
    full cross-turn persistence would require either caching a stale verdict
    across turns or a second, independent debt-tracking mechanism, neither of
    which this task asked for.
- **C3's `Warn` shape (folded into `additionalContext`) is an assumption,
  unverified against a live Claude Code session** - same caveat this
  workspace already carried for C3's `Block` shape before this task (see
  `LANE-C-RESULTS.md`'s own "Not verified" section): there was no existing
  precedent in this codebase for injecting a non-blocking nudge specifically
  in RESPONSE to a `PreToolUse` call before this fix, so prepending the
  warning text to whatever context that call would already render was the
  most consistent choice available (it reuses the exact `HookOutput::Context`
  shape every other injection surface already uses), not a confirmed-correct
  one. If wiring this to a live session ever finds this wrong, only the
  combining logic in `serve/src/bin/serve.rs`'s `PreToolUse` arm needs to
  change - `capture::sink_verdict` and the reason text underneath it are
  unaffected.
- **The judge's citation is trusted verbatim, capped at 200 characters**
  (`QUOTE_MAX_CHARS`, unchanged from before) - the same trust boundary the
  fallback rulebook's own matched-quote always had; a judge that fabricates
  a citation not actually present in the prompt would not be caught by
  anything in this code (the measurement in `AB-JUDGE-VS-RULEBOOK.md` did not
  check citation faithfulness, only verdict correctness).
- **Concurrent hook invocations racing on `capture-owed.json`** were not
  stress-tested here either - the same class of risk this codebase already
  accepted for the response-guard rulebook and the pre-judge marker file;
  nothing in this redesign makes it worse, nothing hardens it further (no
  file-locking dependency was added, consistent with "no new third-party
  dependency").
- **`run_judge_command`'s stdin-write-then-drop is not deadlock-proof for an
  arbitrarily large prompt.** The parent writes the whole judge prompt plus
  the owner's prompt in one `write_all` call before the child has necessarily
  started draining it; for realistic prompt sizes (a few hundred to a few
  thousand characters) this is well within default OS pipe buffers and the
  child (already spawned and running concurrently) drains promptly in every
  test here. An extremely large prompt (many times a pipe buffer's size)
  combined with a judge command that does not read stdin promptly could in
  principle stall - this was not hit in testing and is a known, disclosed
  edge case rather than a hardened one.
- **`fake_judge` (`serve/src/bin/fake_judge.rs`) is a permanent addition to
  the workspace's binary list**, auto-discovered by Cargo the same way
  `repair_project`/`restore_attribution` are - it is a test fixture only,
  clearly marked as such in its own doc comment, never a real judge, and
  costs nothing at runtime for anything that does not spawn it directly.
