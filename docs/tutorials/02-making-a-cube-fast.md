<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dice-dark.png">
    <img src="../assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

# Tutorial 2 — Making a cube fast

**Time:** about fifteen minutes · **Before this:** [Tutorial 1](01-your-first-cube.md)
**Every SQL block below is executed by `crates/sankhya-server/tests/guide.rs`.**

---

## The thing to understand before any of the controls

A materialised cuboid is a **cache**, and it is a cache in a stricter sense than usual: it
cannot change the answer.

The reason is the key. A stored cuboid is keyed by *(definition version, snapshot, scope,
shape)*. Every one of those is part of the lookup, so a new commit produces a **miss**, not a
stale hit. There is no invalidation protocol, no time-to-live to tune, and no window in which
something old is served as though it were current.

That is what makes automatic materialisation safe. Being wrong about what to cache costs you
latency; it cannot cost you correctness. If it could, none of the rest of this tutorial would
be a good idea.

## Step 1 — Find out where an answer came from

Before tuning anything, learn to read what is happening:

```sql
SELECT region, amount, materialised, snapshot
FROM cube_rollup('sales', 'amount', 'by=region');
```

`materialised` is `false` when the answer was computed from the fact table and `true` when it
came from stored cells. It reports **what happened** — not what you asked for.

That distinction is not pedantry. This column previously echoed an argument the caller passed,
so an operator asking "why was this fast?" was told whatever their own query had typed. A
diagnostic that reports its input is worse than no diagnostic, because it looks like evidence.

## Step 2 — Choose a lifetime

| | Persisted | Materialised | Maintained by | Ends when |
|---|---|---|---|---|
| **Ephemeral** | no | no | nothing | the session ends |
| **Declared** | yes | no | nothing | it is dropped |
| **Maintained** | yes | yes | the warehouse | it is dropped |

**Ephemeral is the default, deliberately.** Exploring should not require deciding whether a
question deserves to be durable, and a warehouse should not accumulate a definition per
abandoned question.

**Declared** costs one small file and computes on demand. It is right for a cube asked about
occasionally — and for any cube whose readers have *different* permissions, for a reason
covered in [Tutorial 3](03-completeness-and-policy.md): a stored aggregate is only usable by
callers entitled to exactly the rows it was built from, so materialising a cube read by twenty
differently-restricted analysts mostly produces cells nobody may use.

**Maintained** adds a `target_lag`, and the warehouse keeps the cube within it whether or not
anybody is logged in.

## Step 3 — Understand `target_lag`

`target_lag` is a **staleness target, not a schedule**. `target_lag = 5` means *these cells may
be at most five commits behind* — not *rebuild every five commits*.

A schedule is wrong in both directions at once. It rebuilds when nothing has changed, and it
fails to rebuild when a build takes longer than its interval. A target says the thing you
actually care about.

And here the target is **checkable rather than estimated**, because a stored cuboid records the
version it was computed at. Staleness is the distance from the table's current version: an
integer, known without reading a clock. A duration would have to be inferred from commit rates,
and an inferred SLA is right while the system behaves and wrong exactly when it does not.

**A cuboid past its target is never served as though it were fresh.** The answer falls back to
live aggregation — slower and correct — and says `materialised = false`, so you can see which
one you got.

## Step 4 — The three controls

The lattice of possible cuboids is exponential in the dimension count, so "store everything" is
not a plan. Three controls decide what actually gets stored, and they belong to three different
people on purpose.

| Level | Who | What it says |
|---|---|---|
| **Definition** | whoever models the cube | shapes **pinned** — always worth holding |
| **Configuration** | the operator | the row **budget** automatic selection may spend |
| **Session** | the caller | whether *this* query uses materialisation at all |

### The definition pins

Automatic selection spends the operator's budget on **evidence**: the shapes people have
actually asked for. A pin is the statement that a shape is worth holding *before* any evidence
exists — the month-end roll-up nobody runs until the day it has to be instant.

So pinned shapes are not put through selection. A pin that had to win against a query log would
not be a control at all.

### The operator budgets

```toml
[cubes]
budget_rows = 10000000
```

It is the operator's storage, being spent on their behalf by a selection reading somebody
else's queries — so it is bounded by a number they set rather than by whatever the lattice
happens to contain.

Set it to `0` and automatic selection buys nothing. The base cuboid and any pinned shape are
still built: neither is bought from the budget.

### The caller may ask for less — and only less

```sql
SELECT region, amount, materialised
FROM cube_rollup('sales', 'amount', 'by=region, materialise=false');
```

There is deliberately **no value that widens anything**. A session that could raise the budget
would be an unbounded storage grant to anybody who can open a connection — a resource
exhaustion with a polite interface.

| Value | Effect |
|---|---|
| *(omitted)* | use whatever the definition and configuration provide |
| `materialise=pinned` | use only shapes the definition names, not ones bought from a query log |
| `materialise=false` | compute from the base data |

An unrecognised value is refused while the query is planned, rather than quietly taking its
default:

```sql
-- ERROR: 'materialise' must be true, false or pinned
SELECT region, amount FROM cube_rollup('sales', 'amount', 'by=region, materialise=maybe');
```

That refusal exists because a misspelled option that silently defaults produces a result which
is wrong in a way the query text does not reveal.

## Step 5 — Use `materialise=false` as a check, not a workaround

This is the most useful habit in the tutorial.

```sql
SELECT region, amount
FROM cube_rollup('sales', 'amount', 'by=region, materialise=false');
```

Run the same question with and without it. **The two answers must be bit-identical.** Not close,
not within a tolerance — identical.

If they ever differ, you have found a defect, not a tuning question. Materialisation is a cache
and a cache that changes the answer is not one.

Getting that property was not free, and it is worth knowing why. A cube rolls up in stages and
every stage rounds, so `round(round(a+b) + round(c+d))` is not `round(a+b+c+d)`. Fixing the
*order* of summation makes one reduction reproducible and does nothing about **associativity** —
and a stored cuboid is precisely a re-association of the same addition. The discrepancy measured
at one unit in the last place: large enough for two reports to disagree by a penny, small enough
that nobody can point at a defect. So a stored aggregate keeps its value **unrounded**, as the
components of an exact expansion, and rounds once when read.

## Step 6 — Know what gets chosen, and why

Automatic selection reads a **query log**: a bounded record, per cube, of which dimensions
people grouped by.

The repetition is the weighting — a shape asked ten times counts ten times — and old entries are
overwritten, so a dashboard nobody has opened in a week stops pinning storage without anybody
deciding it should. The log is bounded because a structure that grows once per query and is
never trimmed is a leak with a business justification.

It records a *shape*: which cube, which dimensions. There is nowhere in it to put a member, a
predicate, or who was asking. Worth stating plainly, because a query log is the kind of thing
that quietly becomes a record of who asked what about whom.

**A cube nobody has queried gets its base cuboid and nothing else.** That is the honest answer
rather than a guess — there is no evidence about what would help, and spending an operator's
storage on a guess is worse than spending none.

## Step 7 — Know who a stored cuboid can serve

A background refresh has no principal — nobody is logged in at four in the morning — so it
builds the **unrestricted** cuboid: an aggregate over every row.

That cuboid may serve only a caller whose own permissions withhold nothing. Serving it to
somebody a row policy filters would be a disclosure through arithmetic, and an invisible one:
the number is real, it is simply computed over rows they may not read. No error, nothing in a
log to find.

So: **background refresh helps dashboards and service accounts, and does nothing for a
restricted analyst.** Their cuboids can only be built by their own queries. Know this before you
measure, or you will conclude materialisation is broken when it is behaving exactly as designed.

## A checklist

1. Is the cube read often enough to be worth storage? If not, leave it **Declared**.
2. Do its readers share permissions? If they are all differently restricted, materialising helps
   almost nobody.
3. Set `target_lag` to the staleness you can actually tolerate, not to a rebuild frequency.
4. Pin the shapes you know matter. Let the budget buy the rest from evidence.
5. Check `materialised` to see what is happening, and `materialise=false` to check the number.

## Next

- **[Tutorial 3 — Completeness and policy](03-completeness-and-policy.md).**
- **[Tutorial 4 — When a cube refuses](04-when-a-cube-refuses.md).**
