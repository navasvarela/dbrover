-- BigQuery's GoogleSQL statements, and what dbrover_sql::classify must call them. See
-- classify_corpus.rs. `sqlparser`'s own BigQuery dialect reads them.

-- Reading.
-- expect: read
select * from `my-project.sales.orders` where order_date >= date '2026-01-01' limit 10;
-- expect: read
select word, sum(word_count) as count from `bigquery-public-data`.samples.shakespeare group by word;
-- expect: read
select o.id, item.sku from sales.orders as o, unnest(o.items) as item;
-- expect: read
select * except (secret) replace (upper(name) as name) from users;
-- expect: read
select user_id, row_number() over (partition by country order by ts desc) as rn from events qualify rn = 1;
-- expect: read
select * from (select product, quarter, sales from t) pivot (sum(sales) for quarter in ('Q1', 'Q2'));
-- expect: read
select * from t for system_time as of timestamp_sub(current_timestamp(), interval 1 hour);
-- expect: read
select * from t tablesample system (10 percent);
-- expect: read
select r'\d+', b'bytes', '''triple
quoted''', "double" # a hash comment
from t;
-- expect: read
with recursive r as (select 1 as n union all select n + 1 from r where n < 10) select * from r;
-- expect: read
select * from `region-us`.INFORMATION_SCHEMA.JOBS where creation_time > timestamp_sub(current_timestamp(), interval 1 day);
-- expect: read
select * from t |> where x > 1 |> aggregate count(*) group by y;
-- expect: read
select @param, @@project_id from t;

-- Changing data.
-- expect: write
insert into sales.orders (id, total) values (1, 10.5), (2, 20);
-- expect: write
insert sales.orders (id) values (3);
-- expect: write
insert into t select * from u;
-- expect: write
update sales.orders set status = 'shipped' where id = 1;
-- expect: write
update t set x = u.x from u where t.id = u.id;
-- expect: write
delete from t where id = 1;
-- expect: write
delete t where true;
-- expect: write
merge sales.orders t using staging s on t.id = s.id when matched then update set total = s.total when not matched by target then insert (id, total) values (s.id, s.total) when not matched by source then delete;
-- expect: write
merge t using s on false when not matched then insert row;
-- expect: write
truncate table sales.orders;
-- expect: write
insert into t (a) with s as (select 1 as a) select a from s;

-- Changing the schema.
-- expect: schema
create table sales.orders (id int64 not null, total numeric(10, 2), created_at timestamp) partition by date(created_at) cluster by id options (description = 'orders');
-- expect: schema
create or replace table t as select * from u;
-- expect: schema
create temp table t as select 1 as x;
-- expect: schema
create view sales.v as select * from sales.orders;
-- expect: schema
create materialized view mv as select x, count(*) as n from t group by x;
-- expect: schema
create temp function double_it(x int64) returns int64 as (x * 2);
-- expect: schema
create temp function js_fn(x string) returns string language js as r"""return x.toUpperCase();""";
-- expect: schema
create schema sales options (location = 'EU');
-- expect: schema
alter table t add column c string;
-- expect: schema
alter table t rename to u;
-- expect: schema
drop table if exists t;
-- expect: schema
drop schema sales cascade;

-- Transactions, inside a script or a session.
-- expect: transaction
begin transaction;
-- expect: transaction
begin;
-- expect: transaction
commit transaction;
-- expect: transaction
rollback transaction;

-- Scripting, and files in Cloud Storage.
-- expect: admin
declare n int64 default 3;
-- expect: admin
set n = n + 1;
-- expect: admin
set (a, b) = (1, 2);
-- expect: admin
call sales.p(1);
-- expect: admin
execute immediate 'select 1';
-- expect: admin
execute immediate 'select ?' using 1;
-- expect: admin
assert (select count(*) from t) > 0 as 'empty';
-- expect: admin
begin select 1; end;
-- expect: admin
begin delete from t where true; exception when error then select @@error.message; end;
-- expect: admin
if n > 1 then select 1; else select 2; end if;
-- expect: admin
export data options (uri = 'gs://bucket/out/*.csv', format = 'CSV', overwrite = true) as select * from t;

-- What `sqlparser`'s BigQuery dialect can't read, so nothing can say what it does.
-- expect: unparseable
load data into sales.orders from files (format = 'CSV', uris = ['gs://bucket/in/*.csv']);
-- expect: unparseable
load data overwrite sales.orders from files (format = 'PARQUET', uris = ['gs://bucket/in/*.parquet']);
-- expect: unparseable
create or replace procedure sales.p(x int64) begin select x; end;
-- expect: unparseable
alter table t set options (description = 'x');
-- expect: unparseable
while n > 0 do set n = n - 1; end while;
-- expect: unparseable
loop leave; end loop;
-- expect: unparseable
grant `roles/bigquery.dataViewer` on table t to 'user:a@b.com';
-- expect: unparseable
select safe.substr(s, 1, 2) from t;
-- expect: unparseable
select * from ml.predict(model `p.d.m`, table t);
