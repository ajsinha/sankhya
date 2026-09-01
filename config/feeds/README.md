# Declared feeds

One file per feed. Each declares a source of newline-delimited JSON documents, the columns
they map to, and what happens to a record that does not fit.

A feed is its own file rather than a section of `application.yaml` because a feed is a
*document* — a list of column structures — and the configuration system flattens YAML to
dotted keys, which is right for `server.listen` and wrong for a list. It also means adding or
removing a feed touches one file that nothing else reads.

```yaml
name: orders                    # appears in metrics, in quarantined records, and in refusals
from: /var/spool/sankhya/orders # a directory of newline-delimited JSON, read in name order
schema: sales
table: orders                   # sales.orders must already exist; a feed never creates it

# Where every row's `sank_data_date` comes from. Required: `DEC-34` says the date is declared
# per table and never defaulted, and the two meanings — the date a row is *about* and the date
# we heard about it — are not interchangeable.
date: ingest
# date: { column: booked_on }   # ...or a declared column, which must be a date and not null

columns:
  - name: id
    type: int64                 # never inferred from the data
  - name: amount
    from: total                 # the key in the document, when it differs from the column
    type: decimal(18,2)         # decimals arrive as *strings*: a JSON number is a double
  - name: note
    type: utf8
    nullable: true
    missing: null               # a missing key means null — said by name, on a nullable column

# What a key no column claims means. `refuse` is the default: a source that grew a field is
# news, and discarding it silently is how a schema change becomes visible six months later.
unknown: refuse

# A batch closes on whichever comes first. Either bound alone stalls: by size, the last records
# of a quiet hour are never published; by time, a busy feed writes a file per tick.
microbatch:
  rows: 10000
  seconds: 30

quarantine:
  retain_days: 30               # mandatory in effect: zero is refused
  window: 100                   # the recent records the stop rate is measured over
  stop_above: 0.2               # above this fraction, the feed stops and waits for a person
```

Types: `boolean`, `int16`, `int32`, `int64`, `float32`, `float64`, `decimal(digits,scale)`,
`utf8`, `binary`, `timestamp_utc`, `timestamp_local`, `date`, `time`, `uuid`, `json`.

`binary` is declared and cannot yet be read from a document: bytes in JSON are text in some
encoding, and which one is the producer's decision rather than a guess this makes.
