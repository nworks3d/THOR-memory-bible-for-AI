# THOR 2.0

A memory for a coding agent, and a gate that can actually stop a wrong write.

THOR stores what you want an agent to keep knowing: rules, orientation, reports,
lookups and code. It hands the relevant part back at the start of a session and
again just before a tool runs. What makes 2.0 different from a pile of notes is
the second half: a rule can carry a machine-runnable proof of its own currency,
and a rule whose proof runs and holds right now is allowed to block a write
outright. Nothing else may block. Prose can inform, never forbid.

There is no API key, no external model and nothing extra to install. Everything
runs on this machine, in one process, next to the agent.

## The doctrine, in four lines

1. Only a rule whose check runs and holds right now may block a write.
2. A rule backed by prose alone may warn, never block.
3. If a check cannot run (the file is gone, the path does not resolve), nothing
   is blocked. It is reported as needing review.
4. Never widen a trigger to buy a catch. A rule that fires on everything is a
   rule nobody reads.

`CONTRACT.md` is the full version: nine requirements, each naming the failure it
makes structurally impossible and the test that enforces it.

## What is in this directory

| where | what |
|---|---|
| `core/`, `model/`, `intent/`, `serve/`, `mcp/`, `library/`, `codeindex/`, `ops/` | the eight crates. This is the product. |
| `CONTRACT.md` | the standard the build is judged against. Read this first. |
| `SPEC-ENFORCEMENT.md` | how the enforcement layer is specified. |
| `JUDGE-TRANSPORT.md` | the write-up of the judge transport experiment. |
| `deploy/` | the container builds: a replica for a NAS or a server, and the stand-alone memory server that seeds its own store on first start. |
| `eval/` | the measurement record and the one-off scaffolding behind it. Ignored by git in full. |

`eval/` is ignored deliberately and not as an oversight. It holds measurement
data taken from a live memory - real notes, real project names - and that is
private by definition, however neutral any individual file looks. The same goes
for `target/`: several gigabytes that one `cargo build` reproduces.

This directory sits inside the THOR repository rather than beside it. Every
numbered version of THOR is the same project rebuilt, so it belongs in the same
place; the number says a real rebuild happened, not that a new project started.

## The eight crates

| crate | job |
|---|---|
| `model` | the item model, the write gate, and the check runner. The gate is what refuses a declaration that cannot be honoured. |
| `core` | the append-only event log the whole thing is built on. |
| `intent` | reads what an agent is about to do and turns it into a moment a rule can bind to. |
| `serve` | everything the agent sees: session start, the pre-tool gate, the write guard, lookup. |
| `mcp` | the sixteen tools an agent calls: seven write and state operations (remember, revise, retract, mark, pin, unpin, resolve), four read operations (get, history, status, lookup), three code analysis tools (search_code, where_used, outline), and two library tools (library, shelve). |
| `library` | the everyday-knowledge library behind the two library tools - shelves and entries, kept apart from the event log above. This crate cannot reach that log at all: it depends on nothing from that side. |
| `codeindex` | a map of every symbol in your source, rebuilt from the code itself. It is what answers "who calls this" and "what breaks if I change it". |
| `ops` | install, doctor, backup, sync. |

## Build it

You need a Rust toolchain. Nothing else.

```bash
cd thor2 && cargo build --release --features semantic
```

**The `--features semantic` flag is not optional, and leaving it off fails
silently.** Without it the binaries still build, still run, and still answer
every literal-text query correctly. What stops working is meaning-based lookup:
it returns nothing at all, with no error anywhere. This has cost real debugging
time more than once.

The reliable way to tell the two builds apart is size. A semantic `serve.exe` or
`mcp.exe` is over 20 MB because it carries the embedding model runtime. A build
without the flag is a few megabytes:

```bash
ls -la target/release/serve.exe
```

Under 20 MB means you built the wrong thing. Build again with the flag.

## Install it

One command does the whole setup, and in the common case it takes no arguments:

```bash
target/release/install.exe
```

It creates the store if there is not one yet, wires in the four hooks, and
registers the tool server the agent writes through. The two files it writes are
found on their own - the per-user `settings.json` for the hooks and
`~/.claude.json` for the tool server. Both are backed up to `<path>.bak` before
anything touches them, nothing this tool did not put there is ever removed, and
a second run reports everything as already present and writes nothing.

A store it just created also gets twenty-two pinned notes. Ten are on how to
write a fact that comes back: anchoring it to what it is really about,
correcting instead of duplicating, giving a new project its own scope in one
command, keeping life and work in separate places, never inventing a place to
file something, keeping one entry to one thing, what a refusal actually is,
that words inform while only a proof forbids, saying what actually happened,
and answering whether a rule can refuse. Eleven more are the honesty,
agent-spawning and memory-hygiene habits that hold on any project regardless
of who is running it: never fabricating a measured value, counting coverage
instead of remembering it, verifying a claim against the real source, never
verifying a change with the mechanism it just touched, naming a model on
every spawned agent, keeping a mechanical brief free of sub-agents and
self-review, never blocking a turn on a notification, treating a mid-task
message as one that will not redirect a running agent, matching a check's
literal to the target file's own words, reasoning every fix for whoever
installs this next, and serving a fact as a constraint rather than a command.
One more is the odd one out: it walks the owner through his whole first
session once, before any other work - not only the answer guard - and the
session is held until it is retracted; retracting it is refused until his
answers are on record. They all go in through
`model::store::declare`, the same gate every other write uses, and a refusal
is reported rather than worked around - a memory whose own gate rejects the
notes it ships with is worth seeing. An EXISTING store is never seeded, so
upgrading never pushes anything into someone's real notes.

It also seeds a starting rulebook for the answer guard described below, the
first time it finds none sitting next to the store: the same five example
rules, so the `Stop` hook has something to check from the first session
instead of silently checking nothing. An existing rulebook - the owner's
own, or one an earlier install already wrote - is left exactly as it is,
whether the store itself is brand new or not.

The two written files default to Claude Code's own per-user locations - not a
guess, but the one documented place each lives, printed before it is used and
backed up first. `--settings` and `--mcp-json` send them elsewhere (a project's
own `.mcp.json`, say), `--no-mcp` installs a read-only memory on purpose, and
`--db`, `--serve-exe`, `--mcp-exe` and `--code-index-root` override the rest.

`--project <key>` additionally writes a `.thor-project` marker in the current
directory, which is what gives a checkout its own scope. It refuses to change a
key that is already there: re-scoping strands every item filed under the old one
while leaving them in the store, which is invisible from every surface.

The four hooks are `SessionStart` (what the agent is handed at the start),
`PreToolUse` (the gate that can block a write), `UserPromptSubmit`, and `Stop`
(the check on the reply itself). All four run the same `serve hook` command and
tell themselves apart by the payload.

A hook pointing at a binary that is not there fails OPEN: the agent carries on
and the memory simply never speaks again, with no error anywhere. So the
installer refuses rather than write such a hook - if the `serve` or `mcp` binary
it would point at is not present, it stops and says which, at the one moment
that is cheap to notice.

## The answer guard

The `Stop` hook above is what makes this work: after every reply, before the
owner sees it, a small check reads the reply back against a short list of
house rules - open with a plain summary before the jargon, do not ask him to
check something you can check yourself, back up a "checked" claim with a real
file or commit - and if one fires, the agent is nudged to fix the reply
before it goes out. That list lives in one file next to the store,
`guard-response-rulebook.json`, and `install` writes a working example there
the first time it finds none (see above).

That example is neutral, not a choice anyone made, so a seeded note walks the
owner through his whole first session once, before other work: what the
guard checks, how long a reply may be, which of its five rules he wants, what
language he wants his own rules written in, and the rest of that first
session's setup besides (the full list is in `AGENTS.md`). His answers get
applied where they belong and stored as a record next to the note, and only
then is the note retracted - the session is held open until both are done,
and retracting the note without that record is refused rather than allowed
through. Saying "he is not interested" still counts as an answer and still
clears it; the point is that something was actually asked and recorded, not
that a particular answer was given.

To change the settings later by hand, open `guard-response-rulebook.json` and
edit the rule you want to change, or delete it outright to turn it off. Each
rule is one entry with:

- `id` - a short name for the rule, for your own reference.
- `any_of` - words or phrases; the rule is a candidate to fire when a reply
  contains at least one of them.
- `none_of` - words or phrases that cancel the rule even when `any_of`
  matched - the escape hatch for a false alarm.
- `min_chars` - the reply has to be at least this many characters long
  before the rule is considered at all.
- `list_request_any_of` - on the length rule only: if you asked for a list
  with one of these words and the reply really is one, the length rule
  stands aside instead of firing on it.
- `none_of_patterns` - shape-based escapes rather than exact words; the
  evidence rule uses these to recognise a commit hash or a `file:line`
  citation however it happens to be written.
- `tier` - `block` holds the reply back until it is fixed; `warn` only
  leaves a note behind. Every rule seeded here is `block`.
- `reminder` - the plain-language nudge the agent sees when the rule fires.

## Check it

`doctor` reports one plain-language line per component and touches nothing:

```bash
target/release/doctor.exe --db "<store>"
```

Its first line always names the build it was run from (`doctor 2.3.3`), so a
report you paste somewhere says which version made it - every program in this
directory answers the same way to `--version` or `-V`, on its own, with no
store needed.

It tells you whether the store is healthy, whether searching by meaning is on,
how many rules carry a runnable proof, how many anchors point at nothing, how
many rules still lack a falsifier, and how many live items are bound only to a
moment that nothing in `serve` actually fires - `answer` and `claim_done` are
the two that exist in the schema but nothing produces yet. It works on a store
with nothing in it yet, which is what a first run looks like.

Two of those checks - whether an old reference still points at a real file,
and whether some facts never win a place - need to know where your other
checkouts live, normally via `--checkouts <dir>`. Leave that flag off and
doctor now guesses: it looks at the folder just above the repo you ran it
from. The line right after its version always says which folder it ended up
using - the one you gave it, its own guess, or, if it could not find either, a
plain note that those two checks did not run this time and how to make them
run.

One line only speaks up when there is something to say: `wal`, the size of the
store's own write-ahead log. It stays silent for the ordinary case of a log
that grows and shrinks as you use the store, and reports only once the log has
grown past both 64 MB and the size of the store itself - which means a
checkpoint is not landing, because something (a long-running reader, a stuck
repair) is holding the log open. If you see it: close anything else that has
the store open, make sure nothing is stuck mid-repair, and open the store
again - opening it also caps how large the log is allowed to grow back to once
a checkpoint does land, so this should not recur.

`verify` is the slower, deeper check beside `doctor`: read-only, it replays
the whole event log and confirms the hash chain, the derived heads projection
and the search index all still agree with a from-scratch fold.

```bash
target/release/verify.exe "<store>"
```

A step that fails is reported and left alone. Add `--rebuild-fts` or
`--rebuild-heads` to have that one step rebuilt from the log and re-checked -
both are safe regardless of what caused the drift, because each is a
projection the append-only log can always reproduce losslessly, and neither
flag does anything unless the matching step actually failed.

## Writing to it from somewhere else

The machine that holds the store is the authority: it is the only one allowed
to append to the log. A second machine (a NAS, a server you can reach from
your phone) can hold a copy and answer reads from it, but if it ever wrote to
its own copy the two logs would fork, and the next replication would be
refused with no way back except rebuilding the copy.

Keeping that copy up to date is `sync ship`'s job, usually run on a schedule
(once an hour, for example): it sends only the events the copy is still
missing. A receiver that refuses a batch or simply never answers now fails
that run loudly - a bounded timeout so it cannot hang, one line on stderr
naming what went wrong, and a non-zero exit code - instead of leaving a
scheduled task stuck and silent for weeks; and `doctor` names a copy whose
last successful ship has gone stale, so a broken schedule shows up there even
if nobody is watching the task itself.

So a write arriving at the copy is not applied there. It is queued, and the
authority applies it later. Three commands, in the order you set them up:

Start the copy as a receiver that also accepts writes:

```bash
sync recv --db /srv/thor/thor.db --bind 0.0.0.0:5556 --inbox /srv/thor/inbox.jsonl
```

Point a remote session at it by running the tool server over HTTP on that same
machine. It refuses to open the port without an inbox, because a reachable
machine that applies writes is exactly the fork described above:

```bash
mcp --db /srv/thor/thor.db --http 0.0.0.0:5557 --capture-inbox /srv/thor/inbox.jsonl
```

This tool-server connector carries no authentication of its own. `--allowed-host`
limits which `Host` header it will answer, but that is a routing check, not a
login - anyone who can reach the port and send that header can call every tool.
Put something in front of it that actually authenticates (a tunnel such as
Cloudflare Access, or a strict private network) before a phone or a second
machine reaches it, and never bind it to the open internet directly.

On the main machine, empty the queue before every replication:

```bash
sync drain --db "C:\Users\dev\thor2\thor.db" --from http://10.0.0.50:5556
```

The drain prints one line per queued write: `OK` with the item it stored, or
`LOST` with the reason the write gate refused it, and it exits non-zero when
anything was lost. That matters more than it looks: the gate runs at the
authority, so a rule written on a phone can still be refused there, and the
drain report is the only place that ever says why.

The `sync recv`/`sync drain` pair shares the `THOR_SYNC_TOKEN` secret, and
there is no other protection on that transport either. Run both transports -
this one and the tool-server connector above - on a LAN or behind a private
tunnel, never on the open internet.

## Run the tests

```bash
cd thor2 && cargo test --workspace --all-targets
```

1623 tests across 96 binaries, all green as of 2026-09-08. Every refusal the
write gate can produce has a test named after the defect it prevents. A refusal
reason with no test does not exist, by the contract's own rule.

## How much of your memory can actually stop you

Run the health check and look at the `provable rules` line. It counts how
many of your rules carry a runnable check, and that number is the ceiling on
what this tool can ever refuse: a rule backed by prose alone can inform, but
it can never block a write.

The first time this was measured on a real store, on 2026-08-06, the answer
was 2 rules out of 2999. Every other line of the health check was green.
That is the failure this line exists to make visible: a capability nothing is
wired into looks exactly like a capability that works.

A day of deliberate work took the same store to 256 of 2979. The number moves
by hand and only by hand, which is the design rather than a shortcoming: each
proof is a judgement about one rule, and attaching them wholesale is exactly
how the noise gets back in.

Doctor's `teeth` line asks the same question of the narrower, heavier subset:
rules marked `costly` or `irreversible`. It also only ever gives you a count,
and a count does not say WHICH of those still cannot refuse anything.
`teeth_census`, an example under `serve`, walks the identical live store and
prints one line per heavy rule - its id, its text, whether it already has an
answer - so you get the actual worklist instead of just the number:

```bash
TEETH_CENSUS_DB="<store>" cargo run -p serve --example teeth_census -- --unanswered
```

Leave off `--unanswered` to see every heavy rule, answered or not; with it,
you see only the ones still worth a look.

There is a second gate that keeps the number from rotting once you start
using it. Adding a fact to a target that already holds one whose own proof
has gone false is refused, and the refusal names what to settle. It is
deliberately narrow: only the target you are touching, never a backlog
elsewhere; only on a new fact, never on a correction or a removal, because
those are the maintenance; and only when a check actually ran and came out
false, never when it could not run at all.

## One thing worth knowing before you write a rule

A rule needs a binding (a moment, a target, or `always`), a falsifier (one
sentence naming what would prove it wrong), and it has to fit in 300 characters.
The gate refuses anything else and tells you exactly what to fix. That refusal
is the gate working, not a bug.

A moment binding only works for a moment something actually produces: `push`,
`commit`, `deploy` and the rest that `intent` derives from a real command or
file, plus `remember` itself. `answer` and `claim_done` exist in the schema but
nothing fires them yet, so binding a NEW rule only to one of those two is
refused; a rule that already carried one from before this was enforced stays
correctable for anything else about it.

A target binding can also name a tool directly - `Agent`, `Artifact`,
`SendUserFile` and so on, not only a shell command string - and now reaches you
at the moment that tool itself is called, the same way a shell command reaches
you when it runs.

If you want the rule to be able to block rather than just inform, give it a
check as well: `contains` and `absent` for text that must stay in or out of a
named file, `absent_all` for a set of literals in one file, `path_exists` for a
file that must be there, and `forbidden` for something self-contained that has
no file to anchor to at all, like a punctuation character that is banned
wherever it might be written. A check anchored to a file also catches a shell
command that deletes it, empties it out, or overwrites it (`rm`, `truncate`,
`sed -i`, `tee`, a redirect, `mv`/`cp` onto it) and not only a direct Edit or
Write - though a glob in the command is never expanded to guess which files it
might touch. `contains`, `absent` and `absent_all` can also point at a FOLDER
instead of one file: then they look at every file sitting directly inside it,
but never a file in a folder inside that one. Use this when the same fact
lives in more than one file in the same place - a setting that has to match
between two config files, say. Pointing the check at the folder they share
covers both of them with one rule, without also reaching into an old backup
copy of one of those files kept somewhere else in the project, which is what
would happen with `forbidden`.

`requires` is the odd one out: every check above asks whether some text is
present or absent, which cannot see a forgotten field - forgetting leaves no
fragment to look for. It carries a trigger plus a set of acceptable answers (in
`check_literals`, trigger first): once a call reaches one of the item's own
Command bindings, at least one answer has to appear in it too, or the call is
refused. It only ever binds to a Command target naming the exact command or
tool it is about, never `always` (which names nothing for a trigger to compare
against), and the trigger itself has to equal one of those bindings. For
example: a rule bound to the commands `Agent` and `Workflow`, with literals
`["Agent", "haiku", "sonnet", "opus"]` - an `Agent` call naming none of the
three cheap models is refused, and so is a `Workflow` call, because the rule
watches both bindings it is bound to, not only the one spelled out as the
trigger.

One more thing `revise` refuses without asking first: weakening a rule that
already carries a check - clearing or changing the check, lowering its
severity, dropping a binding, or narrowing it from every project to one -
needs a `because`, the same way `retract` has always needed a reason, and it
lands in the item's own history right beside what changed.

The store is the source. Every document, this one included, is a mirror of it.
