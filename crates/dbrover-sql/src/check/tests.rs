use super::*;
use crate::split::split_statements;

/// Valid PostgreSQL statements: none may get a syntax error. Each construct `sqlparser`
/// rejects is suppressed by the known-gaps list, which this corpus keeps honest.
const VALID_POSTGRES: &str = r#"
select id,name from users where active=true and created_at>=now()-interval '7 days' order by 1 desc limit 10;
SELECT a::int, b->>'key', c#>>'{a,b}', d @> '{"x":1}'::jsonb, e ? 'k', f ?| array['a','b'] FROM t WHERE g<>1 AND i=-1;
select E'it\'s', B'1010', X'1F', U&'d\0061t', $$dollar; 'quoted' body$$, $tag$ nested $$ inside $tag$ from t;
select count(*) filter (where x > 1) over (partition by y order by z rows between unbounded preceding and current row) from t;
with recursive tree(id, parent) as (select id, parent from nodes where parent is null union all select n.id, n.parent from nodes n join tree on n.parent = tree.id) select * from tree;
insert into orders (customer_id, total) values (1, 10.5), (2, 20) on conflict (id) do update set total = excluded.total returning id;
update accounts a set balance = balance - 10 from transfers t where t.account_id = a.id and t.amount between 1 and 100;
delete from sessions using users where sessions.user_id = users.id and users.disabled;
SELECT DISTINCT ON (customer_id) customer_id, created_at FROM orders ORDER BY customer_id, created_at DESC;
select * from a left outer join b on a.id = b.a_id inner join c using (id) cross join lateral (select max(x) from d where d.id = a.id) m;
select * from jobs where state = 'ready' for update skip locked;
select * from jobs for no key update;
select * from jobs for key share;
select $1, $2::text from t where y = $3;
select percentile_cont(0.5) within group (order by x) from t;
select x from generate_series(1, 10) as g(x);
select * from rows from (generate_series(1,2), generate_series(3,4)) as r(a,b);
select * from t1 join t2 using (id) as j;
select xmlelement(name foo);
select * from t where a between symmetric 1 and 2;
select a is distinct from b, a isnull from t;
select now() at time zone 'utc', extract(year from now()), substring('abc' from 2 for 1);
select a from t group by rollup (a), cube (b);
merge into t using s on t.id = s.id when matched then update set a = s.a when not matched then insert (id) values (s.id);
with moved as (delete from a returning *) insert into b select * from moved;
values (1, 'a'), (2, 'b');
select x::double precision, y::timestamp with time zone from t;
insert into t default values;
update t set a = default;
select 10 % 3, 2 ^ 3, ~1, 1 << 2;
"#;

const VALID_SQLITE: &str = r#"
select [my col], `other col`, "third" from [my table] where x = ?1 and y = :name and z = @p and w = $v;
insert or replace into kv (k, v) values ('a', x'00ff'), ('b', null);
replace into kv values ('c', 1);
insert into t (a) select a from u where true on conflict(a) do nothing;
select * from items indexed by items_name where name glob 'a*' limit 5, 10;
select * from items not indexed;
select a is not b, a is b, a is not null from t;
with t(x) as (values (1), (2)) select x from t;
select iif(a > 1, 'big', 'small'), a -> '$.b', a ->> '$.c', group_concat(b, ', ') from items group by a;
update items set price = price * 1.1 where id in (select id from items order by price limit 3) returning *;
update or ignore t set a = 1;
select * from t, json_each(t.j);
select * from pragma_table_info('t');
select a from t where a match 'x';
"#;

const VALID_MYSQL: &str = r#"
select `id`, name from `shop`.`users` where active = true and created_at >= now() - interval 7 day order by 1 desc limit 10;
select "double", 'it\'s', 'C:\\temp', x'1F', b'1010', _utf8mb4'text' collate utf8mb4_bin from t # trailing comment
;
select a from t limit 5, 10;
select a from t limit 10 offset 5;
select sql_calc_found_rows * from t;
select json_extract(doc, '$.a'), doc->'$.b', doc->>'$.c' from docs;
select count(*), group_concat(distinct name order by name separator ', ') from t group by k with rollup;
select * from a left join b on a.id = b.a_id straight_join c on c.id = b.c_id;
select * from t force index (idx_a) where a = 1;
select * from t use index (idx_a) ignore index (idx_b);
select * from t where a regexp '^x' and b rlike 'y' and c sounds like 'z';
select if(a > 1, 'big', 'small'), ifnull(b, 0), case when c then 1 else 2 end from t;
select @row := @row + 1, @@session.sql_mode from t;
select * from t where match (title, body) against ('db' in boolean mode);
select * from t for update nowait;
select * from t lock in share mode;
select * from t partition (p0);
with recursive n(i) as (select 1 union all select i + 1 from n where i < 5) select * from n;
select row_number() over w from t window w as (order by a);
select a div 2, a mod 3, a xor b, a <=> b from t;
select cast(a as unsigned), convert(b, char), convert(c using utf8mb4) from t;
select date_add(now(), interval 1 month), timestampdiff(day, a, b) from t;
insert into t (a, b) values (1, 'x'), (2, 'y') on duplicate key update b = values(b);
insert into t set a = 1, b = 2;
insert ignore into t (a) values (1);
insert into t (a) select a from u;
replace into kv (k, v) values ('a', 1);
replace into kv set k = 'b', v = 2;
update t set a = a + 1 where id = 1 order by id limit 1;
update low_priority ignore t set a = 1;
update a join b on a.id = b.a_id set a.x = b.x;
delete from t where id = 1 order by id limit 1;
delete a from a join b on a.id = b.a_id where b.gone;
delete quick ignore from t where id = 2;
values row(1, 'a'), row(2, 'b');
"#;

const VALID_SQL_SERVER: &str = r#"
select top 10 [id], name from [dbo].[users] where active = 1 and created_at >= dateadd(day, -7, getdate()) order by 1 desc;
select top (5) percent with ties id from t order by id;
select N'it''s', 0x1F, 'a' + 'b', @x, @@rowcount, #tmp.id, ##g.id from #tmp, ##g;
select a from t order by a offset 10 rows fetch next 5 rows only;
select * from t with (nolock) where a = 1;
select * from a cross apply (select top 1 b from c where c.a = a.id) x outer apply string_split(a.list, ',') s;
select iif(a > 1, 'big', 'small'), isnull(b, 0), coalesce(c, d), case when e = 1 then 1 else 2 end from t;
select cast(a as nvarchar(max)), convert(varchar(10), b, 120), try_convert(int, c) from t;
select json_value(doc, '$.a'), (select a, b from t for json path) as j;
select a, count(*) over (partition by b order by c rows between unbounded preceding and current row) from t;
with n(i) as (select 1 union all select i + 1 from n where i < 5) select * from n option (maxrecursion 10);
select * from t where a like N'%x%' and b in (select b from u) and not exists (select 1 from v where v.id = t.id);
select pvt.* from (select a, b, c from t) s pivot (sum(c) for b in ([x], [y])) pvt;
insert into t (a, b) values (1, N'x'), (2, N'y');
insert into t (a) output inserted.id select a from u;
insert into #tmp (a) exec dbo.get_rows;
update t set a = a + 1 output deleted.a, inserted.a where id = 1;
update top (10) t set a = 1 where b = 2;
update t set t.a = u.a from t join u on u.id = t.id where u.b = 1;
delete from t where id = 1;
delete top (5) from t where a = 0;
delete t from t join u on u.id = t.id;
merge into t using s on t.id = s.id when matched then update set a = s.a when not matched by target then insert (id, a) values (s.id, s.a) when not matched by source then delete;
values (1);
"#;

const VALID_DUCKDB: &str = r#"
select * from read_parquet('data/*.parquet') where amount > 1 order by 1 limit 10;
select * from 'events.csv';
from orders select customer, total where total > 10;
select * exclude (note) from orders;
select * replace (total * 2 as total) from orders;
select columns('total|amount') from orders;
select [1, 2, 3] as l, {'a': 1, 'b': 2} as s, l[1], s.a from t;
select list_transform([1, 2], x -> x + 1), list_filter(l, x -> x > 1) from t;
select customer, count(*) from orders group by all order by all;
select * from orders qualify row_number() over (partition by customer order by total desc) = 1;
select * from trades t asof join quotes q on t.symbol = q.symbol and t.ts >= q.ts;
select * from a positional join b;
select 1::integer, '2024-01-01'::date, x::varchar[] from t;
select $1, ? from t;
select struct_pack(a := 1, b := 'x');
select * from orders using sample 10%;
select a // 2, a ** 2, a ^ 2 from t;
with recursive n(i) as (select 1 union all select i + 1 from n where i < 5) select * from n;
insert into orders select * from read_csv('new.csv', header = true);
insert or replace into kv values ('a', 1);
insert into t by name select 1 as b, 2 as a;
update orders set total = total * 1.1 where customer = 'ada' returning *;
delete from orders where total < 0;
select * from orders where note similar to 'a%' and customer ilike '%da%';
select date_trunc('month', ts), epoch_ms(ts) from events;
select string_agg(name, ', ' order by name) from people;
"#;

const VALID_CLICKHOUSE: &str = r#"
select toDate(ts) as day, count() from events where ts >= now() - interval 7 day group by day order by day limit 10;
select `user id`, "event" from events final prewhere event = 'click' where user_id > 0;
select * from events sample 0.1 settings max_threads = 4;
select arrayJoin([1, 2, 3]) as x, [1, 2][1], tuple(1, 'a').2, map('k', 1)['k'];
select x, y from t array join arr as x left array join arr2 as y;
select uniq(user_id), quantile(0.9)(duration), countIf(ok), sumIf(amount, ok) from events;
select * from events order by ts with fill step 1;
select * from events limit 2 by user_id;
select multiIf(a > 1, 'big', a = 1, 'one', 'small') from t;
select 'it\'s', 'a''b', $$dollar$$ from t;
with 5 as n select n * 2;
select toStartOfHour(ts), argMax(value, ts) from metrics group by 1;
insert into events (ts, event) values (now(), 'click');
select * from file('data.csv', 'CSVWithNames');
select * from events where has(tags, 'x') and match(url, '^/a');
select * from t where a global in (select a from u);
select cast(1 as UInt8), 1::UInt8, toTypeName(1);
select * from t any left join u on t.id = u.id;
select a from t final;
select count() from system.tables format JSON;
"#;

/// Trino's statements, each accepted by Trino 484's parser, then Athena's, whose DDL is Hive's
/// and which Trino's parser refuses.
const VALID_TRINO: &str = r#"
select orderkey, totalprice from tpch.tiny.orders where orderdate >= date '1995-01-01' order by 2 desc limit 10;
select "order key", 'it''s', X'1F', 1e3, -0.5 from t;
select count(*) filter (where x > 1), approx_distinct(y), approx_percentile(z, 0.5) from t;
select * from t cross join unnest(arr) as u(x);
select * from t cross join unnest(arr) with ordinality as u(x, n);
select transform(arr, x -> x + 1), filter(arr, x -> x > 0), reduce(arr, 0, (s, x) -> s + x, s -> s) from t;
select cast(a as varchar), try_cast(b as integer), typeof(c) from t;
select date_trunc('month', ts), ts at time zone 'UTC', current_timestamp - interval '7' day from t;
with recent as (select * from orders where ts > now() - interval '1' hour) select * from recent;
select a, row_number() over (partition by b order by c) from t;
select * from t order by a offset 10 limit 5;
select * from t offset 10 rows fetch next 5 rows only;
select * from t tablesample bernoulli (10);
insert into memory.default.t select * from u;
create table memory.default.t (id bigint, name varchar);
create table t with (format = 'PARQUET', partitioned_by = array['ds']) as select * from u;
show catalogs;
show schemas from tpch;
show tables from tpch.tiny like 'o%';
show columns from t;
describe t;
show create table t;
show create view v;
show functions like 'array%';
use tpch.tiny;
set session query_max_run_time = '1h';
reset session query_max_run_time;
explain select 1;
explain (type distributed) select 1;
explain analyze select 1;
explain (type validate) select 1;
prepare q from select * from t where id = ?;
execute q using 1;
deallocate prepare q;
delete from t where id = 1;
update t set a = 1 where id = 2;
merge into t using s on t.id = s.id when matched then update set a = s.a when not matched then insert (id) values (s.id);
select map(array['a'], array[1])['a'], row(1, 'a'), array[1, 2][1], element_at(m, 'k') from t;
select json_extract_scalar(j, '$.a'), json_format(json_parse(j)) from t;
select a, count(*) from t group by grouping sets ((a), ());
select * from (values (1, 'a'), (2, 'b')) as v(n, s);
select if(a > 1, 'big', 'small'), case when a > 0 then 1 end, coalesce(a, 0) from t;
select a from t where a in (select b from u) and exists (select 1 from v) and b like 'x!%' escape '!';
call system.sync_partition_metadata('db', 't', 'ADD');
analyze t;
comment on table t is 'orders';
alter table t add column c varchar;
drop table if exists t;
create or replace view v security invoker as select 1 as a;
start transaction;
commit;
rollback;
select listagg(name, ', ') within group (order by name) from t;
select date '2026-01-01', timestamp '2026-01-01 00:00:00', time '01:02:03', interval '1' day, decimal '1.5';
values 1, 2, 3;
select * from t for version as of 3;
select * from t for timestamp as of timestamp '2026-01-01 00:00:00 UTC';
select a from t where a is distinct from b;
select regexp_extract(url, 'https?://([^/]+)', 1) from logs;
select a.* from t a left join u b on a.id = b.id full outer join w c on c.id = a.id;
create external table if not exists `sales`.`orders` (`id` bigint, `amount` decimal(10,2), `tags` array<string>, `props` struct<a:int,b:string>) partitioned by (`dt` string) row format serde 'org.apache.hadoop.hive.ql.io.parquet.serde.ParquetHiveSerDe' stored as parquet location 's3://bucket/orders/' tblproperties ('parquet.compression'='SNAPPY');
msck repair table orders;
alter table orders add if not exists partition (dt = '2026-01-01') location 's3://bucket/orders/dt=2026-01-01/';
show partitions orders;
create table iceberg_orders with (table_type = 'ICEBERG', location = 's3://bucket/iceberg/', format = 'PARQUET') as select * from orders;
unload (select * from orders) to 's3://bucket/out/' with (format = 'PARQUET');
create database if not exists sales;
drop table `sales`.`orders`;
optimize iceberg_orders rewrite data using bin_pack;
vacuum iceberg_orders;
"#;

/// GoogleSQL statements, as BigQuery's documentation writes each construct: queries and DML,
/// which are checked, then the DDL and scripting a BigQuery console holds, which aren't.
const VALID_BIGQUERY: &str = r##"
select * from `my-project.sales.orders` where order_date >= date '2026-01-01' limit 10;
select word, sum(word_count) as count from `bigquery-public-data`.samples.shakespeare group by word order by count desc limit 10;
select o.id from sales.orders as o where o.status in ('shipped', "delivered");
select struct(1 as a, 'x' as b), [1, 2, 3], array<int64>[1, 2], struct<a int64, b string>(1, 'x') from t;
select x, i from unnest([1, 2, 3]) as x with offset as i;
select o.id, item.sku from sales.orders as o, unnest(o.items) as item;
select arr[offset(0)], arr[safe_offset(1)], arr[ordinal(1)], s.a.b from t;
select * except (secret) replace (upper(name) as name) from users;
select user_id, row_number() over (partition by country order by ts desc) as rn from events qualify rn = 1;
select * from (select product, quarter, sales from t) pivot (sum(sales) for quarter in ('Q1', 'Q2'));
select * from (select product, q1, q2 from t) unpivot (sales for quarter in (q1, q2));
select safe_cast(x as int64), safe.parse_date('%Y-%m-%d', s), safe.substr(s, 1, 2), safe.cast(y as numeric) from t;
select format_date('%Y-%m', current_date()), parse_timestamp('%Y-%m-%d %H:%M:%S', s, 'UTC') from t;
select date_add(current_date(), interval 7 day), timestamp_trunc(ts, week(monday)), extract(dayofweek from d) from t;
select approx_count_distinct(user_id), array_agg(distinct x ignore nulls order by x limit 10), string_agg(name, ', ') from t;
select generate_date_array('2026-01-01', '2026-01-31', interval 1 day);
select json_value(payload, '$.user.id'), json_query(payload, '$.items'), payload.user.id, payload['items'][0] from t;
select st_distance(st_geogpoint(lng1, lat1), st_geogpoint(lng2, lat2)) from places;
select * from t for system_time as of timestamp_sub(current_timestamp(), interval 1 hour);
select * from t tablesample system (10 percent);
select r'\d+', b'bytes', rb'\x00', '''triple
'quoted' string''', """double "triple" quoted""", 'it\'s', "a \"b\"" from t;
select 1 # a hash comment
-- a dash comment
/* a block comment */ from t;
select timestamp '2026-01-01 00:00:00 UTC', datetime '2026-01-01 00:00:00', numeric '1.5', json '{"a": 1}', interval '1-2 3 4:5:6' year to second;
select @param, @@project_id, 0xFF from t where id = @id;
with recursive r as (select 1 as n union all select n + 1 from r where n < 10) select * from r;
select x, countif(y > 0), logical_and(z) from t group by all;
select sum(x) over w as s from t qualify s > 1 window w as (order by y);
select * from `region-us`.INFORMATION_SCHEMA.JOBS where creation_time > timestamp_sub(current_timestamp(), interval 1 day);
select (select as struct 1 as a, 2 as b), array(select as struct a, b from u) from t;
select x from t where x like any ('a%', 'b%');
select * from t1 union all by name select * from t2;
select * from t1 full union all by name select * from t2;
select * from t1 union all by name on (a) select * from t2;
select * from t1 union distinct strict corresponding select * from t2;
select * from t |> where x > 1 |> aggregate count(*) group by y;
select * from ml.predict(model `p.d.m`, table t);
select * from appends(table t, null, null);
select 1 << 2, 8 >> 1, array<array<int64>>[];
select * from `p.d.t*` where _table_suffix between '20260101' and '20260131';
select * from t match_recognize (order by x measures count(*) as n pattern (a+) define a as true);
insert into sales.orders (id, total) values (1, 10.5), (2, 20);
insert sales.orders (id) values (3);
insert into t (a, s) values (1, struct(1, 'x'));
update sales.orders set status = 'shipped' where id = 1;
update t set s.a = 1, x = u.x from u where t.id = u.id;
delete from t where id = 1;
delete t where true;
merge sales.orders t using staging s on t.id = s.id when matched then update set total = s.total when not matched by target then insert (id, total) values (s.id, s.total) when not matched by source then delete;
truncate table sales.orders;
create table sales.orders (id int64 not null, total numeric(10, 2), created_at timestamp, items array<struct<sku string, qty int64>>) partition by date(created_at) cluster by id options (description = 'orders', partition_expiration_days = 30);
create temp function double_it(x int64) returns int64 as (x * 2);
create temp function js_fn(x string) returns string language js as r"""
  return x.toUpperCase();
""";
create or replace procedure sales.p(x int64) begin select x; select x + 1; end;
alter table t set options (description = 'x');
declare n int64 default 3;
set n = n + 1;
call sales.p(1);
execute immediate 'select ?' using 1;
export data options (uri = 'gs://bucket/out/*.csv', format = 'CSV', overwrite = true) as select * from t;
load data into sales.orders from files (format = 'CSV', uris = ['gs://bucket/in/*.csv']);
assert (select count(*) from t) > 0 as 'empty';
begin select 1; exception when error then select @@error.message; end;
if n > 1 then select 1; elseif n < 0 then select 2; else select 3; end if;
while n > 0 do set n = n - 1; end while;
begin transaction;
commit transaction;
"##;

fn syntax_errors(corpus: &str, dialect: Dialect) -> Vec<(String, Problem)> {
    split_statements(corpus, dialect)
        .into_iter()
        .flat_map(|range| {
            let statement = &corpus[range];
            check_statement(statement, dialect)
                .into_iter()
                .filter(|problem| problem.source == ProblemSource::Syntax)
                .map(|problem| (statement.to_owned(), problem))
        })
        .collect()
}

#[test]
fn valid_statements_get_no_syntax_error() {
    assert_eq!(syntax_errors(VALID_POSTGRES, Dialect::Postgres), []);
    assert_eq!(syntax_errors(VALID_SQLITE, Dialect::Sqlite), []);
    assert_eq!(syntax_errors(VALID_MYSQL, Dialect::Mysql), []);
    assert_eq!(syntax_errors(VALID_SQL_SERVER, Dialect::SqlServer), []);
    assert_eq!(syntax_errors(VALID_DUCKDB, Dialect::DuckDb), []);
    assert_eq!(syntax_errors(VALID_CLICKHOUSE, Dialect::ClickHouse), []);
    assert_eq!(syntax_errors(VALID_TRINO, Dialect::Trino), []);
    assert_eq!(syntax_errors(VALID_BIGQUERY, Dialect::BigQuery), []);
}

#[test]
fn marks_the_token_a_syntax_error_points_at() {
    let cases = [
        (
            "select 'a' from t where x = = 1",
            "=",
            "Syntax error: expected an expression, found =",
        ),
        (
            "select a from t where",
            "where",
            "Syntax error: expected an expression at the end of the statement",
        ),
        (
            "select 1,\n  é = = 2",
            "=",
            "Syntax error: expected an expression, found =",
        ),
    ];
    for (sql, marked, message) in cases {
        let problems = check_statement(sql, Dialect::Postgres);
        let error = problems
            .iter()
            .find(|problem| problem.severity == Severity::Error)
            .unwrap_or_else(|| panic!("no error in {sql:?}"));
        assert_eq!(&sql[error.range.clone()], marked, "{sql}");
        assert_eq!(error.message, message, "{sql}");
    }
    // Only data queries and changes are parsed: other statements are where the parser falls
    // short of the database.
    assert!(check_statement("selec 1", Dialect::Postgres).is_empty());
    assert!(check_statement("create tabel t ()", Dialect::Postgres).is_empty());
    assert!(check_statement("do $$ begin perform 1; end $$", Dialect::Postgres).is_empty());
}

fn lints(sql: &str, dialect: Dialect) -> Vec<(LintRule, &str)> {
    check_statement(sql, dialect)
        .into_iter()
        .filter_map(|problem| match problem.source {
            ProblemSource::Lint(rule) => Some((rule, &sql[problem.range])),
            _ => None,
        })
        .collect()
}

/// A statement, its dialect, and the rules it should break with the text each marks.
type LintCase = (&'static str, Dialect, &'static [(LintRule, &'static str)]);

#[test]
fn lint_rules_flag_what_they_describe_and_nothing_else() {
    use LintRule::*;
    let cases: [LintCase; 15] = [
        (
            "select * from t where a = null",
            Dialect::Postgres,
            &[(NullComparison, "= null")],
        ),
        (
            "select * from t where NULL <> a",
            Dialect::Postgres,
            &[(NullComparison, "NULL <>")],
        ),
        (
            "select * from t where a != null",
            Dialect::Sqlite,
            &[(NullComparison, "!= null")],
        ),
        (
            "select * from t where a is null and b is not null",
            Dialect::Postgres,
            &[],
        ),
        (
            "update t set a = null, b = (select null) where c = 1",
            Dialect::Postgres,
            &[],
        ),
        (
            "update t set a = null where b = null",
            Dialect::Postgres,
            &[(NullComparison, "= null")],
        ),
        (
            "update t set a = 1",
            Dialect::Postgres,
            &[(UpdateWithoutWhere, "update")],
        ),
        (
            "DELETE FROM t",
            Dialect::Sqlite,
            &[(DeleteWithoutWhere, "DELETE")],
        ),
        (
            "insert into t select distinct * from u",
            Dialect::Postgres,
            &[(SelectStarInInsert, "*")],
        ),
        (
            "insert into t (a) select a from (select * from u) s",
            Dialect::Postgres,
            &[],
        ),
        (
            "update top (10) t set a = null where b <> NULL",
            Dialect::SqlServer,
            &[(NullComparison, "<> NULL")],
        ),
        (
            "DELETE FROM [order]",
            Dialect::SqlServer,
            &[(DeleteWithoutWhere, "DELETE")],
        ),
        (
            "insert into #t select * from u",
            Dialect::SqlServer,
            &[(SelectStarInInsert, "*")],
        ),
        (
            "update orders set total = 0 where note = null",
            Dialect::DuckDb,
            &[(NullComparison, "= null")],
        ),
        (
            "delete from orders",
            Dialect::DuckDb,
            &[(DeleteWithoutWhere, "delete")],
        ),
    ];
    for (sql, dialect, expected) in cases {
        assert_eq!(lints(sql, dialect), expected, "{sql}");
    }
}

#[test]
fn rule_ids_read_back() {
    for rule in LintRule::ALL {
        assert_eq!(LintRule::from_id(rule.id()), Some(rule));
    }
    assert_eq!(LintRule::from_id("no-such-rule"), None);
}

#[test]
fn still_reports_syntax_errors_around_the_dialects_gaps() {
    let cases = [
        "select a from t where",
        "select a, from t",
        "update t set where id = 1",
    ];
    let everywhere = cases.into_iter().flat_map(|sql| {
        [
            (sql, Dialect::Mysql),
            (sql, Dialect::SqlServer),
            (sql, Dialect::DuckDb),
            (sql, Dialect::ClickHouse),
            (sql, Dialect::Trino),
            (sql, Dialect::BigQuery),
        ]
    });
    // DuckDB accepts a trailing comma in a select list, `select a, from t`. `**` is its power
    // operator only when written together.
    let duckdb = [
        ("select a * * 2 from t", Dialect::DuckDb),
        ("select a,, b from t", Dialect::DuckDb),
        // Trino's gaps leave a `FOR UPDATE` it doesn't have, and a `VALUES` row's own
        // mistakes, checked.
        ("select * from t for for update", Dialect::Trino),
        ("insert into t values (1,, 2)", Dialect::Trino),
        // BigQuery's gaps leave a name called `safe`, a column called `table` and a join
        // checked.
        ("select safe.x from t where", Dialect::BigQuery),
        ("select f(table) from t where", Dialect::BigQuery),
        ("select a from t left join u on", Dialect::BigQuery),
    ];
    for (sql, dialect) in everywhere
        .filter(|&(sql, dialect)| {
            // DuckDB, ClickHouse and BigQuery accept a trailing comma in a select list. Trino
            // doesn't, but `sqlparser`'s generic dialect does, so Trino's own error says so on
            // running.
            !(matches!(
                dialect,
                Dialect::DuckDb | Dialect::ClickHouse | Dialect::Trino | Dialect::BigQuery
            ) && sql == "select a, from t")
        })
        .chain(duckdb)
    {
        let problems = check_statement(sql, dialect);
        assert!(
            problems
                .iter()
                .any(|problem| problem.source == ProblemSource::Syntax),
            "{sql}: {problems:?}"
        );
    }
}
