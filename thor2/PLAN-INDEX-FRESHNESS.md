# Plan: keeping the code index fresh

Status: phase 0 is measured and phase 1 SHIPPED on 2026-08-15 (commit eee47e9,
`ops::githooks`, nine tests). The design below is what was built; it is kept as
the record of why, and of what was measured before anything was written.

This file was written locally on 2026-08-15. An earlier draft existed on a remote
branch that was never delivered, so this is a fresh write from the code as it
stands at 55a8cf5, not a copy of that draft.

## The defect

`codeindex::store::refresh` (store.rs:249-298) already exists. It is cheap: it asks
git for the diff between the indexed commit and HEAD, rereads only the changed
files, and returns `no_op: true` without touching a single file when HEAD has not
moved. It is exactly the right shape.

Nothing in this workspace ever calls it. `install.exe` wires four agent hooks into
`settings.json` (install.rs:130-134, SessionStart / PreToolUse / and the rest) and
registers the tool server, but it writes no git hook. The only git hook in the repo
is `.git/hooks/pre-commit`, which execs `scripts/check-private-data.sh` and was
installed by hand on 2026-07-26.

So the index is only fresh if a person remembers to refresh it. That is CONTRACT R7
word for word: "If the design needs someone to remember something, that is a design
defect." The live index for this project proves the point - it was last built on
10 August, five days before this measurement.

## What is NOT wrong, and why this is a quality fix rather than a defect

A stale index does not weaken enforcement. This correction is load-bearing and was
got wrong twice before it was checked against the source:

- Notes carry no line numbers. A check is `path_exists` / `contains` / `absent` /
  `absent_all` / `forbidden` - paths and literals, never a line.
- The guard reads the working copy off disk: `absent_guard.rs:405`,
  `std::fs::read_to_string(root?.join(file_path))`.
- `decay_check` (`health.rs:355`) walks the working copies under `--checkouts` with
  `std::fs::read_dir` and never opens the index.
- Across `serve/src`, the name `codeindex` appears in `lookup.rs` and nowhere else,
  and `serve/tests/codeindex_is_lookup_only.rs` fails the build if that ever changes.

Only `search_code`, `where_used` and `outline` degrade, and each already prints the
commit it read - `lib.rs:5-9` names that the design goal: a stale answer is always a
LABELLED stale answer. So this is a cheap quality win on the lookup surface, not a
break of the core promise. That is why phase 0 measured before anything got built.

## Phase 0: measured

**0.1 / 0.2 - drift, by file.** 20 commits back is `ac0a3d5`, three days (12-15 Aug).
51 of 190 living files changed = 27%. The other 39 changed paths are the deleted 1.0
tree under `thor/` and do not count. The heavy files grew a lot:
`serve/src/bin/serve.rs` +417 lines, `ops/src/health.rs` +216, `model/src/gate.rs`
+150. A 417-line insertion moves every symbol below it.

**0.3 - drift, by wrong answer.** Two indexes were built, one at `ac0a3d5` and one at
`55a8cf5`, and both were asked the same `where_used` questions against the working
copy at `55a8cf5` - the real situation of a stale index under a current checkout. A
site counts as wrong only when the line it names does not contain the symbol at all,
which is a deliberately lenient test.

| index | symbols | sites | wrong | of which the file is gone |
|---|---|---|---|---|
| stale, symbols that moved | 582 | 1729 | 1432 (82.8%) | 93 |
| stale, symbols that did not move | 1474 | 7229 | 2824 (39.1%) | 2276 |
| fresh, symbols that moved (control) | 582 | 1674 | 0 | 0 |

Read it as: for a symbol that actually moved in three days, 83% of the places
`where_used` sends you to are the wrong line, and every one of the 582 symbols had at
least one wrong site. For symbols that did not move, most of the damage is the
deleted 1.0 tree; on files that still exist 11.1% of sites are wrong.

The control matters as much as the finding: a fresh index scored 0 wrong out of 1674.
The method measures drift and not itself.

**Verdict: build phase 1.** A fresh index returns nearly six times as many correct
answers on precisely the question `where_used` exists to answer.

## Phase 1: the design

### Where it goes

In `ops`, not in `codeindex`. `codeindex/src/lib.rs:22-27` says the crate deliberately
contains no serve path, hook or injection channel, and that is a correct scope choice
worth keeping. `ops` is already the crate that installs things.

`serve/tests/codeindex_is_lookup_only.rs` greps `serve/src` only, so `ops` naming
`codeindex` breaks nothing - but the plan must not put any of this in `serve`.

### What gets written

A `post-commit` dispatcher in THOR's own machine-wide hooks directory (see the
decision below for why it lives there and not in each repository):

```sh
#!/bin/sh
# Installed by THOR. Refreshes the code index for whichever repository this
# commit happened in, so lookup answers point at current line numbers.
# Never fails a commit, and always hands control back to the repo's own hook.
"<codeindex-exe>" "<index-db-for-this-repo>" "$(git rev-parse --show-toplevel)" refresh >/dev/null 2>&1 || true

repo_hook="$(git rev-parse --git-common-dir)/hooks/post-commit"
[ -x "$repo_hook" ] && exec "$repo_hook" "$@"
exit 0
```

`post-commit` is the right event: git ignores its exit code, the commit has already
happened, and `refresh` is a no-op the moment HEAD has not moved.

### The rules it must obey

1. **Never fail or delay a commit.** Output discarded, `|| true`, explicit `exit 0`.
2. **Never swallow someone else's hook.** `install_hooks` already sets the house
   standard (install.rs:240-243): every other group is left byte-for-byte. Here that
   means every dispatcher chains to the repository's own hook, and an existing
   `core.hooksPath` set by something else is reported and left alone, never taken
   over.
3. **Idempotent.** Installing twice reports "already there" and writes nothing, the
   same three-way outcome the agent hooks already report.
4. **Fails open and quiet**, like every other hook here.
5. **LF endings, no BOM.** Written from Windows, possibly run under git's bundled sh
   or on the NAS. A CRLF hook file fails with a confusing error.
6. **Worktrees.** In a worktree `.git` is a file, not a directory, and the hooks are
   shared with the main repository. Resolve them with `git rev-parse
   --git-common-dir`, never by joining `.git/hooks` and never with `--git-path
   hooks`, which returns the global directory once `core.hooksPath` is set.

### Tests

1. `install_writes_a_post_commit_hook_that_calls_refresh`
2. `a_repository_s_own_hook_still_fires_after_the_global_dir_is_set`
3. `the_chain_uses_git_common_dir_so_it_never_points_at_itself`
4. `an_existing_global_hooks_path_is_never_taken_over_silently`
5. `installing_twice_reports_already_present_and_changes_nothing`
6. `the_hook_body_exits_zero_even_when_refresh_fails`
7. `a_repository_with_no_index_db_is_left_untouched`
8. `a_worktree_refreshes_the_index_of_its_main_repository`
9. `the_hook_files_are_written_with_lf_endings_and_no_bom`

Test 2 is the one that matters most: it is the private-data gate, and it must be
written as a real commit that the gate refuses, not as a check that a file exists.

### R9: the compensating report

`doctor`'s drift line already reports how far the index has fallen behind. Under R9
that reporting is the acknowledged compensation for the hook not being able to cover
every case (a commit made by a tool that skips hooks, a fresh clone). It stays after
phase 1 ships, and it is the thing that tells you the hook is not running.

## Decided: it runs everywhere, without being asked

The owner's requirement is that this happens automatically no matter which session
is running. A hook written into one repository at a time does not do that, so the
mechanism is git's own machine-wide hooks directory:

    git config --global core.hooksPath <thor-hooks-dir>

Nothing is set on this machine today (git 2.55, `core.hooksPath` and
`init.templateDir` both unset), so the setting is free to take. `init.templateDir`
is NOT the answer: it only reaches repositories cloned or created after it is set.

### The danger this creates, and the only acceptable way to handle it

Setting `core.hooksPath` makes git ignore every repository's own `.git/hooks`
directory. This repository has an active `pre-commit` hook that blocks a commit
carrying the maintainer's private identifiers into a public repo. A naive install
would silence that gate without a word - a gate going quiet is this project's own
worst failure class, and it would be caused by the very change meant to help.

So the hooks directory does not hold one hook. It holds a small dispatcher per
event, and every dispatcher ends by handing control to the repository's own hook if
it has one:

```sh
repo_hook="$(git rev-parse --git-common-dir)/hooks/<event>"
[ -x "$repo_hook" ] && exec "$repo_hook" "$@"
```

`--git-common-dir` and not `--git-path hooks`: once `core.hooksPath` is set, the
latter returns the global directory and the chain would quietly point at itself.
That single detail is the difference between a working gate and a silent one.

At minimum `pre-commit` needs a dispatcher on day one, because one exists today.

### What the refresh dispatcher does

`post-commit` resolves the repository it fired in, looks for that project's index,
and refreshes it. If no index exists for that repository it does nothing at all -
it never builds a full index behind the owner's back, and it never touches a
repository THOR knows nothing about.

### Proving the gate still works

Not with a file check. After installing, the private-data gate is tested by putting
a forbidden identifier in a commit for real and confirming the commit is refused.
A hook file that exists proves nothing about whether it fires.

## Not in scope

Chasing Graphify's knowledge graph. `symbols.rs:19-23` already states what this index
is and is not, and rebuilding tree-sitter over 37 languages is months of work toward a
worse Graphify that makes no note better.
