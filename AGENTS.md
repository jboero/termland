# AI assistance in Termland

Termland is written with AI assistance — primarily Anthropic's Claude, directed
by the maintainer. This file records how that works in practice, so the
provenance of the code is a matter of record rather than something to be
inferred from commit metadata.

This exists because of [#7](https://github.com/jboero/termland/issues/7), which
asked for exactly that.

## What is recorded, and where

Attribution lives in the git history, per commit, using the standard
`Co-Authored-By` trailer:

```
Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
```

At the time of writing, 49 of the 64 commits on `main` carry such a trailer,
naming the specific model that assisted:

| Model | Commits |
|---|---|
| Claude Opus 5 | 25 |
| Claude Opus 4.8 | 16 |
| Claude Opus 4.6 | 8 |

The commits without a trailer are the earliest ones, plus a few small manual
changes (release packaging, the licence commit, README and demo-video edits).
They are not hidden authorship — they predate the convention.

`git log --format='%h %an %(trailers:key=Co-Authored-By,valueonly)'` gives the
full picture at any time, and is authoritative. This file is a summary and can
go stale; the history cannot.

## Why there is no prompt log

[#7](https://github.com/jboero/termland/issues/7) also asked for the prompts,
with the hope that they would let someone reproduce the code or re-implement
Termland without AI. That part is not possible, and it is worth being straight
about why rather than quietly not doing it:

- **Prompts are not a build input.** Re-running one does not regenerate the
  code. Model versions change, sampling is not deterministic, and the same
  request produces different output on different days.
- **The context is gone.** The code emerged over many sessions of iterative
  work — reading output, correcting, backtracking. Those sessions were
  compacted and are not recoverable. A tidied-up prompt list written after the
  fact would be a reconstruction presented as a record, which is worse than
  having none.
- **It would not support a clean-room re-implementation anyway.** Clean-room
  work needs a specification written by someone who has not seen the
  implementation. A prompt log is neither.

## What is provided instead

The thing a prompt log was meant to enable — being able to understand, audit,
review, or reimplement this code — is served by making the *reasoning*
reviewable:

- **Commit messages carry the engineering rationale**, not a summary of the
  diff: what the defect was, why the fix is shaped the way it is, what was
  ruled out, and what was verified.
- **Claims are checked by running them.** Where a commit or PR asserts a
  behaviour, it says how that was confirmed — a command, a measurement, a
  before/after. Tests added to this repo are expected to be shown to *fail*
  against the bug they cover, not merely to pass.
- **Known limitations are written down** rather than left for a user to
  discover.
- **`docs/`** describes the system independently of the implementation, which
  is the artefact a re-implementation would actually need — currently the QUIC
  transport and the mobile clients, with the protocol data structures being
  written up under [#5](https://github.com/jboero/termland/issues/5).

## Review status

Every line is open to review, and review is welcome — that is the point of the
issue tracker and of pull requests.

Being AI-assisted does not make a change exempt from scrutiny, and it does not
make it correct. Several bugs in this repository were introduced by AI-assisted
commits and later found by AI-assisted review; others were found by users
([#2](https://github.com/jboero/termland/issues/2),
[#3](https://github.com/jboero/termland/issues/3),
[#8](https://github.com/jboero/termland/issues/8)) who hit them in the real
world. Treat the code as you would any other contribution: read it, test it,
and open an issue when it is wrong.

## Licensing

Termland is LGPL-3.0-or-later. AI assistance does not change the licence or the
maintainer's ability to grant it.

If a downstream project has a policy on AI-generated contributions — KDE was
raised in [#7](https://github.com/jboero/termland/issues/7) and does have such
concerns — this file is intended to give them what they need to make an
informed decision, rather than leaving them to guess. If a policy requires
something that is not here, please open an issue and say what is missing.

## Contributing

Contributions do not have to be AI-assisted. If yours is, adding a
`Co-Authored-By` trailer naming the model keeps the history consistent, and is
appreciated — but review standards are the same either way.
