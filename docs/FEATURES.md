# What THOR actually does, in plain words

This page is for someone who has just found THOR and wants to know what it is
for, feature by feature, and whether each piece is worth bothering with. No
setup instructions here - those are in [SETUP.md](SETUP.md). No exhaustive cost
tables either - those are in [OPTIONAL-FEATURES.md](OPTIONAL-FEATURES.md). This
page answers one question per feature: **what does it do for me, and should I
care?**

## The problem it solves

An AI coding agent forgets everything between sessions. Worse, it forgets in the
middle of a long session, when the conversation gets compacted to make room. So
you explain the same things over and over: why that workaround exists, which
command is dangerous here, what you decided last week and why.

THOR is one local file that remembers, plus the plumbing that gives the right
piece back at the right moment without you asking. It is a single program with
no cloud service behind it and no database server to run.

Two things are true of everything below, and they are why the rest is safe to
try:

- **Nothing here breaks if a piece is missing.** Every optional layer falls back
  to plain keyword search when something is absent or broken, so a half-finished
  setup costs you that layer and nothing else. That is a promise about failure,
  not about quality: two of the layers below are measured to help in some places
  and not others, and each says so in its own section.
- **Nothing here deletes a fact.** The store only ever appends. Correcting a
  fact adds a new version; the old one stays in history.

---

## The base: it remembers, you search

Store a fact, search for it later. That is the whole core, and it works with no
configuration at all.

**Why you would care:** it is a notebook that lives next to your code and that
your agent can write to and read from itself.

**Worth it?** This is not optional - it is THOR. Everything else is about
getting the right note back without having to go looking for it.

---

## Automatic recall: the part that earns its keep

Before every prompt you send, THOR searches your memory for what you are talking
about and pastes the top hits into the conversation. You do not ask for it and
you do not see the search happen.

**Why you would care:** this is the difference between "a memory tool I forget
to use" and a memory. It is also what survives a compaction: the moment the
conversation is trimmed, the next prompt still arrives with the relevant history
attached.

It tries not to be annoying about it. It will not show you the same fact twice
in a session, it stays silent rather than injecting a weak one-word match, and
when it quotes a piece of code it re-reads the file first, so you get what is on
disk now rather than a stale snapshot.

**Worth it?** Yes. If you install one thing, install this.

---

## Standing rules that always come back (pins)

Some facts are not "relevant sometimes" - they are rules that must never be
missed. Pin those, and their full text is re-injected at the start of every
session and right after a compaction, whether or not the conversation happens to
mention them.

**Why you would care:** search only finds what the prompt hints at. A rule like
"never run that command against production" needs to be present *before* anyone
thinks to mention production.

**Worth it?** Yes, for a handful of rules. Two things follow from "in full":
keep the *list* short, because every pinned rule is added to every session, and
keep each *rule* short. Write the instruction, not the history behind it - your
assistant has to act on this at the start of every conversation. If more rules
are pinned than fit, the block says so on its last line instead of quietly
leaving them out.

---

## A warning at the moment you act (the guard)

Separate from search: when your agent is about to touch a specific file or run a
specific command, THOR surfaces the notes attached to exactly that file or
command.

**Why you would care:** the moment you are about to do the dangerous thing is
the moment the warning is worth something, and it is usually not the moment the
conversation mentioned it. This catches the class of mistake that search cannot:
the prompt says nothing about the trap, but the file path does.

**Worth it?** Yes if you have hard-won operational lessons ("this deploy step
looks safe and is not"). It never blocks anything - it only adds a note.

---

## Keeping projects apart

THOR holds every project in one file but keeps them separated: searching in
project A never surfaces project B. Knowledge that genuinely applies everywhere
(your conventions, your working rules) goes in a shared tier that surfaces in
all of them.

**Why you would care:** without this, a memory tool gets worse the more you use
it. Ten projects in one pile means every search competes with nine irrelevant
codebases.

**Worth it?** Yes, as soon as you have a second project. It is one command per
project and then you forget about it.

---

## Searching by meaning, not just words

Out of the box the search matches words. The optional layer adds meaning: a
question phrased completely differently from the note can still find it.

**Why you would care:** you rarely remember a fact in the same words you wrote
it in. This is what makes "what did we decide about uploads" find a note that
never uses the word "uploads".

**What it costs:** you supply a language model file yourself (nothing is
downloaded behind your back), and keeping it loaded and ready uses about 650 MB
of memory. If that is too much, skip it - keyword search keeps working.

**Worth it?** Yes on the machine where you actually work. No on a server or a
small box: there is a separate build without any of it.

---

## Making it fast (the warm daemon)

A small background process keeps the search-ready state in memory instead of
rebuilding it for every prompt.

**Why you would care:** measured on a store of about 16 thousand entries, the
per-prompt wait went from 349 ms to 120 ms, and what gets injected is identical
either way. It is pure waiting time, nothing else.

**Worth it?** Recommended, unless memory is tight on your machine. Keep it on
your own computer only - it is not something to expose to a network.

---

## A second opinion on the order (rerank)

An optional extra pass that re-orders search results using a slower, more
careful model.

**Why you would care:** it is better at fuzzily-worded questions. It is also
worse at exact lookups, and the repo says so with the numbers: it improved the
top result by 3 points on a paraphrase-heavy set while making exact references
worse.

**Worth it?** Only as a deliberate second try when an answer looks wrong, and it
is built that way - it never runs automatically. Skip it entirely unless you are
chasing a specific search that keeps missing.

---

## Questions about your code

If you let THOR index your repositories, it can also answer "what calls this
function" and "what would changing this touch".

**Why you would care:** it is the difference between a memory of what you told
it and a memory of your actual codebase.

**Worth it?** Yes if you index code at all - it builds itself as a side effect
of indexing, so there is nothing extra to run.

---

## Keeping the memory honest

A memory that only grows becomes a memory you stop trusting. THOR ships a small
set of tools for that, none of which delete anything behind your back:

- **A health check** that tells you in one command whether the pieces are
  actually in place. Run this first whenever something seems off.
- **An integrity check** that recomputes the whole chain, so tampering or
  corruption is detectable rather than assumed away.
- **A cleanup report** listing near-duplicates and notes that have gone cold.
  It only ever proposes; you decide.
- **"This helped"** - a one-word signal your agent can give when a recalled fact
  actually answered the question. It quietly improves what surfaces next time.
  An optional flag (`THOR-EXP-AUTO-ECHO.flag`) settles part of this
  mechanically: when the session demonstrably acts on a served fact (its
  declared anchor or firing words show up in the actual commands and files),
  THOR records the "helped" at half weight by itself. Deleting the flag makes
  every automatic record inert again.
- **Archive facts step aside** - another optional flag
  (`THOR-EXP-HIST-DEMOTE.flag`) makes a fact that loudly declares itself
  historical give up its injection slot to a comparably-matching live fact.
  It still serves when it is genuinely the best answer.
- **Nothing is served to you twice** - your pinned rules arrive in full at every
  session start, so the per-prompt block stops spending a slot on them and puts
  something you have not been told there instead. They still surface when
  nothing else matches. Measured on 100 real prompts, one in seven served lines
  was such a repeat. The same applies to a fact you tag `guarded`: that tag
  means "this one already has a gate" - a guard anchor on the command it
  governs, or a rule in the response rulebook - and a gate that fires at the
  moment of action beats a reminder in every prompt. On by default;
  `THOR-NO-PIN-DEDUP.flag` puts both back.
- **Signposts to your scopes** - once domain knowledge lives in project scopes
  instead of the global tier (which is how you stop one project's notes bleeding
  into another), a chat outside that project can no longer reach it. Tag one
  small fact per domain `wegwijzer` ("all finance facts live in project
  Investments - recall there") and THOR looks those up by tag on every prompt,
  independently of ranking, appending at most two as a single `Scope hint:` line
  that never takes a content slot. That moved "was the chat told where to look"
  from 30% to 96% on a blind 60-prompt test. On by default;
  `THOR-NO-SCOPE-HINT.flag` switches it off.
- **An expiry date** for facts you know are temporary ("pin to this version
  until the upstream fix lands"). After the date it stops surfacing anywhere
  automatic - search and the moment-of-action warning alike - though it is
  never deleted, and you can still look it up directly. If a fact reads like
  a standing rule rather than a temporary note, THOR warns rather than blocks
  when you try to give it a date too: reports are meant to expire, rules are
  not.
- **A how-do-we-know label** on each fact - `verified` or `inferred`. This one
  has its own section below.

**Worth it?** The health check and the integrity check: yes, keep them in your
back pocket. The rest: only once your store is big enough that you start
wondering what is still true in there.

---

## Checking that nothing broke

One command, `thor fsck`, reads the whole memory and answers one question: is any
of this damaged? It re-checks every entry's fingerprint, and it asks the search
index to verify its own structure. On a healthy memory it prints six `OK` lines
and stops. If something is damaged it says so and exits with an error code, which
is the part that matters: you can put it in a backup script or a nightly job and
have it actually stop you, instead of printing a scary red line into a log nobody
reads. One exception, and it is deliberate: a fact whose little metadata tag went
missing is reported but does not raise the alarm, because nothing is corrupt -
it just needs tidying.

There are two kinds of bad news it can give you, and they are not equally bad.
A broken fingerprint means someone or something altered a past entry - that is
serious, and it is why the fingerprints exist. A damaged search index is not
serious at all: the index is built from your notes rather than being your notes,
so `thor fsck --rebuild-fts` builds a fresh one and nothing is lost. It is worth
knowing about anyway, because a damaged index does not announce itself - searches
just quietly start missing things.

**Why you would care:** this is the difference between "my backups are fine" and
"my backups are verified". It is also the only thing that catches a search index
that has gone half-blind.

**Worth it?** Yes, and it costs nothing to have: it never runs on its own. Run it
after restoring a backup, after a crash, after copying the memory to another
machine, or on a timer if you like sleeping well. It reads the entire memory each
time, so it is a maintenance command, not something to run on every prompt.

---

## Not losing it

Export the whole memory to a plain text file, and restore it back with every
entry's fingerprint re-checked. There is also a one-command backup that commits
the export into a git repository you point it at.

**Why you would care:** it is your notes. The export is a readable file you can
keep anywhere, not a proprietary blob.

**Worth it?** Yes. Point the backup at a private repository and forget about it.

---

## More than one machine

If you work on two machines, one of them holds the real memory and the other
keeps a copy that is kept up to date automatically, entry by entry, with each
one's fingerprint verified on arrival.

**Why you would care:** the same memory on your laptop and your desktop, without
putting the file on a network share - which would quietly corrupt it.

**Worth it?** Only if you actually have a second machine. Nothing to turn on
otherwise, and it does open a network port, so it belongs on your own network.

---

## Your phone, or the web

THOR can also run as a small server so a phone or a browser session can search
the same memory. Writes from those sessions are queued and folded into the real
memory on the next sync rather than written directly, which is what keeps the
history in one unbroken line.

**Why you would care:** capturing a thought where you have it, instead of where
your computer is.

**Worth it?** Only if you want that. It is the most involved thing on this page:
a container to run, a shared secret to manage, and something in front of it that
handles authentication - the connection itself has none.

---

## The off switch

One empty file next to the store silences every automatic surface at once - no
injection, no warnings, no nudges - without uninstalling anything or touching a
single stored fact. Delete the file and everything comes back.

**Why you would care:** for a screen recording, a demo, or an afternoon where
you want the tool to stop talking.

**Worth it?** Good to know it exists. You will use it twice a year and be glad
both times.

---

## So what should I actually turn on?

Almost all of it. Only two things on this page are genuinely a choice, and they
are named at the bottom. Everything else: set it up once and stop thinking about
it. That is not enthusiasm, it is what the person who wrote THOR runs every day.

### Set these up. There is no decision to agonise over.

- **Automatic recall**, **the warning at the moment you act**, and **the check
  before your assistant finishes replying**. All three come on with the one
  install command in [SETUP.md](SETUP.md).
- **Keeping projects apart** - one command per project, then never again. Skip it
  and THOR gets *worse* the more you use it, because every search starts
  competing with projects you were not asking about.
- **Pinning your hard rules** - a handful, not a list. Every pinned line joins
  every conversation, and past the limit the extras are dropped without telling
  you.
- **Meaning-based search.** You fetch the model file yourself, which is the only
  reason it is not already on. Do it.
- **The reranker model.** Installing it and *using* it are two different things -
  see the note below - but have it there, so a search that keeps missing has a
  second gear.
- **A backup to a private repository.** And run `thor fsck` once after your first
  restore, so you know the backup is real instead of assuming it.
- **Syncing**, if you have a second machine. Nothing to weigh up: either you have
  one or you do not.

### The two you can genuinely skip

**1. Reaching your memory from a phone or the web.** A container, a network you
trust, and a login in front of it. It works well and it is real work to set up.
Only if you actually want your memory away from your desk.

**2. The background process that keeps things quick** - and only if the machine
is genuinely short on memory. It costs a few hundred MB and takes roughly two
thirds off the wait on every message. If you have the memory to spare, this is
not a decision either. Leave it out on a small box; keep it everywhere else.

### One note on reranking, since it is easy to get wrong

Install the model, but do not expect it to run on every question - it is per
question, on purpose. Measured: it improves the *single best* result and makes no
measurable difference to the set as a whole, and your assistant reads the set. So
running it on everything would add a second of waiting to every question in
exchange for a gain nobody reads.

What makes it worth having is what it does beyond reordering. With reranking on,
THOR fetches a deeper pile of candidates before sorting, so a note that normally
sits just below the cut can be pulled up into your results. The set changes, not
just the order. That is exactly what you want on the second attempt at a search
that keeps coming back with the wrong thing.

### What we tried and dropped

Kept here because knowing what was thrown away tells you more about a tool than
its feature list.

- **Labelling how a fact was learned** (checked, or reasoned out) ran live for a
  week. The idea was that an unchecked belief would come back with a warning
  attached. In practice assistants label almost everything as checked, so the
  label carried no information, and whether the warning actually prevented
  anything turned out to be unmeasurable in daily use - the thing you are
  measuring is a mistake that did not happen. Ended, not shipped.
- **Four ranking and cleanup ideas** each looked good on a small test of their
  own and were then measured against real data, where all four failed. One
  interrupted the assistant as often wrongly as rightly. Two solved problems that
  turned out not to exist. None of them are here.

The exact commands, costs and undo steps for everything on this page are in
[OPTIONAL-FEATURES.md](OPTIONAL-FEATURES.md).
