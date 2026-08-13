# Research evidence

Somewhere to keep measurements so that a later write-up rests on records rather
than recollection. Deliberately lightweight: a schema, a ledger, and a rule about
what may go in.

## The one rule

**Nothing is recorded here that cannot be tied to a real artifact.**

No reconstructed numbers, no remembered numbers, no numbers from a run whose
inputs no longer exist. A measurement without provenance is not weak evidence, it
is an anecdote wearing evidence's clothes — and the moment one is in the file,
every other number in the file has to be defended individually.

Where a field is not known, it is `null`. `null` is a fine answer. Guessing is
not, and neither is omitting the field so the gap stops being visible.

## Layout

```
research/
├── README.md            this file
├── SCHEMA.md            what an experiment record must contain
├── FINDINGS.md          the ledger, with every claim classified
├── experiments/         one JSON record per run, named <id>.json
└── logs/                raw output referenced by those records
```

## Superseding a record

Records are **append-only**. A run that contradicts an earlier one gets its own
record; the earlier one is never edited, never deleted, and never annotated
after the fact. The new record's `notes` says what it supersedes and why, and
[FINDINGS.md](FINDINGS.md) carries the reconciliation.

This is not politeness towards old data. A superseded record usually documents
*which specific thing was tested*, and that is exactly what a later reader needs
in order to tell a wrong measurement from a right measurement of the wrong thing.
`2026-08-13-macos-inprocess-recompression-probe` is the worked example: every
number in it still reproduces, and its conclusion was still wrong, because the
recipe it tested was not the one the bundler uses. Deleting it would have
destroyed the evidence for how the mistake happened. See F22 and F23.

## How to add an experiment

1. Run the thing. Capture stdout to `logs/<id>.log`.
2. Write `experiments/<id>.json` against [SCHEMA.md](SCHEMA.md). Every field, with
   `null` where unknown.
3. Record the source commit — the exact one, from `git rev-parse HEAD`, on a
   clean tree. If the tree was dirty, say so in `notes`.
4. Add a line to [FINDINGS.md](FINDINGS.md) only if the run supports a *claim*,
   and classify it honestly.

## Why the classifications matter more than the numbers

The ledger's job is to stop a plausible observation becoming a stated fact
through repetition. Something measured once, on one machine, from one pair of
artifacts, is a **strong observation** — it may well be true, and it is not a
demonstrated result. The gap between those two words is where most
overclaiming happens, and naming it per-claim is cheaper than arguing about it
later.

See [FINDINGS.md](FINDINGS.md) for the five classifications and what each
requires.
