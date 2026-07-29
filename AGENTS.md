# AGENTS.md - read this if you are the AI assistant

The person you work with wants THOR running, and you are the one setting it up.
This page is everything you need: how to get them set up, then how to use the
memory so it stays worth having. Everything else in this repo is detail.

**One rule above all: assume they have never done any of this before.** Do every
step you have the tools for yourself. Say what you are doing in ordinary words.
When a step genuinely needs them (a download, a restart), give them exactly one
thing to do, and check it worked before moving on. Never hand back a menu of
options.

---

# Part 1 - setting them up

**1. Get the program.** Download the newest release for their system:
`thor-windows-x86_64.zip` or `thor-linux-x86_64.tar.gz`. The `-bm25` version is
for a server or a NAS.

Building from source instead? Use
`cargo build --release --features semantic --bin thor`, then check the file
size. About 35 MB is right. About 10 MB means the `--features semantic` part was
left off, and the smarter half of the search is silently missing.

**2. Run `thor doctor`.** It tells you honestly what is there and what is not. A
missing language model is not a problem - THOR works without it, just with
simpler matching.

**3. The language model, if they want it.** It makes THOR find things by meaning
rather than only by matching words. They supply the file themselves (about
235 MB); nothing downloads on its own. [docs/SETUP.md](docs/SETUP.md) walks
through it. If you are unsure, skip it for now - it can be added any time.

**4. Connect it to their assistant.**

```sh
thor install --with-courier --with-guard --with-daemon
```

This is what makes memory arrive on its own instead of only when someone asks
for it. It backs up their settings first and is safe to run again.

**5. Register THOR as a tool** so you can read and write memory yourself:

```sh
claude mcp add thor -- <path-to>/thor.exe mcp
```

Then have them restart once, so it gets picked up.

**6. Introduce it to their project.** Run `thor init` in the project folder. It
marks the folder and reads the project's files into memory.

**7. Prove it works before you say it works.** Run `thor doctor` again, then look
something up that you know is in there. Only then tell them setup is done.

---

# Part 2 - using it well

THOR pushes most of what you need to you: standing rules at the start of every
conversation, a memory check on every message, a warning when you touch a file or
run a command one of their rules is about.

What it cannot push is your discipline. That is these six rules.

## 1. Look things up before you start real work

The memory is the source of truth. Not your assumptions, not an old file on
disk, and not this page either. Documents like this one, a CLAUDE.md or a README
are copies, kept around so a fresh start has something to read. When a document
and the memory disagree, **the memory wins**.

And "it is already written in a doc" is never a reason to skip storing a rule.

## 2. Save decisions and gotchas the moment they happen

Without being asked. Give each one:

- **a type** - `gotcha`, `decision` or `preference`
- **`triggers`** - the words that would be in a future question about this. Ask
  yourself: *when should this come back?* Commands, file names, error messages.
- **`anchors`** - the exact files or commands this rule is about. Comma
  separated. A space-separated list is read as one long anchor and never fires.

**Then scope it.** Rules about how you should work belong everywhere, so they go
global. Knowledge about one thing - one project, one hobby, one machine - belongs
to that project. When something lands in the global pile, the reply will ask you
about it. Suggest a home for it instead of letting one project's details leak
into every other conversation they have.

## 3. Never save a second copy - fix the first one

If something already there is wrong or out of date, correct it (`revise`) or
retire it (`retract`). THOR blocks the obvious duplicates, but the judgement is
yours.

- Fixing only a detail - a wrong anchor, a date, the type? Call `revise` with
  just that one thing and no new text.
- Small change to the content? Use `append` to add a dated note underneath, or
  `replace_from`/`replace_to` for one exact edit. Never retype a long note to
  change one line.

**Where this goes wrong most: write-ups.** A "we shipped version 9" note is two
things stuck together, and they need opposite handling.

- **What is true now** - which version is live, where it runs, what the setup
  looks like - is *one* note that each new release should correct. Not a new one
  every time.
- **What happened that day** - what you did, what the tests said - can be its own
  note, but give it an `expires` a few weeks out. Its usefulness is short and its
  length is not, and a stack of eleven of them will bury the actual answer to
  every question about deployment.

You do not have to remember the expiry: a note that opens like a milestone and
carries no date gets six weeks stamped on it automatically, and the reply says
so. Pass your own date to override. The half left to you is the important half -
putting what is *true now* in one note that keeps getting corrected, instead of
writing the twelfth report.

**And the same doctrine the other way round, which is a hard rule: write-ups
expire, rules never.**

Before you put an expiry on anything, check whether its text contains a rule that
still applies today. If it does, lift that rule out into its own note with no
expiry first - anchored to the file or command it is about - and only then let
the write-up expire. THOR warns you when a note that reads like a rule gets a
date; it is a warning and not a refusal, because "pin to version 1.9 until they
fix it upstream" is a rule that genuinely should expire.

An expired note stops appearing anywhere, and stays completely readable through
`get` and `history`. Nothing is deleted, ever.

This is not theoretical. On this project's own memory, one hard rule quietly
expired because it was sitting inside a batch of write-ups, and the sweep that
followed found about forty more still-live rules buried the same way.

`thor consolidate` lists the backlogs for you: write-ups with no expiry, projects
nothing points at, and rules that are already enforced elsewhere but not marked
as such. So none of it depends on someone noticing.

## 4. Settle a contested note as soon as you can

If a note shows as `[DIVERGED]`, two versions of it disagree. Both keep being
served until you pick one with `resolve`. Do it as soon as you know which is
right.

## 5. Say what helped and what got in the way

Use `mark` when something was useful, and `mark` with `noise: true` when it only
distracted you. This is how the memory learns what to show you next time - and it
learns nothing at all if nobody tells it.

When a long conversation gets squeezed down, THOR hands you the list of
everything it showed you and asks you to judge it. Do it then, honestly, one at a
time. But do not save your judging for that list: it only appears in long
conversations, and a short one never gets there. Mark things in the moment they
help or annoy you.

## 6. Standing rules deserve better than being remembered

When they state a standing rule - "never do that on the live server" - `pin` it.
Pinned rules come back in full at the start of every conversation and survive the
squeeze.

**Because it comes back in full, keep a pinned rule short.** Write what to do or
not do, and one line of why. Leave out the dates, the evidence and the story of
which older rule it replaced - that belongs in an ordinary note, not in the text
you re-read at the start of every conversation. When two pinned rules overlap,
merge them into one and retract the other. A pin that has grown into an essay is
one nobody finishes reading, including you.

**Better still, give the rule a gate.** A gate is an anchor naming the command or
the file the rule is about. It fires at the moment you reach for that thing,
which is exactly when the rule matters, and it does not depend on the rule
happening to rank well in a search. Searching is a guess. A gate is not.

When you build one, tag that note `guarded` in the same turn. The per-message
check then stops repeating what the gate already covers - it still shows up if
nothing else matches.

**An anchor must be specific or it is worse than nothing.** THOR refuses a bare
role name like `mod.rs` or `README.md`, and a bare tool name like `git` or
`docker`. A vague anchor fires on unrelated work *and* earns that note credit for
being useful every time it does, so it manufactures its own evidence for being
shown more often. Anchor the full path, or the full command.

`thor consolidate` shows how much of the memory has a gate, and which notes name
a file but have no anchor yet.

And when you decide a note on that list deliberately needs **no** anchor -
because the file it mentions is incidental, not what the note is about - record
that decision by adding the tag `no-gate`. Otherwise the next assistant to come
along re-judges it cold and anchors it anyway. That happened three times in one
day on this project.

---

## Which tool for which question

- **`recall`** - facts, decisions, anything written in prose
- **`where_used`** - a question that names a function or a symbol
- **`impact`** - before you change one
- **`outline`** - the shape of a file
- **`get`** - to open up any id the others hand back

## If you are talking to a remote copy

Some setups connect you to a read-only copy rather than the real memory. It will
tell you so when you connect. Believe it.

"Queued to capture inbox" means **success** there - do not retry. And a refusal
saying "run this on the authority" is the tool protecting itself from getting out
of step, not an error to work around.

## Finally: keep them in the loop

Say in ordinary words what the memory did for them. What came back, what you
saved, and why. A memory that works silently is a memory nobody trusts.

---

## More detail, when you need it

- [docs/SETUP.md](docs/SETUP.md) - the unhurried setup walkthrough
- [docs/FEATURES.md](docs/FEATURES.md) - what every part does, in plain words
- [docs/OPTIONAL-FEATURES.md](docs/OPTIONAL-FEATURES.md) - the extras and what they cost
- [docs/REFERENCE.md](docs/REFERENCE.md) - every command, and how it is built
