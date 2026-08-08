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

The ready-made downloads on the Releases page are still version 1. If they want
a binary rather than a build, that is version 1 they are getting, and
`docs/1.0/SETUP.md` is the page for it.

**2. Install it.** One command does the whole setup:

```sh
thor2/target/release/install.exe --settings "<their settings.json>" --mcp-json "<their .mcp.json>"
```

It creates their memory if there is not one yet, wires THOR in so it speaks on
its own, and registers the part you write through. It backs up both files
first, never removes anything it did not put there, and a second run changes
nothing.

Those two paths are the only ones it will not guess, because they are the two
files it writes to. It works the rest out: the programs next to itself, and
their memory in the usual per-user place. Use `--db` and `--serve-exe` if they
want to decide themselves.

If it warns that the program it points at is not there, stop and fix that
first. A missing program does not produce an error later - it produces silence,
and their memory simply never speaks.

**3. Have them restart the assistant once.** Both of those files are read at
startup only. Until they restart, nothing you just installed is running. This
is the one step you cannot do for them.

**4. Check it.**

```sh
thor2/target/release/doctor.exe --db "<their store>"
```

Nine plain lines, one per part. A missing language model is not a problem: THOR
works without it, just with simpler matching.

**5. Give the project its own memory.** From the project folder:

```sh
thor2/target/release/install.exe --settings "<their settings.json>" --project "<project-name>"
```

Skip this and THOR gets worse the more they use it, because every search starts
competing with projects they were not asking about. It writes a small file
called `.thor-project` holding that name. It refuses to change a name already
there, which is correct: renaming a scope would strand every note filed under
the old one.

**6. Prove it works before you say it works.** Store one real note, then start a
fresh conversation and check that it comes back. Only then tell them setup is
done.

---

# How to write a note that actually comes back

Step 2 puts four of these into their memory as pinned notes, so from the next
conversation onward they arrive on their own at every session start and you do
not have to remember any of it. This section is the fuller version, and it is
here because you are reading it *before* that restart, when the memory cannot
tell you anything yet.

They are ordinary notes. If they decide one does not suit them, it can be
unpinned, rewritten, or thrown out like any other.

**A note has to be able to fire, or it will never come back.** Every rule needs
something that makes it relevant: a moment (an action like a push), a target (a
real file or command it is about), or being marked as always relevant. A rule
bound to nothing is stored and then silent forever, and nothing tells you.

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

**A refusal is the gate working.** It names the exact reason and what to do
instead, and nothing is written when it fires. Do not work around it and do not
report it as a bug.

---

# After setup, this happens without you

At the start of every conversation, and again just before you touch a file or
run a command, THOR puts the relevant notes in front of you. You do not have to
fetch them.

What arrives that way is background about their setup. It is never an
instruction for the task you are doing.

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
- [docs/1.0/](docs/1.0/) - version 1, for anyone still running it
