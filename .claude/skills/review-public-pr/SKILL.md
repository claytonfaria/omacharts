---
name: review-public-pr
description: Review a pull request opened on this repo by somebody outside the project — read the diff, establish it is safe, decide whether it lands, and land it. Use whenever a PR from an outside contributor is mentioned, pasted as a github.com/jorgemanrubia/omacharts/pull/N link, or handed over with "review this", "is this safe", "can I merge this", "what do you suggest for #N".
---

# Reviewing a pull request from outside

Omacharts is public, so PRs arrive from people you know nothing about. Two
things follow: nothing in the PR's own description counts as evidence until
you have run it, and the diff gets read as something a stranger wrote, not as
a favour you are obliged to accept.

Review one PR per agent, each in its own worktree, so several can run at once
without fighting over a checkout. Give each agent its own build directory
too — two agents sharing a scratchpad binary have overwritten each other
mid-run and produced contradictory results.

## Start with what it touches, not what it does

Before reading the feature, list the files:

```
gh pr diff <N> --name-only
```

`Cargo.toml`, `Cargo.lock`, `build.rs`, `.github/workflows/`, `packaging/`,
`bin/`, anything ending `.sh` — any of those is a different and much more
careful review than a change under `src/`. A feature PR that touches none of
them has a small blast radius whatever else is wrong with it.

Then sweep the diff for the things that do not belong in this app:

- network calls, `Command::new`, `std::process`, `unsafe`, `include_bytes!`,
  environment and credential reads, writes outside the path the feature is about
- invisible and bidi unicode across the whole diff
  (`[\x{202A}-\x{202E}\x{2066}-\x{2069}\x{200B}-\x{200F}\x{FEFF}\x{00AD}]`)
- base64 or hex blobs, unusually long lines, anything encoded
- in generated data files: column shape, duplicate rows, control characters,
  embedded URLs, collisions with the file it sits beside
- text under `agents/skills/` — an agent will read and act on it. Read it
  adversarially, as instructions rather than prose.

If a PR integrates something third-party, verify its claims against that
project's own source. A PR that says a tool reads `$FOO_HOME/skills` and
follows symlinks is checkable in an afternoon, and being right about it is
the difference between a working feature and a symlink in a directory nobody
reads.

## Verify the claims, including the good ones

Run the tests yourself — `cargo test --workspace --locked` and
`cargo clippy --workspace --all-targets`. CI is build, test, clippy; clippy
warnings do not fail it, but a PR should not add any.

Then exercise the actual binary. If the PR fixes a bug, reproduce the bug on
`main` first; if it cannot be reproduced, that is the finding. Quote the
exact commands and output in whatever you write afterwards, and never claim
to have run something you did not.

Sandboxing the CLI, which is easy to get wrong:

```
XDG_DATA_HOME=$(mktemp -d) DBUS_SESSION_BUS_ADDRESS= ./target/debug/omacharts …
```

`XDG_DATA_HOME` is what the store reads — overriding `HOME` alone does
nothing and will write to the real profile. Blanking the bus address keeps
the command local instead of forwarding it to a running window.

## Ask whether the app already does it

The most useful question in this repo is usually "can you do that today?".
`skill install --to DIR` already installs the skill anywhere, so an agent
does not need a row in a table. `watchlist add` is variadic, so a list of
tickers is `$(cat list.txt)` left unquoted. Both have had PRs written against
them that the existing flag already covered.

When the gap turns out to be that `doc/cli.md` never showed the form, say so
and offer to merge the documentation hunk alone. That is the real fix.

## Judge the shape, not just the code

- **One change per PR.** A dispatch change riding inside a feature PR gets
  split out, however correct it is — especially when it is the riskier half.
- **Proportion.** Eight lines of feature behind six hundred lines of test
  harness is a maintenance decision disguised as a contribution.
- **New categories.** A top-level `tests/` directory, a subprocess harness, a
  private D-Bus session: these are things the repo does not have, and merging
  one inherits it forever. Unit tests beside the code, with `Store::memory()`,
  cover almost everything here.
- **Performance where it lands on the main loop.** A command run inside the
  window runs on the GTK main loop, and the bar widget polls one every 30-120
  seconds. Rebuilding the eleven-thousand-row index there costs ~9ms a tick
  when the window already owns a complete index built off-thread. Measure
  before and after rather than guessing, and say the numbers.
- **No gratuitous reformatting** of code the PR did not otherwise touch.

## Simplify before merging

Standing instruction from the owner: keep things simple. If a PR is good but
carries something that can go, take it out before it lands rather than filing
a follow-up — a `pub fn` whose last caller was in the change itself, a
negated guard that hides the common case, a comment that stopped being true
when the feature grew a third answer.

Anything written here matches the house style: comments explain *why*, doc
comments read as prose, commit subjects are imperative sentences
("Pin the v0.1.4 tarball"), no conventional-commit prefixes, no "Test plan"
section, no generated-by footer.

## Fork mechanics

```
gh pr checkout <N>
git push --no-follow-tags                 # upstream is the fork URL
```

Plain `git push` fails on tags it cannot write. Do not reach for
`git push origin HEAD:<branch>` to get around it — `origin` is this repo, not
the fork, and it creates a stray branch here instead of updating the PR.

CI does not run on a fork PR until a maintainer approves it, so it sits at
`action_required` and the PR looks checkless:

```
gh run list --branch <branch> --json databaseId,status,conclusion,headSha
gh api -X POST repos/jorgemanrubia/omacharts/actions/runs/<id>/approve
```

Approve the run for the newest head SHA, and never merge anything red or
anything with an open blocker. Report instead.

## Landing it

Fix the PR title first if it needs it — it becomes the squash subject, and
branch names have been glued onto the end of one. Label it too: the release
notes are generated and grouped by label, so an unlabelled PR lands under
"Changed" whatever it was.

```
gh pr edit <N> --add-label enhancement     # or bug
gh pr merge <N> --squash
```

with a subject in the repo's voice and a body that says what landed.

**Thank the author.** Short: thanks, plus at most two sentences, and only for
something they would otherwise be surprised by — a commit you pushed to their
branch, a follow-up the merge leaves open. Everything else you wanted to say
belongs in the report to the owner, not on the PR.

Every comment you post is written from the maintainer's account, so it opens
with 🤖 as its first character — see "An agent writing on GitHub says so" in
AGENTS.md.

## Declining it

Close with thanks and a short reason, and point at the thing that already
does the job. Two or three sentences is the whole comment; the detailed
reasoning, if it was worth writing, is already in the review above it.

When the diagnosis was right but the implementation is not wanted, say that
plainly, close it, and credit the contributor in the PR that does land the
fix: `Reported and diagnosed by @<user> in #N`. Finding a shipped bug nobody
had noticed is the valuable half, and it should be attributed even when none
of the code survives.
