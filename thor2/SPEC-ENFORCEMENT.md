# SPEC - enforcement (Lane B safety catches + Lane C capture guard)

Implementable contract for `PLAN-NEXT.md` sections 0.1, 4 (B3) and 4b (C1-C4).
Written against the code as it stands: `serve/src/respond.rs` (the response
guard) and `serve/src/bin/serve.rs::hook_once` (the hook dispatcher).

Scope of this spec: the SAFETY MODEL (B3) and the CAPTURE GUARD (Lane C).
B1/B2 (the action guard with a judge model) build on the same safety model and
are specified only far enough to make B3 binding on them.

---

## 0. What already exists (do not rebuild)

- `respond::Rule` + `respond::evaluate` - a dependency-free, case-insensitive
  substring matcher: `all_of` AND (`any_of` OR empty) AND NOT `none_of`, with a
  `min_chars` floor. Rulebook is JSON, read from beside the store.
- `hook_once` dispatches by `hook_event_name`: `Stop` (guard, handled first and
  WITHOUT opening the store), `SessionStart`, `UserPromptSubmit`, everything
  else = PreToolUse-shaped.
- `Stop` already honours `stop_hook_active` - a turn that was already blocked
  once is never blocked again. That is the anti-loop guarantee and every rule
  below inherits it.
- Every hook payload carries `session_id` and `cwd`.

The capture guard REUSES this matcher and this rulebook format. It does not get
a matcher of its own. A second rulebook file is the only new data.

---

## 1. B3 - the two safety catches (binding on every enforcement surface)

### 1.1 Three verdicts, one default

```
Verdict = BLOCK | WARN | ALLOW
```

- **BLOCK** only on a conclusive, CITED violation: the surface must be able to
  print WHICH rule fired and WHAT text triggered it. A block that cannot name
  its own cause is a bug, not a strict setting.
- **WARN** = emit text (context/reminder), never stop the action. The cheap
  outcome. A false WARN costs a line; a false BLOCK costs trust.
- **ALLOW** = silence. This is the DEFAULT for every path that is not
  conclusive, including every error path.

Hard rule (mirrors R5 and the existing guard doc): any error - unreadable
rulebook, malformed JSON, missing field, store that will not open, judge that
does not answer - is ALLOW. Never BLOCK, never WARN. An enforcement layer must
never itself become the reason work cannot proceed.

### 1.2 Freshness (anti-stale), default closed

A fact may only BLOCK if its currency is proven AT THAT INSTANT:

1. **Pointer facts** (preferred): the fact stores a rule plus a pointer to the
   canonical source, never a copied value. The value is read LIVE at
   enforcement time. A pointer fact cannot be stale; if the pointer does not
   resolve, the verdict is ALLOW (not BLOCK) and the fact is flagged for revise.
2. **Stored-value facts**: may only BLOCK if they carry a machine-runnable
   currency check that passes at that instant. Check fails -> ALLOW + flag for
   revise (auto-demote). Check missing or unrunnable -> ALLOW.
3. **Code facts**: never a stored snapshot. Enforce against live code through
   the code index, which already carries the commit and the working-copy drift
   on every hit. Drift present -> ALLOW (cannot prove current).

Restated as one invariant, and this is the non-negotiable from section 0.1:

> **There is no path on which a fact whose currency is not proven at this
> instant emits a BLOCK.**

Every enforcement surface must have a test named after that invariant.

### 1.3 What this costs

Soft-fail means the catch rate is bounded by what can be proven current. That
is the intended trade: a missed catch is invisible, a false block is not.

---

## 2. Lane C - the capture guard

Goal: a durable decision stated by the owner cannot end the turn unrecorded.

### 2.1 C1 - flag the decision (UserPromptSubmit)

New rulebook `guard-capture-rulebook.json`, beside the store, same JSON shape as
`guard-response-rulebook.json`, matched with `respond::evaluate` against the
prompt text.

Two confidence tiers, expressed as two rulebooks or one field `tier`:

- **tier `block`** - unambiguous normative statements. Examples of `any_of`
  terms (Dutch and English, the owner works in both): "vanaf nu", "voortaan",
  "nooit meer", "altijd ", "de nieuwe ", "nieuwe huisstijl", "from now on",
  "never again", "the new rule", "always ", "never ".
- **tier `warn`** - weaker signals ("ik wil liever", "beter is", "prefer",
  "should").

`none_of` carries the usual escapes plus the ones this guard needs: quoting or
discussing a rule ("any_of", "for example", "bijvoorbeeld", "als voorbeeld") and
any text that is clearly a question rather than a statement.

On a match, write a marker keyed by `session_id` into a small state file beside
the store (`capture-owed.json`):

```json
{"<session_id>": {"tier": "block", "quote": "<the matched line, trimmed to 200 chars>",
                  "seq_at_flag": <max event seq at flag time>, "blocked_once": false}}
```

`seq_at_flag` is the whole satisfaction mechanism: it is the store's max event
sequence number at the moment of flagging. Nothing else needs to be tracked.

Failure policy: any error writing the marker = no marker. Never fail the prompt.

### 2.2 C2 - enforce at turn end (Stop)

In `hook_once`'s `Stop` branch, AFTER the response guard has had its say (the
response guard stays first and keeps its no-store guarantee; the capture check
is the one Stop path that may open the store, read-only):

1. `stop_hook_active == true` -> return None. Unchanged, inherited anti-loop.
2. No marker for this `session_id` -> return None.
3. Marker present: read the store's max event seq now. If any item-declaring or
   item-revising event exists with seq > `seq_at_flag`, the debt is paid ->
   clear the marker, return None.
4. Debt unpaid and tier is `block` and `blocked_once == false`:
   set `blocked_once = true`, emit
   `{"decision":"block","reason":"[THOR] You were told a durable decision (\"<quote>\") and did not record it in THOR. Record it now with remember or revise."}`
5. Debt unpaid and tier is `warn`, or `blocked_once == true`: clear the marker
   and return None. **One block per flag, ever.** This is what keeps it from
   becoming a nag.

Store cannot be opened, marker file unreadable, JSON malformed -> return None
(ALLOW). Same fail-open as everything else on this surface.

### 2.3 C3 - block the wrong sink (PreToolUse)

Narrow on purpose, so it cannot become a false-block generator. It fires ONLY
when BOTH hold:

- a capture is owed this session (a tier `block` marker exists and is unpaid),
  AND
- the tool call is a Write/Edit whose `file_path` basename is a known mirror
  sink: `CLAUDE.md`, `BRAND.md`, `AGENTS.md`, or a path segment `decisions/`.

Then BLOCK with: "this is a durable decision; record it in THOR first - the .md
is a mirror, never the source." Outside that intersection: ALLOW, silently.

Rationale for the narrowness: without an owed capture, an edit to CLAUDE.md is
ordinary work and blocking it would be exactly the kind of nag this project
exists to remove.

### 2.4 C4 - house style captures the pointer, not the values

When the flagged decision concerns house style / brand / colors, the block
reason must additionally say: "store the POINTER (which source file is
authoritative) plus the rule, never a copied value." This is the write-side of
section 1.2 layer 1 - it is what makes the resulting fact stale-free by
construction.

Detection: the same rulebook, a rule whose id starts with `style-`.

---

## 3. Tests that must exist (each named after the defect it prevents)

Capture guard:
1. `a_stated_decision_that_was_never_recorded_blocks_turn_end`
2. `a_stated_decision_that_was_recorded_does_not_block` (seq moved past
   `seq_at_flag`)
3. `the_same_flag_never_blocks_twice` (`blocked_once`)
4. `a_warn_tier_signal_never_blocks`
5. `a_question_about_a_rule_is_not_a_decision` (none_of escape)
6. `a_missing_or_broken_capture_rulebook_blocks_nothing` (fail-open)
7. `a_marker_from_another_session_is_ignored` (session_id keying)
8. `an_edit_to_claude_md_without_an_owed_capture_is_not_blocked` (C3 narrowness)

Safety model (B3), independent of surface:
9. `a_fact_whose_currency_cannot_be_proven_never_blocks`
10. `a_failed_currency_check_demotes_instead_of_blocking`
11. `every_block_reason_names_the_rule_and_the_trigger`

A refusal ground without a test does not exist (the project's own P2 rule).

---

## 4. Acceptance, and it must be POWERED

The lesson from the underpowered behaviour test (n=14) is binding here.

- Build two sets: (a) prompts that DO state a durable decision, (b) prompts that
  do not but look like it (questions about rules, quoted rules, hypotheticals,
  ordinary work). Minimum 60 per set, drawn from real session transcripts where
  possible, not invented in one sitting.
- Report **false-block rate** (target ~0) and **catch rate**. Report both, always
  together. A catch rate with no false-block rate is not a result.
- The measurement runs against a COPY of the store, never the live one.

---

## 5. Deployment - NOT part of the build

Nothing here is deployed to `C:\Users\dev\thor2\bin` or wired into
`settings.json` without the owner saying so in-session. A surface that can block
turn completion changes his daily work; the build and the numbers come first,
the switch is his call.
