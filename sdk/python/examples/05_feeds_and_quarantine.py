"""What the feeds are doing, and what they refused.

    python3 sdk/python/examples/05_feeds_and_quarantine.py

Read-only.
"""

from _common import heading, open_warehouse


def main():
    with open_warehouse() as db:
        heading("every declared feed, including the ones that have never run")
        # A "list of running feeds" would omit the case an operator most needs to see. A feed
        # that has never managed to run once is not a feed that is fine.
        feeds = db.feeds()
        if not feeds:
            print("  none declared on this warehouse")
        for feed in feeds:
            print(f"  {feed.name:<20} {feed.state:<8} runs={feed.runs} "
                  f"published={feed.published} quarantined={feed.quarantined} "
                  f"skipped={feed.skipped} halts={feed.halts}")
            if feed.is_halted:
                # A halted feed says WHY and SINCE WHEN. A feed that halted twice for the same
                # reason is not in the situation a feed that halted once is in, which is why
                # the count survives a resume.
                print(f"    halted since {feed.halted_since}: {feed.reason}")

        heading("what was refused, and why")
        # A record that does not fit is kept, not dropped. Dropping it would make the feed's
        # published count the only evidence anything went wrong, and a count is not a record
        # anybody can fix.
        result = db.quarantine(limit=5)
        print("  columns:", result.columns)
        for row in result.rows:
            print("   ", row)
        if not result.rows:
            print("  (empty --- nothing has been refused)")


if __name__ == "__main__":
    main()
