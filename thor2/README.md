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
| `core/`, `model/`, `intent/`, `serve/`, `mcp/`, `codeindex/`, `ops/` | the seven crates. This is the product. |
| `CONTRACT.md` | the standard the build is judged against. Read this first. |
| `SPEC-ENFORCEMENT.md` | how the enforcement layer is specified. |
| `JUDGE-TRANSPORT.md` | the write-up of the judge transport experiment. |
| `deploy/` | the container build for running a copy on a NAS or a server. |
| `eval/` | the measurement record and the one-off scaffolding behind it. Ignored by git in full. |

`eval/` is ignored deliberately and not as an oversight. It holds measurement
data taken from a live memory - real notes, real project names - and that is
private by definition, however neutral any individual file looks. The same goes
for `target/`: several gigabytes that one `cargo build` reproduces.

This directory sits inside the THOR repository rather than beside it. Every
numbered version of THOR is the same project rebuilt, so it belongs in the same
place; the number says a real rebuild happened, not that a new project started.

## The seven crates

| crate | job |
|---|---|
| `model` | the item model, the write gate, and the check runner. The gate is what refuses a declaration that cannot be honoured. |
| `core` | the append-only event log the whole thing is built on. |
| `intent` | reads what an agent is about to do and turns it into a moment a rule can bind to. |
| `serve` | everything the agent sees: session start, the pre-tool gate, the write guard, lookup. |
| `mcp` | the fourteen tools an agent calls: remember, revise, retract, recall, lookup, and the three code tools. |
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

One command does the whole setup:

```bash
target/release/install.exe --settings "<agent settings.json>" --mcp-json "<.mcp.json>"
```

It creates the store if there is not one yet, wires in the four hooks, and
registers the tool server the agent writes through. Both files are backed up to
`<path>.bak` before anything touches them, nothing this tool did not put there
is ever removed, and a second run reports everything as already present and
writes nothing.

A store it just created also gets four pinned notes on how to write a fact that
comes back: anchoring, correcting instead of duplicating, what a refusal is, and
that words inform while only a proof forbids. They go in through
`model::store::declare`, the same gate every other write uses, and a refusal is
reported rather than worked around - a memory whose own gate rejects the notes
it ships with is worth seeing. An EXISTING store is never seeded, so upgrading
never pushes anything into someone's real notes.

Those two paths have no defaults, on purpose: they are the two files this
command WRITES to, and a tool that rewrites a config file should never guess
which one. Everything else is worked out - the binaries next to the installer,
and the store in the per-user data directory. `--db`, `--serve-exe`, `--mcp-exe`
and `--code-index-root` override each of those.

`--project <key>` additionally writes a `.thor-project` marker in the current
directory, which is what gives a checkout its own scope. It refuses to change a
key that is already there: re-scoping strands every item filed under the old one
while leaving them in the store, which is invisible from every surface.

The four hooks are `SessionStart` (what the agent is handed at the start),
`PreToolUse` (the gate that can block a write), `UserPromptSubmit`, and `Stop`
(the check on the reply itself). All four run the same `serve hook` command and
tell themselves apart by the payload.

A hook pointing at a binary that is not there fails OPEN: the agent carries on
and the memory simply never speaks again, with no error anywhere. The installer
prints a warning when the path it is about to write does not exist yet, which is
the only moment that is cheap to notice.

## Check it

`doctor` reports one plain-language line per component and touches nothing:

```bash
target/release/doctor.exe --db "<store>"
```

It tells you whether the store is healthy, whether searching by meaning is on,
how many rules carry a runnable proof, how many anchors point at nothing, and
how many rules still lack a falsifier. It works on a store with nothing in it
yet, which is what a first run looks like.

## Writing to it from somewhere else

The machine that holds the store is the authority: it is the only one allowed
to append to the log. A second machine (a NAS, a server you can reach from
your phone) can hold a copy and answer reads from it, but if it ever wrote to
its own copy the two logs would fork, and the next replication would be
refused with no way back except rebuilding the copy.

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

On the main machine, empty the queue before every replication:

```bash
sync drain --db "C:\Users\dev\thor2\thor.db" --from http://10.0.0.50:5556
```

The drain prints one line per queued write: `OK` with the item it stored, or
`LOST` with the reason the write gate refused it, and it exits non-zero when
anything was lost. That matters more than it looks: the gate runs at the
authority, so a rule written on a phone can still be refused there, and the
drain report is the only place that ever says why.

Both halves share the `THOR_SYNC_TOKEN` secret, and there is no other
protection. Run it on a LAN or a private tunnel, never on the open internet.

## Run the tests

```bash
cd thor2 && cargo test --workspace --all-targets
```

1043 tests across 72 binaries, all green as of 2026-08-07. Every refusal the
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

If you want the rule to be able to block rather than just inform, give it a
check as well: `contains` and `absent` for text that must stay in or out of a
named file, `absent_all` for a set of literals in one file, `path_exists` for a
file that must be there, and `forbidden` for something self-contained that has
no file to anchor to at all, like a punctuation character that is banned
wherever it might be written.

The store is the source. Every document, this one included, is a mirror of it.
