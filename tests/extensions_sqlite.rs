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
