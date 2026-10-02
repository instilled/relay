-- A project's rows in the order you put them on the panel (`digest.order`):
-- the project by its header's key (`g:<folder>`, `g:everywhere`), its rows
-- by theirs (`r:<id>`, `j:<kind>:<name>@<group>`), 0 on top. A project with
-- no rows here sorts itself.
CREATE TABLE row_order (
  group_key TEXT NOT NULL,
  row_key   TEXT NOT NULL,
  pos       INTEGER NOT NULL,
  PRIMARY KEY (group_key, row_key)
);
