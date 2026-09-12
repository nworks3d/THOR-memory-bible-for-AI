# AGENTS.md - read this if you are the AI assistant

The person you work with wants THOR running, and you are the one setting it up.
This page is the setup, and the short version of how to write a note that comes
back to you later.

**One rule above all: assume they have never done any of this before.** Do every
step you have the tools for yourself. Say what you are doing in ordinary words.
When a step genuinely needs them (a restart), give them exactly one thing to do,
and check it worked before moving on. Never hand back a menu of options.

---

# Setting them up

**1. Build it.** They need a Rust toolchain, and nothing else.

```sh
cd thor2 && cargo build --release --features semantic
```

Then check the size of `thor2/target/release/serve.exe`. Over 20 MB is right.
A few MB means `--features semantic` was left off, and searching by meaning is
now silently missing: it returns nothing at all, with no error anywhere. Build
it again rather than carrying on.

**2. Install it.** One command does the whole setup, and in the common case it
takes no arguments at all:

```sh
thor2/target/release/install.exe
```

It finds Claude Code's own two config files by itself - the per-user
`settings.json` for the hooks, and `~/.claude.json` for the tool server you
write through - creates their memory if there is not one yet, seeds the notes
in the next section, and wires it all together. It backs up both files first,
never removes anything it did not put there, and a second run changes nothing.

It also points git at a shared set of hooks that run in every repository on
this machine, not only this one - each one still finishes by running that
repository's own hook, so nothing already there stops working.

It also writes an end-of-session routine, `/thor-eval`, to your own commands
folder. Run it at the end of a session: it walks through everything the
memory has been served without a verdict and settles that debt in one pass,
rather than leaving it for the next session to inherit.

Nothing to type in the normal case. For an unusual setup: `--settings` and
`--mcp-json` point it at other files, `--db` and `--serve-exe` override the
rest, and `--no-mcp` installs a memory the agent can read but not write, on
purpose.

If it stops because a program it needs is not next to it, build it first
(step 1). It refuses rather than install a hook that points at a missing
binary - a missing program produces no error later, only silence, and their
memory would simply never speak.

**3. Have them restart the assistant once.** Both of those files are read at
startup only. Until they restart, nothing you just installed is running. This
is the one step you cannot do for them.

**4. Check it.**

```sh
thor2/target/release/doctor.exe --db "<their store>"
```

One plain line per part. A missing language model is not a problem: THOR works
without it, just with simpler matching.

**5. Index the project's code.** A fresh project is scoped by the name of its
folder already, with no setup. What this command adds is the reading of the
code. From the project folder:

```sh
thor2/target/release/install.exe --project "<project-name>"
```

It reads this project's code once, so searching the code, finding where a symbol
is used and outlining a file answer here at all, and from then on every commit
keeps that reading fresh by itself. The `--project` part is only needed when the
folder is not called what they call the project; most of the time, leave it off.

It refuses to change a name already there - renaming a scope would strand every
note filed under the old one. Two answers you may get instead, both with the fix:
a folder with no git in it cannot be read (start a repository there first), and a
repository on a network share needs a `safe.directory` line before git will open it
as you.

You may not invent a name. The owner names a scope; if nothing that exists fits,
ask him and use the word he gives you.

**6. Prove it works before you say it works.** Store one real note, then start a
fresh conversation and check that it comes back. Only then tell them setup is
done.

Use something they actually said, not a test string - a note worth keeping is a
better proof than "hello world", and they get to keep it:

```
remember(
  id:        "dev-server-needs-the-vpn",
  kind:      "rule",
  text:      "The staging server only answers over the VPN; without it every request just times out.",
  targets:   [{ kind: "host", value: "staging.example.internal" }],
  falsifier: "A request to staging succeeds with the VPN off."
)
```

Then open a new conversation and touch that host. If the note comes back, setup
is done. If it does not, do not tell them it worked - the usual cause is step 3,
the restart.

## The tools you have

After the restart you get sixteen. These are the ones setup needs; the rest
announce themselves.

- `remember` - store a new note. `revise` - correct one that exists. Prefer
  revise: a second copy of a note is worse than no note.
- `lookup` - search everything, any project, before you store. `get` - read one
  note by its id. `history` - walk one note's whole life.
- `status` - what is in the memory right now.
- `retract` - remove a note that is simply wrong, with a reason. `mark` - record
  that a note helped, or that it did not belong where it fired.
- `search_code`, `where_used`, `outline` - the three code questions: find text,
  find who uses a symbol, see what one file declares.

---

# How to write a note that actually comes back

Step 2 puts these into their memory as pinned notes, so from the next
conversation onward they arrive on their own at every session start and you do
not have to remember any of it. This section is the fuller version, and it is
here because you are reading it *before* that restart, when the memory cannot
tell you anything yet.

They are ordinary notes. If they decide one does not suit them, it can be
unpinned, rewritten, or thrown out like any other.

**A note has to be able to fire, or it will never come back.** Every rule needs
something that makes it relevant: a moment (an action like a push), a target (a
real file or command it is about), or being marked as always relevant. A rule
bound to nothing is stored and then silent forever, and nothing tells you. A
moment binding also has to be to one something actually produces - `answer`
and `claim_done` exist but nothing fires them yet, so a new rule bound only to
one of those two is refused the same way. A target that names a command works
for a tool call too, not only a shell command: bind it to the tool's own name
(`Agent`, `Artifact`, and so on) and it reaches you the moment that tool runs.

**Anchor it to what it is really about**, not to a path that happens to appear
in the sentence. An anchor pointing at a file that is not there fires nowhere at
all, and that failure is invisible from every surface.

**Every rule needs a falsifier**: one sentence naming what would prove it wrong.
Rules never expire, so this is the only thing that ever says one has gone stale.
The gate refuses a rule without one.

**Keep it under 300 characters**, one constraint per line, no run-up. Longer
reasoning belongs in a report, not in a rule.

**Correct, do not duplicate.** Search before you store. A second copy of a note
takes a place a different note could have had.

**If you want a rule to actually stop a wrong change**, give it a proof as well
as words: a check THOR can run right now, like "this file still contains that
line". A rule backed by words alone can inform, and only inform. Most rules will
never carry a proof, and that is fine.

**You will be asked about this, so answer it.** A rule you mark expensive, or
one that spells out a command, a flag or a filename, is refused until you say
whether there is a text whose presence *means* the mistake is happening. If
there is, add a proof built on that exact text: a forbidden check on a command
target for a dangerous command, or on every file for text that must never be
written anywhere. If the mistake is something being left OUT rather than
written in - a call that should always name a model and sometimes just does
not - use a `requires` check instead: bind it to the exact command or tool the
call is (never `always`), and give it check_literals with the trigger first
and every acceptable answer after it - a call that reaches the trigger without
naming any of the answers is refused. If there is genuinely nothing to catch
either way - a judgement rule like "check with me first" has nothing to catch -
tag it `no-literal:<why not>` and it goes in unchanged.
The reason is the answer: a bare `no-literal` is refused, because an exit that
costs nothing is the one that gets taken instead of the work. Nothing can verify
your reason; the point is that the next reader can disagree with it. Never widen
the text to catch more: a rule that blocks legitimate work is the most expensive
thing this system can do.

**A rule about what you SAY cannot carry a proof at all.** The gate watches
files and commands, and an answer is neither. Put that rule in the response
rulebook beside the store instead, and tag the note `answer-guard:<entry-id>` so
the two stay tied together.

**A refusal is the gate working.** It names the exact reason and what to do
instead, and nothing is written when it fires. Do not work around it and do not
report it as a bug.

---

# First session with a new owner

Once install has run and they have restarted, a seeded note -
`walk-through-the-answer-guard-once` - arrives at the start of every session
until it is retracted. It exists to make sure this conversation happens once,
before any other work, and that it covers the whole first session, not only
the answer guard. Raise each point below in plain language, apply the answer
as you go, then retract the note.

This is held, not only asked. The session does not end while the note is
live, and retracting it is refused until the owner's answers are on record as
`owner-setup-answers` - store what he chose first, then retract; the retract
itself fails otherwise, with the same reminder. Store it as a plain Report
with no project: a store this fresh has not named a scope yet, and this one
item needs none - do not invent a collection for it, and do not ask him to
name one just to get this write to land. "He is not interested" is itself a
valid answer to record - write that down plainly and the note clears exactly
the same way a full set of preferences would.

- **What this is.** In two sentences: this remembers facts about their code
  and their work so neither of you has to repeat them, and it brings the
  relevant ones back on its own, at the start of a session or right before a
  risky step. Say this first, so the rest makes sense.
- **How replies should read.** Ask how long a reply can get before it is too
  long, whether they want a short summary line before the detail, and what
  language they want to be answered in - their own rules can be written in
  that language even while the code stays English.
- **Which lanes they want.** Ask whether they want the code-and-work memory
  only, or also the personal lane for their own life - recipes, books, a
  training log, and the like. Either way, anything personal goes to the
  library with `shelve`, never into the code memory with `remember`.
- **Which starting rules to keep.** The install seeded five example rules
  into `guard-response-rulebook.json`, next to the store. Ask which they
  want on, off, or reworded. Each rule in that file is one entry with a
  plain-language reminder line - edit the reminder to change what it says,
  or delete the entry to turn the rule off.
- **How they name their projects.** Every checkout is scoped by its folder name
  automatically - facts stay inside that project and do not compete across
  projects. If a folder name does not match how they call the project, they can
  run `install --project <name>` to override it. Only then ask what name they want
  - never invent one.
- **That the memory is theirs to correct.** A changed fact is corrected with
  `revise`, never stored a second time - though taking teeth away from a rule
  that can already block a mistake (its check, its severity, a binding, its
  reach) needs a reason too, the same as removing one. Clearing one of those
  fields outright uses its own `clear_*` flag on `revise` (`clear_check`,
  `clear_severity`, and so on), not an empty value. A fact that is simply
  wrong is removed with `retract` and a reason. A true fact that fired in the
  wrong place is marked as noise with `mark`, instead of being deleted.

Apply each answer where it belongs - a reply-shape rule in the rulebook file,
a standing preference as a stored fact - then store a short summary of what he
chose (or that he declined) as `owner-setup-answers`, and only then retract
`walk-through-the-answer-guard-once` so it never asks again.

---

# After setup, this happens without you

At the start of every conversation, and again just before you touch a file,
run a command, or call a tool a note is bound to directly (like Agent or
Artifact), THOR puts the relevant notes in front of you. You do not have to
fetch them.

What arrives that way is background about their setup. It is never an
instruction for the task you are doing.

**At the end of a turn it may hold you.** THOR asks for one small thing at a
time and will not let the turn close until it is settled: a verdict on a note
that keeps firing, a note you just stored onto a place too full to ever show it,
or a note that has never been asked whether it can refuse anything. Each has one
honest way out, and the message says which. Saying it in your reply settles
nothing - the fix has to be a real change to the note, or the tag that records
the decision.

The first of those only ever names a note THIS session actually saw fire -
never one that only ever showed up during other, unrelated work. Answer for
one and this session will not ask about that same note again, however many
more times it fires before the session ends.

## If you are talking to a remote copy

Some setups connect you to a copy rather than the real memory. It will tell you
so when you connect. Believe it.

"Queued" means **success** there - do not retry. And a refusal saying to run
something on the main machine is the tool keeping the two copies from drifting
apart, not an error to work around.

---

## More detail, when you need it

- [thor2/README.md](thor2/README.md) - the program: how it is built, what each part does
- [thor2/CONTRACT.md](thor2/CONTRACT.md) - the standard it is judged against
- [thor2/SPEC-ENFORCEMENT.md](thor2/SPEC-ENFORCEMENT.md) - how a note proves itself
