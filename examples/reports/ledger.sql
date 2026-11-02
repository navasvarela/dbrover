-- A synthetic ledger for the example reports: a chart of accounts and three months of
-- balanced journal entries, from 2026-09-01 to 2026-11-30. Every amount comes from a formula,
-- so the ledger is the same on every machine; none of it is anyone's real data.
--
-- Make it in DbRover: add a SQLite data source named "ledger" with a new file, open this file
-- in its console (File → Open…), and run it all.

CREATE TABLE accounts (
  id   INTEGER PRIMARY KEY,
  code TEXT NOT NULL UNIQUE,
  name TEXT NOT NULL,
  -- asset, liability, equity, income or expense
  kind TEXT NOT NULL
);

INSERT INTO accounts (id, code, name, kind) VALUES
  (1, '1000', 'Cash', 'asset'),
  (2, '1100', 'Accounts receivable', 'asset'),
  (3, '2000', 'Accounts payable', 'liability'),
  (4, '3000', 'Owner''s equity', 'equity'),
  (5, '4000', 'Sales', 'income'),
  (6, '5000', 'Cost of sales', 'expense'),
  (7, '6100', 'Rent', 'expense'),
  (8, '6200', 'Software', 'expense');

-- Each journal is two lines whose debit and credit are equal. SQLite has no exact decimal
-- type, so the amounts are floating-point numbers here; the reports' decimal(18,2) columns
-- round each total once, at the end. PostgreSQL's numeric would keep every digit.
CREATE TABLE entries (
  id         INTEGER PRIMARY KEY,
  journal    INTEGER NOT NULL,
  account_id INTEGER NOT NULL REFERENCES accounts (id),
  booked_on  TEXT NOT NULL,
  memo       TEXT NOT NULL,
  debit      NUMERIC NOT NULL DEFAULT 0,
  credit     NUMERIC NOT NULL DEFAULT 0
);

-- The owner puts 50,000.00 in on the first day.
INSERT INTO entries (journal, account_id, booked_on, memo, debit, credit) VALUES
  (0, 1, '2026-09-01', 'Opening balance', 50000, 0),
  (0, 4, '2026-09-01', 'Opening balance', 0, 50000);

-- 360 journals over 91 days, cycling through four kinds of transaction:
--   0: a sale on credit       debit 1100 Accounts receivable, credit 4000 Sales
--   1: a customer pays        debit 1000 Cash,                credit 1100 Accounts receivable
--   2: a purchase on credit   debit 5000 Cost of sales,       credit 2000 Accounts payable
--   3: an expense paid        debit 6100 Rent or 6200 Software, credit 1000 Cash
CREATE TEMP VIEW journals AS
WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 360)
SELECT i AS journal,
       date('2026-09-01', '+' || ((i - 1) * 91 / 360) || ' days') AS booked_on,
       i % 4 AS kind,
       round(100 + (i * 7919) % 4900 + ((i * 31) % 100) / 100.0, 2) AS amount
FROM n;

INSERT INTO entries (journal, account_id, booked_on, memo, debit, credit)
SELECT journal,
       CASE kind WHEN 0 THEN 2 WHEN 1 THEN 1 WHEN 2 THEN 6 ELSE 7 + (journal / 4) % 2 END,
       booked_on,
       CASE kind
         WHEN 0 THEN 'Invoice ' || journal
         WHEN 1 THEN 'Payment for invoice ' || (journal - 1)
         WHEN 2 THEN 'Supplier bill ' || journal
         ELSE 'Card payment ' || journal
       END,
       amount,
       0
FROM journals;

INSERT INTO entries (journal, account_id, booked_on, memo, debit, credit)
SELECT journal,
       CASE kind WHEN 0 THEN 5 WHEN 1 THEN 2 WHEN 2 THEN 3 ELSE 1 END,
       booked_on,
       CASE kind
         WHEN 0 THEN 'Invoice ' || journal
         WHEN 1 THEN 'Payment for invoice ' || (journal - 1)
         WHEN 2 THEN 'Supplier bill ' || journal
         ELSE 'Card payment ' || journal
       END,
       0,
       amount
FROM journals;

DROP VIEW journals;
