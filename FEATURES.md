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

- **Nothing here can make search worse.** Every optional layer falls back to
  plain keyword search when a piece is missing or broken.
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

**Worth it?** Yes, for a handful of rules. Keep the list short: every pinned
line is added to every session, so a long list is a permanent tax and the extras
get silently dropped past the cap.

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
- **An expiry date** for facts you know are temporary ("pin to this version
  until the upstream fix lands"). After the date it stops surfacing; it is not
  deleted.
- **A how-do-we-know label** on each fact - `verified` or `inferred`. This one
  has its own section below.

**Worth it?** The health check and the integrity check: yes, keep them in your
back pocket. The rest: only once your store is big enough that you start
wondering what is still true in there.

---

## Facts that admit they were never checked

The newest piece, and the one most worth understanding before you decide about
it. Every fact can carry a label saying **how it was learned**: `verified` means
something was actually checked - a test was run, a file or an error message was
read, or you confirmed it - and `inferred` means it was reasoned out and never
checked against anything.

On its own that label does nothing. Switch on the matching setting and it starts
earning its keep: when a fact marked `inferred` comes back later, *and the
conversation is about its topic again*, the line it appears on carries a
reminder - check this against the source before you build on it.

**Why you would care:** this is the failure that costs you a day. An agent
reasons its way to something plausible, writes it down, and from then on every
session treats it as established fact. Nothing about a stored note tells you
which ones were actually checked. This makes that visible at the one moment it
matters: when the thing resurfaces and you are about to act on it.

It is deliberately narrow. It does not try to work out whether a fact is *true* -
that is undecidable at the moment of writing. It only records what the writer
already knew: did I check this, yes or no.

**What it costs:** nothing you can measure. It reads one setting per prompt and
scans the text of facts it was already showing you. It never adds or removes a
search result, so it cannot make recall noisier - it only annotates a line that
was already there. And it only fires on facts explicitly marked `inferred`, so a
store where nobody labels anything sees no change at all.

**Worth it?** Yes if other people's agents, or cheap fast models, write into your
memory - that is where it pays. On a 20-scenario test a weak model built on a
stale belief 12 times out of 20 without the reminder and 5 times with it; a
strong model got 1 wrong without it and none with it, and neither was made worse.
Fair warning: that test is not part of this repository, so it is not something
you can re-run here. Also fair warning: it is still marked as an experiment, and
the setting name says so.

It only works if the labels get written. That is the honest catch - a memory full
of unlabelled facts gives the reminder nothing to fire on.

## Checking that nothing broke

One command, `thor fsck`, reads the whole memory and answers one question: is any
of this damaged? It re-checks every entry's fingerprint, and it asks the search
index to verify its own structure. On a healthy memory it prints six `OK` lines
and stops. If something is wrong it says so and exits with an error code, which
is the part that matters: you can put it in a backup script or a nightly job and
have it actually stop you, instead of printing a scary red line into a log nobody
reads.

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

## For contributors only

The repository also carries the measurement harnesses behind its published
numbers, so a stranger can re-run them rather than trust them. They are not part
of using THOR, they make nothing faster, and two of them write to whatever store
you point them at - so read before running.

**Worth it?** Only if you are changing THOR itself or checking its claims.

---

## So what should I actually turn on?

If you take nothing else from this page:

1. Install the hooks - automatic recall is the whole point.
2. Set up your projects, so searches stay clean as you add more.
3. Add the meaning-based search layer on the machine you work on, if you can
   spare the memory.
4. Pin your handful of hard rules.
5. Point the backup at a private repository, and run `thor fsck` once after the
   first restore so you know the backup is real.

Everything else on this page is worth reading once and turning on the day you
have the problem it solves. The exact commands, costs and undo steps for each
are in [OPTIONAL-FEATURES.md](OPTIONAL-FEATURES.md).
