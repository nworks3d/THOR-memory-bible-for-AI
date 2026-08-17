# De bibliotheek

Everyday knowledge - recipes, books read, a training log, expenses - kept
completely apart from the code lane. Written down before a line of it exists,
because the point is the constraints, not the feature.

## What this is NOT

It is not a scope, not a project, not a new item kind, and not a field on an
existing item. Every one of those was tried in conversation and every one of
them ends the same way: the everyday facts and the code facts land in one list,
and then they compete. The owner's requirement, stated three times and finally
in capitals, is that this stands FULLY apart from code and design work and
never competes with the guard, the four slots, or the global rules.

So: its own file, its own doors, its own rules. Nothing in the event store
knows it exists, and no injection surface can reach it - not because it is
filtered out, but because it is not there to filter.

## The lessons this is built from

Each of these is a measured failure of the existing system, recorded in the
store itself. They are the reason for every rule below.

1. **Crowding hides things silently.** More anchored facts than slots means the
   best-ranked win and the rest vanish without a word. Measured on a busy
   command: an anchor there yields nothing at all.
2. **A cap that says nothing reads as "that is all there is."** A search
   returned ten of hundreds and looked complete.
3. **A holder nobody can discover is a write-only holder.** Two register items
   existed for months; nothing could ask which keys existed, so they were never
   once served.
4. **A reminder does not work; a gate does.** A rule that was in the store AND
   injected still went unfollowed. Only refusing the write changed behaviour.
5. **An optional field nobody fills stays empty.** The scope field was optional,
   so 1362 items carry none - not by choice, by default.
6. **Near-duplicates accumulate** when nothing checks before writing.
7. **Whole-word matching fails on ordinary language.** "ribbetjes" finds
   nothing while "ribben" finds three. The words a person uses to ask are not
   the words they used to write.
8. **Facts that store a value instead of a pointer rot silently.**

## The shape

**One shelf, one flat list.** A shelf holds entries. There are no shelves
inside shelves - nesting is exactly how the code side sprawled.

**An entry is a title plus a body.** The title is one line and is what the
index shows; the body is the whole thing. A shelf listing is one line per
entry, so a shelf of two hundred is still readable at a glance.

**Labels grow inside a shelf, never as new shelves.** When a shelf gets big,
entries get labels and you filter on them. The number of shelves stays flat
while the content grows - this is the answer to lesson 1, applied before the
crowding can start rather than after.

## What the librarian enforces

1. An entry names a shelf that already exists, or it is refused - and the
   refusal lists the shelves, so being refused costs one more call, not three.
   (Lessons 4 and 5: not optional, and not a reminder.)
2. Only the owner creates a shelf. An agent that finds no fitting shelf must
   ASK. It may never invent one, and it may never file something under a shelf
   it does not belong to just to get past the gate.
3. A hard ceiling on the number of shelves. At the ceiling, something must be
   merged first. (Lesson 1, prevented rather than repaired.)
4. A new entry too close to an existing one on the same shelf is refused,
   pointing at the one that already exists. (Lesson 6.)
5. Nothing is deleted. An entry is retired, and a retired entry is still
   findable. (Lesson 3 in reverse: nothing becomes unreachable.)
6. Every answer that shows less than everything says how much it held back.
   (Lesson 2.)
7. A search that finds nothing never answers "nothing". It falls back to the
   shelf index, because a list you can read beats a guess at your wording.
   (Lesson 7.)

## Searching, in four steps

Each step only runs when the one before it found nothing, so a good match is
never widened into a worse one:

1. the exact phrase
2. every word, in any order
3. every word as a prefix of at least four characters - this is what makes
   "ribbetjes" reach "ribben"
4. the shelf index itself, to read

## Health

The librarian reports a shelf that is too thin (one entry, so it should
probably be folded into another) and one that is too fat (past the readable
band, so it needs labels). It reports; it does not act on its own.
