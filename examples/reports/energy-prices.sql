-- Made-up hourly energy prices for the battery examples: three price zones, in euros per kWh,
-- every hour from 2026-09-01 to the end of November, with 8 decimal places. Every price comes
-- from a formula, so the table is the same on every machine; none of it is a real market's
-- data.
--
-- Make it in DbRover: add a DuckDB data source named "prices-duckdb" with a new file, open
-- this file in its console (File → Open…), and run it all. Then edit the data source and tick
-- Read-only, so a report's run can attach the file while DbRover has it open.

CREATE OR REPLACE TABLE hourly_prices AS
SELECT zone,
       hour,
       CAST(base + (datediff('hour', TIMESTAMP '2026-09-01', hour) * 37 % 1000) * step
            AS DECIMAL(18,8)) AS price_eur
FROM (VALUES ('NORTH', 0.06000000, 0.00012345),
             ('SOUTH', 0.09000000, 0.00023456),
             ('WEST',  0.12000000, 0.00011111)) AS zones(zone, base, step),
     range(TIMESTAMP '2026-09-01', TIMESTAMP '2026-12-01', INTERVAL 1 HOUR) AS hours(hour)
ORDER BY zone, hour;
