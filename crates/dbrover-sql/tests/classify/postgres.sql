-- PostgreSQL statements and what dbrover_sql::classify must call them. See classify_corpus.rs.

-- Reading.
-- expect: read
SELECT 1;
-- expect: read
select * from orders where id = 3 order by id limit 5;
-- expect: read
SELECT a FROM t UNION ALL SELECT b FROM u;
-- expect: read
WITH recent AS (SELECT * FROM orders) SELECT count(*) FROM recent;
-- expect: read
WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 5) SELECT * FROM n;
-- expect: read
SELECT * FROM t WHERE id IN (SELECT id FROM u);
-- expect: read
SELECT * FROM generate_series(1, 3);
-- expect: read
SELECT current_setting('search_path');
-- expect: read
VALUES (1, 2);
-- expect: read
EXPLAIN SELECT * FROM t;
-- expect: read
EXPLAIN (FORMAT JSON) SELECT * FROM t;
-- expect: read
EXPLAIN (ANALYZE, FORMAT JSON) SELECT * FROM t;
-- expect: read
SHOW search_path;

-- Writing, including writes hidden in a WITH.
-- expect: write
INSERT INTO t (a) VALUES (1);
-- expect: write
INSERT INTO t SELECT * FROM u ON CONFLICT DO NOTHING;
-- expect: write
UPDATE t SET a = 1 WHERE id = 2;
-- expect: write
DELETE FROM t;
-- expect: write
MERGE INTO t USING u ON t.id = u.id WHEN MATCHED THEN DELETE;
-- expect: write
TRUNCATE t;
-- expect: write
TRUNCATE TABLE a, b RESTART IDENTITY;
-- expect: write
WITH gone AS (DELETE FROM t WHERE id = 1 RETURNING *) SELECT * FROM gone;
-- expect: write
WITH a AS (SELECT 1), b AS (UPDATE t SET x = 1 RETURNING x) SELECT * FROM b;
-- expect: write
WITH moved AS (INSERT INTO log SELECT * FROM t RETURNING *) SELECT 1;
-- expect: write
SELECT * FROM (WITH d AS (DELETE FROM t RETURNING id) SELECT id FROM d) AS sub;
-- expect: write
SELECT * FROM t FOR UPDATE;
-- expect: write
SELECT * FROM t FOR SHARE;
-- expect: write
EXPLAIN ANALYZE DELETE FROM t;
-- expect: write
EXPLAIN DELETE FROM t;
-- expect: write
EXPLAIN (ANALYZE, FORMAT JSON) DELETE FROM t;
-- expect: write
LOCK TABLE t IN ACCESS EXCLUSIVE MODE;

-- Changing the schema.
-- expect: schema
CREATE TABLE t (id INTEGER PRIMARY KEY);
-- expect: schema
CREATE TABLE copy AS SELECT * FROM t;
-- expect: schema
SELECT * INTO archive FROM orders;
-- expect: schema
SELECT * INTO TEMPORARY scratch FROM orders;
-- expect: schema
WITH recent AS (SELECT * FROM orders) SELECT * INTO archive FROM recent;
-- expect: schema
CREATE INDEX i ON t (a);
-- expect: schema
CREATE VIEW v AS SELECT 1;
-- expect: schema
CREATE MATERIALIZED VIEW mv AS SELECT 1;
-- expect: schema
CREATE SCHEMA reporting;
-- expect: schema
CREATE SEQUENCE s;
-- expect: schema
CREATE FUNCTION f() RETURNS int LANGUAGE sql AS $$ SELECT 1 $$;
-- expect: schema
ALTER TABLE t ADD COLUMN b TEXT;
-- expect: schema
ALTER TABLE t RENAME TO u;
-- expect: schema
DROP TABLE t;
-- expect: schema
DROP VIEW IF EXISTS v CASCADE;
-- expect: schema
DROP SCHEMA reporting;
-- expect: schema
COMMENT ON TABLE t IS 'x';
-- expect: schema
COMMENT ON COLUMN t.a IS 'x';

-- Transactions belong to whoever runs the statement.
-- expect: transaction
BEGIN READ WRITE;
-- expect: transaction
START TRANSACTION;
-- expect: transaction
COMMIT;
-- expect: transaction
ROLLBACK;
-- expect: transaction
SAVEPOINT a;
-- expect: transaction
RELEASE SAVEPOINT a;

-- Administration: grants, roles, databases, settings, files, sessions, procedures.
-- expect: admin
GRANT SELECT ON t TO someone;
-- expect: admin
REVOKE ALL ON t FROM someone;
-- expect: admin
CREATE ROLE intruder;
-- expect: admin
ALTER ROLE intruder WITH SUPERUSER;
-- expect: admin
DROP ROLE intruder;
-- expect: admin
CREATE USER intruder;
-- expect: admin
DROP USER intruder;
-- expect: admin
CREATE DATABASE other;
-- expect: admin
DROP DATABASE other;
-- expect: admin
CREATE EXTENSION pgcrypto;
-- expect: admin
DROP EXTENSION pgcrypto;
-- expect: admin
SET default_transaction_read_only = off;
-- expect: admin
SET TRANSACTION READ WRITE;
-- expect: admin
SET search_path TO other;
-- expect: admin
SELECT set_config('default_transaction_read_only', 'off', false);
-- expect: admin
SELECT pg_reload_conf();
-- expect: admin
COPY t TO '/tmp/out.csv';
-- expect: admin
COPY t FROM '/tmp/in.csv';
-- expect: admin
COPY (SELECT * FROM t) TO PROGRAM 'gzip > /tmp/out.gz';
-- expect: admin
SELECT pg_read_file('/etc/passwd');
-- expect: admin
SELECT lo_export(42, '/tmp/blob');
-- expect: admin
SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE usename = 'someone';
-- expect: admin
SELECT pg_cancel_backend(42);
-- expect: admin
WITH victims AS (SELECT pid FROM pg_stat_activity) SELECT pg_terminate_backend(pid) FROM victims;
-- expect: admin
VACUUM;
-- expect: admin
ANALYZE t;
-- expect: admin
CALL do_things();
-- expect: admin
CALL reporting.refresh(1, 'x');

-- What can't be parsed can't be known.
-- expect: unparseable
DO $$ BEGIN DELETE FROM t; END $$;
-- expect: unparseable
SELEC * FROM t;
