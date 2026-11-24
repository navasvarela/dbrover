-- Databricks's statements, and what dbrover_sql::classify must call them. See
-- classify_corpus.rs. `sqlparser`'s own Databricks dialect reads them.

-- Reading.
-- expect: read
select * from main.sales.orders where placed >= date'2026-01-01' limit 10;
-- expect: read
select `Order Id`, o.id from `my catalog`.sales.orders as o;
-- expect: read
select * from orders version as of 3;
-- expect: read
select * from orders timestamp as of '2026-01-01';
-- expect: read
select o.id, explode(o.items) from orders o;
-- expect: read
select * from orders o lateral view explode(o.items) t as i;
-- expect: read
select payload:customer.name, items[0].sku from orders;
-- expect: read
select * from orders qualify row_number() over (partition by customer order by placed desc) = 1;
-- expect: read
select * except (secret) from users;
-- expect: read
select * from t1 left anti join t2 on t1.id = t2.id;
-- expect: read
select * from read_files('/Volumes/main/raw/files/', format => 'csv');
-- expect: read
select * from range(10);
-- expect: read
with recursive r (n) as (select 1 union all select n + 1 from r where n < 10) select * from r;
-- expect: read
values (1, 'a'), (2, 'b');
-- expect: read
show catalogs;
-- expect: read
show schemas in main;
-- expect: read
show tables in main.sales;
-- expect: read
show views in main.sales;
-- expect: read
show columns in orders;
-- expect: read
show create table orders;
-- expect: read
show functions;
-- expect: read
describe table orders;
-- expect: read
desc orders;
-- expect: read
describe history orders;
-- expect: read
describe detail orders;
-- expect: read
explain select * from orders;
-- expect: read
explain cost select * from orders;
-- expect: read
explain extended select * from orders;
-- expect: write
explain cost delete from orders;
-- expect: read
describe schema main.sales;
-- expect: read
describe extended orders;

-- Writing rows.
-- expect: write
insert into orders (id, total) values (1, 10.5);
-- expect: write
insert overwrite orders select * from staging;
-- `sqlparser` reads neither `BY NAME` nor `REPLACE WHERE` after `INSERT INTO t`, nor Delta's
-- `COPY INTO`, `VACUUM` and `RESTORE`: read-only mode refuses them as statements it can't
-- classify, which is what it does to a write.
-- expect: unparseable
insert into orders by name select total, id from staging;
-- expect: unparseable
insert into orders replace where placed >= '2026-01-01' select * from staging;
-- expect: write
update orders set status = 'shipped' where id = 1;
-- expect: write
delete from orders where id = 1;
-- expect: write
merge into target t using source s on t.id = s.id when matched then update set * when not matched then insert *;
-- expect: write
truncate table orders;
-- expect: unparseable
copy into orders from '/Volumes/main/raw/files/' fileformat = csv;
-- expect: unparseable
vacuum orders retain 168 hours;
-- expect: unparseable
restore table orders to version as of 3;
-- `OPTIMIZE` rewrites a table's files without changing its rows: maintenance, as ClickHouse's.
-- expect: admin
optimize orders zorder by (customer);

-- Changing the schema.
-- `USING delta` is a gap of `sqlparser`'s, so the table is refused as unknown in read-only mode.
-- expect: unparseable
create or replace table orders (id bigint, total decimal(10, 2)) using delta;
-- expect: schema
create table orders (id bigint, total decimal(10, 2));
-- expect: schema
create table t as select * from u;
-- expect: schema
create or replace view v as select * from orders;
-- expect: schema
create materialized view mv as select customer, sum(total) from orders group by customer;
-- expect: schema
create schema if not exists main.sales;
-- expect: schema
alter table orders add column note string;
-- expect: schema
drop table if exists orders;
-- expect: schema
drop view v;

-- Sessions, grants and administration.
-- expect: admin
use catalog main;
-- expect: admin
use schema sales;
-- expect: admin
use main.sales;
-- expect: admin
set ansi_mode = true;
-- expect: admin
grant select on table orders to `analysts`;
-- expect: admin
revoke select on table orders from `analysts`;
-- expect: transaction
begin transaction;
-- expect: transaction
commit;
-- expect: transaction
rollback;
