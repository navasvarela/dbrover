-- Synthetic transactions for the valuation example: 300 trades of BTC, ETH and SOL by three
-- wallets, from 2026-09-01 to the end of November, each quantity with 18 decimal places, as a
-- token's amounts have. Every value comes from a formula, so the table is the same on every
-- machine; none of it is anyone's real data.
--
-- Make it in DbRover: add a PostgreSQL data source named "ledger-postgres", open this file in
-- its console (File → Open…), and run it all. It replaces the schema "valuation".

DROP SCHEMA IF EXISTS valuation CASCADE;
CREATE SCHEMA valuation;

CREATE TABLE valuation.transactions (
  id          integer PRIMARY KEY,
  wallet      text NOT NULL,
  asset       text NOT NULL,
  -- numeric keeps every digit: a quantity of 12.345000000000104729 stays one.
  quantity    numeric(38,18) NOT NULL,
  executed_at timestamp NOT NULL
);

-- A whole part and thousandths from one formula, and the last 18 places from another, both
-- exact: no division, no floating point.
INSERT INTO valuation.transactions (id, wallet, asset, quantity, executed_at)
SELECT n,
       'wallet-' || (n % 3 + 1),
       (ARRAY['BTC', 'ETH', 'SOL'])[n % 3 + 1],
       ((n * 7919) % 100000)::numeric / 1000
         + ((n * 104729) % 1000000)::numeric * 0.000000000000000001,
       timestamp '2026-09-01 00:00' + n * interval '6 hours 47 minutes'
FROM generate_series(1, 300) AS n;
