![The AI Memory Bible](assets/banner.png)

# THOR - a memory for your AI coding assistant

**Your assistant forgets you the second you close the window. THOR does not.**

Tell it once:

- *never deploy on a Friday*
- *the invoice number goes in the payment reference, never in the description*
- *the dough is 65% water and rests overnight, not two hours*

Weeks later, in a conversation that has never heard of any of it, the right one
comes back on its own - while you are deploying, while you are invoicing, while
you are making dough. You did not search for it. You did not remind anyone.

It was built for code and it turned out not to care what the subject is. The
same memory holds your deploy rules, how your company does its billing, and what
you learned the last time you made pizza.

Runs on your own machine. No account, no key, nothing sent anywhere.

---

THOR is free and GPLv3. If it earns its keep, you can support the work:
**[buy me a Ko-fi](https://ko-fi.com/noizieworks)** or
**[become a YouTube member](https://www.youtube.com/channel/UCrEZc_oJR9mywNjqY115mRg/join)** -
no pressure, no paywall, everything stays open either way.

> **Are you an AI assistant, setting THOR up for the person you work with?** Go
> straight to **[AGENTS.md](AGENTS.md)**. It is written for you.

[![Watch: THOR explained on YouTube](https://img.youtube.com/vi/tCQOUdkhErQ/maxresdefault.jpg)](https://youtu.be/tCQOUdkhErQ)

*Prefer watching? The video above walks through what THOR is and why it exists.*

## What it does

**Remembers what you tell it.** A rule, a gotcha, a decision, the shape of the
project. Once, in your own words. It stays until you change it.

**Hands it back at the right moment.** Not a search box you have to remember.
The note arrives while you are touching the file or running the command it is
about, in a conversation that never heard it.

**Stops a wrong change, not just warns about it.** A note carrying something
checkable can refuse the write outright. Most notes only inform, and that is
deliberate: a rule that blocks honest work is the most expensive thing this
system can do.

**Keeps projects apart.** Every project has its own memory. One repo's rules
never leak into another.

**Says when it has rotted.** It counts its own dead ends: notes pointing at
files that moved, notes nothing ever reads, notes crowded out by louder ones.
Out loud, in plain language, so you can fix them.

**Stays on your machine.** No account, no key, no server. Nothing is sent
anywhere, ever.

## What that looks like in practice

Months ago you found out the hard way that this project is pinned to an older
Node, and anything newer breaks the build. You said so once, and moved on.

Today a fresh conversation opens `package.json` to add a dependency, sees an
engine range that looks out of date, and is one helpful edit away from bumping
it. Right then, before it types, your own sentence is in front of it.

That is the whole idea. Not a search box you remember to use. A memory that
shows up on time.

### "I already have a CLAUDE.md for that"

Most assistants read a rules file at startup - `CLAUDE.md`, `AGENTS.md`,
`.cursorrules`. It helps, and it runs out of road quickly.

Past a certain size it becomes a phone book, and nobody reads a phone book front
to back. Your assistant skims it, takes the gist and moves on. The rule was in
there. It got skipped. Nothing looks wrong afterwards, because the line is still
sitting in the file, so you go on believing you are covered.

Here is the part that is genuinely different. Picture a fresh agent, no history,
no idea what this project has already cost you, one keystroke away from the
exact write that broke production last spring. A rules file would have mentioned
it somewhere on page four. THOR stops the keystroke. The write does not happen -
and what stops it is the note *you* wrote, the day it broke.

That is the whole promise: not better advice, but a wrong change that does not
land.

Getting there is not free, and it is worth knowing before you start. A note only
earns that power if it is written to earn it: tied to a real file or command,
carrying something checkable that shows it still applies. THOR ships with a
handful of starting notes that teach exactly that, and refuses the ones that
cannot work. [AGENTS.md](AGENTS.md) spells out the rules of the game in full.

Three things make that work, and they all live in one file on your machine:

**1. Nothing is ever lost.** Every note is kept forever. Change your mind and
the old version stays too, so you can always look back at what you used to
think and when it changed. If two versions of a note ever conflict, THOR keeps
both and tells you, rather than quietly picking one and throwing the other away.
It is the same care you would give your source code, given to the things you
know.

**2. It arrives at the right moment.** THOR checks your memory on every message
you send. And the first time your assistant reaches for a file or runs a command
that one of your rules is about, that rule gets put in front of it right then.
Before the mistake, not after.

**3. Your assistant looks after it.** It is not a notebook you have to fill in
by hand. Your assistant can add notes, correct them, retire ones that stopped
being true, and flag which ones actually helped. A THOR that is used well is a
THOR your assistant is quietly tidying as you work.

It reads your code too. Point it at a project and it takes in the source and the
documentation, so "how does this bit work here" gets answered from your actual
project instead of a guess.

Everything stays on your machine. No cloud, no account, no subscription, and
nothing to sign up for. If some optional piece is missing, THOR quietly falls
back to a simpler way of working instead of breaking.

## A note that can actually stop you

Showing a warning at the right moment is worth a lot, and for a long time that
was all THOR could do. A note could speak. It could not refuse.

Version 2 lets a note carry a **proof of its own currency**: a small check THOR
can run right now to see whether the note is still true of your project. "This
file still contains that line." "That file is still there." "This character
never appears in anything we write."

That changes what a note is allowed to do:

- A note whose proof **runs and holds right now** may stop a wrong change
  outright.
- A note backed by words alone may warn, and only warn. It can inform your
  assistant; it can never forbid.
- If the proof **cannot run** - the file moved, the path is gone - nothing is
  blocked. It is reported as needing a look.

The reason for the split is uncomfortable and worth saying out loud. Notes rot.
You write one, the project moves on, and the note quietly becomes wrong. A tool
that let any old note block your work would spend most of its time blocking you
for reasons that stopped being true months ago. So THOR only hands that power to
notes that can prove, at that exact second, that they still describe your
project.

Which is why, as the top of this page already said, most of your notes will
never block anything. The health check prints how many can, and you should look
at it. On the author's own memory, when this was first measured, 2 notes out of
2999 could prove themselves; a day of deliberate work took that to 256. It moves
by hand, because deciding what proves a note is a judgement about that one note.

That the number is printed at all is the point: a safety net nothing is attached
to looks exactly like a safety net that works.

### THOR asks, so you do not have to remember to

Leaving that to whoever thinks of it means it never happens. So THOR asks, by
itself, in two places.

**When a note is written.** A note you call expensive, or one that spells out a
command, a flag or a filename, is not stored until one question is answered: is
there a text whose presence *means* the mistake is happening? If there is, the
note gets a proof built on exactly that text. If there is not - and often there
is not, because "check with me first" has nothing to catch - you say so and the
note goes in unchanged. Both are real answers. Only saying nothing is not.

**For the notes you already had.** Once per session, THOR picks one note that
names something concrete, has never been asked, and holds the turn until it is.
One at a time, forever, so a memory written before any of this existed still
gets worked through instead of being declared hopeless.

A caution worth stating plainly: THOR can prove that a note is *wired* so a
matching change would be stopped. It cannot know whether the text you typed is
the text the real command uses. A misspelled fragment is wired perfectly and
guards nothing. That is why the health check reports two different numbers - how
many notes could refuse something, and how many ever actually did. Trust the
second one.

## What changed in version 2

Version 1 remembered well and never argued. It would hand your assistant a note
at the right moment and hope. Version 2 is the same memory with a spine.

- **A note can refuse.** The headline, and the rest of this section is about it.
  Version 1 could only speak.
- **Bad notes no longer get in.** A note that cannot ever fire, has nothing that
  would prove it wrong, runs on for a page, or simply repeats one you already
  have: turned away at the door, with the reason and the fix. Version 1 stored
  whatever it was given, and you found out months later that half of it was
  unreachable.
- **It counts what it actually did.** How many notes can refuse something, how
  many ever have, how much of your memory nothing re-reads. Version 1 had no
  number for the one thing it was built to do, which is how a safety net stays
  broken for a year.
- **Maintenance is no longer optional.** At the end of a turn it asks for one
  thing: judge a note that keeps firing, fix one you just filed where it will
  never be seen, answer whether an expensive note can refuse anything. One at a
  time, and it will not be waved off with a promise to do it later.
- **It costs almost nothing to carry.** A note is capped at 300 characters and a
  block at four notes and 1200 characters, so what lands in the conversation is
  a few hundred tokens, not a whole rules file re-read on every turn. A memory
  of ten thousand notes costs the same per turn as one of fifty. Nothing calls
  out to a model to decide what to send - it is a local program reading a local
  file, so there is no network round trip in front of your keystroke. The one
  exception is the handful you deliberately pin: those are read out in full at
  the start of a conversation, so pin sparingly and the cap does the rest.
  Measured on the author's own machine and memory: about 110 ms per call and
  around 1500 characters delivered when something applies, against roughly 660
  ms for version 1. One machine, one memory - treat it as an order of magnitude,
  not a specification.
- **Stale notes are hunted, not left to rot.** A note whose proof comes out
  false is reported instead of quietly going on being wrong. A note that keeps
  firing without anyone ever saying whether it belonged gets asked about, and
  two verdicts of "this did not belong" retire it from every channel while
  leaving it findable. In version 1 a note that went wrong simply stayed.
- **Crowding is visible and refused.** Only a few notes fit in a block, so notes
  compete. Version 2 counts that competition, tells you when you have just
  stored something onto a spot too crowded to ever show it, names what is
  holding the place, and refuses the write outright when every spot the note
  could take is already full of heavier ones. Version 1 accepted it and said
  nothing, which is how a memory fills up with advice nobody will ever see.

## New in 2.1: a second memory, for everything that is not code

The memory above is built for work. It has a gate, notes that interrupt you,
and a hard cap on how much ever reaches the conversation - all of which is
exactly wrong for a recipe.

So 2.1 adds a library, and it is a genuinely separate thing: its own file, its
own two commands, and no way to reach the first memory at all. Nothing you put
in it can ever interrupt you, compete with a note, or take up room in a block.
You only ever see it because you asked.

It works the way a shelf works.

- **Everything lives on a shelf**, and shelves do not nest. Books, recipes, a
  training log, what you spent. Filing something without naming a shelf is
  refused, and the refusal lists the shelves you have, so your assistant picks
  from real ones instead of inventing a name.
- **Only you create a shelf.** If nothing fits, your assistant has to ask you
  what the new one should be called. This is the rule that stops a tidy list of
  eight from becoming a sprawl of sixty.
- **A shelf that grows gets labels, never a split.** Two hundred recipes on one
  shelf, filtered by "bbq" or "dessert", stays one shelf. That is what keeps the
  list of shelves short enough to hold in your head.
- **You get an index, not a wall of text.** Open a shelf and you see one line
  per entry. Ask for one by number to read it whole.
- **The same thing twice is refused**, pointing at the entry you already have.
- **Nothing is ever deleted.** Retiring an entry takes it out of the listing and
  leaves it readable.
- **A search never answers "nothing".** If your words miss - and they will, since
  the words you ask with are rarely the words you wrote - it hands you the shelf
  to read instead. Asking for "ribbetjes" when you wrote "ribben" finds it.

## Getting started

**The short way - one command.** It downloads the latest release, checks the
download against the checksum published next to it, unpacks it into your home
folder, and runs the whole setup.

Windows, in PowerShell:

```powershell
irm https://raw.githubusercontent.com/nworks3d/THOR-memory-bible-for-AI/main/install.ps1 | iex
```

Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/nworks3d/THOR-memory-bible-for-AI/main/install.sh | sh
```

No administrator rights, and nothing is installed outside your own user folder.
It touches two files of yours, your assistant's settings and the list of tools
it may use, and backs up both before it does. Rather read the script before you
run it? Open that same link in a browser first. There is no macOS build yet, so
on a Mac take the route below.

**Or build it yourself.** You need a Rust toolchain. Nothing else: no key to
get, no model to download first, no account.

```sh
cd thor2 && cargo build --release --features semantic
```

> **`--features semantic` is not optional, and leaving it off fails silently.**
> Without it everything still builds, still runs, and still answers every
> word-for-word search correctly. What stops working is searching by meaning: it
> returns nothing at all, with no error anywhere. The reliable way to tell the
> two apart is size. Look at `thor2/target/release/serve.exe` - over 20 MB is
> the right build, a few MB is the wrong one. Build it again with the flag.

Then run the setup yourself. That is the same step the one-command install ends
with, and in the normal case you type no paths at all:

```sh
thor2/target/release/install.exe
```

It finds your assistant's own two configuration files by itself, creates your
memory if you do not have one yet, wires THOR into your assistant, and registers
the part your assistant writes through. It backs up both files before it touches
them, it never removes anything it did not put there, and running it twice
changes nothing the second time.

A brand new memory does not arrive empty. It gets a handful of short notes on
how to write a note that comes back to you later, and your assistant is handed
them at the start of every conversation from then on. That matters more than it
sounds: an assistant with nothing in front of it writes notes in a shape that
never fires again, and neither of you would notice for weeks. They are ordinary
notes - unpin one, rewrite it in your own words, or throw it out. An existing
memory is never seeded, so upgrading never pushes anything into your own notes.

You only reach for a flag if your setup is unusual: `--settings` and `--mcp-json`
send it at other files, `--db` and `--serve-exe` override where it looks, and
`--no-mcp` sets up a memory your assistant can read but not write, on purpose.
If a program it needs is missing, it stops and says so rather than installing
something that would sit silent.

**Then restart your assistant.** The part it writes through only comes alive
after a restart. Until then, it can already read the memory but not add to it.

**Step 3 - check it.**

```sh
thor2/target/release/doctor.exe --db "C:\Users\you\AppData\Local\thor2\thor.db"
```

Nine plain-language lines, one per part: whether your memory is healthy, whether
searching by meaning is switched on, how many of your notes can prove
themselves, and how many point at files that are no longer there. It changes
nothing.

**Step 4 - give each project its own memory.** From that project's folder:

```sh
thor2/target/release/install.exe --project "my-project"
```

This matters more than it sounds. Skip it and THOR gets *worse* the more you use
it, because every search starts competing with projects you were not asking
about. All it does is write a small file called `.thor-project` holding that
name, so you can also just create that file yourself. It refuses to change a
name that is already there, because renaming a project's scope would strand
every note already filed under the old one.

If you are the assistant doing the setup, [AGENTS.md](AGENTS.md) is the
walkthrough for the steps above.

## Stay in one conversation

The old advice was to start a fresh chat often, because long ones got worse and
you lost everything anyway. With THOR that advice is out of date. **One long
conversation is now the better habit.**

When a conversation gets long, the assistant's tools squeeze out the older parts
to make room. THOR covers that moment: your standing rules come straight back,
and it nudges your assistant to write down anything important that was never
saved. Starting a fresh chat is covered too - your rules and your project's
background are loaded in from the start.

So stay in one conversation while you are on one piece of work. Start a fresh
one on purpose - because you have moved on to something else, or because this
one has talked itself into a corner - not because it is getting long.

## What the first week actually looks like

Worth knowing before you start, because the beginning is the least impressive
part and it is easy to conclude too early that nothing is happening.

**Day one, it stops nothing.** A fresh memory holds a handful of starting notes
about how to write notes, and nothing else. The part of THOR that can refuse a
wrong change only works on notes that carry a proof, and you have not written
any yet. So on the first day you get those notes at the start of a conversation
and a nudge at the end, and no refusals at all. That is not a fault; there is
simply nothing yet to refuse with.

**Your first note will probably be turned down.** It asks for two things most
people leave out: when the note should come back to you, and what would show it
had gone wrong. If the note is about something expensive, or names a command or
a filename, it asks a third: is there a text whose presence means the mistake is
happening? Answering "no, there is nothing to catch here" is enough, and often
it is the truthful answer. The refusal names everything that is missing at once
and says what to write instead, so the second attempt usually lands. It is
strict on purpose - a note nobody can ever prove wrong is a note that quietly
stops being true.

**The value arrives once you have notes about real places.** A note tied to a
file, a folder or a command comes back exactly when you touch that thing. A
handful of those is worth more than fifty general ones, and after a week or two
of writing them down as you go, your assistant stops asking you the same
questions.

Then check what you have built with `doctor`. It tells you plainly how much of
your memory can actually stop a wrong change, how often it has, and which parts
nothing ever re-reads.

### If you arrive with a memory you already have

Everything above describes a memory that starts empty. If you are coming from
version 1, or from any pile of notes written before proofs existed, the shape is
different and worth saying plainly, because the obvious plan does not work.

THOR asks you about one old note per session. That is a brake, not a broom. It
keeps the pile from growing while you work, and it was never meant to clear one:
at one a day, a backlog of thousands outlives you.

The broom is the health check, pointed at the folder your projects actually live
in:

```bash
doctor --db <your thor.db> --checkouts <the folder holding your projects> --full
```

That names every note whose anchor points at nothing, every proof that now comes
out false, and every note stored somewhere too crowded to ever be shown. Set
aside an afternoon rather than a coffee, and go through it in one sitting. The
list is long because the memory is old, not because anything is broken.

Two things make that afternoon safe to be decisive in. Correcting a note keeps
the old version, so nothing you wrote is lost and you can always read back what
it used to say. And removing one does not delete it either: it stops being
handed to anyone, stays findable, and the reason you gave stays attached to it.

## Does it work?

Use it for a week and see whether your assistant stops asking you the same
things. That is the only test that answers the question you actually have.

THOR was measured head to head against another memory tool for months, and those
numbers are not here any more. Not because they were bad - they were good - but
because a score measured on someone else's notes tells you about them, not about
you. The tool is here. The verdict is yours.

## Documentation

| page | what it answers |
|---|---|
| [AGENTS.md](AGENTS.md) | for your AI assistant: how to set THOR up |
| [thor2/README.md](thor2/README.md) | the version 2 program: how it is built and what each part does |
| [thor2/CONTRACT.md](thor2/CONTRACT.md) | the standard version 2 is judged against, and the test enforcing each rule |
| [thor2/SPEC-ENFORCEMENT.md](thor2/SPEC-ENFORCEMENT.md) | how a note proves itself, in detail |
| [CONTRIBUTING.md](CONTRIBUTING.md) | changing THOR: the bar for a pull request |

## Thanks

- **MakerViking** - for the inspiration and the great fight. This project would
  not exist without the spark, and it would not be half as good without someone
  worth pushing against. Skål!
- **mimir** ([MakerViking/mimir](https://github.com/MakerViking/mimir)) - the
  reason THOR exists at all. In the old Norse stories, Mimir guards the well of
  knowledge; here it set the bar THOR had to clear, and for a long stretch it
  cleared plenty of its own. Every early comparison in this project was against
  mimir, wins and losses both published on purpose, because a rival that good
  deserves honest numbers.
- **Ideas borrowed, both ways.** Two things THOR does came from mimir's own
  work and were rebuilt here in THOR's own way, and mimir in turn credits THOR
  for reading code into memory and for checking memory on every message -
  exactly the kind of exchange open source is for. Thanks, MakerViking.

## Support this project

THOR is built by [N-Works 3D](https://www.youtube.com/channel/UCrEZc_oJR9mywNjqY115mRg). If it has
earned its keep - saved you an explanation, caught a mistake before it cost you,
or just meant you did not have to start from scratch - there are two easy ways
to help keep it going:

- **[Buy me a Ko-fi](https://ko-fi.com/noizieworks)** - a one-off, whenever you
  feel like it.
- **[Become a YouTube member](https://www.youtube.com/channel/UCrEZc_oJR9mywNjqY115mRg/join)** -
  monthly, if you want to keep it going.

No pressure and no paywall - it all stays open either way. Skål, and thanks for
reading this far.

## Contributing

Bug reports and pull requests welcome. THOR is a memory your assistant is
supposed to trust, so the bar is being right rather than having more features.
The checklist is in **[CONTRIBUTING.md](CONTRIBUTING.md)**.

## License

GPLv3.
