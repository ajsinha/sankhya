<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/wordmark-dice-dark.png">
    <img src="assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

# SANKHYA — Adversarial review

**Document ID:** SNK-AR-001
**Version:** 0.1.0
**Status:** Implementation — M0, M1, M3, M4, M7 and M10 complete; M2 and M13 substantially built; M5 closed on four of five exit criteria; M6 on six of seven; M8 on six of eight, its scale-out half moved to M12 for want of a second machine; M9 in progress, its work built and demonstrated and its gate held for M11; M14, M17 and M18 in progress
**Date:** 2026-09-01
**Companions:** `STATUS.md`, `INVARIANTS.md`, `SOAK.md`

---

## 1. What this document is

The method and the record of a **timed adversarial review**: several reviewers, working
independently against one running server, each trying to break a different property, with every
finding verified before it is believed.

It is not a replacement for the gate, the soak, or the mutation audit. Those check that the
system does what it is built to do. This checks the other thing: **that what it is built to do
is reachable, and that the reasons it gives when it refuses are true.**

## 2. Why the review exists, in one paragraph of evidence

Between 2026-08-31 and 2026-09-01, four defects were found and every one of them had passing
tests, several had passing *mutation* tests, and none was found by the suite:

| Defect | What was passing while it was broken |
|---|---|
| `SHOW FEEDS` answered by the catalogue as an empty setting | the command's own unit tests, and two mutations |
| `sales.orders` did not resolve; the schema was discarded at registration | every query test, all of which used bare names |
| Two tables of one name silently replaced each other | every test, none of which had two |
| `CREATE TABLE ... CLONE` had never worked against a servable table | thirteen clone tests, on a fixture with a layout no deployment has |

The pattern does not vary. **A surface's own tests call the surface**, and the layer above it is
where the statement goes missing. The fourth is the sharpest: the fixture was built through the
product's own writer --- the rule that exists to prevent exactly this --- and still encoded a
shape the product does not use.

So the review's bias is toward **execution through the front door**, by clients that know
nothing about the code.

## 3. The method

### 3.1 One server, many reviewers

One SANKHYA process, one warehouse, one schema per reviewer, started by
[`tools/review-server.sh`](../tools/review-server.sh) and the warehouse written by
`make_warehouse.rs`'s `write_a_review_warehouse`.

**No reviewer runs `cargo`.** Not a suggestion: this is a one-machine project, a workspace build
takes minutes and tens of gigabytes, and several at once take the box down. A review that kills
the machine it is reviewing has proved nothing. Builds are serialised through one person.

Sharing is not only a concession to the machine. Several clients reading and writing
concurrently in several schemas *is* the isolation the server claims to provide, exercised by
people trying to break it rather than by the person who wrote it.

### 3.2 Two kinds of client, deliberately

- **`psql` 17.11**, built from the vendored source. A real third-party client that sends the
  catalogue queries real tools send, in the spellings they send them --- which is how `\dt` and
  the settings queries get exercised without anybody thinking to write them down.
- **The Python binding**, `sdk/python`. Pure Python, no compiled dependency, so it runs anywhere
  the interpreter does.

Both, because they fail differently. `psql` catches what a driver expects and does not get; the
binding catches what a program can and cannot do with the answer.

### 3.3 Distinct lenses, not more reviewers

Reviewers told to "find bugs" return the same three findings. Each is given one property and
told to attack it:

| Lens | The question |
|---|---|
| Reachability | Is every statement the server claims to implement actually reachable, from both doors, with the same answer? |
| Refusals | Does every refusal fail closed, carry a code and a remediation, and name what it cites? |
| Isolation | What can one schema learn about another --- through queries, catalogues, error text, or timing? |
| Concurrency and durability | Under several clients at once and a restart, does anything duplicate, skip, or vanish? |
| Correctness | Decimals, dates, nulls, empty tables, projections --- and does the documentation describe what actually happens? |

### 3.4 Every finding is verified before it is believed

A reported finding is a **claim**, not a defect. Each one is independently attacked --- the
verifier's job is to *refute* it --- and only what survives is acted on. Without this step the
time goes on plausible-but-wrong claims, which is the characteristic failure of a large review.

Findings are recorded with their verdict either way. A refuted claim is worth keeping: it says
where the system is confusing enough that a careful reader got it wrong.

### 3.5 Rounds, not one long run

Round one runs blind. Findings are verified, the confirmed ones are fixed or filed, and round
two is aimed at what the first round disturbed. A single long round spends its second half
re-finding what its first half already found.

## 4. Runs

### 4.1 Run 1 --- 2026-09-01

**Set-up.** Warehouse: `common.orders` (4 files), `common.regions`, `common.empty` (a table with
a log and no rows), `probe_a.orders`, and `scratch` in each of `probe_a` .. `probe_e`.
`common.orders` and `probe_a.orders` **share a bare name on purpose**, so an ambiguous name is a
live case rather than a unit test.

**Findings before the reviewers started.** Two, from building the harness itself, which is worth
recording as evidence that the method works before it is applied:

1. **An ambiguous bare name said "table not found".** True and useless: `orders` resolves to
   nothing when two schemas hold one, and a user cannot tell that from a typo. The refusal now
   names both candidates and says to qualify. The SQLSTATE is unchanged, because a client
   dispatches on it and a message is not an API --- only the detail changed, and only so that a
   person can act on it.

2. **The harness needed a client, and the client is the SDK.** Writing `sdk/python` as the
   review's instrument rather than as a deliverable meant its first user was somebody trying to
   break it. It found nothing yet; the point is the order.

_Reviewer findings are recorded below as they are verified._

---

<p align="center"><sub>SANKHYA — to count is to make completely known.</sub></p>
