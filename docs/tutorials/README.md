<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../assets/wordmark-dice-dark.png">
    <img src="../assets/wordmark-dice.png" alt="SANKHYA" width="300">
  </picture>
</p>

<p align="center"><em>To count is to make completely known.</em></p>

# Tutorials

> **The book.** [`docs/book/`](../book/README.md) is the long-form companion to this document --- twenty-seven chapters, and the only complete table of `SANKHYA_*` environment variables (Chapter 17, *Packaging and deployment*).

Hands-on, in order, each one about ten to fifteen minutes. They assume a running server —
[`QUICKSTART.md`](../QUICKSTART.md) gets you one.

**Every SQL example in every tutorial is executed by a test.**
`crates/sankhya-server/tests/guide.rs` extracts the fenced `sql` blocks from these files and
runs them against a real server, so an example that stops working breaks the build rather than
misleading you. A second test asserts that every tutorial on disk is in that list, so a new one
cannot be added and quietly left unverified.

That is worth saying plainly, because a tutorial is the document a reader trusts most — you are
following it step by step with no independent way to tell a stale instruction from a current
one. An untested tutorial rots in the worst possible place.

| | Tutorial | What you get |
|---|---|---|
| 1 | [Your first cube](01-your-first-cube.md) | Declare, roll up, slice, and read what an answer says about itself |
| 2 | [Making a cube fast](02-making-a-cube-fast.md) | Lifetimes, staleness targets, and the three controls over what gets stored |
| 3 | [Completeness and policy](03-completeness-and-policy.md) | Why two people correctly get two different totals |
| 4 | [When a cube refuses](04-when-a-cube-refuses.md) | Every refusal, what it means, and what to do instead |

## Where to go next

| | |
|---|---|
| [`../GUIDE.md`](../GUIDE.md) | Every feature by worked example — tables, vectors, graphs, security, repair, metrics |
| [`../QUICKSTART.md`](../QUICKSTART.md) | Build it, load ten gigabytes, watch capture reconcile |
| [`../ARCHITECTURE.md`](../ARCHITECTURE.md) | How it is put together, and the measurements behind the choices |
| [`../adr/`](../adr/) | The decisions, each with the question that prompted it |
| [`../STATUS.md`](../STATUS.md) | What is built, what is not, and what was got wrong on the way |
