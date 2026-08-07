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

## What is in this repository

| where | what |
|---|---|
| `thor2/` | the Rust workspace. This is the product. |
| `CONTRACT.md` | the standard the build is judged against. Read this first. |
| `PLAN-NEXT.md` | the roadmap that is still open. |
| `SWITCH.md` | step by step, in Dutch: how to switch a live 1.0 setup over to 2.0. |
| `SPEC-ENFORCEMENT.md` | how the enforcement layer is specified. |
| the other `*.md` at the root | the measurement record. Every number 2.0 claims was measured, and these are the write-ups: A/B tests, blind hold-outs, recall batteries, guard evaluations. They are kept because a claim without its measurement is a habit. |
| `harness/`, `probe/`, `slices/`, `tools/` | one-off measurement scaffolding, not shipped. |
| `nas-pakket/`, `rollback/`, `reuse/` | operational scratch from the 1.0 to 2.0 move. |

The build directories (`target/`, `target-p*/`) and every data directory
(`out*/`, `store-copy*/`, `migrated*/`, `fixtures/`) are ignored by git. They
hold real private memory, or six gigabytes of build output that one `cargo
build` reproduces.

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
ls -la thor2/target/release/serve.exe
```

Under 20 MB means you built the wrong thing. Build again with the flag.

## Check that it works before you install anything

`doctor` reports one plain-language line per component and touches nothing:

```bash
thor2/target/release/doctor.exe --db "C:\Users\dev\thor2\thor.db"
```

It tells you whether the store is healthy, whether the code index is current,
whether the replica is reachable, and how many rules still lack a falsifier.

## Install it into Claude Code

THOR hangs off three hooks: `SessionStart` (what you get handed at the start),
`PreToolUse` (the gate that can block a write), and `UserPromptSubmit`.

The `install` binary wires all three into an agent's `settings.json` for you. It
writes a `.bak` copy of that file first, so the step is reversible:

```bash
thor2/target/release/install.exe --settings "C:\Users\dev\.claude\settings.json" --serve-exe "C:\Users\dev\thor2\bin\serve.exe" --db "C:\Users\dev\thor2\thor.db"
```

`--settings` deliberately has no default. A tool that rewrites a config file
should never guess which one.

The hooks alone let THOR hand facts to the agent and stop a wrong write. To also
let the agent write to its own memory, register `mcp.exe` as a tool server with
the same `--db`, in the same settings file. `SWITCH.md` walks through both
files line by line, in Dutch, with the exact blocks to paste and how to undo
each step.

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

1007 tests across 71 binaries, all green as of 2026-08-06. Every refusal the
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
