# AGENTS.md - read this if you are the AI assistant

The person you work with wants THOR running, and you are the one setting it up.
This page is the setup, and only the setup.

How to *use* the memory well is deliberately not here. THOR seeds that into the
memory itself during the steps below, as a pinned rule, so it arrives in your
context at the start of every conversation - without anyone having to remember
to open a file. A page can be skipped. What the memory hands you cannot.

**One rule above all: assume they have never done any of this before.** Do every
step you have the tools for yourself. Say what you are doing in ordinary words.
When a step genuinely needs them (a download, a restart), give them exactly one
thing to do, and check it worked before moving on. Never hand back a menu of
options.

---

# Setting them up

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

If that comes back "command not found", the program is simply not in a folder the
shell searches - which is normal, since the usual home for it on Windows is not on
PATH. Do not go hunting or reinstall: run it by its full path
(`& "$env:LOCALAPPDATA\thor\thor.exe" doctor` in PowerShell, the `&` included) and
keep doing that for every command on this page. Mention to them once that adding
the folder to PATH makes the short form work, and leave that choice to them.

**3. The language model, if they want it.** It makes THOR find things by meaning
rather than only by matching words. They supply the file themselves (about
235 MB); nothing downloads on its own. [docs/SETUP.md](docs/SETUP.md) walks
through it. If you are unsure, skip it for now - it can be added any time.

**4. Connect it to their assistant.**

```sh
thor install --with-courier --with-guard --with-daemon
```

This is what makes memory arrive on its own instead of only when someone asks
for it. It backs up their settings first and is safe to run again. This is also
where THOR seeds its working contract, so watch for the line saying it did.

**5. Register THOR as a tool** so you can read and write memory yourself:

```sh
claude mcp add thor -- <path-to>/thor.exe mcp
```

Then have them restart once, so it gets picked up.

**6. Introduce it to their project.** Run `thor init` in the project folder. It
marks the folder and reads the project's files into memory.

**7. Offer them a few example rules.** Their memory is empty at this point, so
there is nothing for you to find and nothing to show them what a rule worth
keeping looks like. `thor starter-pack` puts three short working rules in it -
finish what you start, split work by what it costs to be wrong, let nothing leave
the machine without being asked. They arrive unpinned, so they change nothing on
their own; say that plainly, and say they can pin the ones they like, rewrite
them, or throw them out. Do not pin any of them for them.

**8. Prove it works before you say it works.** Run `thor doctor` again, then look
something up that you know is in there. Only then tell them setup is done.

---

# After setup, this happens without you

At the start of every conversation THOR hands you its working contract - how to
store things so they actually come back, what to anchor, when to correct
something instead of saving a second copy - along with any standing rules this
person has pinned. You do not have to fetch it and you should not go hunting for
a document version of it.

To read it directly anyway: `thor get thor-working-contract`. It is an ordinary
note, so they can edit it, and unpinning it makes it stop arriving. THOR seeds
it once and then leaves it alone.

## If you are talking to a remote copy

Some setups connect you to a read-only copy rather than the real memory. It will
tell you so when you connect. Believe it.

"Queued to capture inbox" means **success** there - do not retry. And a refusal
saying "run this on the authority" is the tool protecting itself from getting out
of step, not an error to work around.

---

## More detail, when you need it

- [docs/SETUP.md](docs/SETUP.md) - the unhurried setup walkthrough
- [docs/FEATURES.md](docs/FEATURES.md) - what every part does, in plain words
- [docs/OPTIONAL-FEATURES.md](docs/OPTIONAL-FEATURES.md) - the extras and what they cost
- [docs/REFERENCE.md](docs/REFERENCE.md) - every command, and how it is built
