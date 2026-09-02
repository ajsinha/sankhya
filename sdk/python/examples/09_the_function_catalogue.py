"""Every built-in this server offers, found and called from Python.

    python3 sdk/python/examples/09_the_function_catalogue.py

Read-only.
"""

from _common import heading, open_warehouse
import sankhya


def main():
    with open_warehouse() as db:
        heading("what can this server compute")
        # The question that had no answer until the catalogue existed: the only way to learn
        # was to read the server's source. A capability nobody can enumerate is a reference
        # manual nobody reads.
        catalogue = db.functions()
        print(f"  {len(catalogue)} function(s)")
        by_category = {}
        for entry in catalogue:
            by_category.setdefault(entry.category, []).append(entry)
        for category, entries in sorted(by_category.items()):
            print(f"    {category:16} {len(entries)}")

        heading("calling them")
        # `db.fn` is driven by that same catalogue rather than by a stub per function. A
        # function added to the server appears here with no change to the binding.
        print("  norm_inv(0.975)          ", db.fn.norm_inv(0.975))
        print("  t_inv(0.975, 10)         ", db.fn.t_inv(0.975, 10))
        print("  chisq_sf(18.307, 10)     ", db.fn.chisq_sf(18.307, 10))
        print("  erf(1.0)                 ", db.fn.erf(1.0))

        heading("a series in, a series out")
        # A Python list becomes a SQL array on the way in and comes back a list, so nothing
        # here is a string the caller has to pick apart.
        covariance = [4, 12, -16, 12, 37, -43, -16, -43, 98]
        print("  mat_cholesky(...)        ", db.fn.mat_cholesky(covariance))
        print("  mat_eigenvalues(...)     ", db.fn.mat_eigenvalues(covariance))
        print("  is it positive definite? ", db.fn.mat_is_positive_definite(covariance))

        heading("a matrix no data could have produced")
        # Every pair correlated at -0.9, which three variables cannot simultaneously be. The
        # factorisation failing IS the test: Cholesky succeeds exactly on the positive-definite
        # matrices, so the refusal names a real impossibility.
        impossible = [1, -0.9, -0.9, -0.9, 1, -0.9, -0.9, -0.9, 1]
        print("  is it positive definite? ", db.fn.mat_is_positive_definite(impossible))
        try:
            db.fn.mat_cholesky(impossible)
            print("  ...it factored, which it must not")
        except sankhya.Refusal as refused:
            print(f"  {refused.sqlstate}  {refused.message[:78]}")

        heading("statistics over a series")
        before = [10.0, 12.0, 9.0, 11.0, 13.0, 10.5]
        after = [12.1, 13.9, 11.2, 12.8, 15.3, 12.4]
        print("  paired t                 ", db.fn.ttest_paired_t(after, before))
        print("  its p-value              ", db.fn.ttest_paired_p(after, before))
        print()
        print("  A slope with no standard error is a number nobody can act on, so the")
        print("  uncertainty is named beside the estimate rather than left to be derived:")
        x = [1, 2, 3, 4, 5, 6, 7, 8]
        y = [2.1, 4.2, 5.9, 8.1, 10.0, 12.2, 13.8, 16.1]
        print("  slope                    ", db.fn.regress_slope(x, y))
        print("  standard error           ", db.fn.regress_stderr(x, y))
        print("  p-value                  ", db.fn.regress_pvalue(x, y))
        print("  R-squared                ", db.fn.regress_r2(x, y))

        heading("a name this server does not have")
        # Caught by the binding, because a typo in a function name is the binding's to catch.
        # A wrong *argument* is the server's, and its refusal is the one that knows why.
        try:
            db.fn.norm_invv(0.5)
        except AttributeError as missing:
            print(f"  {missing}")

        heading("and a wrong argument, which the server answers")
        try:
            db.fn.norm_inv(1.5)
        except sankhya.Refusal as refused:
            print(f"  {refused.sqlstate}  {refused.message[:96]}")


if __name__ == "__main__":
    main()
