-- MySQL and MariaDB statements and what dbrover_sql::classify must call them. See
-- classify_corpus.rs.

-- Reading.
-- expect: read
SELECT 1;
-- expect: read
SELECT `order` FROM `t`;
-- expect: read
SELECT * FROM shop.orders WHERE id IN (SELECT id FROM shop.items);
-- expect: read
WITH recent AS (SELECT * FROM orders) SELECT count(*) FROM recent;
-- expect: read
SHOW TABLES;
-- expect: read
SHOW CREATE TABLE t;
-- expect: read
SHOW COLUMNS FROM t;
-- expect: read
DESCRIBE t;
-- expect: read
EXPLAIN SELECT * FROM t;
-- expect: read
EXPLAIN FORMAT=JSON SELECT * FROM t;
-- expect: read
EXPLAIN ANALYZE FORMAT=JSON SELECT * FROM t;
-- expect: read
ANALYZE FORMAT=JSON SELECT * FROM t;
-- expect: write
EXPLAIN ANALYZE FORMAT=JSON DELETE FROM t;
-- expect: write
ANALYZE FORMAT=JSON DELETE FROM t;
-- expect: admin
ANALYZE TABLE t;

-- Writing.
-- expect: write
INSERT INTO t (a) VALUES (1);
-- expect: write
INSERT INTO t (a) VALUES (1) ON DUPLICATE KEY UPDATE a = a + 1;
-- expect: write
REPLACE INTO t (a) VALUES (1);
-- expect: write
UPDATE t SET a = 1 WHERE id = 2;
-- expect: write
DELETE FROM t WHERE id = 2;
-- expect: write
WITH ids AS (SELECT 1 AS id) DELETE FROM t WHERE id IN (SELECT id FROM ids);
-- expect: write
WITH ids AS (SELECT 1 AS id) UPDATE t SET a = 2 WHERE id IN (SELECT id FROM ids);
-- expect: write
TRUNCATE t;
-- expect: write
TRUNCATE TABLE t;
-- expect: write
SELECT * FROM t FOR UPDATE;
-- expect: write
LOCK TABLES t WRITE;

-- Changing the schema.
-- expect: schema
CREATE TABLE t (id INT PRIMARY KEY);
-- expect: schema
CREATE TABLE copy AS SELECT * FROM t;
-- expect: schema
CREATE INDEX i ON t (a);
-- expect: schema
CREATE VIEW v AS SELECT 1;
-- expect: schema
ALTER TABLE t ADD COLUMN b TEXT;
-- expect: schema
DROP TABLE t;
-- expect: schema
RENAME TABLE t TO u;
-- expect: schema
SELECT a INTO @x FROM t;

-- Transactions.
-- expect: transaction
START TRANSACTION READ WRITE;
-- expect: transaction
BEGIN;
-- expect: transaction
COMMIT;
-- expect: transaction
ROLLBACK;

-- Administration.
-- expect: admin
GRANT SELECT ON shop.* TO 'someone'@'%';
-- expect: admin
REVOKE INSERT ON shop.t FROM 'someone'@'%';
-- expect: admin
CREATE USER intruder;
-- expect: admin
DROP USER intruder;
-- expect: admin
CREATE ROLE reporting;
-- expect: admin
CREATE DATABASE other;
-- expect: admin
DROP DATABASE other;
-- expect: admin
SET autocommit = 0;
-- expect: admin
SET GLOBAL max_connections = 1;
-- expect: admin
SET SESSION TRANSACTION READ WRITE;
-- expect: admin
USE other;
-- expect: admin
KILL 42;
-- expect: admin
KILL QUERY 42;
-- expect: unparseable
LOAD DATA INFILE '/tmp/in.csv' INTO TABLE t;
-- expect: admin
CALL do_things();
-- expect: admin
CALL shop.refresh(1);
-- expect: admin
FLUSH PRIVILEGES;

-- Neither read nor known: files written by a SELECT.
-- expect: unparseable
SELECT * FROM t INTO OUTFILE '/tmp/out';
-- expect: unparseable
SELECT * FROM t INTO DUMPFILE '/tmp/out';
