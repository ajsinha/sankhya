"""Naming an instant, reading it back, and the log underneath the name.

    python3 sdk/python/examples/08_snapshots_and_history.py

Creates and drops its own snapshot. Safe to re-run.
"""

from _common import a_table, heading, open_warehouse
import sankhya


def main():
    with open_warehouse() as db:
        table = a_table(db)
        db.drop_snapshot("example_eod", if_exists=True)

        heading("take one")
        # The expiry is REQUIRED and there is no unbounded form. A snapshot pins files, so one
        # that never expired would hold a whole warehouse's versions alive and the cost would
        # fall on somebody who did not ask for it.
        db.take_snapshot("example_eod", expire_after_days=7)
        for held in db.snapshots():
            print(f"  {held.name}  {held.state}  {held.tables} table(s)  taken by {held.taken_by}")

        heading("read as of it")
        # A SESSION setting, because a run reads one instant across many statements. A
        # calculation reading a population, a set of rates, a set of curves and a hierarchy
        # must read all four AS OF ONE INSTANT, or the reconciliation problem this system
        # exists to remove reappears inside a single query.
        db.read_as_of("example_eod")
        print("  as of the snapshot:", db.scalar(f"SELECT count(*) FROM {table}"))
        db.read_the_present()
        print("  right now         :", db.scalar(f"SELECT count(*) FROM {table}"))

        heading("the log underneath the tag")
        for change in db.history(table):
            keeper = change.kept_by or "-"
            changed = "yes" if change.changed_data else "no"
            print(
                f"  v{change.version:<4} {change.what:<10} at {str(change.at or '-'):<21} "
                f"+{change.files_added} -{change.files_removed}  "
                f"changed data: {changed:<3}  kept by: {keeper}"
            )
        print()
        print("  `changed_data` is the WRITER'S OWN DECLARATION, not a guess from the file")
        print("  counts. A compaction rewrites files and changes not one row, so it says no.")
        print("  `kept_by` is empty for a version nothing is keeping alive --- and history is")
        print("  readable only where something is keeping it alive.")

        heading("one table, at a version")
        # Per table and independent of the snapshot: this answers "what did THIS table look
        # like then", where a snapshot answers "what did EVERYTHING look like then".
        # The newest version but one, where there is one --- an older version whose files the
        # present may already have let go is the *other* lesson, and it is below.
        changes = db.history(table)
        if changes:
            at = changes[-1].version
            try:
                db.read_version(table, at)
                print(f"  at version {at}:", db.scalar(f"SELECT count(*) FROM {table}"))
                db.read_the_present_of(table)
                print("  right now    :", db.scalar(f"SELECT count(*) FROM {table}"))
            except sankhya.Refusal as refused:
                # Which is the other half of the lesson: that version's files may be gone.
                print(f"  {refused.sqlstate}  {refused.message}")

        heading("a version this table does not have")
        # Replaying a log stops at its end, so this once handed back the NEWEST version: one
        # nobody has, served as though they had it.
        try:
            db.read_version(table, 9999)
            print("  ...was accepted, which it must not be")
        except sankhya.Refusal as refused:
            print(f"  {refused.sqlstate}  {refused.message}")

        heading("what this is not")
        print("  Not version control. There is no diff between two versions and no way to")
        print("  restore one, because the log records FILES rather than rows: a compaction")
        print("  replaces every file and changes nothing, so a file-level diff would report a")
        print("  maintenance job as a total rewrite. Closer to a tag than a branch.")

        heading("clean up")
        db.drop_snapshot("example_eod")
        print("  snapshots now:", [held.name for held in db.snapshots()])


if __name__ == "__main__":
    main()
