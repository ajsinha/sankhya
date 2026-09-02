-- SANKHYA from SQL: what is on this server, and how tables are named.
--
--   psql -h 127.0.0.1 -p 5432 -U you -d sankhya -f 01-connect-and-discover.sql
--
-- Nothing here writes. Run it against any SANKHYA.

\echo '== what this server is =='
SELECT version();

\echo ''
\echo '== every table this connection may see =='
-- Filtered by policy server-side. A catalogue listing tables the caller cannot read would
-- disclose their existence -- the leak the policy component refuses everywhere else,
-- arriving through a schema browser.
SELECT table_schema, table_name FROM information_schema.tables;

\echo ''
\echo '== one schema =='
SELECT table_schema, table_name FROM information_schema.tables WHERE table_schema = 'sales';

\echo ''
\echo '== the columns of a table =='
-- Qualify the table. `orders` may exist in several schemas, and asking about the bare name
-- returns every one of their columns interleaved.
SELECT column_name, data_type, is_nullable
FROM information_schema.columns
WHERE table_schema = 'sales' AND table_name = 'orders';

\echo ''
\echo '== psql meta-commands work too =='
\dt
\dn
