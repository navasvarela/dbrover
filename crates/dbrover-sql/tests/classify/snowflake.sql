-- Snowflake's statements, and what dbrover_sql::classify must call them. See
-- classify_corpus.rs. `sqlparser`'s own Snowflake dialect reads them.

-- Reading.
-- expect: read
select * from sales.public.orders where order_date >= '2026-01-01'::date limit 10;
-- expect: read
select "Order Id", ORDERS.ID from "My DB"."public"."orders" as orders;
-- expect: read
select src:customer.name::string, value:price::number(10, 2) from car_sales, lateral flatten(input => src:items);
-- expect: read
select iff(x > 0, 'pos', 'neg'), nvl(a, 0), zeroifnull(b) from t // a slash comment
;
-- expect: read
select user_id from events qualify row_number() over (partition by user_id order by ts desc) = 1;
-- expect: read
select * from t at(timestamp => '2026-01-01 00:00:00'::timestamp_ltz);
-- expect: read
select * from orders sample (10 rows);
-- expect: read
select * from monthly_sales pivot (sum(amount) for month in ('JAN', 'FEB')) as p;
-- expect: read
select * from stock_price_history match_recognize (partition by company order by price_date measures first(price_date) as start_date one row per match pattern (up+) define up as price > lag(price));
-- expect: read
select $$a dollar; string$$, current_warehouse(), current_role();
-- expect: read
select $1, metadata$filename from @my_stage/path/ (file_format => 'my_csv');
-- expect: read
select * from table(result_scan(last_query_id()));
-- expect: read
with recursive r (n) as (select 1 union all select n + 1 from r where n < 10) select * from r;
-- expect: read
show databases;
-- expect: read
show terse objects in schema sales.public limit 10 from 'A';
-- expect: read
show tables like 'ORD%' in schema sales.public;
-- expect: read
show columns in table orders;
-- expect: read
show warehouses like 'ANALYTICS_WH';
-- expect: read
show grants to role analyst;
-- expect: read
describe table orders;
-- expect: read
desc table orders;
-- expect: read
describe view sales.public.v;
-- expect: read
desc stage my_stage;
-- expect: read
describe warehouse analytics_wh;
-- expect: read
list @my_stage pattern = '.*[.]csv';
-- expect: read
ls @~;
-- expect: read
explain select * from orders;
-- expect: read
explain using json select * from orders;
-- expect: read
explain using tabular select * from orders;

-- Changing data.
-- expect: write
insert into orders (id, total) values (1, 10.5), (2, 20);
-- expect: write
insert overwrite into orders select * from staging;
-- expect: write
insert all into t1 into t2 (a) values (x) select x from src;
-- expect: write
update orders set status = 'shipped' where id = 1;
-- expect: write
update t set x = u.x from u where t.id = u.id;
-- expect: write
delete from t using u where t.id = u.id;
-- expect: write
merge into target t using source s on t.id = s.id when matched then update set t.total = s.total when not matched then insert (id, total) values (s.id, s.total);
-- expect: write
truncate table if exists orders;
-- expect: write
copy into orders from @my_stage/path/ file_format = (type = csv skip_header = 1) on_error = 'continue';
-- expect: write
copy into orders (id, total) from (select $1, $2 from @my_stage) pattern = '.*[.]csv';
-- expect: write
explain delete from orders where id = 1;

-- Changing the schema.
-- expect: schema
create or replace table orders (id number(38, 0) not null primary key, total number(10, 2), v variant) cluster by (id);
-- expect: schema
create or replace transient table staging (id int, v variant);
-- expect: schema
create temporary table t (id int);
-- expect: schema
create table t clone source_t at(offset => -3600);
-- expect: schema
create or replace secure view v as select 1 as a;
-- expect: schema
create or replace dynamic table dt target_lag = '1 minute' warehouse = wh as select * from t;
-- expect: schema
create schema sales.staging;
-- expect: schema
alter table orders add column note varchar;
-- expect: schema
alter table orders cluster by (placed_at);
-- expect: schema
drop table if exists orders;

-- Transactions, which last only as long as one request to the SQL API.
-- expect: transaction
begin;
-- expect: transaction
begin transaction;
-- expect: transaction
begin work;
-- expect: transaction
start transaction;
-- expect: transaction
commit;
-- expect: transaction
rollback;

-- The session, the account, files on stages and scripting.
-- expect: admin
use warehouse analytics_wh;
-- expect: admin
use role analyst;
-- expect: admin
use database sales;
-- expect: admin
use schema sales.public;
-- expect: admin
use sales.public;
-- expect: admin
alter session set query_tag = 'DbRover';
-- expect: admin
alter session unset query_tag;
-- expect: admin
set myvar = 10;
-- expect: admin
set (a, b) = (1, 2);
-- expect: admin
create database sales;
-- expect: admin
create or replace stage my_stage url = 's3://bucket/path/' storage_integration = my_int;
-- expect: admin
create or replace file format my_csv type = csv field_delimiter = ',';
-- expect: admin
copy into @my_stage/out/ from orders file_format = (type = parquet) header = true;
-- expect: admin
copy into 's3://bucket/out/' from (select * from orders) storage_integration = my_int;
-- expect: admin
put 'file:///tmp/data/orders.csv' @%orders;
-- expect: admin
remove @my_stage/path/ pattern = '.*[.]csv';
-- expect: admin
rm @%orders;
-- expect: admin
grant select on table orders to role analyst;
-- expect: admin
revoke select on table orders from role analyst;
-- expect: admin
call p(1);
-- expect: admin
execute immediate 'select 1';
-- expect: admin
execute immediate $$
declare
  n int default 3;
begin
  return n;
end;
$$;
-- expect: admin
execute task t;
-- expect: admin
begin
  select 1;
  delete from t where true;
end;

-- What `sqlparser`'s Snowflake dialect can't read, so nothing can say what it does.
-- expect: unparseable
create or replace stream s on table orders;
-- expect: unparseable
create or replace task t warehouse = wh schedule = '5 minute' as insert into log select 1;
-- expect: unparseable
create or replace pipe p as copy into orders from @my_stage;
-- expect: unparseable
create or replace sequence seq start = 1 increment = 1;
-- expect: unparseable
create or replace function add_one(x number) returns number as 'x + 1';
-- expect: unparseable
create or replace procedure p(x number) returns varchar language sql as $$
begin
  return 'x is ' || x;
end;
$$;
-- expect: unparseable
alter warehouse wh suspend;
-- expect: unparseable
alter task t resume;
-- expect: unparseable
undrop table orders;
-- expect: unparseable
put file:///tmp/data/orders.csv @my_stage auto_compress = true;
-- expect: unparseable
get @my_stage/out/ file:///tmp/out/;
-- expect: unparseable
unset myvar;
-- expect: unparseable
begin name t1;
-- expect: unparseable
describe file format my_csv;
-- expect: unparseable
merge into t using s on t.id = s.id when matched then update all by name;
-- expect: unparseable
select {*} from t;
-- expect: unparseable
begin
  let n := 1;
  if (n > 0) then
    return 'positive';
  end if;
end;
-- expect: unparseable
declare
  total number default 0;
begin
  for i in 1 to 3 do
    total := total + i;
  end for;
  return total;
end;
