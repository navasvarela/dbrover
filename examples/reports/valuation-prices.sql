-- Synthetic hourly prices for the valuation example: BTC, ETH and SOL in US dollars, every
-- hour from 2026-09-01 to the end of November, with 8 decimal places. Every price comes from
-- a formula, so the table is the same on every machine; none of it is a real market's data.
--
-- Make it in DbRover: add a DuckDB data source named "prices-duckdb" with a new file, open
-- this file in its console (File → Open…), and run it all. Then edit the data source and tick
-- Read-only, so a report's run can attach the file while DbRover has it open.

CREATE OR REPLACE TABLE hourly_prices AS
SELECT asset,
       hour,
       CAST(base + (datediff('hour', TIMESTAMP '2026-09-01', hour) * 37 % 1000) * step
            AS DECIMAL(18,8)) AS price_usd
FROM (VALUES ('BTC', 61000.00000000, 0.12345678),
             ('ETH',  2400.00000000, 0.01234567),
             ('SOL',   150.00000000, 0.00123456)) AS assets(asset, base, step),
     range(TIMESTAMP '2026-09-01', TIMESTAMP '2026-12-01', INTERVAL 1 HOUR) AS hours(hour)
ORDER BY asset, hour;
