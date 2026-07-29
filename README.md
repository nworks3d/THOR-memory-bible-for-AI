![The AI Memory Bible](assets/banner.png)

# THOR - a memory for your AI coding assistant

THOR is free and GPLv3. If it earns its keep, you can support the work:
**[donate via PayPal](https://www.paypal.com/paypalme/ognoizieworks)** or
**[become a YouTube member](https://www.youtube.com/@NoizieWorks/join)** -
no pressure, no paywall, everything stays open either way.

> **Are you an AI assistant, setting THOR up for the person you work with?** Go
> straight to **[AGENTS.md](AGENTS.md)**. It is written for you.

[![Watch: THOR explained on YouTube](https://img.youtube.com/vi/tCQOUdkhErQ/maxresdefault.jpg)](https://youtu.be/tCQOUdkhErQ)

*Prefer watching? The video above walks through what THOR is and why it exists.*

## The problem

AI coding assistants forget everything. Close the window, and it is gone. Even
in one long conversation, the older parts get squeezed out to make room.

You know the feeling if you have worked with one for more than a day:

- You explain your project again. And again.
- You say "never do that on the live server" - and three days later it does it,
  because that sentence lived in a chat that no longer exists.
- It makes a mistake you already caught and fixed weeks ago.

Writing it all down in a notes file does not fix it, because nobody opens the
notes file at the moment it would have helped.

## What THOR does about it

THOR keeps what you tell it, and hands the right piece back to your assistant at
the moment it needs it - without you asking.

Say you once told it: *"the database must never be opened over the network
drive, it corrupts."* Weeks later, in a brand new conversation, your assistant
starts editing the config file that points at that database. Right then, before
it types anything, that sentence appears in front of it.

That is the whole idea. Not a search box you remember to use. A memory that
shows up on time.

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

Everything stays on your machine. One program, one file. No cloud, no account,
no subscription. And if some optional piece is missing, THOR quietly falls back
to a simpler way of working instead of breaking.

## Getting started

**Step 1 - get the program.** Download it from
[Releases](../../releases). Pick `windows-x86_64` or `linux-x86_64` for your own
computer (on Windows you also need Microsoft's Visual C++ Redistributable, a
free one-time install from Microsoft). Pick `linux-x86_64-bm25` if you are
putting it on a small server or a NAS. Each download has a matching `.sha256`
file so you can check it arrived intact.

Rather build it yourself? `cd thor && cargo build --release`.

**Step 2 - connect it to your assistant.** One command:

```sh
thor install --with-courier --with-guard --with-daemon
```

It backs up your settings first, only adds its own lines, and you can safely run
it again later.

**Step 3 - introduce it to a project.** From that project's folder:

```sh
thor init
```

That is it. From here on, your memory gets checked on every message, and your
assistant can save new things as you work.

Forget this step and just start working in a fresh folder? Your assistant now
notices that the folder is not a project yet and offers to set it up first, so
your notes get their own scope instead of piling up in the shared memory every
project sees. A brand-new folder with no git repository used to get no such
hint - a poor first impression that is now fixed.

**Step 4 - and this is the one people skip.** Ask your assistant to read
**[AGENTS.md](AGENTS.md)**. It is one page, written for it rather than for you.

Skipping this is the difference between a memory that works and a memory that
quietly does nothing. An assistant that has not read it will save notes in a
shape that never comes back to it later - and neither of you will notice for
weeks, because a memory that fails does it silently. Ten minutes here.

### What that command switches on

No menu to work through - it turns on the lot, which is what the author runs:

- **A memory check on every message** - the whole point. Without it you have a
  notebook you must remember to open, which is the problem you started with.
- **A warning at the moment you act** - the one thing searching cannot do. Your
  question rarely mentions the trap; the file path always does.
- **A check before your assistant finishes replying**, for the rules that are
  about how it answers rather than about a file.
- **A background process that keeps it quick** - roughly two thirds off the wait
  on every message, for a few hundred MB of memory. The only one worth leaving
  out, and only if memory is tight; everything still works, just slower.

Then **step 3 keeps your projects apart**, which matters more than it sounds:
skip it and THOR gets *worse* the more you use it, because every search starts
competing with projects you were not asking about.

A few things are not switched on for you only because they need something you
have to fetch or choose - the model file for searching by meaning, somewhere to
back up to, a second machine to sync with. Set them up too.
[docs/FEATURES.md](docs/FEATURES.md) walks through each one, and is straight
about the only two you can genuinely skip and about what got tried and thrown
away.

**Never done any of this before?** Read these in order. They assume nothing:

1. **[docs/FEATURES.md](docs/FEATURES.md)** - what each part does, in plain
   words, and whether it is worth your time.
2. **[docs/SETUP.md](docs/SETUP.md)** - the full walkthrough, one step at a
   time.
3. **[AGENTS.md](AGENTS.md)** - how to work with it well. For your assistant,
   and worth ten minutes of your own time too.
4. **[docs/OPTIONAL-FEATURES.md](docs/OPTIONAL-FEATURES.md)** - the extras. What
   each one costs, how to switch it on, how to undo it.

## Stay in one conversation

The old advice was to start a fresh chat often, because long ones got worse and
you lost everything anyway. With THOR that advice is out of date. **One long
conversation is now the better habit.**

When a conversation gets long, the assistant's tools squeeze out the older
parts to make room. THOR covers that moment: your standing rules come straight
back, it hands the assistant the list of things it remembered for you so far so
it can say which were useful, and it nudges it to save anything important that
was never written down. Starting a fresh chat is covered too - your rules and
your project's background are loaded in from the start.

So stay in one conversation while you are on one piece of work. Start a fresh
one on purpose - because you have moved on to something else, or because this
one has talked itself into a corner - not because it is getting long.

One honest note: the "which of these were useful" moment only happens when a
conversation gets long enough to need squeezing. A short chat never reaches it.
Nothing breaks and nothing is lost; THOR just learns what is useful to you a
little slower.

## Documentation

| page | what it answers |
|---|---|
| [AGENTS.md](AGENTS.md) | for your AI assistant: how to set THOR up and how to use it well |
| [docs/FEATURES.md](docs/FEATURES.md) | what does each part do, and should I care? (plain words, no commands) |
| [docs/SETUP.md](docs/SETUP.md) | the full walkthrough, for someone who has never done this |
| [docs/OPTIONAL-FEATURES.md](docs/OPTIONAL-FEATURES.md) | the extras: what each costs, switching on, checking, undoing |
| [docs/REFERENCE.md](docs/REFERENCE.md) | the technical depth: how it is built, every command, every setting |
| [CONTRIBUTING.md](CONTRIBUTING.md) | changing THOR: the bar for a pull request |

## Does it work?

Use it for a week and see whether your assistant stops asking you the same
things. That is the only test that answers the question you actually have.

THOR was measured head to head against another memory tool for months, and those
numbers are not here any more. Not because they were bad - they were good - but
because a score measured on someone else's notes tells you about them, not about
you. The tool is here. The verdict is yours.

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

THOR is built by [N-Works 3D](https://www.youtube.com/@NoizieWorks). If it has
earned its keep - saved you an explanation, caught a mistake before it cost you,
or just meant you did not have to start from scratch - there are two easy ways
to help keep it going:

- **PayPal**: https://www.paypal.com/paypalme/ognoizieworks
- **YouTube members**: https://www.youtube.com/@NoizieWorks/join

No pressure and no paywall - it all stays open either way. Skål, and thanks for
reading this far.

## Contributing

Bug reports and pull requests welcome. THOR is a memory your assistant is
supposed to trust, so the bar is being right rather than having more features.
The checklist is in **[CONTRIBUTING.md](CONTRIBUTING.md)**.

## License

GPLv3.
