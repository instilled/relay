-- One open question per group and kind of asker: a flow step's and a plain `rl ask`'s never
-- restate each other, so neither's answer does what the other meant.
DROP INDEX decisions_open_group;
CREATE UNIQUE INDEX decisions_open_group ON decisions (group_key, by_step IS NULL)
  WHERE status = 'open' AND group_key IS NOT NULL;
-- The version of its flow's folder a firing began on, by hash: its later steps start only while
-- that is still the one said yes to, so a firing never runs as a mix of two versions.
ALTER TABLE firings ADD COLUMN hash TEXT;
