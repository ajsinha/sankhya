"""Zero-copy cloning: a table that costs a log and no files.

    python3 sdk/python/examples/03_cloning.py

Creates and drops its own clone. Safe to re-run.
"""

from _common import a_table, heading, open_warehouse
import sankhya


def main():
    with open_warehouse() as db:
        origin = a_table(db)
        schema = origin.split(".")[0] if "." in origin else ""
        # A clone STAYS IN ITS ORIGIN'S SCHEMA. Not a limitation --- a clone that could move
        # would let one schema's access rules be escaped by cloning across the boundary.
        clone = f"{schema}.example_clone" if schema else "example_clone"

        db.drop(clone, if_exists=True)

        heading("clone it")
        rows = db.scalar(f"SELECT count(*) FROM {origin}")
        db.clone(clone, origin)
        print(f"  {origin}: {rows} rows")
        print(f"  {clone}: {db.scalar(f'SELECT count(*) FROM {clone}')} rows")
        print("  and nothing was copied: the clone's log names none of the origin's files")

        heading("lineage")
        for step in db.lineage_of(clone):
            print(f"  step {step.step}: {step.origin} at version {step.origin_version}")

        heading("who still reads the origin")
        # The question that has to be answerable before anything is dropped. A clone reads its
        # origin's files, so dropping the origin would take a table that is still being read.
        for dependent in db.dependents_of(origin):
            kind = "directly" if dependent.is_direct else "indirectly"
            print(f"  {dependent.name} reads it {kind}, at version {dependent.reads_version}")

        heading("a clone of a clone")
        deeper = f"{schema}.example_clone_2" if schema else "example_clone_2"
        db.drop(deeper, if_exists=True)
        db.clone(deeper, clone)
        print("  lineage of the second:")
        for step in db.lineage_of(deeper):
            print(f"    step {step.step}: {step.origin} at version {step.origin_version}")
        print("  each step is recorded, so nothing has to guess where the files are")

        heading("dropping an origin something still reads is refused")
        try:
            db.drop(clone)
            print("  ...after the clone of it went, this one drops cleanly")
        except sankhya.Refusal as refused:
            print(f"  {refused.sqlstate}: {refused.message}")

        heading("clean up")
        db.drop(deeper, if_exists=True)
        db.drop(clone, if_exists=True)
        print("  gone:", not db.exists(clone) and not db.exists(deeper))


if __name__ == "__main__":
    main()
