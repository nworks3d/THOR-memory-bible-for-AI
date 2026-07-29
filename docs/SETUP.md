# Setting up THOR, start to finish

For a person or for an AI assistant doing it on their behalf. It assumes you have
never done any of this. Every command here is safe to run a second time.

A word on two names you will see throughout, so nothing below is a surprise:

- **Keyword search** - THOR finding a note because it contains words you typed.
  This always works and needs nothing from you.
- **Meaning search** - THOR also finding a note that means the same thing in
  different words. This is optional, needs a language model file you supply
  yourself, and is covered in step 3.

If the second one is missing or broken, THOR quietly uses the first. You lose
that layer and nothing else.

---

## 1. Get the program

### The easy way: download it

Take the file for your system from [Releases](../../../releases). Next to each
download is a `.sha256` file - a short code you can use to confirm the download
arrived intact.

| download | what you get | who it is for |
|---|---|---|
| `thor-windows-x86_64.zip` | keyword + meaning search | your own Windows computer |
| `thor-linux-x86_64.tar.gz` | keyword + meaning search | your own Linux computer |
| `thor-linux-x86_64-bm25.tar.gz` | keyword search only | a server or a NAS |

Unpack it and **put the program where it is going to live permanently before you
go any further.** Step 2 writes down wherever it is at that moment, so moving it
afterwards means extra work. Somewhere on your PATH is the simple choice. On
Windows, a tidy home is `%LOCALAPPDATA%\thor\thor.exe`, which the examples below
use.

If you do move it later, running step 2 again is not enough on its own. It leaves
the old entries behind - pointing at a file that is no longer there - and adds a
second set beside them. Open your assistant's `settings.json`, delete the THOR
lines with the old path, then run step 2 again.

**Read this before the first command fails on you.** Every example in these pages
is written as `thor something`, which is short and readable but only works as
typed if the program sits in a folder your shell already searches. That tidy
Windows home above is *not* one of those folders. So either write the path out
each time, in PowerShell:

```powershell
& "$env:LOCALAPPDATA\thor\thor.exe" doctor
```

(the leading `&` is what tells PowerShell that a quoted string is a program to
run, not text to print), or spend a minute now and never think about it again:
put that folder on your PATH, and every example works exactly as printed. On
Windows: Settings, then "Edit environment variables for your account", then Path,
then New. Open a fresh terminal afterwards - an already-open one keeps the old
PATH. Check it took with `thor doctor`.

**Two things the download does not include:**

- **The language model** for meaning search. You supply that yourself in step 3.
  Without it, THOR uses keyword search and works fine.
- **On Windows: Microsoft's Visual C++ Redistributable.** This is a free one-time
  install from Microsoft that many programs need. If THOR refuses to start at
  all, this is almost always why.

### The other way: build it yourself

```sh
cd thor
cargo test
cargo build --release --features semantic
```

The program appears at `thor/target/release/thor`. Same advice about giving it a
permanent home.

**Do not leave off `--features semantic`** unless you are building for a server.
Without it you get a smaller program that can only do keyword search - about
10 MB instead of about 35 MB. It still runs, and nothing warns you, so the file
size is your check.

### Replacing a copy that is already running

Parts of THOR keep the file open, so writing over it fails. Move the old one
aside first:

```sh
mv "$LOCALAPPDATA/thor/thor.exe" "$LOCALAPPDATA/thor/thor.exe.old"
cp <the new file> "$LOCALAPPDATA/thor/thor.exe"
```

The per-message memory check starts fresh every time, so it uses the new version
straight away. The longer-running parts keep the old one until they restart -
which for the connection to your assistant means restarting your assistant.

---

## 2. Connect it to your assistant

```sh
thor install --with-courier --with-guard --with-daemon
```

That is the full setup, and it belongs on the machine your assistant runs on.

It edits your assistant's `settings.json`. It makes a backup first, only ever
touches its own lines, and never disturbs anything another tool put there.

Here is what each piece does, so nothing is a black box:

- **A memory check on every message you send.** THOR looks for anything relevant
  and quietly puts it in front of your assistant.
- **A warm-up when a conversation starts**, so the first message is not slow.
- **A refresh of the project index** when a conversation starts. If you open a
  project THOR does not know yet, it prompts your assistant to offer to set it up
  rather than indexing your files behind your back.
- **A warning at the moment of action.** The first time your assistant touches a
  file or runs a command that one of your rules is about, that rule appears.
- **A check before your assistant finishes replying.** Unlike the others this one
  can hold the reply open so your assistant reconsiders. Only when a rule
  actually applies; the rest of the time you never notice it.
- **Right after a long conversation gets squeezed down**, one reminder: save
  anything important that was never written down, and say which of the things
  THOR showed you were actually useful.

**About `--with-daemon` and memory use.** This keeps one background process
running with your memory already loaded, which cuts the wait on every message by
roughly two thirds (measured at 349 down to 120 milliseconds). It costs a few
hundred MB of RAM. That number has never been measured properly for this
project, so watch your own task manager rather than trusting a figure here. Drop
it if the RAM matters more than the wait - everything still works, just slower.

One thing to know before you make that trade: THOR can end up running **two**
background processes and they are unrelated. `--with-daemon` starts the one just
described. The other one holds the language model from step 3, and THOR starts it
by itself as soon as a model is present. Leaving off `--with-daemon` does not
avoid that second one. The only way to avoid it is to not install a model.

Optional: add `--backup-repo <path to a git clone>` for a daily backup.

---

## 3. Meaning search (worth it on your own machine)

Keyword search is always on. This adds the ability to find a note that says the
same thing in different words - useful when you half-remember what you wrote.

Skip it if you are setting up a server or a NAS, or if you cannot spare about
650 MB of RAM. If anything about it is missing or broken, THOR falls back to
keyword search on its own.

Honest about what it buys: it clearly helps on notes you wrote by hand, and makes
little difference on indexed source code.

The downloads for Windows and Linux can already do this. What they need from you
is the model file.

Put the model in a folder called `model/` inside THOR's own folder - the same
place your memory file lives:

- Windows: `%LOCALAPPDATA%\thor\model\`
- Linux and macOS: `$HOME/.local/share/thor/model/`

Then:

```sh
thor vectors build
thor vectors status
```

The first reads through everything you have stored once. The second confirms it
worked.

**Which model?** Any sentence-embedding model in ONNX format, with its tokenizer,
that produces 384 numbers per piece of text. That width is fixed in THOR and a
different one is refused. A multilingual MiniLM is a good default and handles
more than one language.

You never wait for the model to load while typing. A background process keeps it
ready. `thor warm` starts that process, and THOR also starts it by itself: the
first message that finds it down starts it in the background and answers using
keyword search in the meantime.

---

## 4. Keeping projects apart - do not skip this

THOR keeps everything in one file, but keeps your projects **separate**, while
letting things that apply everywhere show up everywhere. This is what stops one
project's details turning up in an unrelated conversation.

**The project is the folder your conversation started in.** So start each
conversation in the right folder.

**Set up a project:** run `thor init` in its folder. In a git project it reads
only the files git tracks, so anything in your `.gitignore` - your secrets, your
keys - is never read.

If you forget and just start working in a fresh folder - one with no git
repository and no `.thor` marker yet - your assistant notices at the start of the
session and offers to run this for you, instead of quietly saving everything to
the shared global memory. You can decline if it is only a scratch folder.

**Searches stay inside the current project**, plus anything filed as applying
everywhere. To reach further:

```sh
thor recall --all-projects "..."          # everything
thor recall --project <name> "..."        # one specific project
```

Your assistant has the same two options through its own tools.

**Documents that apply to every project:**

```sh
thor ingest --global <folder>
```

**Keep them all in one folder.** Each time you run this, THOR treats that folder
as the complete list, and withdraws anything filed this way that the folder does
not contain. So pointing it at a second folder removes the first folder's
documents. Passing two folders in one command does the same thing - the second
undoes the first. And you cannot rely on spotting it, because the withdrawal step
is skipped when the folder could not be read properly, so it does not even go
wrong the same way twice.

**A folder that is not a git project** works too: `thor ingest <folder>` reads
the text files directly, skipping hidden folders and anything huge. Point it at
documentation. The one gap: a password sitting in a plain file directly in that
folder would be read, because there is no `.gitignore` to protect it.

**When the folder name is not the project name** - a backup copy, an export - pin
it: `thor ingest --project <name> <path>`. Design and 3D files (STEP, STL,
Gerber and friends) are always skipped so they cannot drown your actual notes.

**Something filed under the wrong project?**

```sh
thor reproject <id> --project <name>
```

**A safety net.** Notes saved from somewhere with no project context land in the
everywhere pile. `thor review-scope` lists those, and once a day THOR nudges your
assistant to go through them with you. Nothing moves without your say-so.

**Separating projects never costs you anything.** Inside the right project you
find exactly what an unfiltered search would find. You only lose the other
projects' clutter.

One ordering note if you run THOR on more than one machine: upgrade all of them
before the first time you move a note between projects. An older copy cannot read
that change.

---

## 5. Reaching THOR from a phone or the web (optional)

`thor/deploy/` has a `Dockerfile` and a `docker-compose.yml` to fill in. Run
`thor mcp --http 0.0.0.0:<port>` in the container, keep it on an internal
network, and put a reverse proxy with a login in front of it. The connection has
no password of its own, so that proxy is the security.

---

## 6. Check that it works

```sh
thor doctor
thor fsck
thor recall "how does X work"
```

**`thor doctor`** is the first thing to run after installing, and the first thing
to include in a bug report. It says plainly what is present and what is missing,
and names the exact folder it looked in for the model.

**`thor fsck`** checks that nothing in your memory has been damaged. Six `OK`
lines means healthy. It fails loudly rather than silently, so it is safe to put
in a backup script.

If it reports `FTS INTEGRITY ERROR`, the search index is damaged - a bad disk, an
interrupted copy. **Your memory itself is fine**, because the index is rebuilt
from it:

```sh
thor fsck --rebuild-fts
thor fsck
```

A complaint about a footer is something else entirely. That is about the shape of
a note, never a failure, and it does not stop anything.

**One thing `doctor` does not check:** the process holding the language model.
Its `injection daemon:` line is about the other background process. So if meaning
search seems to do nothing while `doctor` says the model is there, that is the
piece to look at - check for a file called `thor-embedd.json` next to your memory
file, and run `thor warm` if it is not there.

---

Everything this guide left out because it is optional - reranking, syncing to a
second machine, the rulebooks, the off switch, the tidying commands - is in
[OPTIONAL-FEATURES.md](OPTIONAL-FEATURES.md), each with a reason to turn it on
and a reason not to.
