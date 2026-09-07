# ------------------------------------------------------------ CH 6
chapter("6", "Storage, and why the log is the table")

sl, top = content("The layout, on disk, and why the name is the interface",
                  kicker="STORAGE · WHAT IS THERE")
code(sl, ML, top, CW * 0.55, [
    "warehouse/sales/orders/",
    "  _delta_log/00000000000000000000.json   <- create",
    "  _delta_log/00000000000000000001.json   <- a commit",
    "  _delta_log/00000000000000000002.json",
    "  sank_data_date=2026-09-06/",
    "      part-0000-v0000001-2d10910.parquet",
    "      part-0001-v0000002-2d10911.parquet",
], fs=9, title="one self-contained folder per table")
tf = txt(sl, ML + CW * 0.59, top, CW * 0.41, 3.2)
bullets(tf, [
    "`<schema>/<table>/` --- the same names the transactional tier uses, so the "
    "two line up without a mapping table for somebody to get wrong.",
    "The log is the table. The **live set comes from the log, never from a "
    "directory listing** --- a listing includes files a commit has retired and "
    "files a writer has not yet claimed.",
    "`sank_data_date=` is the **business** date of a record, not the moment it "
    "arrived. Conflating those two is the commonest defect in a warehouse, and "
    "Chapter 7 is why it was settled before anything was written.",
    "Statistics live in the log too, so a cold process prunes exactly as well as "
    "a warm one --- 800 files read in 1.37 ms against 10.33 ms for a listing.",
], size=10, gap=8)

sl, top = content("How a commit claims its version, and why rename cannot do it",
                  kicker="STORAGE · CONCURRENCY CONTROL, IN ITS ENTIRETY")
tf = txt(sl, ML, top, CW, 1.0)
runs(tf, [("A committer picks the next version number and creates that file with "
           "`link(2)`. ", CRIMSON, True),
          ("If the name is taken the call **fails**, and that refusal is the whole of the "
           "protocol's concurrency control. There is no lock, no coordinator and no lease --- "
           "the filesystem is the arbiter, and it is one every reader already trusts.",
           SLATE, False)], size=12, first=True, space_after=10, line=1.28)
runs(tf, [("`rename(2)` cannot provide it, because it replaces its destination "
           "**silently**. ", DEEP, True),
          ("Which is exactly what this code did until M8: two committers could both see a "
           "version as free, and the second would overwrite the first with no error to "
           "either. The loser's rows were gone and both writers were told they had "
           "succeeded.", SLATE, False)], size=12, space_after=0, line=1.28)
h = table(sl, [
    ["Requirement", "Consequence"],
    ["The filesystem must implement `link(2)` faithfully", "ext4, xfs, btrfs, zfs, APFS and NTFS qualify"],
    ["**FAT and exFAT do not, and are not supported**", "Not degraded --- unsupported. There is no fallback that is still correct"],
    ["Some network filesystems implement it unreliably", "The failure there is at least loud"],
], ML, top + 1.35, CW, col_w=[5.1, 5.5], fs=10, hfs=10)
note(sl, ML, top + 1.35 + h + 0.24, CW, 1.0,
     "And the commit carries a length seal. ",
     "A truncated commit --- a crash mid-write, a full disk --- would otherwise replay as a "
     "**shorter valid commit**, which is a silently wrong table rather than a broken one. The "
     "seal records how many actions the commit contains, so a short read is refused instead "
     "of being believed.")

sl, top = content("Validated by somebody else's reader",
                  kicker="STORAGE · WHAT OPEN IS WORTH")
tf = txt(sl, ML, top, CW, 1.15)
runs(tf, [("SANKHYA writes the Delta log by hand, so the claim that it is readable "
           "elsewhere had to be checked by something that is not SANKHYA. ", CRIMSON, True),
          ("An independent implementation of the format is a dev-dependency and an oracle: "
           "it must agree about the schema, the version and the live set, across a "
           "compaction and across a checkpoint whose commits were deleted. Disagreeing fails "
           "the build. It found an invalid action that SANKHYA's own reader round-tripped "
           "perfectly --- which is the entire argument for testing against a stranger.",
           SLATE, False)], size=11.5, first=True, space_after=0, line=1.28)
h = table(sl, [
    ["What is actually claimed", "And what is not"],
    ["**One** external reader agrees, on every build", "Nothing in this repository executes Spark, Trino, DuckDB, Snowflake or Athena"],
    ["A protocol version this build cannot honour is **refused**", "Rather than read partially --- a reader that ignores a feature it does not know serves wrong rows confidently"],
    ["Ordinary tables are readable by that implementation", "**A clone is not.** Its log names none of the inherited files, so a foreign reader sees only what the clone wrote"],
    ["The JSON log carries partition values", "A **checkpoint** does not, so an engine starting from one gets none. Still open"],
], ML, top + 1.30, CW, col_w=[4.6, 6.0], fs=9.5, hfs=10)
tf = txt(sl, ML, top + 1.30 + h + 0.22, CW, 0.8)
runs(tf, [("Three exceptions, named. ", DEEP, True),
          ("A bounded claim that survives being checked is worth more than an unbounded one "
           "that does not, and \u201copen storage\u201d with three stated exceptions is a "
           "sentence a reader can act on.", SLATE, False)],
     size=11, first=True, space_after=0, line=1.26)
