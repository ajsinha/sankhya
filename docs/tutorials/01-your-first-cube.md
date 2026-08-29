<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dice-dark.png">
    <img src="../assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

# Tutorial 1 — Your first cube

**Time:** about ten minutes · **You need:** a running server and a table
**Every SQL block below is executed by `crates/sankhya-server/tests/guide.rs`.** An example
that stops working breaks the build rather than misleading you.

---

## What a cube is here, and what it is not

A cube in Sankhya is a **declared view over a published table**. It is not a second copy of
your data, not a separate store, and not something you build before you can query it.

That matters for what you are about to do. There is no load step, no build step and no wait.
You declare which columns are dimensions and which are measures, and the cube answers from the
table you already have.

The trade is that a cube is a *contract*: you say up front how each measure is allowed to
combine. That is the part most systems leave implicit, and it is why they can hand you a
wrong number without noticing.

## Step 1 — See what is there

Connect with any PostgreSQL client. Start by asking what the warehouse has:

```sql
SELECT * FROM cubes();
```

Then ask what one cube is made of. You do not have to know the model in advance, and neither
does a dashboard built against it:

```sql
SELECT * FROM cube_dimensions('sales');
```

```sql
SELECT * FROM cube_measures('sales');
```

`cube_measures` is the more interesting of the two. It tells you each measure's **rule** along
each dimension — how it is allowed to combine — and that is the thing that decides which
questions the cube will answer.

## Step 2 — Roll up

Rolling *up* means rolling a dimension **away**. The `sales` cube has two dimensions, `region`
and `period`. Group by one and the other is aggregated out:

```sql
SELECT region, amount
FROM cube_rollup('sales', 'amount', 'by=region');
```

Group by the other instead:

```sql
SELECT period, amount
FROM cube_rollup('sales', 'amount', 'by=period');
```

Group by both and you have the base grain — nothing is rolled away. Note the `|`: the options
string is itself comma-separated, so a list uses a different separator. Nesting one inside the
other is how a list silently truncates at its first element.

```sql
SELECT region, period, amount
FROM cube_rollup('sales', 'amount', 'by=region|period');
```

Notice what did *not* happen: no build step ran between these three queries. Each one is
answered from the fact table, or from a stored cuboid if one happens to fit. You did not have
to know which, and the next tutorial shows how to find out.

## Step 3 — Slice

Slicing fixes a member on one axis and drops that axis from the result. It is not the same as
filtering — the dimension is *gone*, not merely narrowed:

```sql
SELECT period, amount
FROM cube_slice('sales', 'amount', 'where=region:north');
```

There is no `region` column in that result, because you have already said which region. A cube
that returned it anyway would be inviting you to group by a column with one value in it.

## Step 4 — Read what the answer says about itself

This is the step people skip, and it is the one worth learning first.

```sql
SELECT region, amount, snapshot, completeness, withheld, materialised
FROM cube_rollup('sales', 'amount', 'by=region');
```

Every cube answer carries these columns:

| Column | The question it answers |
|---|---|
| `snapshot` | *"As of when?"* — the table version this was computed at |
| `completeness` | *"How much of the input did this see?"* |
| `withheld` | *"How many rows did not reach it?"* |
| `materialised` | *"Did this come from stored cells or from the table?"* |
| `from_cuboid` | *"Which stored cells, when it did?"* |

They are columns rather than query metadata on purpose. Metadata is tidier and is lost by the
first `SELECT` that does not mention it — and the moment it is lost, a filtered total looks
exactly like a complete one.

**`snapshot` is what lets you reconcile.** A cube figure and a relational figure taken a minute
apart will differ, and without a version stamp the only available explanation is "one of them
is wrong". With it, the explanation is arithmetic.

## Step 5 — Meet a refusal

Ask for something the cube cannot honestly answer:

```sql
-- ERROR: a ratio cannot be derived from its parts
SELECT region, margin_pct FROM cube_rollup('sales', 'margin_pct', 'by=region');
```

`margin_pct` is a ratio. The margin of two regions together is not the sum of their margins,
nor the mean, nor anything else you can compute from the two margins alone — you need the
underlying numerators and denominators, which a rolled-up cell no longer has.

So `margin_pct` is declared as composing along nothing, and the query is refused **while it is
being planned**. Not after a plausible number has been produced and put on a slide.

This is the trade named at the top. You told the system how the measure combines, so it can
tell you when a question does not have an answer — instead of answering anyway.

## What you have learned

- A cube is a declared view over a published table: no build step, no second store.
- `cube_dimensions` and `cube_measures` let a client discover the model instead of hardcoding it.
- Roll up rolls a dimension *away*; slice *removes* an axis rather than filtering it.
- Every answer states its snapshot, its completeness and where it came from.
- A measure that cannot compose is refused at planning time, not approximated.

## Next

- **[Tutorial 2 — Making a cube fast](02-making-a-cube-fast.md).** Lifetimes, staleness targets,
  and the three controls over what gets stored.
- **[Tutorial 3 — Completeness and policy](03-completeness-and-policy.md).** Why two people can
  correctly get two different totals.
- **[Tutorial 4 — When a cube refuses](04-when-a-cube-refuses.md).** Every refusal, what it
  means, and what to do about it.
