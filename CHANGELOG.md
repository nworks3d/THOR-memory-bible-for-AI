# Changelog

What changed in each release, newest first. The release workflow copies the section for the tag being released to the top of the release notes and refuses to publish without one.

## Unreleased

- **A session that starts right after a compaction is told so.** A `SessionStart` payload whose own `source` field reads `"compact"` gets one added line: the summary just given is not proof, so anything it calls done, tested, flashed, deployed or live should be checked again first. It never fires for any other source, and never for a subagent.
- **The evaluation starts with the previous report's open points.** Before judging what fired, it looks up the newest evaluation report filed for this project and either settles what that report left open - unjudged items, gaps, anything still to be probed - now, or carries it into this report's own open points with the reason it is still open.
- **A project worked in for an hour cannot end a turn without its daily evaluation report, only inside a project.** Every day (the current UTC calendar day), in every project a session has worked in for at least an hour, the Stop hook blocks the FIRST Stop of EVERY turn - not merely once per session - until an evaluation report is filed for that project today, before the judgement debt, when both are due, and regardless of how many notes currently owe a verdict. Each ask names how many times this project has already been asked and since when. Filing the evaluation report lets the turn end and stays quiet until tomorrow. A checkout that resolves to no project is never asked at all, since there would be no way to ever file the report that silences it.
- **The shipped evaluation now files its own report into the memory**, so a later session (and `doctor`) can see one was done. The 2.4.0 notes below already claimed this; it arrives now.
- **The ship line counts what is waiting and only alarms on a failed attempt.** A healthy hourly ship with nothing new to send still reads as fresh no matter its age; changes waiting behind an old success are named and counted, never mistaken for a failure; only an attempt that actually failed raises the alarm.
- **A backup push that failed is retried.** A commit that landed locally but could not reach the remote is pushed again on the very next run, before that run's own once-a-day schedule is even checked.

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
