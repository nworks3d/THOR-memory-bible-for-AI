# THOR 2.0 - the contract

This is what the build is judged against. Not taste, not elegance. Every
requirement names the failure class it must make structurally impossible, and the
test that enforces it. A requirement no test can enforce is not a requirement
here - it is a habit, and habits are what we are trying to stop relying on.

## Where this comes from

Three adversarial rounds over 1.0 produced ~30 findings; 15 were real defects.
Every single one had the same shape: **the tool does less than it promised, in
silence.** Not one was "the tool gave a wrong answer". That is not thirty bugs.
It is one architectural property with thirty faces, and it comes from three
choices 1.0 cannot be patched out of:

1. Fail-open everywhere, including on writes that declare intent.
2. One object type doing three jobs (measured: 59% of notes are a rule buried in
   a report).
3. Documents through a scarce channel (two anchors per target, chosen
   alphabetically), which needs six compensating mechanisms.

## R1 - A declaration that cannot be honoured is refused, not stored

Kills: anchors on a full target, anchors that match nothing, metadata silently
dropped on a revise.

Test: every refusal class has a test named after the defect it prevents. A
refusal reason with no test does not exist.

PAID 2026-08-08. Noticed unpaid the same morning: "anchors on a full target"
was named here and did not exist in the code, so by this requirement's own
last line that refusal did not exist. Version 1 had it (`unservable_anchor` in
`thor/src/mcp.rs`, two places per target, naming the incumbents); it did not
come across.

It was rebuilt around the right unit, which is not the one version 1 used.
Version 1 counted rivals per ANCHOR. Here the unit that competes is the pool
`rank::select` assembles for one event, and most crowding is not per anchor at
all: of the 38 items on the worst target, 33 are bound to a MOMENT and would
have walked straight past a per-anchor gate.

`model::store::capacity` therefore asks per BINDING and refuses only when
EVERY binding is hopeless (`DeadOnArrival`, naming the holders); a pool full of
equals is a note, not a refusal, because closeness at some later moment decides
that one and nobody can decide it at write time. It runs on `declare` and on
`revise` - the second was missed on the first pass, which made the refusal
bypassable. A directory anchor counts as a rival of a file inside it (every
surface puts both in the same pool on one touch) but never the reverse, since
a file's own anchor fires only when that file is touched.

Measured against the owner's real store on the day it landed: 52 of 2766
fireable items could not reach a block under any binding, 613 sit in a pool
full of equals. The refusal is not turning writes away wholesale; it is naming
a backlog that predates it.

## R2 - The gate carries constraints, not documents

A constraint is one sentence plus a binding. Nothing is ever truncated at
delivery, because nothing delivered is long.

Kills: truncation inside an item, the invariants-note convention.

Test: no served item exceeds the per-item limit at write time, so the serve path
has no truncation branch to test.

CORRECTED 2026-08-08, and the correction matters more than the rule. This
requirement used to claim it also killed "the cap lottery, the character budget,
crowding reports". It does not, and saying so talked this project out of the one
measurement that would have shown the damage.

Two different things were run together. Making every item short does remove
truncation WITHIN an item, which is real and still holds. It says nothing about
how many items are dropped FROM the list: a block shows at most four, and a
pool can hold far more than four. Measured on the owner's own store the day
this was written: 698 items are eligible at one of their own triggers, 459 are
shown there at least once, and 239 are never shown at any trigger they carry.
They are correct, current, properly bound, and permanently unreachable. The
worst single pool holds 38 items for 4 places.

The lottery was not killed either. It was made deterministic, which is worse in
one specific way: the final tiebreak is the item id, so the same four win
forever, and this store mixes several id conventions, meaning a naming accident
decides visibility permanently.

Crowding reports are therefore REQUIRED, not killed. A capability nothing
measures looks exactly like a capability that works - the same lesson the
proof-coverage line already taught here.

## R3 - Delivery is observable

Every item records when it last fired and how often, as an event in the log, not
in a sidecar. "Declared but never delivered" is a query.

Kills: silent starvation, a gate that goes dark, a correction that never arrives.

Test: a rule that can never fire is queryable immediately after the write.

## R4 - One object, one job

`Rule`, `Orientation`, `Report`, `Lookup`, `Chunk` are separate kinds with
their own lifetimes, their own retrieval surfaces and their own write rules.
`Chunk` (a piece of source code or documentation pulled from a repo) is
archive exactly like `Report`: fully searchable, never served at a gate.

Kills: expiry mistakes, rules buried in narratives, the lift-it-out ritual.

Test: a `Report` or a `Chunk` can never reach a gate; a `Rule` can never
expire. Which kinds can fire is decided in exactly one place
(`model::item::Kind::can_fire`), never re-decided locally by a serve
channel - see `model/tests/single_can_fire_definition.rs`.

## R5 - Two failure policies, declared at the boundary, not per function

- Read and inject: never block, never speak on failure.
- Write and declare: never silent.

Test: a corrupt store makes the hook exit 0 and print nothing; the same corrupt
store makes a write fail loudly.

## R6 - Identity is data, not a formatted string

On 2026-07-30 two places in 1.0 built the same ledger key differently and two
checks disagreed about what "the same target" meant. Keys are typed values,
normalised in exactly one place.

Test: a normalisation function has exactly one definition; a grep for a second
one fails the suite.

## R7 - No behaviour that only a convention protects

If the design needs someone to remember something, that is a design defect.

## R8 - The redundancy gate runs before anything ships

Measured: of 18 rules the file gate served, 14 were already in the repo it served
them to (78%). The action gate scored 4 of 57 (10%). A gate that repeats the repo
pays tokens for redundancy.

Test: the redundancy number is produced by a script over the whole item corpus,
per document, stopwords removed, and it is reported with every serving change.

## R9 - Every compensating mechanism is a reported design failure

If 2.0 adds a nudge, a warning, a cap or a fair-share to rescue a modelling
decision, it does not go quietly into the code. It goes on a list that is read
back at every gate review. This is the rule against exactly what happened to 1.0.

## What 2.0 may not claim until it is measured

That it reduces drift, and that it improves the learning loop. Every measurement
from rounds 1 to 4 is on the input side - how much is served, how precisely, how
readable a panel finds it. None measures an outcome.

## Hard boundaries

- **1.0 is frozen.** No commit in the 1.0 tree, no
  replacement of the running binary, no rewrite of the hooks. The live store is
  read-only; this sandbox works on a copy. A defect found in 1.0 is written to
  `FINDINGS-1.0.md` and becomes a test case for 2.0, not a patch.
- **The seed is real private memory.** It never leaves this folder. No remote, no
  branch of the public repo, no push path.
