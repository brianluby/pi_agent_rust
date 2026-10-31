//! Real SQLite initialization through the PiJS ESM and createRequire loaders.
//! The fixtures exercise the API shape used by context-mode's Bun adapter,
//! including durable session data, migrations, transactions and RETURNING.

use pi::extensions::{ExtensionPolicy, ExtensionPolicyMode};
use pi::extensions_js::{PiJsRuntime, PiJsRuntimeConfig};
use pi::scheduler::DeterministicClock;
use serde_json::{Value, json};
use std::path::Path;

// A process intentionally has at most eight SQLite workers. Keep these
// independent runtime fixtures from competing for that production budget.
static SQLITE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[allow(clippy::future_not_send)]
async fn runtime(
    root: &Path,
    owner: &str,
    policy: Option<ExtensionPolicy>,
) -> PiJsRuntime<DeterministicClock> {
    PiJsRuntime::with_clock_and_config_with_policy_for_extension(
        DeterministicClock::new(0),
        PiJsRuntimeConfig {
            cwd: root.to_string_lossy().into_owned(),
            ..Default::default()
        },
        policy,
        owner.to_string(),
    )
    .await
    .expect("runtime")
}

#[allow(clippy::future_not_send)]
async fn evaluate(runtime: &PiJsRuntime<DeterministicClock>, source: &str) -> Value {
    runtime
        .eval(&format!(
            r#"
        globalThis.sqliteResult = null;
        globalThis.sqliteError = null;
        Promise.all([import('bun:sqlite'), import('node:module'), import('node:fs')])
          .then(([sqlite, module, fs]) => {{ {source} }})
          .catch(error => {{ globalThis.sqliteError = String(error.stack || error); }});
    "#
        ))
        .await
        .expect("evaluate");
    runtime.drain_microtasks().await.expect("drain");
    assert_eq!(
        runtime
            .read_global_json("sqliteError")
            .await
            .expect("error value"),
        Value::Null
    );
    runtime
        .read_global_json("sqliteResult")
        .await
        .expect("result")
}

#[test]
#[allow(clippy::too_many_lines)]
fn sqlite_imports_initialize_a_persistent_session_database_and_reopen_it() {
    let _serial = SQLITE_TEST_LOCK.lock().expect("SQLite test lock");
    futures::executor::block_on(async {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical tempdir");
        let storage = root.join(".pi/context-mode");
        let database = storage.join("sessions/project.db");
        let path = serde_json::to_string(&database.to_string_lossy()).expect("path");
        let dir = serde_json::to_string(&database.parent().unwrap().to_string_lossy())
            .expect("directory");
        let first = runtime(&root, "context-mode", None).await;
        first
            .add_persistent_storage_root(storage.clone(), "context-mode")
            .expect("storage grant");
        let result = evaluate(&first, &format!(r#"
            const {{ Database }} = module.createRequire('file:///extension/index.mjs')('bun:sqlite');
            if (Database !== sqlite.Database) throw new Error('ESM and require must share the constructor');
            fs.mkdirSync({dir}, {{ recursive: true }});
            const db = new Database({path}, {{ create: true }});
            db.prepare('PRAGMA journal_mode = WAL').all();
            db.prepare('PRAGMA busy_timeout = 30000').all();
            db.prepare('PRAGMA synchronous = NORMAL').all();
            // context-mode's adapter runs schema DDL through prepare().run().
            db.prepare(`CREATE TABLE IF NOT EXISTS session_meta (
              session_id TEXT PRIMARY KEY, project_dir TEXT NOT NULL,
              event_count INTEGER NOT NULL DEFAULT 0
            )`).run();
            db.prepare(`CREATE TABLE IF NOT EXISTS session_events (
              id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL,
              data TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT (datetime('now'))
            )`).run();
            const columns = db.prepare('PRAGMA table_xinfo(session_meta)').all();
            if (!columns.some(column => column.name === 'usage_cursor')) {{
              db.prepare('ALTER TABLE session_meta ADD COLUMN usage_cursor TEXT').run();
            }}
            const save = db.transaction((session, project, data) => {{
              db.prepare('INSERT OR IGNORE INTO session_meta(session_id, project_dir) VALUES (?, ?)').run(session, project);
              db.prepare('INSERT INTO session_events(session_id, data) VALUES (?, ?)').run(session, data);
              db.prepare('UPDATE session_meta SET event_count = event_count + 1 WHERE session_id = ?').run(session);
            }});
            save('session-1', 'C:\\work\\project', 'durable; unicode λ');
            const statement = db.prepare('SELECT data FROM session_events WHERE session_id = ?');
            globalThis.sqliteResult = {{
              first: statement.get('session-1'),
              rows: [...statement.iterate('session-1')],
              count: db.query('SELECT event_count FROM session_meta WHERE session_id = ?').get('session-1'),
              inTransaction: db.inTransaction,
            }};
            db.close();
        "#)).await;
        assert_eq!(
            result,
            json!({
                "first": {"data": "durable; unicode λ"},
                "rows": [{"data": "durable; unicode λ"}],
                "count": {"event_count": 1}, "inTransaction": false,
            })
        );
        drop(first);
        assert!(database.metadata().expect("real database file").len() > 0);
        let second = runtime(&root, "context-mode", None).await;
        second
            .add_persistent_storage_root(storage, "context-mode")
            .expect("storage grant");
        let persisted = evaluate(&second, &format!(r#"
            const db = new sqlite.Database({path}, {{ readonly: true, create: true }});
            let writeDenied = false;
            try {{ db.run("INSERT INTO session_events(session_id,data) VALUES ('x','must fail')"); }}
            catch {{ writeDenied = true; }}
            globalThis.sqliteResult = {{
              row: db.query('SELECT data FROM session_events').get(), writeDenied,
              migration: db.query('PRAGMA table_xinfo(session_meta)').all().some(column => column.name === 'usage_cursor'),
            }};
            db.close();
        "#)).await;
        assert_eq!(
            persisted,
            json!({"row": {"data": "durable; unicode λ"}, "writeDenied": true, "migration": true})
        );
    });
}

#[test]
fn sqlite_busy_error_reaches_context_mode_retry_and_preserves_persistent_state() {
    let _serial = SQLITE_TEST_LOCK.lock().expect("SQLite test lock");
    futures::executor::block_on(async {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let storage = root.join("storage");
        std::fs::create_dir(&storage).expect("storage");
        let path = serde_json::to_string(&storage.join("retry.db")).unwrap();
        let runtime = runtime(&root, "retry", None).await;
        runtime
            .add_persistent_storage_root(storage, "retry")
            .expect("storage grant");
        let result = evaluate(&runtime, &format!(r#"
            const locker = new sqlite.Database({path});
            locker.exec('PRAGMA journal_mode = WAL');
            locker.exec('CREATE TABLE events (id INTEGER PRIMARY KEY, data TEXT)');
            const db = new sqlite.Database({path}, {{ create: false }});
            db.exec('PRAGMA busy_timeout = 0');
            const insert = db.prepare('INSERT INTO events VALUES (?, ?)');
            const save = db.transaction(() => insert.run(1, 'persisted after retry'));
            locker.exec('BEGIN IMMEDIATE');
            locker.prepare('INSERT INTO events VALUES (?, ?)').run(99, 'uncommitted owner');
            const failures = [];
            let attempts = 0;
            // Match context-mode 1.0.169 db-base.withRetry's message test.
            // Release the genuine competing writer only after that test
            // recognizes the first failure, then retry the same transaction.
            for (;;) {{
              attempts++;
              try {{ save(); break; }}
              catch (error) {{
                if (!error.message.includes('SQLITE_BUSY') && !error.message.includes('database is locked')) throw error;
                if (attempts !== 1) throw error;
                failures.push({{code:error.code, errno:error.errno, primary:error.primaryCode,
                  message:error.message, rolledBack:!db.inTransaction}});
                locker.exec('ROLLBACK');
              }}
            }}
            const inTransaction = db.inTransaction;
            db.close();
            locker.close();
            const reopened = new sqlite.Database({path}, {{ readonly: true }});
            globalThis.sqliteResult = {{ attempts, failures, inTransaction,
              rows: reopened.query('SELECT id, data FROM events ORDER BY id').values() }};
            reopened.close();
        "#)).await;
        assert_eq!(result["attempts"], 2, "{result}");
        assert_eq!(result["inTransaction"], false, "{result}");
        assert_eq!(
            result["rows"],
            json!([[1, "persisted after retry"]]),
            "{result}"
        );
        let failure = &result["failures"][0];
        assert_eq!(failure["primary"], 5, "{result}");
        assert_eq!(failure["errno"].as_i64().unwrap() & 0xff, 5);
        assert!(failure["code"].as_str().unwrap().starts_with("SQLITE_BUSY"));
        assert!(failure["message"].as_str().unwrap().contains("SQLITE_BUSY"));
        assert_eq!(failure["rolledBack"], true, "{result}");
    });
}

#[test]
fn sqlite_engine_errors_remain_distinct_from_admission_and_nested_rollback_errors() {
    let _serial = SQLITE_TEST_LOCK.lock().expect("SQLite test lock");
    futures::executor::block_on(async {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let storage = root.join("storage");
        std::fs::create_dir(&storage).expect("storage");
        let invalid = storage.join("invalid.db");
        std::fs::write(&invalid, [0x61_u8; 4096]).expect("invalid database fixture");
        let invalid_path = serde_json::to_string(&invalid).unwrap();
        let missing_path = serde_json::to_string(&storage.join("missing.db")).unwrap();
        let runtime = runtime(&root, "errors", None).await;
        runtime
            .add_persistent_storage_root(storage, "errors")
            .expect("storage grant");
        let result = evaluate(
            &runtime,
            &format!(
                r#"
            const db = new sqlite.Database(':memory:');
            const errors = {{}};
            function capture(name, operation) {{
              try {{ operation(); throw new Error('expected SQLite rejection'); }}
              catch (error) {{
                errors[name] = {{code:error.code, errno:error.errno ?? null,
                  primary:error.primaryCode ?? null, message:error.message}};
              }}
            }}
            // A table name that looks like lock prose must not alter its
            // actual native SQLITE_ERROR classification.
            capture('missingTable', () => db.prepare('SELECT * FROM "database is locked"'));
            capture('admission', () => db.exec("ATTACH ':memory:' AS other"));
            capture('missingFile', () => new sqlite.Database({missing_path}, {{create:false}}));
            capture('invalidFile', () => new sqlite.Database({invalid_path}, {{create:false}}));
            const readonly = new sqlite.Database(':memory:', {{readonly:true}});
            capture('readonly', () => readonly.exec('CREATE TABLE denied(value)'));
            readonly.close();
            db.exec('CREATE TABLE entries(id INTEGER PRIMARY KEY)');
            db.run('INSERT INTO entries VALUES (1)');
            capture('nestedRollback', () => db.transaction(() => {{
              db.run('INSERT INTO entries VALUES (2)');
              db.transaction(() => db.run('INSERT OR ROLLBACK INTO entries VALUES (1)'))();
            }})());
            const ended = !db.inTransaction;
            db.transaction(() => db.run('INSERT INTO entries VALUES (3)'))();
            globalThis.sqliteResult = {{ errors, ended,
              rows:db.query('SELECT id FROM entries ORDER BY id').values() }};
            db.close();
        "#
            ),
        )
        .await;
        assert_eq!(result["ended"], true, "{result}");
        assert_eq!(result["rows"], json!([[1], [3]]), "{result}");
        for (name, code, primary) in [
            ("missingTable", "SQLITE_ERROR", 1),
            ("missingFile", "SQLITE_CANTOPEN", 14),
            ("readonly", "SQLITE_READONLY", 8),
            ("nestedRollback", "SQLITE_CONSTRAINT", 19),
        ] {
            let error = &result["errors"][name];
            assert_eq!(error["code"], code, "{name}: {result}");
            assert_eq!(error["primary"], primary, "{name}: {result}");
            assert_eq!(error["errno"].as_i64().unwrap() & 0xff, primary);
            assert!(error["message"].as_str().unwrap().starts_with(code));
        }
        let admission = &result["errors"]["admission"];
        assert_eq!(admission["code"], "SQLITE_ERROR", "{result}");
        assert_eq!(admission["errno"], Value::Null, "{result}");
        assert_eq!(admission["primary"], Value::Null, "{result}");
        let invalid = &result["errors"]["invalidFile"];
        assert!(
            matches!(
                invalid["code"].as_str(),
                Some("SQLITE_CORRUPT" | "SQLITE_NOTADB")
            ),
            "native corruption/open failure must keep its actual code: {result}"
        );
        assert!(
            invalid["message"]
                .as_str()
                .unwrap()
                .starts_with(invalid["code"].as_str().unwrap())
        );
    });
}

#[test]
fn sqlite_transactions_returning_bindings_and_binary_values_are_real() {
    let _serial = SQLITE_TEST_LOCK.lock().expect("SQLite test lock");
    futures::executor::block_on(async {
        let temp = tempfile::tempdir().expect("tempdir");
        let runtime = runtime(temp.path(), "sqlite-api", None).await;
        let result = evaluate(&runtime, r#"
            const db = new sqlite.Database(':memory:', { strict: true });
            db.exec('CREATE TABLE entries(id INTEGER PRIMARY KEY, name TEXT, payload BLOB); CREATE TABLE resumed(id INTEGER PRIMARY KEY, consumed INTEGER)');
            const pendingSchema = db.prepare('CREATE TABLE deferred(value TEXT)');
            const beforeStep = db.query("SELECT count(*) AS count FROM sqlite_master WHERE name = 'deferred'").get().count;
            pendingSchema.run();
            let malformedRejected = false;
            try { db.prepare('CREATE TABLE malformed('); } catch { malformedRejected = true; }
            const insert = db.prepare('INSERT INTO entries VALUES ($id, $name, $blob)');
            insert.run({id: 1, name: 'first', blob: new Uint8Array([0, 255, 128])});
            const outer = db.transaction(() => {
              insert.run({id: 2, name: 'second', blob: new Uint8Array([5])});
              try { db.transaction(() => {
                insert.run({id: 3, name: 'rolled back', blob: new Uint8Array([])});
                throw new Error('rollback inner');
              })(); } catch (error) { if (error.message !== 'rollback inner') throw error; }
            });
            outer.immediate();
            try { db.transaction(() => {
              insert.run({id: 4, name: 'rolled back outer', blob: new Uint8Array([])});
              throw new Error('rollback outer');
            })(); } catch (error) { if (error.message !== 'rollback outer') throw error; }
            db.run('INSERT INTO resumed VALUES(1,0), (2,0)');
            const claimed = db.prepare('UPDATE resumed SET consumed = 1 WHERE id = (SELECT id FROM resumed WHERE consumed = 0 ORDER BY id LIMIT 1) RETURNING id').get();
            let missingRejected = false;
            try { insert.run({id: 5}); } catch { missingRejected = true; }
            const first = db.query('SELECT * FROM entries ORDER BY id').get();
            globalThis.sqliteResult = {
              ids: db.query('SELECT id FROM entries ORDER BY id').values(),
              first: {id: first.id, name: first.name, payload: Array.from(first.payload)},
              claimed, consumed: db.query('SELECT SUM(consumed) AS count FROM resumed').get().count,
              missingRejected, empty: db.query('SELECT id FROM entries WHERE id = 99').get(),
              aliases: db.prepare('SELECT :same AS first, ?1 AS second').get({same: 7}),
              beforeStep, malformedRejected,
            };
            db.close();
        "#).await;
        assert_eq!(
            result,
            json!({
                "ids": [[1], [2]], "first": {"id": 1, "name": "first", "payload": [0,255,128]},
                "claimed": {"id": 1}, "consumed": 1, "missingRejected": true, "empty": null,
            "aliases": {"first": 7, "second": 7}, "beforeStep": 0, "malformedRejected": true,
            })
        );
    });
}

#[test]
fn sqlite_dml_row_methods_execute_returning_and_commit_once() {
    let _serial = SQLITE_TEST_LOCK.lock().expect("SQLite test lock");
    futures::executor::block_on(async {
        let temp = tempfile::tempdir().expect("tempdir");
        let runtime = runtime(temp.path(), "returning", None).await;
        let result = evaluate(&runtime, r#"
            const db = new sqlite.Database(':memory:');
            db.prepare('CREATE TABLE claims(id INTEGER PRIMARY KEY, consumed INTEGER DEFAULT 0)').run();
            const inserted = db.prepare('INSERT INTO claims(id) VALUES (1), (2), (3) RETURNING id, consumed').all();
            const updated = db.prepare('UPDATE claims SET consumed = consumed + 1 WHERE id <= ? RETURNING id, consumed').values(2);
            const removed = [...db.prepare('DELETE FROM claims WHERE id = ? RETURNING id').iterate(3)];
            const first = db.prepare('UPDATE claims SET consumed = consumed + 1 RETURNING id, consumed').get();
            const ignoredRows = db.prepare('UPDATE claims SET consumed = consumed + 1 RETURNING id').run();
            const noProjection = db.prepare('INSERT INTO claims(id, consumed) VALUES (?, ?)').all(4, 7);
            const noMatch = db.prepare('UPDATE claims SET consumed = 99 WHERE id = 99 RETURNING id').get();
            globalThis.sqliteResult = {
              inserted: inserted.sort((a,b) => a.id-b.id), updated: updated.sort((a,b) => a[0]-b[0]),
              removed, firstConsumed: first.consumed, firstIsOneRow: [1,2].includes(first.id),
              changes: ignoredRows.changes, noProjection, noMatch,
              rows: db.query('SELECT id, consumed FROM claims ORDER BY id').all(), inTransaction: db.inTransaction,
            };
            db.close();
        "#).await;
        assert_eq!(
            result,
            json!({
                "inserted": [{"id":1,"consumed":0},{"id":2,"consumed":0},{"id":3,"consumed":0}],
                "updated": [[1,1],[2,1]], "removed": [{"id":3}], "firstConsumed":2, "firstIsOneRow":true,
                "changes":2, "noProjection":[], "noMatch":null,
                "rows":[{"id":1,"consumed":3},{"id":2,"consumed":3},{"id":4,"consumed":7}],
                "inTransaction":false,
            })
        );
    });
}

#[test]
fn sqlite_returning_encoding_failure_rolls_back_only_its_own_statement() {
    let _serial = SQLITE_TEST_LOCK.lock().expect("SQLite test lock");
    futures::executor::block_on(async {
        let temp = tempfile::tempdir().expect("tempdir");
        let runtime = runtime(temp.path(), "returning-limit", None).await;
        let result = evaluate(&runtime, r#"
            const db = new sqlite.Database(':memory:');
            db.exec('CREATE TABLE state(value INTEGER, payload TEXT); CREATE TABLE audit(value TEXT)');
            db.prepare('INSERT INTO state VALUES(0, ?)').run('x'.repeat(600 * 1024));
            const tooLarge = db.prepare('UPDATE state SET value = value + 1 RETURNING payload || payload AS expanded');
            let autocommitRejected = false;
            try { tooLarge.all(); } catch (error) { autocommitRejected = /cell exceeds/.test(error.message); }
            const afterAutocommit = db.query('SELECT value FROM state').get().value;
            const noLeakedTransaction = !db.inTransaction;
            db.prepare('BEGIN IMMEDIATE').run();
            db.prepare('INSERT INTO audit VALUES(?)').run('before');
            let nestedRejected = false;
            try { tooLarge.get(); } catch (error) { nestedRejected = /cell exceeds/.test(error.message); }
            const enclosingTransactionSurvived = db.inTransaction;
            db.prepare('INSERT INTO audit VALUES(?)').run('after');
            db.prepare('COMMIT').run();
            globalThis.sqliteResult = {
              autocommitRejected, afterAutocommit, noLeakedTransaction,
              nestedRejected, enclosingTransactionSurvived,
              finalValue: db.query('SELECT value FROM state').get().value,
              audit: db.query('SELECT value FROM audit ORDER BY rowid').values(),
            };
            db.close();
        "#).await;
        assert_eq!(
            result,
            json!({
                "autocommitRejected":true, "afterAutocommit":0, "noLeakedTransaction":true,
                "nestedRejected":true, "enclosingTransactionSurvived":true,
                "finalValue":0, "audit":[["before"],["after"]],
            })
        );
    });
}

#[test]
fn sqlite_returning_row_limit_rolls_back_and_select_get_stops_after_one_row() {
    let _serial = SQLITE_TEST_LOCK.lock().expect("SQLite test lock");
    futures::executor::block_on(async {
        let temp = tempfile::tempdir().expect("tempdir");
        let runtime = runtime(temp.path(), "returning-rows", None).await;
        let result = evaluate(
            &runtime,
            r#"
            const db = new sqlite.Database(':memory:');
            db.exec('CREATE TABLE many_rows(value INTEGER)');
            // Each seed batch remains below the SQL token limit. Two copies
            // reach 10,004 rows without 10,004 separate native calls.
            db.exec('INSERT INTO many_rows(value) VALUES ' + Array(2501).fill('(0)').join(','));
            db.exec('INSERT INTO many_rows SELECT value FROM many_rows');
            db.exec('INSERT INTO many_rows SELECT value FROM many_rows');
            const first = db.query('SELECT value FROM many_rows').get();
            let rejected = false;
            try { db.prepare('UPDATE many_rows SET value = 1 RETURNING value').all(); }
            catch (error) { rejected = /row or time limit/.test(error.message); }
            globalThis.sqliteResult = {
              first, rejected, inTransaction: db.inTransaction,
              state: db.query('SELECT count(*) AS count, sum(value) AS sum FROM many_rows').get(),
            };
            db.close();
        "#,
        )
        .await;
        assert_eq!(
            result,
            json!({"first":{"value":0}, "rejected":true, "inTransaction":false,
            "state":{"count":10004,"sum":0}})
        );
    });
}

#[test]
fn sqlite_prepared_ddl_preserves_defaults_and_checks_from_its_original_source() {
    let _serial = SQLITE_TEST_LOCK.lock().expect("SQLite test lock");
    futures::executor::block_on(async {
        let temp = tempfile::tempdir().expect("tempdir");
        let runtime = runtime(temp.path(), "ddl-source", None).await;
        let result = evaluate(&runtime, r#"
            const db = new sqlite.Database(':memory:');
            db.prepare("/* original source offsets */ CREATE TABLE quoted(value TEXT DEFAULT ('semi;''quote λ'), n INTEGER CHECK(n > 3))").run();
            db.prepare('INSERT INTO quoted(n) VALUES(?)').run(4);
            let constraintRejected = false;
            try { db.prepare('INSERT INTO quoted(n) VALUES(?)').run(2); } catch { constraintRejected = true; }
            globalThis.sqliteResult = {constraintRejected, rows: db.query('SELECT value, n FROM quoted').all()};
            db.close();
        "#).await;
        assert_eq!(
            result,
            json!({"constraintRejected":true, "rows":[{"value":"semi;'quote λ","n":4}]})
        );
    });
}

#[test]
fn sqlite_returning_preserves_native_conflict_policies_and_dqs_literals() {
    let _serial = SQLITE_TEST_LOCK.lock().expect("SQLite test lock");
    futures::executor::block_on(async {
        let temp = tempfile::tempdir().expect("tempdir");
        let runtime = runtime(temp.path(), "conflict-policy", None).await;
        let result = evaluate(&runtime, r#"
            const db = new sqlite.Database(':memory:');
            db.exec('CREATE TABLE conflicts(id INTEGER PRIMARY KEY, value TEXT)');
            const quoted = db.prepare('INSERT INTO conflicts VALUES (9, "DQS literal") RETURNING id, value').get();
            let failRejected = false;
            try { db.prepare("INSERT OR FAIL INTO conflicts VALUES (1,'kept'), (9,'duplicate'), (2,'unreached') RETURNING id").all(); }
            catch { failRejected = true; }
            const afterFail = db.query('SELECT id FROM conflicts ORDER BY id').values();
            db.exec('BEGIN IMMEDIATE');
            db.run("INSERT INTO conflicts VALUES (5,'outer transaction')");
            try { db.prepare("INSERT OR FAIL INTO conflicts VALUES (3,'kept in transaction'), (9,'duplicate') RETURNING id").all(); } catch {}
            try { db.prepare("INSERT OR ABORT INTO conflicts VALUES (6,'must roll back'), (9,'duplicate') RETURNING id").all(); } catch {}
            const duringTransaction = db.query('SELECT id FROM conflicts ORDER BY id').values();
            const stillInTransaction = db.inTransaction;
            let rollbackRejected = false;
            try { db.prepare("INSERT OR ROLLBACK INTO conflicts VALUES (7,'must roll back'), (9,'duplicate') RETURNING id").all(); }
            catch { rollbackRejected = true; }
            db.exec('CREATE TABLE schema_fail(id INTEGER UNIQUE ON CONFLICT FAIL)');
            db.run('INSERT INTO schema_fail VALUES (9)');
            try { db.prepare('INSERT INTO schema_fail VALUES (1), (9), (2) RETURNING id').all(); } catch {}
            globalThis.sqliteResult = {
              quoted, failRejected, afterFail, duringTransaction, stillInTransaction,
              rollbackRejected, transactionEnded: !db.inTransaction,
              finalRows: db.query('SELECT id FROM conflicts ORDER BY id').values(),
              schemaFailRows: db.query('SELECT id FROM schema_fail ORDER BY id').values(),
            };
            db.close();
        "#).await;
        assert_eq!(
            result,
            json!({
                "quoted":{"id":9,"value":"DQS literal"}, "failRejected":true,
                "afterFail":[[1],[9]], "duringTransaction":[[1],[3],[5],[9]], "stillInTransaction":true,
                "rollbackRejected":true, "transactionEnded":true, "finalRows":[[1],[9]],
                "schemaFailRows":[[1],[9]],
            })
        );
    });
}

#[test]
fn sqlite_schema_and_sql_cannot_bypass_file_capabilities() {
    let _serial = SQLITE_TEST_LOCK.lock().expect("SQLite test lock");
    futures::executor::block_on(async {
        let temp = tempfile::tempdir().expect("tempdir");
        let runtime = runtime(temp.path(), "sqlite-security", None).await;
        let result = evaluate(&runtime, r#"
            const db = new sqlite.Database(':memory:');
            db.exec('CREATE TABLE protected(value TEXT)');
            const attacks = [
              `ATTACH DATABASE '/tmp/escape.db' AS outside`,
              `/* prefix */ VaCuUm INTO '/tmp/escape.db'`,
              `PRAGMA temp_store_directory = '/tmp'`,
              `PRAGMA main.journal_mode = WAL`,
              `PRAGMA writable_schema = ON`,
              `SELECT "load_extension"('/tmp/native.so')`,
              "SELECT `readfile`('/tmp/private')",
              `SELECT [writefile]('/tmp/private','bad')`,
              `SELECT 'load_extension'('/tmp/native.so')`,
              `SELECT * FROM pragma_temp_store_directory('/tmp')`,
              `CREATE VIRTUAL TABLE files USING csv(filename='/tmp/private')`,
              `CREATE TRIGGER bad AFTER INSERT ON protected BEGIN SELECT 'writefile'('/tmp/private','bad'); END`,
              `INSERT INTO protected VALUES('must not commit'); /* tail */ ATTACH '/tmp/escape.db' AS x`,
            ];
            const denied = attacks.map(sql => { try { db.exec(sql); return false; } catch { return true; } });
            db.prepare("INSERT INTO protected VALUES (?)").run("ATTACH; readfile('text only')");
            globalThis.sqliteResult = {
              denied, rows: db.query('SELECT value FROM protected').all(),
              aliases: db.query('PRAGMA database_list').all().map(row => row.name),
            };
            db.close();
        "#).await;
        assert!(
            result["denied"]
                .as_array()
                .unwrap()
                .iter()
                .all(|value| value == &json!(true))
        );
        assert_eq!(
            result["rows"],
            json!([{"value": "ATTACH; readfile('text only')"}])
        );
        assert!(
            result["aliases"]
                .as_array()
                .unwrap()
                .iter()
                .all(|name| name == "main" || name == "temp")
        );
    });
}

#[test]
fn sqlite_file_open_requires_its_own_storage_grant_and_write_capability() {
    let _serial = SQLITE_TEST_LOCK.lock().expect("SQLite test lock");
    futures::executor::block_on(async {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let storage = root.join("owner");
        std::fs::create_dir(&storage).expect("storage");
        let own = serde_json::to_string(&storage.join("own.db").to_string_lossy()).unwrap();
        let outside = serde_json::to_string(&root.join("outside.db").to_string_lossy()).unwrap();
        let traversal =
            serde_json::to_string(&storage.join("../escape.db").to_string_lossy()).unwrap();
        let runtime = runtime(&root, "owner", None).await;
        runtime
            .add_persistent_storage_root(storage.clone(), "owner")
            .expect("owner grant");
        let result = evaluate(&runtime, &format!(r#"
            const paths = [{outside}, {traversal}, 'file:{own}'];
            const denied = paths.map(path => {{ try {{ new sqlite.Database(path); return false; }} catch {{ return true; }} }});
            const db = new sqlite.Database({own});
            db.exec('CREATE TABLE data(value TEXT)');
            db.close();
            globalThis.sqliteResult = denied;
        "#)).await;
        assert_eq!(result, json!([true, true, true]));
        assert!(!root.join("outside.db").exists());
        assert!(!root.join("escape.db").exists());
        let peer = self::runtime(&root, "peer", None).await;
        peer.add_persistent_storage_root(storage.clone(), "owner")
            .expect("foreign owner grant");
        assert_eq!(
            evaluate(
                &peer,
                &format!(
                    r#"
            try {{ new sqlite.Database({own}); globalThis.sqliteResult = false; }}
            catch {{ globalThis.sqliteResult = true; }}
        "#
                )
            )
            .await,
            json!(true)
        );
        let mut policy = ExtensionPolicy {
            mode: ExtensionPolicyMode::Strict,
            ..Default::default()
        };
        policy.deny_caps.push("write".into());
        let restricted = self::runtime(&root, "owner", Some(policy)).await;
        restricted
            .add_persistent_storage_root(storage, "owner")
            .expect("restricted grant");
        assert_eq!(
            evaluate(
                &restricted,
                &format!(
                    r#"
            try {{ new sqlite.Database({own}); globalThis.sqliteResult = false; }}
            catch {{ globalThis.sqliteResult = true; }}
        "#
                )
            )
            .await,
            json!(true)
        );
    });
}

#[cfg(unix)]
#[test]
fn sqlite_rejects_database_and_companion_symlinks_without_touching_targets() {
    let _serial = SQLITE_TEST_LOCK.lock().expect("SQLite test lock");
    use std::os::unix::fs::symlink;
    futures::executor::block_on(async {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let storage = root.join("storage");
        std::fs::create_dir(&storage).expect("storage");
        let outside = root.join("outside");
        std::fs::write(&outside, b"sentinel").expect("sentinel");
        symlink(&outside, storage.join("linked.db")).expect("main symlink");
        symlink(&outside, storage.join("sidecar.db-wal")).expect("WAL symlink");
        symlink(&outside, storage.join("locks.db-lock-shared")).expect("Windows sidecar symlink");
        std::fs::hard_link(&outside, storage.join("hard.db")).expect("main hard link");
        let runtime = runtime(&root, "owner", None).await;
        runtime
            .add_persistent_storage_root(storage.clone(), "owner")
            .expect("grant");
        let paths = serde_json::to_string(&[
            storage.join("linked.db"),
            storage.join("sidecar.db"),
            storage.join("locks.db"),
            storage.join("hard.db"),
        ])
        .unwrap();
        assert_eq!(
            evaluate(
                &runtime,
                &format!(
                    r#"
            globalThis.sqliteResult = {paths}.map(path => {{
              try {{ new sqlite.Database(path); return false; }} catch {{ return true; }}
            }});
        "#
                )
            )
            .await,
            json!([true, true, true, true])
        );
        assert_eq!(std::fs::read(outside).unwrap(), b"sentinel");
        assert!(!storage.join("sidecar.db").exists());
        assert!(!storage.join("locks.db").exists());
    });
}

#[test]
fn sqlite_fts5_search_uses_real_indexed_content() {
    let _serial = SQLITE_TEST_LOCK.lock().expect("SQLite test lock");
    futures::executor::block_on(async {
        let temp = tempfile::tempdir().expect("tempdir");
        let runtime = runtime(temp.path(), "fts", None).await;
        let result = evaluate(&runtime, r#"
            const db = new sqlite.Database(':memory:');
            db.exec("CREATE VIRTUAL TABLE chunks USING fts5(title, content, tokenize='porter unicode61')");
            const insert = db.prepare('INSERT INTO chunks(title, content) VALUES (?, ?)');
            insert.run('SQLite', 'Transactions keep persistent session records consistent');
            insert.run('Unrelated', 'A completely different document');
            globalThis.sqliteResult = db.prepare('SELECT title FROM chunks WHERE chunks MATCH ? ORDER BY bm25(chunks)').all('transaction');
            db.close();
        "#).await;
        assert_eq!(result, json!([{"title": "SQLite"}]));
    });
}

#[test]
fn sqlite_readonly_memory_is_enforced_by_the_native_engine() {
    let _serial = SQLITE_TEST_LOCK.lock().expect("SQLite test lock");
    futures::executor::block_on(async {
        let temp = tempfile::tempdir().expect("tempdir");
        let runtime = runtime(temp.path(), "readonly", None).await;
        let result = evaluate(
            &runtime,
            r#"
            const db = new sqlite.Database(':memory:', {readonly: true, create: true});
            let writeDenied = false;
            try { db.prepare('CREATE TABLE forbidden(value TEXT)').run(); }
            catch { writeDenied = true; }
            globalThis.sqliteResult = {writeDenied, row: db.query('SELECT 42 AS value').get()};
            db.close();
        "#,
        )
        .await;
        assert_eq!(result, json!({"writeDenied": true, "row": {"value": 42}}));
    });
}

#[test]
#[allow(clippy::large_futures)]
fn sqlite_rejects_preexisting_schema_triggers() {
    let _serial = SQLITE_TEST_LOCK.lock().expect("SQLite test lock");
    futures::executor::block_on(async {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let storage = root.join("storage");
        std::fs::create_dir(&storage).expect("storage");
        let path = storage.join("preexisting.db");
        let seed_path = path.to_string_lossy().into_owned();
        // This host-created database deliberately bypasses guest SQL
        // admission to reproduce an unsafe schema present before opening.
        std::thread::Builder::new().name("sqlite-schema-fixture".into())
            .stack_size(16 * 1024 * 1024).spawn(move || {
                futures::executor::block_on(async {
                    let mut env = fsqlite::ConnectionEnv::default();
                    env.mark_blocking_io_inline_safe();
                    let conn = fsqlite::Connection::open_with_env(seed_path, env).await.expect("seed database");
                    conn.execute_batch("CREATE TABLE data(value TEXT); CREATE TABLE audit(value TEXT); CREATE TRIGGER changed AFTER INSERT ON data BEGIN INSERT INTO audit VALUES(new.value); END")
                        .await.expect("seed trigger");
                    conn.close().await.expect("close fixture");
                });
            }).expect("fixture worker").join().expect("fixture worker join");
        let runtime = runtime(&root, "owner", None).await;
        runtime
            .add_persistent_storage_root(storage, "owner")
            .expect("grant");
        let path = serde_json::to_string(&path.to_string_lossy()).unwrap();
        let error = evaluate(
            &runtime,
            &format!(
                r#"
            try {{ new sqlite.Database({path}); globalThis.sqliteResult = 'unexpected success'; }}
            catch (error) {{ globalThis.sqliteResult = error.message; }}
        "#
            ),
        )
        .await;
        assert!(error.as_str().unwrap().contains("triggers"), "{error}");
    });
}

#[cfg(unix)]
#[test]
fn sqlite_retires_a_handle_if_a_companion_becomes_a_symlink() {
    let _serial = SQLITE_TEST_LOCK.lock().expect("SQLite test lock");
    use std::os::unix::fs::symlink;
    futures::executor::block_on(async {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let storage = root.join("storage");
        std::fs::create_dir(&storage).expect("storage");
        let path = storage.join("live.db");
        let filename = serde_json::to_string(&path.to_string_lossy()).unwrap();
        let runtime = runtime(&root, "owner", None).await;
        runtime
            .add_persistent_storage_root(storage.clone(), "owner")
            .expect("grant");
        evaluate(
            &runtime,
            &format!(
                r#"
            globalThis.liveDatabase = new sqlite.Database({filename});
            liveDatabase.exec('CREATE TABLE data(value TEXT)');
            liveDatabase.prepare('INSERT INTO data VALUES (?)').run('durable before link');
            globalThis.sqliteResult = true;
        "#
            ),
        )
        .await;
        let outside = root.join("outside");
        std::fs::write(&outside, b"sentinel").expect("sentinel");
        // This Windows companion is also checked on Unix, where the native
        // engine has not created it. No database file needs to be removed.
        symlink(&outside, storage.join("live.db-lock-pending")).expect("late companion link");
        let result = evaluate(&runtime, r#"
            let readDenied = false;
            let reuseDenied = false;
            try { liveDatabase.query('SELECT * FROM data').all(); } catch { readDenied = true; }
            try { liveDatabase.exec("INSERT INTO data VALUES ('must fail')"); } catch { reuseDenied = true; }
            globalThis.sqliteResult = {readDenied, reuseDenied};
        "#).await;
        assert_eq!(result, json!({"readDenied": true, "reuseDenied": true}));
        drop(runtime);
        assert_eq!(std::fs::read(outside).unwrap(), b"sentinel");
    });
}
