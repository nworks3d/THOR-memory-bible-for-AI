# Changelog

What changed in each release, newest first. The release workflow copies the section for the tag being released to the top of the release notes and refuses to publish without one.

## 2.4.1 (2026-09-17)

- **A session that starts right after a compaction is told so.** A `SessionStart` payload whose own `source` field reads `"compact"` gets one added line: the summary just given is not proof, so anything it calls done, tested, flashed, deployed or live should be checked again first. It never fires for any other source, and never for a subagent.
- **The evaluation starts with the previous report's open points.** Before judging what fired, it looks up the newest evaluation report filed for this project and either settles what that report left open - unjudged items, gaps, anything still to be probed - now, or carries it into this report's own open points with the reason it is still open.
- **A project worked in for an hour cannot end a turn without a recent evaluation report, only inside a project.** In every project a session has worked in for at least an hour, the Stop hook blocks the FIRST Stop of EVERY turn - not merely once per session - until an evaluation report is filed for that project, before the judgement debt, when both are due, and regardless of how many notes currently owe a verdict. A filed report covers the next 16 hours: a rolling window rather than a calendar day, so a report filed late at night is not undone by the next midnight, and one filed early in the afternoon no longer buys silence clean through to the next morning. Each ask names how many times this project has already been asked and since when. A checkout that resolves to no project is never asked at all, since there would be no way to ever file the report that silences it.
- **Filing that report is not the end of it - but only when something risky happened since.** A new evaluation is due after every three hours of work since the last report, pauses excluded, again as a hard stop until its own report is filed - but only once a real risk has shown up too: three or more code changes with no test or build run in between, or a context summary. Three quiet hours with neither stays silent. The repeat covers only what happened since the last report, plus the state of the work right now, and names the risk that triggered it. "Work" is now measured from actual hook activity, gap-filtered, rather than a single timestamp, so an idle stretch of half an hour or more no longer counts toward either the first or the repeat ask.
- **The shipped evaluation pre-approves shell commands one by one.** The `allowed-tools` frontmatter no longer carries a bare `Bash`, which let any shell command through unasked during an evaluation; it now names the git, test and build commands the routine actually runs, plus the health check.
- **The shipped evaluation now files its own report into the memory**, so a later session (and `doctor`) can see one was done. The 2.4.0 notes below already claimed this; it arrives now.
- **The ship line counts what is waiting and only alarms on a failed attempt.** A healthy hourly ship with nothing new to send still reads as fresh no matter its age; changes waiting behind an old success are named and counted, never mistaken for a failure; only an attempt that actually failed raises the alarm.
- **A backup push that failed is retried.** A commit that landed locally but could not reach the remote is pushed again on the very next run, before that run's own once-a-day schedule is even checked.
- **The evaluation's teeth step tries a check before it accepts that none is possible.** Step 4 no longer lets "it's a judgement rule" end the question on its own: for a heavy fact with no check it now names, in the report, the literal fragment, the file or command it belongs at, and the check kind - and only after that attempt may it answer "no literal", with one of two named reasons. A fact that already carries that reason gets it read again whenever the fact names a command or file. The report states the health check's teeth count before and after the step.
- **A new heavy Rule or Orientation must say how it can refuse, or why it cannot.** Gate ground 11 has asked this of a Rule since 2.4.0; it now asks the same of an Orientation. Remembering one, or revising one so its severity becomes `costly`/`irreversible` or its last check is cleared, is refused when it carries neither a check nor a `no-literal:<why>` tag - the refusal names both honest reasons a "no-literal" answer may give.
- **A version bump without a changelog section fails before a tag ever exists.** CI checks that `thor2/Cargo.toml`'s version has a matching `CHANGELOG.md` section on every push, not just at release; the release workflow then copies that section into the published notes and refuses to publish without one.
- **A directory-only rule's own note stopped reading as a full room.** Every binding on an item being a directory got the same wording as a crowded pool - "stored onto a place that is already full" - and the Stop hook nagged to fold, re-anchor or tag it `crowded-on-purpose` even when the place was never full at all. The two are told apart now: a directory-only note stays exactly as broad as before, but the Stop hook's own displacement check only ever reacts to a genuinely crowded pool.
- **`doctor`'s crowding line now also counts a crowd hiding behind a command.** It used to probe only file anchors; an item bound to a command that a real block would never fully show went uncounted, however many rivals shared it. The line now probes commands the same way, naming an invisible one `<command> (command)` so it reads apart from a file.
- **A repeat verdict is accepted again once the item earned a fresh one.** `mark` used to refuse a second identical verdict on the same id for the rest of a session, full stop - even once the item had fired 40 more times since and `doctor` listed it as owed again, with no way to pay that debt down in the same sitting. It is written again now, once that threshold is crossed, and the reply says how many more times it fired and since which earlier verdict.
- **The dead-on-arrival refusal stopped suggesting a bigger hammer.** Its own fix text used to float raising severity as one of the ways out, which is exactly the compensating knob the evaluation routine forbids - it pushes a heavier warning out to show a lighter one. The refusal now points only at a narrower binding or folding into an item that already holds the ground, and says plainly not to raise severity to win the place.
- **A served item now says where it fired, not just that it did.** `doctor`'s judgement-debt list named an item's binding but never which command or file among what it reaches had actually triggered it, so an evaluation judging "did it belong where it fired" sometimes had no place to look. Every serving now carries its own trigger - the command, the file, or `session start` - and the newest one is named after the binding as `last fired at '...'`.

**Upgrading from 2.4.0**: unpack over the old programs and run `install` once from inside your repository, so it refreshes `/thor-eval` and the project record. Restart your assistant afterwards.

## 2.4.0 (2026-09-12)

- **The evaluation ships with the install.** `install` writes `/thor-eval` into your assistant's commands. Run it at the end of a session: it judges what fired, repairs what has rotted, and gives teeth to what could stop nothing.
- **The memory asks for that evaluation.** Once per session, the Stop hook holds the turn when a project owes a verdict on ten or more notes and nothing there has been judged for a day.
- **`doctor` names the debt.** The notes that owe a verdict are listed with the place they fired (all of them with `--full`), and the line says how long ago the newest verdict was.
- **A fresh install opens the project it read.** The first note filed under your repository lands without being asked to name a new collection; the refusal text no longer points to `install --project`.
- **`revise` can clear a field on purpose**: `clear_check`, `clear_severity`, `clear_project`, `clear_expires`, `clear_key`, `clear_falsifier`. An empty string never reached the server from an assistant's tool layer.
- **Pinned notes owe no verdict.** A note bound `Always` never appears in the judgement debt, and `mark` on one says so instead of writing a verdict that changes nothing.
- **`mark` notices a repeat.** The same verdict twice in one session is not counted twice; a different verdict is written and says which one counts.
- **The crowding note measures.** On a `revise` that keeps its bindings, the note says how often the item was actually shown in the last 30 days instead of guessing that it may never be shown.
- **The response guard knows who is reading.** A rule can carry `owner_reading_only`: style rules step aside for a subagent's report to the main agent, honesty rules keep applying to everyone.
- **The hint under a truncated block runs.** It prints the real path of `serve` with `--db`, so pasting it works.
- **Weakening a checked rule needs a reason.** `revise` asks for `because` when it removes or changes a check, lowers severity, drops a binding or narrows scope; the reason is kept in the note's history. A check that is the direct opposite of another check on the same file is refused.
- **Windows release builds are reproducible.**
- Six Stop-hook debts now: judgement, crowding, false proof, setup, teeth, evaluation.

**Upgrading from 2.3.x**: unpack over the old programs and run `install` once from inside your repository. It records the project, writes `/thor-eval`, and leaves your notes untouched. Restart your assistant afterwards.

## 2.3.3 (2026-09-08)

- **The library is yours to shape.** When nothing fits, your assistant asks what the new shelf should be called and opens it under that name. An entry you no longer want can be retired with a reason; nothing is ever deleted.
- **A note stays inside its own project.** A note anchored on a file path used to reach any file with that name anywhere on your machine; now it resolves against the project it belongs to.
- **Taking the teeth out of a note now needs a reason.** Clearing a proof, lowering its weight, or narrowing where a note applies requires a reason, kept in that note's history.
- **Sending your memory to a second machine fails loudly.** It used to hang without a word; now it times out, says what went wrong, and the health check names a copy that has gone stale.
- **You are asked about a note once.** At the end of a turn you are only asked to judge notes this session actually used, and once you have answered for a note it will not ask again that session.
- **2.3.2 was tagged but never released**: its Linux build was red because a few tests stood in for an absolute path with a Windows one. 2.3.3 is that same release with the tests fixed.

## 2.3.1 (2026-09-07)

- **The setup note gets real teeth**, and its own answer is exempted from the scope gate.
- **`install` seeds a starting rulebook**, and a note walks a new owner through setup.
- **A security policy is added**, spelling out the network transport's lack of auth.
- **The README covers another assistant**, its container, and the tool list.
- **The sixteen tool descriptions get one shape**, and shrink hard.
- **`install` seeds the honesty and agent-spawning habits too**: twenty-one notes now.
