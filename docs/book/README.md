<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/wordmark-dice-dark.png">
    <img src="assets/wordmark-dice.png" alt="SANKHYA" width="320">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

---

# SANKHYA

**One deployable artifact that is a transactional store, an analytical engine, and a temporal
graph — over one governed copy of the data.**

The standard enterprise data estate runs three engines, keeps three copies, enforces three
security models, and pays a permanent reconciliation cost to explain why the three disagree.
That cost has no analytical output. It exists because of the architecture, not because of
carelessness, and no vendor sells a cure, because curing it collapses three licences into one.

This book is the whole of SANKHYA: the argument for collapsing that estate, the machine that
does it, how to operate it, how to use it, and — at length, because it is the part most books
omit — **how every claim in the preceding chapters is verified**.

## How to read this

| If you are | Start at |
|---|---|
| Evaluating whether this is worth your time | Chapter 2, *The thesis* |
| An architect assessing the design | Chapter 5, *Architecture* |
| An engineer who wants to run a query today | Chapter 18, *Getting started* |
| Responsible for operating it | Part III |
| Wondering whether to believe any of it | Chapter 23, *How this is tested* |

The contents are in [`SUMMARY.md`](SUMMARY.md). Chapters are being written in order; this book
is assembled from the repository's working documents rather than written apart from them, so a
chapter appears when its subject is settled enough to describe honestly.

## Three commitments

Everything in this book follows from three commitments, and each is **tested rather than
asserted** — a distinction Chapter 23 takes seriously enough to spend a chapter on.

1. **A general-purpose core, with domains as packs.** The engine knows about tenants, tables,
   columns, edges, versions and policies. It knows nothing about any industry. A build check
   fails if a domain noun appears in a core crate, and two reference packs from unrelated
   industries must each change zero core files.

2. **Open storage.** Analytical tables are readable by other engines directly, in a layout that
   mirrors operational naming. The Delta kernel reads the log in the test suite, so the
   open-storage claim is exercised rather than claimed. SANKHYA is a participant in a data
   estate, not a replacement for one.

3. **Provable correctness over asserted correctness.** *"Zero data loss"* is a continuously
   measured metric with an alert attached, not a sentence in a brochure. Every guard has a
   test, and every test has been checked against the defect it claims to catch by applying that
   defect deliberately and requiring the test to fail.

## A note on what is built

This book describes a system under active construction, and it is written to be **honest about
which parts exist**. Where something is designed and not yet built, the text says so and names
the milestone. Where a decision has been made and not yet implemented, it names the decision
record. The status of every milestone is in Chapter 26, and the authoritative, continuously updated
version is in the repository's [`STATUS.md`](../STATUS.md).

A document that describes an intention in the present tense is a document that lies to the
person least able to tell.

---

<p align="center"><sub>SANKHYA — to count is to make completely known.</sub></p>
