-- CQL statements, for Cassandra 5 and ScyllaDB, and what dbrover_sql::classify must call them.
-- From both servers' CQL references. See classify_corpus.rs.

-- Reading.
-- expect: read
SELECT * FROM shop.orders;
// a comment of CQL's own
-- expect: read
SELECT id, total FROM orders WHERE id = 1;
-- expect: read
SELECT JSON * FROM orders WHERE id = 1;
-- expect: read
SELECT DISTINCT customer FROM orders;
-- expect: read
SELECT count(*) FROM orders WHERE customer = 'ann' ALLOW FILTERING;
-- expect: read
SELECT * FROM events WHERE day = '2026-11-01' AND at > '2026-11-01 09:00' ORDER BY at DESC LIMIT 10;
-- expect: read
SELECT * FROM events PER PARTITION LIMIT 2;
-- expect: read
SELECT writetime(total), ttl(total), token(id) FROM orders;
-- expect: read
SELECT toJson(items) FROM orders WHERE id = 1 BYPASS CACHE;
-- expect: read
SELECT * FROM orders USING TIMEOUT 5s;
-- expect: read
select * from "Shop"."Orders";
-- expect: read
SELECT * FROM system.local;
-- expect: read
DESCRIBE KEYSPACES;
-- expect: read
DESCRIBE TABLE shop.orders;
-- expect: read
DESC SCHEMA;
-- expect: read
USE shop;
-- expect: read
USE "Shop";

-- Writing.
-- expect: write
INSERT INTO orders (id, total) VALUES (1, 12.50);
-- expect: write
INSERT INTO orders (id, total) VALUES (1, 12.50) IF NOT EXISTS;
-- expect: write
INSERT INTO orders (id, items) VALUES (2, ['a', 'b']) USING TTL 86400 AND TIMESTAMP 123456789;
-- expect: write
INSERT INTO orders JSON '{"id": 3, "total": 1.5}';
-- expect: write
UPDATE orders SET total = 13 WHERE id = 1;
-- expect: write
UPDATE orders USING TTL 60 SET tags = tags + {'new'} WHERE id = 1 IF total > 10;
-- expect: write
UPDATE counters SET hits = hits + 1 WHERE page = '/';
-- expect: write
DELETE FROM orders WHERE id = 1;
-- expect: write
DELETE total, items[0] FROM shop.orders WHERE id = 1 IF EXISTS;
-- expect: write
TRUNCATE orders;
-- expect: write
TRUNCATE TABLE shop.orders;
-- expect: write
BEGIN BATCH
  INSERT INTO orders (id, total) VALUES (4, 1);
  UPDATE totals SET sum = 1 WHERE day = '2026-11-01';
  DELETE FROM drafts WHERE id = 4;
APPLY BATCH;
-- expect: write
BEGIN UNLOGGED BATCH USING TIMESTAMP 123
  INSERT INTO a (id) VALUES (1)
  INSERT INTO b (id) VALUES (1)
APPLY BATCH;
-- expect: write
BEGIN COUNTER BATCH UPDATE counters SET hits = hits + 1 WHERE page = '/'; APPLY BATCH;

-- Changing the schema.
-- expect: schema
CREATE KEYSPACE shop WITH replication = {'class': 'NetworkTopologyStrategy', 'replication_factor': 3};
-- expect: schema
CREATE KEYSPACE IF NOT EXISTS shop WITH replication = {'class': 'SimpleStrategy', 'replication_factor': 1} AND durable_writes = true;
-- expect: schema
ALTER KEYSPACE shop WITH replication = {'class': 'NetworkTopologyStrategy', 'dc1': 3};
-- expect: schema
DROP KEYSPACE IF EXISTS shop;
-- expect: schema
CREATE TABLE orders (
  id int,
  placed timestamp,
  total decimal,
  items list<text>,
  PRIMARY KEY ((id), placed)
) WITH CLUSTERING ORDER BY (placed DESC) AND comment = 'orders';
-- expect: schema
CREATE COLUMNFAMILY t (id int PRIMARY KEY);
-- expect: schema
ALTER TABLE orders ADD note text;
-- expect: schema
ALTER TABLE orders WITH gc_grace_seconds = 3600;
-- expect: schema
DROP TABLE orders;
-- expect: schema
CREATE TYPE address (street text, city text);
-- expect: schema
ALTER TYPE address ADD zip text;
-- expect: schema
DROP TYPE address;
-- expect: schema
CREATE INDEX ON orders (customer);
-- expect: schema
CREATE INDEX IF NOT EXISTS by_customer ON orders (customer);
-- expect: schema
CREATE CUSTOM INDEX by_note ON orders (note) USING 'StorageAttachedIndex';
-- expect: schema
DROP INDEX shop.by_customer;
-- expect: schema
CREATE MATERIALIZED VIEW orders_by_customer AS
  SELECT * FROM orders WHERE customer IS NOT NULL AND id IS NOT NULL
  PRIMARY KEY (customer, id);
-- expect: schema
ALTER MATERIALIZED VIEW orders_by_customer WITH comment = 'by customer';
-- expect: schema
DROP MATERIALIZED VIEW orders_by_customer;
-- expect: schema
CREATE OR REPLACE FUNCTION twice (input int) RETURNS NULL ON NULL INPUT RETURNS int
  LANGUAGE lua AS $$ return input * 2 $$;
-- expect: schema
DROP FUNCTION twice;
-- expect: schema
CREATE AGGREGATE total (int) SFUNC plus STYPE int INITCOND 0;
-- expect: schema
DROP AGGREGATE total;
-- expect: schema
CREATE TRIGGER audit ON orders USING 'org.example.Audit';

-- Administering.
-- expect: admin
CREATE ROLE analyst WITH PASSWORD = 'secret' AND LOGIN = true;
-- expect: admin
ALTER ROLE analyst WITH PASSWORD = 'other';
-- expect: admin
DROP ROLE analyst;
-- expect: admin
CREATE USER ann WITH PASSWORD 'secret' NOSUPERUSER;
-- expect: admin
DROP USER ann;
-- expect: admin
GRANT SELECT ON KEYSPACE shop TO analyst;
-- expect: admin
REVOKE MODIFY ON shop.orders FROM analyst;
-- expect: admin
GRANT analyst TO ann;
-- expect: admin
LIST ROLES;
-- expect: admin
LIST ALL PERMISSIONS OF analyst;
-- expect: admin
CREATE SERVICE LEVEL slow WITH timeout = 30s;
-- expect: admin
ATTACH SERVICE LEVEL slow TO analyst;
-- expect: admin
PRUNE MATERIALIZED VIEW orders_by_customer WHERE token(customer) > 0;
-- expect: admin
USE system_auth;
-- expect: admin
USE "system";

-- Not CQL.
-- expect: unparseable
SELEC * FROM orders;
-- expect: unparseable
BEGIN TRANSACTION;
-- expect: unparseable
'not a statement';
