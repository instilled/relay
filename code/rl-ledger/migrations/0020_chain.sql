-- A run's chain (what set it off, and what it set off) was called its flow.
-- Flows are now the definitions in ~/.relay/flows, so the chain says chain.
ALTER TABLE runs RENAME COLUMN flow TO chain;
ALTER TABLE decisions RENAME COLUMN flow TO chain;
