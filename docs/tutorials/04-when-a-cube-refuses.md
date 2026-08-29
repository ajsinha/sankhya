<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dice-dark.png">
    <img src="../assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

# Tutorial 4 — When a cube refuses, and why that is the feature

**Time:** about ten minutes · **Before this:** [Tutorial 1](01-your-first-cube.md)
**Every SQL block below is executed by `crates/sankhya-server/tests/guide.rs`.**

---

## The idea

Most of what follows is a list of things this system will not do. That is not a limitations
page. Each refusal replaces a **wrong number** that another system would have handed you
without comment.

A wrong aggregate is the worst kind of defect available. It has the right type, a plausible
magnitude, and no error attached. It gets copied into a report, and by the time anybody
disagrees with it there is no way to reconstruct which of the two figures was wrong.

So the design rule is: **if a question does not have an answer, say so while the query is being
planned.** Not afterwards, and not approximately.

## Refusal 1 — A ratio has no roll-up

```sql
-- ERROR: a ratio cannot be derived from its parts
SELECT region, margin_pct FROM cube_rollup('sales', 'margin_pct', 'by=region');
```

**Why.** The margin of two regions together is not the sum of their margins. It is not the mean
either — that is only right when the regions are the same size, and regions are never the same
size. It is not anything you can compute from the two margins alone: you need the underlying
numerators and denominators, and a rolled-up cell no longer has them.

So `margin_pct` is declared as composing along nothing.

**What to do.** Roll up the *parts* and divide at the end. Store margin as its numerator and
denominator — both of which are additive — and compute the ratio in the outer query, after the
aggregation, where the inputs still exist.

This is the general shape of the fix, and it works because the thing that does not compose is
the division, not the data.

## Refusal 2 — Averaging is the same mistake wearing a friendlier name

An average of averages is an average only when every group is the same size.

It reads as harmless because `avg` is a function every database offers, and because the answer
looks fine. It is the single most common wrong aggregate in analytics, and it is wrong by the
identical argument as the ratio above — a mean is a ratio with the denominator hidden.

**What to do.** The same thing: roll up the sum and the count separately, and divide once at the
end.

## Refusal 3 — A semi-additive measure across the wrong axis

A stock level, a headcount, an account balance: these add across *regions* and do not add across
*time*. Adding January's closing balance to February's gives a number that means nothing.

Here that is not merely rejected — it is **not expressible**. The reduction operator belongs to
the measure, declared per dimension, and is never the caller's to choose. There is no way to
write the query that would sum a balance across time, because there is nowhere in the syntax to
put the wrong operator.

That is a stronger guarantee than a check. A check has to be reached; a thing that cannot be
said has nothing to reach.

**What to do.** Declare the measure with the rule it actually has — `Last` across time,
`Sum` across everything else — and ask for it. The cube applies the declared rule and the
question answers itself.

## Refusal 4 — A measure with no rule at all

A measure declared without an aggregation rule is refused **when the cube is declared**, naming
the measure.

**Why not default to summation?** Because summation is the wrong answer for balances, for
ratios, for averages, for prices and for rates — and it is *silently* wrong for every one of
them. A default here converts a modelling oversight into a wrong number in production, and the
oversight is invisible from that point on.

A cube whose measures nobody has thought about should not become a cube.

**What to do.** Declare a rule per dimension. It takes a minute and it is the minute that makes
every later answer trustworthy.

## Refusal 5 — An option that does not exist

```sql
-- ERROR: 'materialise' must be true, false or pinned
SELECT region, amount FROM cube_rollup('sales', 'amount', 'by=region, materialise=maybe');
```

Unknown option names and unusable option values are both refused rather than ignored.

**Why.** An option that is quietly ignored takes its default, and the result is wrong in a way
the query text does not reveal. Somebody reads the statement, sees the option they meant, and
has no way to know it did nothing. `materialise=fasle` should not silently become
`materialise=true`.

## Refusal 6 — A member reachable by two paths

In a hierarchy with alternate roll-ups, a member can be reachable more than one way. It
contributes **once**.

This is not a refusal you will see as an error — it is a wrong number that does not happen. It
is worth knowing about because double-counting in a ragged or alternate hierarchy is
approximately undetectable by inspection: the total is too large by an amount that looks like
growth.

Ragged hierarchies are handled **natively, never padded**. Padding invents members that do not
exist, and invented members show up in results as real ones.

## Refusal 7 — A threshold you set, enforced

```sql
SELECT region, amount
FROM cube_rollup('sales', 'amount', 'by=region, min_completeness=0.5');
```

If less than half the input reached the cube, this fails rather than returning a number. See
[Tutorial 3](03-completeness-and-policy.md).

An empty result carries *no* completeness rather than `1.0`, so it cannot pass a threshold
either — otherwise the strictest check you can write would pass on the emptiest possible answer.

## What is deliberately not here

**MDX.** Not planned — see [ADR-0007](../adr/0007-the-cube-model.md). The navigation vocabulary
is SQL table functions instead, so a cube is reachable from any client that speaks the
PostgreSQL wire protocol, with no second query language to learn or to secure.

**`CREATE CUBE`.** A cube is registered against a warehouse rather than written in SQL. That is
a gap rather than a decision, and it is recorded as one.

## The shape of all of it

Every refusal above is the same move: **make the contract explicit, then enforce it while the
query is being planned.**

The cost is that you have to say how a measure combines before you can use it. The return is
that no cube in this system can hand you a confidently wrong total — and confidently wrong
totals are what analytical systems are actually for, most of the time, by accident.

## Next

- **[Tutorial 1 — Your first cube](01-your-first-cube.md)**, if you arrived here first.
- **[Tutorial 2 — Making a cube fast](02-making-a-cube-fast.md).**
- **[Tutorial 3 — Completeness and policy](03-completeness-and-policy.md).**
- **[The guide](../GUIDE.md)** for everything else the server does.
