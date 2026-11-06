use super::*;
use crate::split::split_statements;

/// PostgreSQL statements, written the way people type them: odd spacing, comments in awkward
/// places, and every kind of literal the lexer knows.
const POSTGRES_CORPUS: &str = r#"
select id,name,email from users where active=true and created_at>=now()-interval '7 days' order by created_at desc limit 10;
SELECT a::int, b->>'key', c#>>'{a,b}', d @> '{"x":1}'::jsonb, e ? 'k', f ?| array['a','b'] FROM t WHERE g<>1 AND h!=2 AND i=-1;
select E'it\'s', B'1010', X'1F', U&'d\0061t\+000061', $$dollar; 'quoted' body$$, $tag$ nested $$ inside $tag$ from t;
select count(*) filter (where x > 1) over (partition by y order by z rows between unbounded preceding and current row) as running from t;
with recursive tree(id, parent) as (select id, parent from nodes where parent is null union all select n.id, n.parent from nodes n join tree on n.parent = tree.id) select * from tree;
insert into orders (customer_id, total) values (1, 10.5), (2, 20) on conflict (id) do update set total = excluded.total returning id;
update accounts a set balance = balance - 10, updated_at = now() from transfers t where t.account_id = a.id and t.amount between 1 and 100;
delete from sessions using users where sessions.user_id = users.id and users.disabled;
SELECT DISTINCT ON (customer_id) customer_id, created_at FROM orders ORDER BY customer_id, created_at DESC;
select case when a > 1 then 'big' when a = 1 then 'one' else 'small' end as size, case when b then 1 end from t;
select * from a left outer join b on a.id = b.a_id inner join c using (id) cross join lateral (select max(x) from d where d.id = a.id) m natural join e;
select id from t where id in (select id from u where u.flag) and exists(select 1 from v where v.id = t.id) or not x;
select a from t1 union all select a from t2 except select a from t3 order by 1 offset 5 fetch first 10 rows only;
select * from jobs where state = 'ready' for update skip locked;
select $1, $2::text, x from t where y = $3;
select a -- trailing comment
, b /* block
comment */ , c from t -- after from
where /* inline */ d = 1;
SELECT 1 - -1, 2 - - 2, 'a' || 'b', x ~* '^a', y !~~ 'b%', array[1,2][1:2], (a).b, t.*, -x;
create table if not exists public.orders (id bigint generated always as identity primary key, customer_id int not null references customers(id) on delete cascade, total numeric(10, 2) check (total >= 0), note text default 'none', unique (customer_id, note));
create view big_orders as select * from orders where total > 100;
create function add(a int, b int) returns int language sql immutable as $$ select a + b; -- body
$$;
do $$ begin raise notice 'hello  %', 1; perform pg_sleep(0); end $$;
copy orders (id, total) from stdin with (format csv, header true);
alter table orders add column shipped_at timestamptz, alter column total set not null;
grant select, insert on orders to app_user;
explain analyze select * from orders where id = 1;
create index concurrently orders_customer_idx on orders using btree (customer_id) where total > 0;
select 'a''b', "Quoted ""Name""", "select" from "Table";
values (1, 'a'), (2, 'b');
select * from t where a like 'x\%' escape '\' and b similar to '%(b|c)%';
begin; commit;
select json_build_object('a', 1, 'b', array_agg(x order by x)) from t group by y having count(*) > 1 and sum(z) < 10;
"#;

/// SQLite statements.
const SQLITE_CORPUS: &str = r#"
select [my col], `other col`, "third" from [my table] where x = ?1 and y = :name and z = @p and w = $v;
insert or replace into kv (k, v) values ('a', x'00ff'), ('b', null);
replace into kv values ('c', 1);
insert into t (a) select a from u where true on conflict(a) do nothing;
create table if not exists items (id integer primary key autoincrement, name text not null collate nocase, price real check (price > 0), created text default (datetime('now')));
create trigger items_touch after update on items for each row begin update items set created = datetime('now') where id = new.id; insert into log values (new.id); end;
pragma foreign_keys = on;
attach database 'other.db' as other;
select * from items indexed by items_name where name glob 'a*' limit 5, 10;
with t(x) as (values (1), (2)) select x from t;
select iif(a > 1, 'big', 'small'), a -> '$.b', a ->> '$.c', group_concat(b, ', ') from items group by a;
update items set price = price * 1.1 where id in (select id from items order by price limit 3) returning *;
delete from items where rowid = 1;
select a from t order by a nulls last;
vacuum;
explain query plan select * from items where id = 1;
create view v as select id, name from items;
select 1 /* c */ -- d
;
"#;

/// MySQL statements, with a `DELIMITER` script.
const MYSQL_CORPUS: &str = r#"
select `id`, "double", 'it\'s', 'C:\\temp' from `shop`.`users` where a = 1 # note
order by 1 limit 5, 10;
insert into t (a, b) values (1, 'x') on duplicate key update b = values(b);
update t set a = a + 1 where id = 1 order by id limit 1;
create table if not exists items (id int unsigned not null auto_increment primary key, name varchar(100) not null, price decimal(10, 2)) engine = InnoDB default charset = utf8mb4;
select 1--1, @@session.sql_mode, @row := @row + 1;
DELIMITER //
CREATE PROCEDURE touch(n INT) BEGIN IF n > 0 THEN UPDATE items SET price = price + 1 WHERE id = n; END IF; END//
DELIMITER ;
show tables;
"#;

/// SQL Server statements, with `GO` batches, a procedure whose body runs to the end of its
/// batch, and Transact-SQL's control flow.
const SQL_SERVER_CORPUS: &str = r#"
select top (10) [id], [order]]s], N'it''s', @x, @@rowcount from [dbo].[users] u with (nolock) where u.active = 1 -- note
order by 1 desc offset 0 rows fetch next 10 rows only;
declare @n int = 5, @name nvarchar(50) = N'x';
set @n = @n + 1
select * from #tmp t cross apply string_split(t.list, ',') s;
insert into t (a, b) output inserted.id values (1, 0x1F);
update t set a = isnull(a, 0) + 1 from t join u on u.id = t.id where u.b = 1;
create table dbo.items (id int identity(1, 1) not null primary key, name nvarchar(100) not null, price decimal(10, 2) check (price > 0), created datetime2 default sysdatetime());
GO
CREATE OR ALTER PROCEDURE dbo.touch @n INT AS
SET NOCOUNT ON;
IF @n > 0 BEGIN UPDATE dbo.items SET price = price + 1 WHERE id = @n; END
ELSE BEGIN SELECT 1 / 0; END
GO
begin try begin tran; delete from t where id = @n; commit; end try begin catch rollback; throw; end catch;
exec dbo.touch @n = 1;
merge into t using s on t.id = s.id when matched then update set a = s.a when not matched then insert (id) values (s.id);
/* a /* nested */ comment */ select 1;
"#;

/// DuckDB statements: its friendlier SQL, file-reading table functions and statements of its
/// own.
const DUCKDB_CORPUS: &str = r#"
select * from read_parquet('data/*.parquet') where amount > 1 order by 1 limit 10;
from orders select customer, total where total > 10;
select * exclude (note), [1, 2] as l, {'a': 1} as s, list_transform(l, x -> x + 1) from orders;
select customer, count(*) from orders group by all order by all;
select * from orders qualify row_number() over (partition by customer order by total desc) = 1;
select * from trades t asof join quotes q on t.symbol = q.symbol and t.ts >= q.ts;
create table orders as select * from read_csv('orders.csv', header = true);
copy orders to 'orders.parquet' (format parquet);
create macro add(a, b) as a + b;
attach 'other.duckdb' as other (read_only);
pivot sales on year using sum(amount);
describe orders;
summarize orders;
insert or replace into kv values ('a', 1);
select $$dollar; 'quoted'$$, E'it\'s', 1::integer, x::varchar[] from t;
"#;

/// ClickHouse statements: its clauses (`FINAL`, `PREWHERE`, `ARRAY JOIN`, `LIMIT BY`,
/// `SETTINGS`), table engines, `#` comments and backslash escapes.
const CLICKHOUSE_CORPUS: &str = r#"
select toDate(ts) as day, count() from events where ts >= now() - interval 7 day group by day order by day limit 10;
select `user id`, "event" from events final prewhere event = 'click' where user_id > 0 # note
;
select x, y from t array join arr as x left array join arr2 as y;
select uniq(user_id), quantile(0.9)(duration), countIf(ok) from events limit 2 by user_id settings max_threads = 4;
select 'it\'s', 'a''b', $$dollar$$, [1, 2][1], tuple(1, 'a').2 from t;
create table events (ts DateTime, event LowCardinality(String), user_id UInt64) engine = MergeTree order by (event, ts) partition by toYYYYMM(ts);
insert into events (ts, event) values (now(), 'click');
optimize table events final;
system flush logs;
"#;

/// Trino's queries and Athena's DDL in one script, as an Athena console holds them: lambdas,
/// `UNNEST`, typed literals, double-quoted names, then Hive's backquoted names and
/// `struct<…>` types.
const TRINO_CORPUS: &str = r#"
select "order key", transform(arr, x -> x + 1), date '2026-01-01', interval '7' day from t cross join unnest(arr) with ordinality as u(x, n) where ts > now() - interval '1' hour;
with recent as (select * from orders where ts > now() - interval '1' hour) select approx_percentile(total, 0.5) from recent group by grouping sets ((a), ());
explain (type distributed) select * from t tablesample bernoulli (10) for version as of 3;
values 1, 2, 3;
create external table if not exists `sales`.`orders` (`id` bigint, `props` struct<a:int,b:string>) partitioned by (`dt` string) stored as parquet location 's3://bucket/orders/';
msck repair table `sales`.`orders`;
unload (select * from orders) to 's3://bucket/out/' with (format = 'PARQUET');
"#;

fn format_all(text: &str, dialect: Dialect, options: &FormatOptions) -> String {
    let ranges = split_statements(text, dialect);
    format_ranges(text, &ranges, dialect, options)
        .unwrap_or_else(|error| panic!("{error}:\n{text}"))
        .into_text()
}

fn pretty(sql: &str) -> String {
    format_all(sql, Dialect::Postgres, &FormatOptions::default())
}

#[test]
fn formatting_the_corpus_never_changes_a_token_and_is_stable() {
    for (corpus, dialect) in [
        (POSTGRES_CORPUS, Dialect::Postgres),
        (SQLITE_CORPUS, Dialect::Sqlite),
        (MYSQL_CORPUS, Dialect::Mysql),
        (SQL_SERVER_CORPUS, Dialect::SqlServer),
        (DUCKDB_CORPUS, Dialect::DuckDb),
        (CLICKHOUSE_CORPUS, Dialect::ClickHouse),
        (TRINO_CORPUS, Dialect::Trino),
    ] {
        for case in [
            KeywordCase::Upper,
            KeywordCase::Lower,
            KeywordCase::Preserve,
        ] {
            for indent_width in [2, 4] {
                let options = FormatOptions {
                    keyword_case: case,
                    indent_width,
                };
                let once = format_all(corpus, dialect, &options);
                assert!(same_meaning(corpus, &once, dialect), "{once}");
                let twice = format_all(&once, dialect, &options);
                assert_eq!(once, twice, "formatting again changed it");
            }
        }
    }
}

/// Formatting may move tokens between lines but never across a statement boundary: the
/// formatted text splits into as many statements, each the same tokens as before.
#[test]
fn formatting_keeps_every_statement_boundary() {
    for (corpus, dialect) in [
        (POSTGRES_CORPUS, Dialect::Postgres),
        (SQLITE_CORPUS, Dialect::Sqlite),
        (MYSQL_CORPUS, Dialect::Mysql),
        (SQL_SERVER_CORPUS, Dialect::SqlServer),
        (DUCKDB_CORPUS, Dialect::DuckDb),
        (CLICKHOUSE_CORPUS, Dialect::ClickHouse),
        (TRINO_CORPUS, Dialect::Trino),
    ] {
        let formatted = format_all(corpus, dialect, &FormatOptions::default());
        let before = split_statements(corpus, dialect);
        let after = split_statements(&formatted, dialect);
        assert_eq!(before.len(), after.len(), "{formatted}");
        for (old, new) in before.into_iter().zip(after) {
            assert!(
                same_meaning(&corpus[old.clone()], &formatted[new.clone()], dialect),
                "{} became {}",
                &corpus[old],
                &formatted[new]
            );
        }
    }
}

#[test]
fn keeps_sql_server_row_limits_on_the_select_line() {
    let cases = [
        (
            "select top (5) percent with ties id, name from t order by id",
            "SELECT TOP (5) PERCENT WITH TIES\n    id,\n    name\nFROM t\nORDER BY id",
        ),
        ("select top 1 * from t", "SELECT TOP 1 *\nFROM t"),
        (
            "select distinct top 3 a, b from t",
            "SELECT DISTINCT TOP 3\n    a,\n    b\nFROM t",
        ),
    ];
    for (sql, expected) in cases {
        let formatted = format_all(sql, Dialect::SqlServer, &FormatOptions::default());
        assert_eq!(formatted, expected, "{sql}");
    }
}

/// ClickHouse's names are case-sensitive, functions, types and columns alike, and columns are
/// often named like keywords: its words keep their case, and only the layout changes.
#[test]
fn keeps_the_case_of_every_clickhouse_word() {
    let formatted = format_all(
        "select date, array(1) from t array join arr as x where user = 'ada'",
        Dialect::ClickHouse,
        &FormatOptions::default(),
    );
    assert_eq!(
        formatted,
        "select\n    date,\n    array(1)\nfrom t\narray join arr as x\nwhere user = 'ada'"
    );
}

#[test]
fn every_statement_formats_on_its_own_too() {
    for (corpus, dialect) in [
        (POSTGRES_CORPUS, Dialect::Postgres),
        (SQLITE_CORPUS, Dialect::Sqlite),
        (MYSQL_CORPUS, Dialect::Mysql),
        (SQL_SERVER_CORPUS, Dialect::SqlServer),
        (DUCKDB_CORPUS, Dialect::DuckDb),
        (CLICKHOUSE_CORPUS, Dialect::ClickHouse),
        (TRINO_CORPUS, Dialect::Trino),
    ] {
        for range in split_statements(corpus, dialect) {
            let statement = &corpus[range];
            let formatted = format_all(statement, dialect, &FormatOptions::default());
            assert!(same_meaning(statement, &formatted, dialect), "{formatted}");
        }
    }
}

#[test]
fn lays_out_queries_clause_by_clause() {
    let cases = [
        (
            "select id,name from users u join orders o on o.user_id=u.id where u.active and o.total between 1 and 10 or u.vip order by id desc limit 5",
            "SELECT\n    id,\n    name\nFROM users u\nJOIN orders o ON o.user_id = u.id\nWHERE u.active\n    AND o.total BETWEEN 1 AND 10\n    OR u.vip\nORDER BY id DESC\nLIMIT 5",
        ),
        ("select * from t", "SELECT *\nFROM t"),
        (
            "select a from t where id in (select id from u where x)",
            "SELECT a\nFROM t\nWHERE id IN (\n    SELECT id\n    FROM u\n    WHERE x\n)",
        ),
        (
            "with a as (select 1) select * from a",
            "WITH a AS (\n    SELECT 1\n)\nSELECT *\nFROM a",
        ),
        (
            "insert into t (a, b) values (1, 2), (3, 4)",
            "INSERT INTO t (a, b)\nVALUES\n    (1, 2),\n    (3, 4)",
        ),
        (
            "update t set a=1, b=2 where id=3",
            "UPDATE t\nSET\n    a = 1,\n    b = 2\nWHERE id = 3",
        ),
        (
            "select case when a then 1 when b then 2 else 3 end as x from t",
            "SELECT CASE\n    WHEN a THEN 1\n    WHEN b THEN 2\n    ELSE 3\nEND AS x\nFROM t",
        ),
        (
            "select id, case when a then 1 else 2 end as x, case when b then 1 end from t",
            "SELECT\n    id,\n    CASE\n        WHEN a THEN 1\n        ELSE 2\n    END AS x,\n    CASE WHEN b THEN 1 END\nFROM t",
        ),
        (
            "select a, -- the a\nb from t",
            "SELECT\n    a, -- the a\n    b\nFROM t",
        ),
        (
            "create view v as select a, b from t",
            "CREATE VIEW v AS\nSELECT\n    a,\n    b\nFROM t",
        ),
        (
            "create table t (id int primary key, name text not null)",
            "CREATE TABLE t (\n    id int PRIMARY KEY,\n    name TEXT NOT NULL\n)",
        ),
    ];
    for (sql, expected) in cases {
        assert_eq!(pretty(sql), expected, "{sql}");
    }
}

#[test]
fn keeps_line_breaks_of_other_statements_and_never_touches_bodies() {
    let sql = "create function f() returns int as $$\n  select  1;   -- keep\n$$   language   sql";
    assert_eq!(
        pretty(sql),
        "CREATE FUNCTION f() RETURNS int AS $$\n  select  1;   -- keep\n$$ LANGUAGE sql"
    );
    let block = "do $$\nbegin\n  raise notice 'a  b';\nend\n$$";
    assert_eq!(
        pretty(block),
        "DO $$\nbegin\n  raise notice 'a  b';\nend\n$$"
    );
}

#[test]
fn keyword_case_and_indent_width_are_options() {
    let sql = "Select a, b From t";
    let format = |keyword_case, indent_width| {
        format_all(
            sql,
            Dialect::Postgres,
            &FormatOptions {
                keyword_case,
                indent_width,
            },
        )
    };
    assert_eq!(format(KeywordCase::Upper, 2), "SELECT\n  a,\n  b\nFROM t");
    assert_eq!(
        format(KeywordCase::Lower, 4),
        "select\n    a,\n    b\nfrom t"
    );
    assert_eq!(format(KeywordCase::Preserve, 1), "Select\n a,\n b\nFrom t");
}

#[test]
fn continuation_lines_follow_the_statement_indentation_and_the_rest_is_kept() {
    let text = "-- first\n  select a, b from t;\n\n/* between */ select 1";
    let formatted = format_all(text, Dialect::Postgres, &FormatOptions::default());
    assert_eq!(
        formatted,
        "-- first\n  SELECT\n      a,\n      b\n  FROM t;\n\n/* between */ SELECT 1"
    );
}

#[test]
fn offsets_stay_on_their_token() {
    let text = "select a,b from t";
    let ranges = split_statements(text, Dialect::Postgres);
    let formatted =
        format_ranges(text, &ranges, Dialect::Postgres, &FormatOptions::default()).unwrap();
    let new = formatted.text();
    assert_eq!(new, "SELECT\n    a,\n    b\nFROM t");
    // Inside `from`, after `b`, at the start and at the end.
    let cases = [
        (
            text.find("from").unwrap() + 2,
            new.find("FROM").unwrap() + 2,
        ),
        (text.find('b').unwrap() + 1, new.find('b').unwrap() + 1),
        (0, 0),
        (text.len(), new.len()),
    ];
    for (old, expected) in cases {
        assert_eq!(formatted.map_offset(old), expected, "offset {old}");
    }
}

#[test]
fn the_safety_check_catches_changed_meaning() {
    let cases = [
        ("select a::int", "select a: :int"),
        ("select B'1010'", "select B '1010'"),
        ("select a - -1", "select a --1"),
        ("select 'a  b'", "select 'a b'"),
        ("select 1 -- note\n, 2", "select 1, 2"),
        ("select x from t", "select X from t"),
        ("select $1", "select $ 1"),
        ("select a b", "select ab"),
    ];
    for (before, after) in cases {
        assert!(
            !same_meaning(before, after, Dialect::Postgres),
            "{before:?} -> {after:?}"
        );
    }
    assert!(same_meaning(
        "select a=1,b from t",
        "SELECT a = 1, b\nFROM t",
        Dialect::Postgres
    ));
}
