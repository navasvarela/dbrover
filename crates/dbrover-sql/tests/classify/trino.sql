-- Trino's and Athena's statements, and what dbrover_sql::classify must call them. See
-- classify_corpus.rs. Athena's DDL is Hive's, which `sqlparser`'s generic dialect reads too.

-- Reading.
-- expect: read
select orderkey, totalprice from tpch.tiny.orders where orderdate >= date '1995-01-01' order by 2 desc limit 10;
-- expect: read
select * from t cross join unnest(arr) with ordinality as u(x, n);
-- expect: read
select transform(arr, x -> x + 1), reduce(arr, 0, (s, x) -> s + x, s -> s) from t;
-- expect: read
with recent as (select * from orders where ts > now() - interval '1' hour) select * from recent;
-- expect: read
select * from t offset 10 rows fetch next 5 rows only;
-- expect: read
select * from t tablesample bernoulli (10);
-- expect: read
select * from (values (1, 'a'), (2, 'b')) as v(n, s);
-- expect: read
select * from system.runtime.queries;
-- expect: read
show catalogs;
-- expect: read
show schemas from tpch;
-- expect: read
show tables from tpch.tiny like 'o%';
-- expect: read
show columns from t;
-- expect: read
describe t;
-- expect: read
show create table t;
-- expect: read
show create view v;
-- expect: read
show functions like 'array%';
-- expect: read
show session;
-- expect: read
show stats for t;
-- expect: read
show partitions orders;
-- expect: read
explain (type distributed) select 1;
-- expect: read
explain (type validate) select 1;
-- expect: read
explain analyze select 1;

-- Changing data, and what runs a change.
-- expect: write
insert into memory.default.t select * from u;
-- expect: write
insert into t values (1);
-- expect: write
delete from t where id = 1;
-- expect: write
update t set a = 1 where id = 2;
-- expect: write
merge into t using s on t.id = s.id when matched then update set a = s.a when not matched then insert (id) values (s.id);
-- expect: write
truncate table t;
-- expect: write
explain analyze delete from t where id = 1;

-- Changing the schema.
-- expect: schema
create table memory.default.t (id bigint, name varchar);
-- expect: schema
create table t with (format = 'PARQUET', partitioned_by = array['ds']) as select * from u;
-- expect: schema
create table iceberg_orders with (table_type = 'ICEBERG', location = 's3://bucket/iceberg/', format = 'PARQUET') as select * from orders;
-- expect: schema
create external table if not exists `sales`.`orders` (`id` bigint, `props` struct<a:int,b:string>) partitioned by (`dt` string) stored as parquet location 's3://bucket/orders/';
-- expect: schema
alter table orders add if not exists partition (dt = '2026-01-01') location 's3://bucket/orders/dt=2026-01-01/';
-- expect: schema
alter table t add column c varchar;
-- expect: schema
comment on table t is 'orders';
-- expect: schema
create schema s;
-- expect: schema
drop table `sales`.`orders`;

-- Transactions.
-- expect: transaction
start transaction;
-- expect: transaction
commit;
-- expect: transaction
rollback;

-- The session, the server and files in S3.
-- expect: admin
use tpch.tiny;
-- expect: admin
set session query_max_run_time = '1h';
-- expect: admin
set role admin;
-- expect: admin
execute q using 1;
-- expect: admin
deallocate prepare q;
-- expect: admin
call system.sync_partition_metadata('db', 't', 'ADD');
-- expect: admin
analyze t;
-- expect: admin
grant select on t to user x;
-- expect: admin
msck repair table orders;
-- expect: admin
unload (select * from orders) to 's3://bucket/out/' with (format = 'PARQUET');
-- expect: admin
create database if not exists sales;
-- expect: admin
vacuum iceberg_orders;

-- What `sqlparser`'s generic dialect can't read, so nothing can say what it does.
-- expect: unparseable
values 1, 2, 3;
-- expect: unparseable
select * from t for version as of 3;
-- expect: unparseable
prepare q from select * from t where id = ?;
-- expect: unparseable
optimize iceberg_orders rewrite data using bin_pack;
-- expect: unparseable
refresh materialized view mv;
