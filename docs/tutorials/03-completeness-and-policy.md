<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dice-dark.png">
    <img src="../assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

# Tutorial 3 — Completeness, and why two people get two different totals

**Time:** about ten minutes · **Before this:** [Tutorial 1](01-your-first-cube.md)
**Every SQL block below is executed by `crates/sankhya-server/tests/guide.rs`.**

---

## The problem this solves

Two analysts run the same query. One may see every region, the other only the north. They get
different totals.

**Both totals are correct.** An aggregate is computed over the rows the caller may read, so a
restricted caller's total is genuinely the total of what they may see.

The danger is not the difference. It is that a number arrives with no indication that it is
partial. A total over half the data looks exactly like a total over all of it — same type, same
plausible magnitude, no error anywhere. Somebody puts it in a report and nobody can tell.

Most systems make an operator choose: enforce the policy and hand out silently-partial
aggregates, or bypass it and leak. Neither is acceptable, and the choice is a false one.

## Step 1 — Every answer states what it saw

```sql
SELECT region, amount, completeness, withheld
FROM cube_rollup('sales', 'amount', 'by=region');
```

| Column | Meaning |
|---|---|
| `completeness` | the fraction of the intended input that reached these cells |
| `withheld` | how many rows did not |

A completeness of `1.0` means every intended row contributed. Anything less means something did
not, and `withheld` says how much.

So a filtered total is **distinguishable by looking at it**, rather than by knowing which role
you happened to be connected as.

## Step 2 — Understand where the number comes from

This is the part worth being precise about, because the obvious implementation is wrong.

Completeness **cannot be computed from the result**. A withheld row leaves no trace: it is not
in the cells, not in a null, not anywhere. An aggregate that counts what arrived and divides by
what arrived reports itself complete however much policy removed — always, and with total
confidence.

So the withheld count comes from **the filter that did the withholding**, or it does not exist.
It is carried from the point of enforcement to the point of presentation, and it is a required
field the whole way. A stored cuboid carries it too, so a cube served from storage still says
how much of the fact table it saw rather than assuming the answer.

## Step 3 — Rows that could not be placed

Policy is not the only reason a row fails to reach a cube. A row with a null dimension key, or a
null measure, cannot be placed on the grid.

Those rows are **counted, never dropped**. Skipping them leaves the total quietly short — which
is the same failure as a policy-filtered total presented as complete, so it gets the same
machinery and shows up the same way in `withheld`.

## Step 4 — Insist on a threshold

Reading the column is good. Sometimes you want the query to refuse rather than hand you
something you have to remember to check:

```sql
SELECT region, amount
FROM cube_rollup('sales', 'amount', 'by=region, min_completeness=0.5');
```

If less than half the input reached the cube, the query fails instead of returning a number.

Use this on anything automated. A human might notice a completeness of `0.3`; a nightly job
writing to a dashboard will not.

## Step 5 — Know the empty case

An aggregate over **no rows** has *no* completeness. It is not complete.

That sounds like hair-splitting and is not. If "nothing at all" reported completeness `1.0`,
then an empty result would sail through every `min_completeness` threshold you set — the
strictest possible check would pass on the emptiest possible answer. So the fraction is *absent*
rather than `1.0`, and a threshold refuses it.

It is the same absent-versus-zero distinction the cube keeps everywhere: a cell that does not
exist is not a cell containing zero.

## Step 6 — What this means for materialising

A stored cuboid is keyed by the **scope** it was computed under — a digest of what the guard
*permits*: tenant, table, action, row filter, column masks.

Deliberately **not** who was asking. That single decision does three jobs:

1. **It bounds the population.** A thousand analysts across six roles produce six scopes, not a
   thousand.
2. **It answers the standing-grant problem.** A cuboid is tied to an entitlement *set*, not a
   person. Change the policy and the digest changes, the key no longer matches, and the old
   cells are simply never found. There is no stale-permission window to close, because there is
   no lookup that can succeed.
3. **It decides who may be served.** The background refresher builds the *unrestricted* cuboid.
   It may serve only a caller whose guard withholds nothing — checked by asking the guard, not
   by comparing digests.

The practical consequence, again: **materialising helps callers who share an entitlement set.**
A cube read by twenty differently-restricted analysts mostly produces cells nobody else may use.
That is a reason to leave such a cube Declared, not a defect.

## What you have learned

- Two principals correctly get different totals; the difference is the point, not the bug.
- Completeness is carried from the filter, never derived from the result — a withheld row leaves
  no trace.
- Unplaceable rows are counted, not dropped.
- `min_completeness` turns a thing you must remember to check into a thing the query enforces.
- An empty aggregate has no completeness, so it cannot pass a threshold.
- Stored cells are keyed by entitlement set, which is what makes them safe to reuse.

## Next

- **[Tutorial 4 — When a cube refuses](04-when-a-cube-refuses.md).**
- **[Tutorial 2 — Making a cube fast](02-making-a-cube-fast.md)**, if you skipped it.
