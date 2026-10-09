-- What a firing's steps set for the steps after them: `rl flow state set`, and the `key=value`
-- lines a step appends to `$RELAY_OUT`. rld checks each (a key, 4 KB at most) before it writes.
CREATE TABLE step_values (
  firing TEXT NOT NULL REFERENCES firings(id),
  step TEXT NOT NULL,
  key TEXT NOT NULL,
  value TEXT NOT NULL,
  PRIMARY KEY (firing, step, key)
);
