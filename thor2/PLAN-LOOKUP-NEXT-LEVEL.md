# Plan: a lookup that answers the question you actually asked

Status: measured, not built. Nothing in this file has shipped.

Written 2026-08-16 after a live failure: `pizzadeeg hydratatie rijzen bloem`
returned nothing, while the answer - a stored pizza dough recipe - contains
`pizzadeeg`, `hydratatie` and `bloem` literally. Dropping the one word the store
spells differently (`rijs`, not `rijzen`) returned it immediately.

## THE PLAN, after the swarm (2026-08-16)

Six lenses argued this out and one measured them against 2304 real session
transcripts. Most of what the plan proposed below was aimed at the wrong target.
What survives, in order:

**1. Finish the anchor fix on the prompt surface. About ten lines.** On
2026-08-14 a multi-word anchor was taught to match by word prefix - but only in
`rank::binding_matches`, never in `prompt::resolve`, which still compares whole
tokens. A value with a space can therefore never match there. MEASURED: 305
fireable items (21.8% of 1397) are unreachable for exactly this reason, using 254
distinct multi-word anchors that already exist. False-fire cost, run against 3145
of the owner's own prompts: 20.25% to 20.57%. This is the whole prize, and it
needs no new binding type, no vocabulary, no migration.

**2. Nothing else gets built until 1 is measured in real use.** Three lenses
proposed a new anchor type or a facet system for life knowledge. The measurement
says life knowledge is 6 of those 305 items - 2%. The defect lives in the code
projects. Building a second placement axis while the first one strands a fifth of
the store is the same bet twice.

**3. Do NOT add a scope guard as described further down.** It was written to catch
`BBQ ideetjes` beside `eten`. Measured: zero scope names collide on a normalised
form, so the guard would not have caught the one case it exists for. The 35 real
collisions are in free TAGS, which is a different problem.

### What the measurements changed about the target

- **The client is not a person asking a question.** Of 1038 real lookup calls,
  1.5% are question-shaped and 72.8% are eight tokens or more of stacked
  keywords. A first pass designed for "what did I cook last month" optimises the
  demo.
- **The tuning battery covers 6% of real traffic.** BM25 fusion was tuned on
  identifier queries of one to four tokens. Phase 3's gate guards that 6%.
- **The ceiling above all of this: the memory is consulted on 6.7% of turns**
  (408 of 6062). No retrieval design moves that number.

### Still open, and honestly unsolved

- **The silent cap.** A semantic answer is capped at ten, and the "N more not
  shown" line only prints above 25 - so for exactly the query class this plan
  cares about, the cap can never announce itself.
- **Time.** A holder item solves the COLLECTION case (which books, which cooks)
  and silently loses the LOG case (a fitness diary), because no surface filters
  by date. Those are different questions and the brief named both.

## What is actually wrong, established by reading and measuring

Three things were checked tonight, and two of my own first explanations were
wrong. They are written out because the wrong ones are the tempting ones.

**The literal pass is all-or-nothing.** `lookup::search` matches the whole query
as one substring; failing that, it requires EVERY word as a substring. One word
the caller spelled differently returns zero, however much of the rest is present.

**The meaning pass is not gated on the literal pass.** My first explanation -
"semantic can only extend literal hits" - is FALSE. `search_best_effort_cached`
builds its candidate pool from every live item, minus the literal hits and the
expired ones. With zero literal hits the whole store is still a candidate. So the
meaning pass CAN rescue an empty answer, and did not.

**The model is present and the vectors are current.** Model present, `model_id`
matches, 3520 vectors for 3521 live items. So the silent-degrade paths
(`search_best_effort`'s documented fallbacks) were not taken either.

That leaves one explanation standing: the recipe scored below `MIN_SIMILARITY`
(0.50). The likely reason is dilution - one embedding for a long document. A
recipe covering flour, hydration, kneading, proving, shaping and oven
temperature averages into a vector that is close to nothing in particular. The
floor was tuned on identifier-style queries, where documents are short and
sharp, and 0.50 was chosen deliberately to cut padding.

NOT YET VERIFIED, and phase 1 exists to settle it: the actual cosine of that
query against that item. Everything below is conditional on that number.

## Phase 0 - the regression net, before touching anything

The requirement is no regression, so the net comes first.

Build a battery from the owner's real store, not a synthetic one, in two halves:

- **Identifier half.** The queries today's ranking was tuned and validated on -
  function names, config keys, file names. This half must not move.
- **Human half.** Queries a person types from memory: domain words, a wrong
  inflection, a synonym, a half-remembered number. Every entry names the item
  the person meant.

Record per half: recall@1, recall@5, and the empty-answer rate. The last one is
the number this plan exists to move.

Hold a third of the human half back, unseen, and never tune against it. The
store already carries the lesson twice: a rulebook that scored 97% on its own
tuning set scored 55% blind.

## Phase 1 - find the cause per failing query, do not guess

Add a diagnostic that answers, for one query: did the literal pass fire, how many
candidates did the meaning pass score, what were the top cosines, and where did
the floor cut. Print it; ship nothing on it.

Tonight two confident explanations were wrong because this did not exist. An hour
went into a fix built on the second one. A number would have cost minutes.

## The best idea in this file is not mine

Raised by the owner on 2026-08-16, and it reframes the whole problem: "if I say
search my ribs under scope `eten`, it should find it."

He is right, and the reason is arithmetic. The 0.50 floor exists to stop noise
across roughly 3500 items - it is a defence against a big pool. Inside a scope of
a dozen food notes there is almost nothing to be noisy WITH: the BBQ note scoring
0.141 would still be the best answer in that pool, and the second-best would be
another recipe, not a Klipper gotcha. The same number that is meaningless
globally is decisive locally.

So the floor is not really a constant. It is a stand-in for "how much can go
wrong here", and the pool size is what that actually depends on.

WHAT IS MISSING TODAY: surface 4 has no scope at all. `lookup` takes a query and
nothing else, and the CLI's `search` takes a query and nothing else. There is no
way to say "only this project", so the pool is always everything. Version 1's
recall had exactly this parameter and 2.0 did not carry it over.

That makes scoping the first thing to build, ahead of both fixes below:

- Add a project scope to `lookup` and to `search`. Nothing else changes: same
  ranking, same floor, just a smaller candidate set.
- Then, and only then, let the floor fall when the scoped pool is small. A pool
  of a dozen cannot produce the 2.54 padded items the 0.45 floor produced across
  the whole store - but that has to be measured per pool size, not assumed.

This costs no noise anywhere, because a caller who does not pass a scope gets
exactly today's behaviour.

## The store pushes back about PLACE, the way it already does about SHAPE

The owner's framing, 2026-08-16: THOR should not quietly take whatever scope an
agent picked. On a write it should first say whether a scope for this kind of
thing already exists, and if none does, that a new one is being created. On a
read it should do the reverse and name which scopes could answer.

This is not a new doctrine, it is the existing one applied to a second field.
The write gate already refuses a shapeless fact and says what to fix. It has
nothing at all to say about WHERE the fact lands - and that silence is what
produced three homes for one subject.

### The primitive both halves need first: what scopes exist

There is no way for an agent to ask. Not in the tool surface, not on the command
line. An agent inventing `BBQ ideetjes` cannot see that `eten` is right there,
because nothing can be asked. Every idea below rests on this one call:

    scopes -> each scope, how many items it holds, when it was last written to

Cheap: one query over the project column. It is the missing primitive, not a
feature.

### Write: placement, and a new scope only on purpose

Two answers, and the second is the one that stops the mess:

1. **A scope for this already exists.** The new fact's nearest neighbours agree
   on one. Offer it. The agent may still choose otherwise - a similarity score
   does not get to file a person's knowledge - but it can no longer be unaware.

2. **No scope holds anything like this.** Then the agent is CREATING a scope,
   and that should read as a decision rather than a side effect of typing a
   string. Today `project: "Boeken"` and `project: "boeken"` and a typo are all
   equally acceptable and equally silent, which is exactly how `BBQ ideetjes`
   came to exist beside `eten`.

   The rule that follows: a scope that holds no other item is a NEW scope, and
   a write creating one says so. Not refused - refusing would push agents to
   dump everything global, which is worse - but never silent.

Cheap guard worth having alongside: a new scope name that differs from an
existing one only by case, spacing or a hyphen is almost certainly the same
scope. That is the one case to refuse outright, because it is never intended.

### Read: which scopes could answer this

The reverse call. Given a question, name the scopes its nearest neighbours live
in, ranked, before spending anything on ranking inside them. Then search there
first, and only fall back to the whole store when the neighbours disagree.

That is what makes narrow search safe rather than a trade: inside a dozen food
notes the similarity bar can fall a long way without noise, because there is
nothing there to be noisy with.

### The shape trap this plan must also close: lists

Some knowledge is a LIST - books read, cooks tried, suppliers used. Stored as one
fact per entry, a question about it gets the best few and a cap, so the answer
looks complete and is not. Silent incompleteness is worse than an empty answer.

List-shaped knowledge belongs in ONE item that holds the list and grows by
revision - which is what the `Lookup` kind already is: it answers only to its own
exact key, and returns whole. What is missing is that an agent cannot discover
that a key exists, for the same reason it cannot discover a scope. The `scopes`
call above should name keys too.

## One mechanism for both sides: ask the store where this belongs

Raised by the owner on 2026-08-16, after watching food knowledge end up in three
places: can something make this easier for BOTH writing and looking up?

It can, and it is one computation used twice, because both sides are asking the
same question: what does this look like, and where does that kind of thing
already live?

The store can already answer it. Embedding one text and finding its nearest
neighbours is exactly what the meaning search does today - the model is loaded,
the vectors are built, nothing new is needed to compute it.

**At write time - placement.** Before storing, find the new fact's nearest
neighbours and look at where they live. If they agree on a scope, offer it. If
the fact is about to land in a scope where no neighbour lives, say so out loud:
that is the signal that was missing every time a recipe went global. This is a
proposal, never an automatic reassignment - a similarity score may not silently
decide where a person's knowledge is filed.

**At read time - narrowing.** The same neighbours name the scope the question is
about, so a search can look inside that scope first. And that is what makes the
floor problem go away rather than be traded off: inside a dozen food notes the
bar can fall a long way without noise, because there is almost nothing there to
be noisy with. Disagreeing neighbours mean no scope was inferred, and the search
stays exactly as it is today.

Two things this must not become. It must not file anything without being asked -
a wrong scope buries a fact more thoroughly than a wrong anchor. And it must not
widen a global search: the narrowing is a first pass, and a miss falls back to
the whole store rather than to a lower bar everywhere.

MEASURED, the reason this belongs above the fixes below: consolidating 9 facts
into one scope changed the search results by exactly nothing. Scope is invisible
to lookup today, so tidying alone buys nothing until one of these two halves
exists.

## Phase 2 - the fixes, in the order their evidence justifies

**2a. Embed long items per chunk, score by best chunk.** Split an item's text the
way the code index already splits a file, embed each piece, and score the item by
its strongest piece. A four-word question about hydration then meets the
paragraph about hydration instead of the average of a whole recipe. This is the
direct answer to dilution and it costs nothing at query time - only a bigger
sidecar and a longer `vectors-build`.

**2b. A floor that knows whether it is rescuing or padding. WITHDRAWN, and the
reason is worth more than the idea was.** The proposal was: keep 0.50 when the
literal pass found something, allow lower when it found nothing. The store
already holds the measurement that kills it. The floor was raised from 0.45 to
0.50 on 2026-08-05 and the number quoted for it is padding on NO-ANSWER queries,
2.54 items down to 0.82 - a 68% cut, bought at about 0.5pp of recall@5. The
no-answer query IS the empty-literal case. Lowering the floor exactly there is
not a new idea; it is the old setting, already measured and deliberately given
up.

What is still open, and is genuinely different: the same rescue capped at ONE or
TWO extras instead of ten. The 2.54 was measured with the extras cap at 10, so a
rescue that can add at most one item has a bounded cost that has never been
measured. That is the only version of 2b worth building, and only behind
phase 3's gate.

MEASURED 2026-08-16, the case that prompted this: a short BBQ note (747 chars,
one part, so chunking does nothing for it) scores 0.402 against "ribbetjes met
mac and cheese en zoete aardappel" - a good answer, rejected by the floor. The
same note scores 0.141 against "ribbetjes" alone, which no sane floor rescues.
The single word needs the LITERAL side to know that ribbetjes, ribben and ribs
are the same word, not a lower cosine bar. See 2c.

**2c. Loosen the literal pass, last and only if still needed.** Requiring most of
the words rather than all of them, ranked by how many matched, and only when the
strict pass found nothing. This was BUILT tonight and reverted: it fixed the
pizza query and left six other queries untouched, but two queries went from 3
hits to 30 and the cause was not understood before the revert. The current
suspicion is that those extra hits were the meaning pass extending a now-larger
literal set, not the loosening itself - which would make this change less
harmful than it looked. Do not retry it without phase 1's diagnostic.

## Phase 3 - the gate this ships behind

- The identifier half does not lose recall@1. Not "about the same": not lower.
- The human half's empty-answer rate drops, on the held-back third.
- Padding on no-answer queries does not rise above where the 0.50 floor put it.

If 2a alone clears that gate, stop there. The cheapest version that clears it is
the one that ships.

## What this plan refuses to do

Widen the query until something matches. An answer that is always non-empty is
not a better memory, it is a worse one: the owner learns to distrust every
result, which is the one thing this system cannot afford. An honest empty answer
stays a valid outcome.
