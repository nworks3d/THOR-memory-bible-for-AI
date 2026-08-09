![The AI Memory Bible](assets/banner.png)

# THOR - a memory for your AI coding assistant

**In one line:** your AI assistant forgets everything between sessions. THOR
remembers for it, and reminds it at the exact moment it is about to get
something wrong.

Picture a new colleague who is brilliant, fast, and has no memory at all. Every
morning you explain the project from scratch. You tell them on Monday never to
touch the live server; on Thursday they touch the live server, because Monday
never happened for them.

You cannot fix that by writing a document. Nobody opens a document at the second
they are about to make the mistake. THOR does the opening for you: you say a
thing once, and it comes back on its own when it matters - when that file is
opened, when that command is about to run.

Three things change for you:

- **You stop repeating yourself.** The same explanation, the same warning, the
  same preference: said once.
- **Some mistakes become impossible, not just discouraged.** A note can be given
  a proof. A note that can prove it still applies is allowed to refuse the
  change outright instead of merely warning about it.
- **You can see what it is actually doing.** It prints how much of your memory
  can stop a mistake, and how often it really has. No dashboard-flattery.

It runs entirely on your own machine. No account, no key, no data leaving your
computer, nothing sent anywhere.

---

THOR is free and GPLv3. If it earns its keep, you can support the work:
**[buy me a Ko-fi](https://ko-fi.com/noizieworks)** or
**[become a YouTube member](https://www.youtube.com/channel/UCrEZc_oJR9mywNjqY115mRg/join)** -
no pressure, no paywall, everything stays open either way.

> **Are you an AI assistant, setting THOR up for the person you work with?** Go
> straight to **[AGENTS.md](AGENTS.md)**. It is written for you.

[![Watch: THOR explained on YouTube](https://img.youtube.com/vi/tCQOUdkhErQ/maxresdefault.jpg)](https://youtu.be/tCQOUdkhErQ)

*Prefer watching? The video above walks through what THOR is and why it exists.*

## What that looks like in practice

Say you once told it: *"the database must never be opened over the network
drive, it corrupts."* Weeks later, in a brand new conversation, your assistant
starts editing the config file that points at that database. Right then, before
it types anything, that sentence appears in front of it.

That is the whole idea. Not a search box you remember to use. A memory that
shows up on time.

It is worth being clear about why a notes file is not the same thing. A notes
file is only as good as someone remembering to open it, and nobody opens one at
the second they are about to get something wrong. Being on time is the entire
feature.

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

## What version 2 adds: a note that can actually stop you

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

There is an honest consequence: **most of your notes will never be able to block
anything, and that is by design.** The health check prints the number, and you
should look at it. On the author's own memory, when this was first measured, 2
notes out of 2999 could prove themselves. A day of deliberate work took that to
256. It moves by hand and only by hand, because deciding what proves a note is a
judgement about that one note.

That number being printed at all is the point: a safety net nothing is attached
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

## Getting started

**Step 1 - build it.** You need a Rust toolchain. Nothing else: no key to get,
no model to download first, no account.

```sh
cd thor2 && cargo build --release --features semantic
```

> **`--features semantic` is not optional, and leaving it off fails silently.**
> Without it everything still builds, still runs, and still answers every
> word-for-word search correctly. What stops working is searching by meaning: it
> returns nothing at all, with no error anywhere. The reliable way to tell the
> two apart is size. Look at `thor2/target/release/serve.exe` - over 20 MB is
> the right build, a few MB is the wrong one. Build it again with the flag.

The downloads on the [Releases](../../releases) page are still version 1. If you
want a ready-made binary rather than a build, that is version 1 you are getting,
and [docs/1.0/SETUP.md](docs/1.0/SETUP.md) is the page for it.

**Step 2 - install it.** One command does the whole setup:

```sh
thor2/target/release/install.exe --settings "C:\Users\you\.claude\settings.json" --mcp-json "C:\Users\you\.mcp.json"
```

It creates your memory if you do not have one yet, wires THOR into your
assistant, and registers the part your assistant writes through. It backs up
both files before it touches them, it never removes anything it did not put
there, and running it twice changes nothing the second time.

A brand new memory does not arrive empty. It gets four short notes on how to
write a note that comes back to you later, and your assistant is handed them at
the start of every conversation from then on. That matters more than it sounds:
an assistant with nothing in front of it writes notes in a shape that never
fires again, and neither of you would notice for weeks. They are ordinary notes
- unpin one, rewrite it in your own words, or throw it out. An existing memory
is never seeded, so upgrading never pushes anything into your own notes.

Those two paths are the only ones it will not guess, because they are the two
files it writes to. Everything else it works out on its own: the programs next
to itself, and your memory in the usual per-user place for your system. Name
them yourself with `--db` and `--serve-exe` if you would rather decide.

**Then restart your assistant.** It reads both of those files once, when it
starts. Until you restart it, nothing you just installed is running.

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
thor2/target/release/install.exe --settings "C:\Users\you\.claude\settings.json" --project "my-project"
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
| [docs/1.0/](docs/1.0/) | version 1, kept for anyone still running it |

## Thanks

- **MakerViking** - for the inspiration and the great fight. This project would
  not exist without the spark, and it would not be half as good without someone
  worth pushing against. Skål!
- **mimir** ([MakerViking/mimir](https://github.com/MakerViking/mimir)) - the
  reason THOR exists at all. In the old Norse stories, Mimir guards the well of
  knowledge; here it set the bar THOR had to clear, and for a long stretch it
  cleared plenty of its own. Every early comparison in this project was against
  mimir, wins and losses both published on purpose, because a rival that good
  deserves honest numbers. Moving from mimir? `thor/tools/export_mimir.py`
  brings your notes across.
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
