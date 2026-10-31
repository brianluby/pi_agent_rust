// The public synchronous bun:sqlite subset. All authority, SQL validation and
// database ownership live in Rust; altering this facade cannot grant access.
const native = globalThis.__pi_sqlite_native;
if (typeof native !== "function") throw new Error("PiJS SQLite host is unavailable");

function request(value) {
  const result = JSON.parse(native(JSON.stringify(value)));
  if (typeof result.error === "string") {
    const error = new Error(result.error);
    error.name = "SQLiteError";
    error.code = result.error.includes("SQLITE_INTERRUPT") ? "SQLITE_INTERRUPT"
      : /database is (locked|busy)|SQLITE_BUSY/.test(result.error) ? "SQLITE_BUSY" : "SQLITE_ERROR";
    throw error;
  }
  return result.ok;
}

function encode(value) {
  if (value === null || typeof value === "string" || typeof value === "boolean") return value;
  if (typeof value === "number") {
    if (!Number.isFinite(value)) throw new TypeError("SQLite numbers must be finite");
    return value;
  }
  if (typeof value === "bigint") return { $sqliteInteger: String(value) };
  if (value instanceof ArrayBuffer || ArrayBuffer.isView(value)) {
    const bytes = value instanceof ArrayBuffer ? new Uint8Array(value)
      : new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
    if (bytes.byteLength > 1024 * 1024) throw new RangeError("SQLite blob exceeds 1 MiB");
    let text = "";
    for (let offset = 0; offset < bytes.length; offset += 8192) {
      text += String.fromCharCode(...bytes.subarray(offset, offset + 8192));
    }
    return { $sqliteBlob: btoa(text) };
  }
  throw new TypeError("Unsupported SQLite parameter type");
}

function bindings(args) {
  if (args.length === 1 && args[0] && typeof args[0] === "object"
      && !(args[0] instanceof ArrayBuffer) && !ArrayBuffer.isView(args[0])) {
    if (Array.isArray(args[0])) return args[0].map(encode);
    return Object.fromEntries(Object.entries(args[0]).map(([key, value]) => [key, encode(value)]));
  }
  return args.map(encode);
}

function decode(value) {
  if (value && typeof value === "object") {
    if (typeof value.$sqliteInteger === "string") return BigInt(value.$sqliteInteger);
    if (typeof value.$sqliteBlob === "string") {
      const text = atob(value.$sqliteBlob);
      return Uint8Array.from(text, ch => ch.charCodeAt(0));
    }
  }
  return value;
}

function record(columns, values) {
  // defineProperty handles SQL column names such as __proto__ as data keys.
  const result = {};
  columns.forEach((name, index) => Object.defineProperty(result, name, {
    value: decode(values[index]), enumerable: true, configurable: true, writable: true,
  }));
  return result;
}

const statementKey = Symbol("PiJS SQLite statement");
const executeKey = Symbol("PiJS SQLite execute");

export class Statement {
  #database;
  #sql;
  #columns;
  #parameters;
  #bound = [];
  #finalized = false;

  constructor(key, database, sql, metadata) {
    if (key !== statementKey) throw new TypeError("Use Database.prepare() to create a Statement");
    this.#database = database;
    this.#sql = sql;
    this.#columns = metadata.columns;
    this.#parameters = metadata.parameters;
  }

  #execute(mode, args) {
    if (this.#finalized) throw new Error("SQLite statement is finalized");
    const result = this.#database[executeKey](this.#sql, mode, args.length ? bindings(args) : this.#bound);
    if (result.columns) this.#columns = result.columns;
    return result;
  }

  all(...args) {
    const result = this.#execute("all", args);
    return result.rows.map(row => record(result.columns, row));
  }

  get(...args) {
    const result = this.#execute("get", args);
    return result.rows.length ? record(result.columns, result.rows[0]) : null;
  }

  values(...args) {
    return this.#execute("values", args).rows.map(row => row.map(decode));
  }

  run(...args) {
    const result = this.#execute("run", args);
    return { changes: result.changes, lastInsertRowid: decode(result.lastInsertRowid) };
  }

  iterate(...args) {
    // Materialization is bounded natively (10,000 rows / 8 MiB), and errors
    // remain errors. Never return a fabricated empty iterator on exhaustion.
    return this.all(...args)[Symbol.iterator]();
  }

  [Symbol.iterator]() { return this.iterate(); }
  bind(...args) {
    if (this.#finalized) throw new Error("SQLite statement is finalized");
    this.#bound = bindings(args);
    return this;
  }
  finalize() { this.#finalized = true; this.#bound = []; }
  get columnNames() { return this.#columns.slice(); }
  get paramsCount() { return this.#parameters; }
  toString() { return this.#finalized ? "" : this.#sql; }
}

export class Database {
  static MAX_QUERY_CACHE_SIZE = 20;
  #id;
  #filename;
  #closed = false;
  #queries = new Map();
  #transactionId = 0;

  constructor(filename = ":memory:", options = {}) {
    if (typeof filename !== "string") throw new TypeError("SQLite filename must be a string");
    if (!options || typeof options !== "object" || Array.isArray(options)) {
      throw new TypeError("SQLite options must be an object");
    }
    for (const key of Object.keys(options)) {
      if (!["readonly", "readwrite", "create", "strict", "safeIntegers"].includes(key)) {
        throw new TypeError(`Unsupported SQLite option: ${key}`);
      }
      if (options[key] !== undefined && typeof options[key] !== "boolean") {
        throw new TypeError(`SQLite option ${key} must be boolean`);
      }
    }
    if (options.readonly && options.readwrite) throw new TypeError("Conflicting SQLite access modes");
    if (options.readwrite === false && options.readonly !== true) {
      throw new TypeError("readwrite:false requires readonly:true in PiJS");
    }
    this.#filename = filename || ":memory:";
    this.#id = request({ op: "open", path: this.#filename,
      readonly: !!options.readonly, create: options.create !== false,
      strict: !!options.strict, safe_integers: !!options.safeIntegers }).id;
  }

  static open(...args) { return new Database(...args); }
  #check() { if (this.#closed) throw new Error("Database has closed"); }
  get filename() { return this.#filename; }
  get inTransaction() {
    this.#check();
    return request({ op: "in_transaction", id: this.#id });
  }

  [executeKey](sql, mode, values) {
    this.#check();
    return request({ op: "query", id: this.#id, sql, mode, bindings: values });
  }

  prepare(sql, ...args) {
    this.#check();
    if (typeof sql !== "string") throw new TypeError("SQLite SQL must be a string");
    const metadata = request({ op: "prepare", id: this.#id, sql });
    const statement = new Statement(statementKey, this, sql, metadata);
    return args.length ? statement.bind(...args) : statement;
  }

  query(sql) {
    this.#check();
    if (this.#queries.has(sql)) {
      const statement = this.#queries.get(sql);
      this.#queries.delete(sql);
      this.#queries.set(sql, statement);
      return statement;
    }
    const statement = this.prepare(sql);
    const limit = Math.max(0, Math.min(128, Number(Database.MAX_QUERY_CACHE_SIZE) || 0));
    if (limit) {
      this.#queries.set(sql, statement);
      while (this.#queries.size > limit) this.#queries.delete(this.#queries.keys().next().value);
    }
    return statement;
  }

  run(sql, ...args) {
    this.#check();
    if (typeof sql !== "string") throw new TypeError("SQLite SQL must be a string");
    if (args.length) return this.prepare(sql).run(...args);
    const result = request({ op: "exec", id: this.#id, sql });
    return { changes: result.changes, lastInsertRowid: decode(result.lastInsertRowid) };
  }

  exec(sql, ...args) { return this.run(sql, ...args); }

  transaction(fn) {
    this.#check();
    if (typeof fn !== "function") throw new TypeError("SQLite transaction requires a function");
    const database = this;
    function wrapper(mode) {
      return function (...args) {
        database.#check();
        const nested = database.inTransaction;
        const savepoint = `__pijs_tx_${++database.#transactionId}`;
        database.exec(nested ? `SAVEPOINT ${savepoint}` : `BEGIN ${mode}`);
        try {
          const result = fn.apply(this, args);
          if (result && typeof result.then === "function") {
            throw new TypeError("SQLite transactions must be synchronous");
          }
          database.exec(nested ? `RELEASE ${savepoint}` : "COMMIT");
          return result;
        } catch (error) {
          try {
            if (nested) {
              database.exec(`ROLLBACK TO ${savepoint}`);
              database.exec(`RELEASE ${savepoint}`);
            } else if (database.inTransaction) database.exec("ROLLBACK");
          } catch (rollbackError) {
            throw new Error(`SQLite transaction rollback failed: ${rollbackError.message}`, { cause: error });
          }
          throw error;
        }
      };
    }
    const transaction = wrapper("DEFERRED");
    transaction.deferred = transaction;
    transaction.immediate = wrapper("IMMEDIATE");
    transaction.exclusive = wrapper("EXCLUSIVE");
    return transaction;
  }

  close() {
    if (this.#closed) return;
    request({ op: "close", id: this.#id });
    this.#closed = true;
    this.#queries.clear();
  }

  loadExtension() { throw new Error("Native SQLite extensions are prohibited in PiJS"); }
}

if (typeof Symbol.dispose === "symbol") {
  Object.defineProperty(Database.prototype, Symbol.dispose, { value() { this.close(); } });
  Object.defineProperty(Statement.prototype, Symbol.dispose, { value() { this.finalize(); } });
}

export default Database;
