-- One open question per group and kind of asker: a flow step's and a plain `rl ask`'s never
-- restate each other, so neither's answer does what the other meant.
DROP INDEX decisions_open_group;
CREATE UNIQUE INDEX decisions_open_group ON decisions (group_key, by_step IS NULL)
  WHERE status = 'open' AND group_key IS NOT NULL;
