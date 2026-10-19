-- Frozen Buildscope database version 1, before typed query projections.
CREATE TABLE IF NOT EXISTS runs(
    id TEXT PRIMARY KEY, started INTEGER NOT NULL, status TEXT NOT NULL,
    kind TEXT NOT NULL, configuration TEXT NOT NULL, targets TEXT NOT NULL,
    local INTEGER NOT NULL, manifest TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS launcher_receipts(build TEXT NOT NULL,invocation TEXT NOT NULL,run TEXT NOT NULL,
    PRIMARY KEY(build,invocation));
CREATE INDEX IF NOT EXISTS run_history ON runs(configuration, kind, started);
CREATE TABLE IF NOT EXISTS events(
    run TEXT NOT NULL, unit TEXT NOT NULL, idx INTEGER NOT NULL,
    name TEXT NOT NULL, detail TEXT NOT NULL, start REAL NOT NULL,
    duration REAL NOT NULL, data TEXT NOT NULL,
    PRIMARY KEY(run,unit,idx));
CREATE INDEX IF NOT EXISTS event_query ON events(run,unit,duration DESC);
CREATE INDEX IF NOT EXISTS compiler_work_query ON events(run,name,detail,unit)
    WHERE name IN ('InstantiateClass','InstantiateFunction','OptFunction','CodeGen Function','isIntegerConstantExpr')
        OR name GLOB 'Evaluate*';
CREATE TABLE IF NOT EXISTS headers(
    run TEXT NOT NULL, path TEXT NOT NULL, key TEXT NOT NULL,
    trigger INTEGER NOT NULL, contributor INTEGER NOT NULL,
    rebuilt INTEGER NOT NULL, work REAL NOT NULL,
    PRIMARY KEY(run,path));
CREATE INDEX IF NOT EXISTS header_history ON headers(key,run);
CREATE TABLE IF NOT EXISTS report_runs(id TEXT PRIMARY KEY,data TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS profiles(run TEXT NOT NULL,unit TEXT NOT NULL,data TEXT NOT NULL,
    PRIMARY KEY(run,unit));
CREATE TABLE IF NOT EXISTS report_units(run TEXT NOT NULL,unit TEXT NOT NULL,data TEXT NOT NULL,
    PRIMARY KEY(run,unit));
CREATE TABLE IF NOT EXISTS report_headers(run TEXT NOT NULL,path TEXT NOT NULL,data TEXT NOT NULL,
    PRIMARY KEY(run,path));
CREATE TABLE IF NOT EXISTS include_paths(run TEXT NOT NULL,path TEXT NOT NULL,
    chain TEXT NOT NULL,unit TEXT NOT NULL,PRIMARY KEY(run,path,chain,unit));
CREATE INDEX IF NOT EXISTS event_parent ON events(run,unit,json_extract(data,'$.parent'));
PRAGMA user_version=1;
