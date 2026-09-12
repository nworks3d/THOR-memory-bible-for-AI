---
description: THOR session evaluation - judge what fired, repair what has rotted, give teeth to what nothing could stop, capture what was missing, report what the gate did and where it chafed, and close with a plain-language summary
allowed-tools: mcp__thor__mark, mcp__thor__get, mcp__thor__lookup, mcp__thor__remember, mcp__thor__revise, mcp__thor__retract, mcp__thor__status, mcp__thor__history
---

THOR session evaluation. Work through steps 1 to 7, then the report. You need
no prior knowledge for this: everything is below. Do not skip a step because
it seems like there is "nothing to report" - write "nothing" for that step
explicitly instead. An empty step is a finding; a skipped step is a gap.

Invent nothing. If a step turned up nothing, say so. A made-up finding costs
more than a missed one.

## What a mark means

A useful mark is not a dead signal. The latest verdict is what counts:
- a useful mark clears whatever noise came before it and resets the count to
  zero;
- a noise mark after that counts again, on its own;
- two noise marks since the last useful mark take a fact out of the
  injection channels - it stays findable through lookup regardless.

The Stop hook already asks for a verdict on the single most-served unjudged
item, once per turn. That is the same debt this routine pays; here you
settle it deliberately, over the whole session, instead of one item at a
time.

---

## 1. Judge what fired

The test is not "was it useful" but **did it belong where it fired**.
- `mcp__thor__mark(id: "<id>")` - it belonged there.
- `mcp__thor__mark(id: "<id>", noise: true)` - it did not.

Two patterns, and how to tell them apart:
- A fact anchored to a FILE you edited often fires dozens of times. That is
  not noise: the anchor is right, and you were there.
- A fact bound to a broad MOMENT (`remember`, `commit`, `deploy`) that
  advises on a narrow subclass fires on every action of that kind. That is
  noise. If no narrower moment exists, moving it to lookup is the correct
  repair - let the system do that; do not force it.

When in doubt: skip it. A guessed verdict pollutes the one signal that
maintenance depends on. This is the ONE place in this evaluation where doubt
is a reason NOT to act. Everywhere else below, the opposite holds: doubt is
never a reason to leave a step undone. Here it is, because a verdict cannot
be taken back once it counts.

The argument is called `id`, not `entity_id`. On an "unknown entity" error,
retry the call, first with and then without a project prefix - the local
mcp__thor__ server is the authority.

Start by pulling the debt: run the health check (its full path is at the
bottom, by the report) and read the items it lists under `judgement debt`
for this checkout - with `--full` if there are more than it shows. Every
item on that list gets a verdict: useful if it belonged where it fired,
noise if it did not. On top of that: every fact you recognised as noise
this session, and every useful fact the Stop hook itself never asked about.
Do not re-mark something that already carries a verdict - that would count
twice. Step 1 is done once this project's debt is at zero, or every item
still left is named in the report with the reason you could not judge it.

## 2. Repair what has rotted (this is not noise)

Did a fact come up that was still RELEVANT but no longer matches the
artifact - the code, the file, the current state? That is not noise, and
not "missing" either. Verify it against the artifact and revise it NOW.

Check explicitly every fact that carries a VALUE (a number, a line number, a
percentage, a date) rather than a pointer. These rot silently. The fix is
not a new value but a pointer: "read this from the health check" instead of
a number that will drift the moment the store changes.

Also check every fact that describes a problem as still open when it has
since been fixed.

THOR wins for decisions; the artifact wins for the current state of the
build.

Does the gate refuse your revision - the text too long, the anchor full?
That is not a finding but work for the same turn: keep the text short and
put the reasoning in a separate Report as step 4 describes, or move it to
the right anchor with a route from step 5. Step 2 is done once every fact
you recognised as rotten has actually been revised - no revision is left
hanging on a refusal.

## 3. Give teeth to what nothing could stop

Step 2 asks whether a fact is still TRUE. This step asks whether it can DO
anything.

A fact about something irreversible or costly that carries no check only
informs: it sits there while the mistake happens and says nothing. Walk the
facts that fired this session and are heavy (`irreversible` or `costly`),
and decide for each. `get` shows you `severity`; a fact with no `check`
field can stop nothing. The health check counts the state under `teeth`.

Two outcomes, and both belong in the report:

- **There is a literal fragment that makes the mistake.** Attach a check,
  in the forms step 4 describes. Often the ANCHOR turns out to be the real
  problem: a line about a dangerous command bound to a broad moment fires
  everywhere and refuses nowhere. Move it to that command and attach the
  check there.
- **There is no literal fragment.** A judgement rule ("check first", "assume
  nothing") has nothing to catch. That is meant to inform, and that is not
  a shortcoming. Say so, and move on.

What you NEVER do here: stretch the literal to catch more. If it catches one
form and not another, that is the honest answer - write down the gap you
left open. A broader rule that blocks legitimate work is the most expensive
outcome this system knows (see step 5), and the doctrine forbids widening a
trigger to buy a catch.

This evaluation cannot probe anything itself; that needs a real command. So
every check you add here goes on step 7's list as "still to be probed". A
check that has never been probed is an assumption.

Does the gate refuse the check you are adding - does it not hold, or is the
anchor full after moving it? Correct the check until it holds, or store the
fact without a check and put the reason nothing can catch it in the tags; a
full anchor is solved with a route from step 5. That happens in the same
turn, not as a line in the report. Step 3 is done once every heavy fact that
fired has a decision (a check, or a recorded reason why not) and nothing is
left hanging on a refusal.

## 4. Capture what was missing

Did you find a gotcha, a contract, or a behaviour detail that should have
been in THOR and that you can state concretely? Store it NOW with
`remember`. Reporting a missed lesson without storing it is half the work.

Where the gate will refuse you, so you get it right immediately:
- at most 300 characters for a Rule or Orientation - put the reasoning in a
  separate Report, do not lengthen the sentence;
- at least one binding (a moment, a target, or `always`) and a falsifier;
- a fact with no project scope may not name or anchor to a source file;
- anchor it to the file or command it is REALLY about, never to a path that
  merely happens to appear in the sentence;
- every binding already full of heavier rivals: solve it with a route from
  step 5 (fold it into an existing fact, anchor it narrower, or deliberately
  leave it with the label `crowded-on-purpose`) and name the occupants in
  your report - dropping it is not a route;
- a NEW place - a project, scope or purpose that does not exist yet: choose
  an existing place that fits, or ask the owner for a name for the new one.

Want a fact to be able to REFUSE a wrong action instead of only informing:
- forbidden text in files: `always` plus check_kind `forbidden`;
- a forbidden COMMAND: a command target plus check_kind `forbidden`, with
  the dangerous fragment as the literal. Do not give a rule like this an
  `always` binding unless those words must also never be written - otherwise
  it refuses its own documentation;
- text that must not appear in ONE file: check_kind `absent`.

All other forms only inform, and that is usually the right choice.

Step 4 is done once every fact you tried to capture is actually stored - no
attempt is left hanging on a refusal from the gate.

## 5. What the gate did

This is the signal that counts here, not a mark.

- Did the gate refuse something, rightly? Name the rule id and what was
  refused.
- Was there a FALSE BLOCK: something refused that should have gone through?
  **This is the most expensive outcome this system knows**, and it always
  belongs in the report - with the rule id, what you tried, and why it was
  legitimate. A false block teaches people to work around the gate, and
  that cannot be undone.
- Did the gate refuse one of your OWN write actions (`remember`/`revise`)?
  That is not an annoyance but a signal about the shape of the store. Name
  the reason.
- Did a displacement notice show up on a write ("this may well never be
  shown there", "no automatic serving surface can reach")? That notice
  means you just stored something that will probably never be read.
  **Do not just report it: resolve it now, before you continue.** This is
  the ONLY signal this evaluation ever gets about the invisible side of the
  memory - a displaced fact does not fire, so it never comes up to be
  judged. Reporting it and leaving it means it is never looked at again.

  Three ways out, in this order:
  1. **Fold it in.** Does an existing fact already say almost the same
     thing? Fold yours into it with `revise` and retract yours.
  2. **Anchor it narrower.** Is it hanging off a broad moment or a busy file
     when it is really about a more specific file or command? Re-anchor it.
  3. **Leave it, and record why.** Sometimes it genuinely belongs there and
     the place is legitimately full of heavier things. Then that is the
     answer - name which facts occupy the place in your report, and put the
     label `crowded-on-purpose` on the fact. Without that label it is not a
     decision but an oversight, and the debt comes back every turn until
     someone throws away a true fact just to be rid of it.

  What you NEVER do: raise the severity to make it visible. That pushes a
  heavier warning out of the way to show a lighter one, which is exactly
  the compensating trick this design forbids.

Step 5 is done once every refusal is named, every false block is in the
report, and every displacement notice is either resolved or deliberately
tagged - reporting alone is never enough here.

## 6. Friction - where it chafed

This is a full outcome, not an afterthought. Name everything that cost you
time or confidence, even what you fixed yourself:

- **Tooling.** A tool that refused, an unclear error, an argument that did
  not do what its description promised, an id shape that did not work.
- **The gate.** A refusal whose text did not say what to do instead. A rule
  that fired where it did not belong. A repeat handled differently than you
  expected.
- **The memory itself.** A fact that contradicted another fact. A
  near-duplicate you only noticed after writing yours. A fact whose anchor
  you could not trust.
- **The shape of the record.** Something you wanted to capture but could not
  fit within the rules (too long, no honest anchor, no honest test). This is
  the most important kind of friction, because it is where the store loses
  information.

For each point of friction: is this a one-off, or will it happen again next
session? If it will recur, THOR should hold a fact about it - capture that
under step 4 instead of only reporting it.

Step 6 is done once you have decided that for every point of friction - not
every point needs to become a fact, but every point needs a decision.

## 7. What you did NOT verify

Name explicitly what you are assuming but have not checked. If you added a
fact bound to a target, this evaluation only ever sees what FIRED - a
silently displaced binding shows up nowhere here. Flag such a fact as
"still to be probed": the probe itself happens outside this evaluation, by
putting the forbidden fragment literally into a command and seeing whether
the gate responds. Never test with a file check or with free prose - that
proves nothing.

Step 7 is done once every untested addition from this session is on that
list, named with what should make it refuse.

---

## The report

Open with a plain-language summary: what happened, does it work, what does
it mean for the owner. No jargon, no hashes, no test names. The detail
follows below, in the order of the steps above.

Report what you DID, not what you noticed - evidence, not assertion. The
owner cannot see into your session: "I repaired a rotten fact" is, to him,
indistinguishable from doing nothing. So for every fact you changed: its id,
what it said BEFORE, what it says NOW, and the artifact you checked it
against. For every re-anchored fact: from which anchor to which anchor. For
every displacement notice: how you resolved it, or that you deliberately
left it, with the occupants named.

If you deliberately left something alone, say why in one sentence - that is
an outcome too, the same as a change; if you changed nothing at all, the
same holds. If a revision, check or capture could not get past the gate,
resolve it before you report: "I could not save this" does not belong in
this report - if it is still there when you write the report, the work is
not done.

1. what you judged as noise and why, and what belonged where it fired;
2. what you revised, with before and after, and which artifact you checked
   it against;
3. which heavy facts gained teeth, and which could not because there was
   nothing literal to catch - with the gap you left open;
4. what you stored that had been missing;
5. what the gate did - including every false block and every refusal of
   your own write actions;
6. the friction, and which of it will happen again next session;
7. what you did not verify.

Get the numbers about the state of the store from the health check, never
from memory and never from a fact. That is a SEPARATE program, not a
subcommand, and it is not on the PATH - run it with its full path:

    {{THOR_DOCTOR}}

The lines you need: `provable rules` (how many can refuse anything at all),
`gate` (how often it did), `teeth` (how many heavy rules have teeth),
`pinned` (how much of the memory is never re-read), `decay`, `crowding`,
`ship` (how long ago the copy on another machine last succeeded; this line
stays silent when there is no copy, and speaks up when one has gone stale),
`judgement debt` (how many items still owe a verdict, store-wide and for
this project), and `contradictions` (how many pairs of notes carry a proof
that cannot both be true right now; this line stays silent when there are
none). Do not copy those numbers into a fact - point to them instead.

Do not store or change anything else unless the owner asks for it. The
exceptions are the work the steps themselves call for: the verdicts, the
revisions of rotten facts, the checks you attach to heavy facts, and the
captures of what was missing.
