# Extension Runtime Compatibility Matrix

This document describes the Node.js and Bun API surface available to Pi
extensions running in the PiJS (QuickJS-based) runtime. Extension authors can
use this matrix to determine whether their extension will run unmodified.

---

## Conformance Snapshot

These are the existing corpus results, not a new conformance run for the
context-mode compatibility changes described below. Adding a module or passing
a focused fixture does not establish compatibility with every npm dependency.

| Surface | Result | Details |
|---|---:|---|
| Extension corpus (223 extensions) | 205/223 pass (91.9%) | 100% Tier 1, 95.4% Tier 2 |
| Scenario conformance | 24/25 pass (96.0%) | Registration, events, tools, session |
| Node API matrix | 13/13 pass (100%) | All critical Node builtins covered |
| Bun API matrix | 7/7 pass (stubbed) | `connect`/`listen` stubbed (no network I/O) |

---

## 1. Compatibility Tiers

| Tier | Description | Corpus coverage | Policy |
|------|---------|---:|--------|
| T1 | Simple single-file extensions | 38/38 (100%) | Run silently |
| T2 | Multi-registration (tools + events + flags) | 83/87 (95.4%) | Run silently |
| T3 | Multi-file with local imports | 79/90 (87.8%) | Run + warn if relative imports fail |
| T4 | Extensions with npm dependencies | 1/3 (33.3%) | Resolution stays within allowed roots; compatibility depends on required APIs |
| T5 | Extensions using exec/network APIs | 4/5 (80.0%) | Capability-gated |

---

## 2. Node.js Builtin Modules

### Fully Supported

These modules are shimmed in PiJS and behave like their Node.js counterparts
for the API subset that extensions actually use.

| Module | Key APIs | Test coverage | Notes |
|--------|----------|---:|-------|
| `node:path` | `join`, `resolve`, `dirname`, `basename`, `extname`, `sep`, `posix`, `win32` | Full | Native Windows or POSIX behavior; both explicit namespaces available on every host |
| `node:fs` | `readFileSync`, `writeFileSync`, `statSync`, `mkdirSync`, `readdirSync`, `unlinkSync`, `rmSync`, `copyFileSync`, `renameSync`, `appendFileSync`, `accessSync`, `existsSync`, `realpathSync`, `readlinkSync`, `createReadStream`, `createWriteStream`, `chmodSync`, `chownSync` | 39 tests | Rooted to extension dir by default; permission changes are path-checking no-ops |
| `node:fs/promises` | `readFile`, `writeFile`, `stat`, `mkdir`, `readdir`, `unlink`, `rm`, `access`, `copyFile`, `rename`, `chmod`, `chown`, `utimes` | Included above | Async versions of fs shim; permission changes are path-checking no-ops |
| `node:crypto` | `createHash`, `createHmac`, `randomUUID`, `randomBytes`, `randomInt`, `timingSafeEqual`, `getHashes`, Ed25519 `sign`/`verify` | 56 tests | SHA-256/SHA-384/SHA-512, SHA-1, MD5, HMAC, KDFs; ciphers and RSA/ECDSA fail closed |
| `node:buffer` | `Buffer.from`, `Buffer.alloc`, `Buffer.concat`, `Buffer.isBuffer`, `Buffer.byteLength`, `.toString()`, `.slice()`, `.subarray()`, `.compare()`, `.equals()`, `.indexOf()`, `.copy()` | 41 tests | Full Buffer protocol |
| `node:child_process` | `spawnSync`, `execSync`, `execFileSync`, `spawn`, `exec`, `execFile` | Shim and persistent-session regressions | Capability-gated (`exec`); bounded piped sessions support interactive protocols |
| `node:http` | `request`, `get`, `createServer`, `STATUS_CODES`, `METHODS`, `Agent` | HTTP shim and virtual server regressions | Outbound requests use the HTTP capability; explicit loopback servers are private in-memory routes, never OS listeners |
| `node:https` | `request`, `get` | Shared with http | Same as `node:http` |
| `node:events` | `EventEmitter`, `on`, `emit`, `once`, `removeListener`, `removeAllListeners`, `listenerCount` | 26 tests | Full EventEmitter pattern |
| `node:os` | `platform`, `hostname`, `tmpdir`, `homedir`, `cpus`, `arch`, `type`, `release`, `userInfo`, `EOL` | Included | Returns host values |
| `node:url` | `URL`, `URLSearchParams`, `parse`, `format`, `resolve` | URL shim regressions | URL construction and base resolution use the native URL parser; the mutable JS surface remains bounded |
| `node:process` | `env`, `argv`, `cwd`, `exit`, `platform`, `arch`, `version`, `pid`, `hrtime` | 24 tests | `exit` is sandboxed |
| `node:util` | `format`, `inspect`, `inherits`, `deprecate`, `debuglog`, `types`, `TextEncoder`, `TextDecoder`, `stripVTControlCharacters` | Included | Standard utility functions |
| `node:stream` | `Readable`, `Writable`, `Transform`, `Duplex`, `PassThrough`, `pipeline`, `finished` | Included | Stream constructors + helpers |
| `node:stream/promises` | `pipeline`, `finished` | Included | Promise-based stream helpers |
| `node:querystring` | `parse`, `stringify`, `encode`, `decode` | Included | Query string utilities |
| `node:assert`, `node:assert/strict` | `ok`, `strictEqual`, `deepStrictEqual`, `throws`, `rejects`, `fail` | Included | Test assertion helpers |
| `node:string_decoder` | `StringDecoder` | Included | UTF-8 string decoding |
| `node:module` | `createRequire` | Included | Module system compat |

### Partially Supported

These modules expose a subset of their Node.js API. Missing functions throw
with a clear error message identifying the unsupported call.

| Module | Supported | Unsupported | Notes |
|--------|-----------|-------------|-------|
| `node:net` | `createConnection` (stub), `Socket` (stub) | `createServer` | Stubbed sockets (no network I/O); use `pi.http()` |
| `node:readline` | `createInterface`, `promises.createInterface` | Full interactive readline | Uses `pi.ui('input')` when available; non-interactive prompts resolve to empty strings |

### Blocked

These modules are blocked because they require capabilities outside the
extension sandbox.

| Module | Reason | Alternative |
|--------|--------|-------------|
| `vm` | Arbitrary code execution | Use extension API directly |
| `worker_threads` | Thread creation | Single-threaded runtime |
| `cluster` | Process forking | Single-process runtime |
| `dgram` | Raw UDP sockets | Use `pi.http()` for network |
| `tls` | Raw TLS sockets | Use `node:https` instead |

---

## 3. Bun APIs

Pi provides a targeted Bun compatibility surface via `globalThis.Bun` and
`import "bun"`.

| API | Status | Notes |
|-----|--------|-------|
| `Bun.argv` | Supported | Process arguments |
| `Bun.file(path)` | Supported | Returns object with `exists()`, `text()`, `arrayBuffer()`, `json()` |
| `Bun.write(path, data)` | Supported | Write files |
| `Bun.which(command)` | Supported | Metadata-only lookup in the runtime's configured PATH; requires `exec` permission |
| `Bun.spawn(...)` | Supported | Capability-gated (`exec`) |
| `bun:sqlite` | Supported subset | Real FrankenSQLite databases; ESM and `createRequire()` return the same `Database` constructor |
| `Bun.connect(...)` | Stubbed (no network) | In-memory socket emitter; use `pi.http()` or `node:http` for real network |
| `Bun.listen(...)` | Stubbed (no network) | In-memory server emitter; use `pi.http()` or `node:http` for real network |

### SQLite databases

`import { Database } from "bun:sqlite"` and
`createRequire(import.meta.url)("bun:sqlite").Database` use the repository's
existing FrankenSQLite engine. Supported operations include schema creation
and migration, parameterized statements, `run`, `get`, `all`, `values`,
`iterate`, nested synchronous transactions, WAL pragmas, and close/reopen.
`prepare()` parses schema and transaction statements using the engine's SQL
parser; their semantic checks happen when executed because the engine does not
compile those statements into prepared programs. Query and DML statements use
the engine's prepared programs.

`INSERT`, `UPDATE`, and `DELETE ... RETURNING` produce real result rows,
including the resume-state updates used by context-mode. A bridge row or
encoding-limit failure rolls back that statement's changes while preserving
earlier work in an outer transaction. Native constraint failures retain
SQLite's `ABORT`, `FAIL`, and `ROLLBACK` semantics.

File-backed databases require an extension-owned persistent storage grant and
the corresponding read/write capabilities. `:memory:` databases acquire no
filesystem permission. Database handles cannot be transferred to another
extension. SQL batches are admitted in full before their first statement runs;
SQL cannot acquire additional paths through `ATTACH`, `VACUUM INTO`, loadable
extensions, filesystem functions, arbitrary virtual-table modules, or writable
schema/directory pragmas. FTS5 is the only admitted virtual-table module.
Triggers and allocating scalar functions such as `randomblob`, `zeroblob`,
`printf`, and `format` are unsupported. Unsupported APIs and pragmas throw errors. In
particular, the optional `mmap_size` optimization used by context-mode is not
implemented.

The bridge bounds database handles, SQL/request size, returned rows and bytes,
page cache, and workers. Each engine operation has a cooperative cancellation
deadline; a timed-out worker is retired and never reused. These limits do not
provide an OS memory sandbox for every SQL intermediate allocation. The native
database-family size limit is checked before and after operations, so it is
not a hard per-write disk quota.

Database files and the engine's companion files are checked for symlinks,
reparse points, and hard links, with pinned parent identities rechecked around
operations. Guest filesystem APIs cannot create native links in a storage
grant. FrankenSQLite's native VFS still opens pathnames: on Unix, this does not
isolate the database from a separate native process with the same OS user
identity concurrently replacing directories. Applications requiring that
stronger boundary need an OS sandbox or a descriptor-based engine VFS.

### Persistent subprocess protocols

`spawn()` supports real piped stdin/stdout/stderr for protocols such as MCP,
including writes queued before admission, write callbacks, backpressure, EOF,
and process exit. Creation goes through the existing `exec` policy, command
mediation, and admission accounting. Native callbacks operate only on opaque,
extension-owned handles; they cannot launch another process or signal an
arbitrary PID. Input, output, session counts, environment size, and lifetime
are bounded. Shutdown retires the owned process group and retains admission
accounting until cleanup finishes.

The runtime recognizes only the exact conventional executable probes
`command -v NAME` on POSIX and `where NAME` on Windows. It answers them using
file metadata and the configured PATH/PATHEXT, without running a shell.
General synchronous process execution retains its existing opt-in requirement.
Child environments are explicit and filtered for secrets and loader injection.
Spreading `process.env` copies only the owner-permitted environment snapshot,
in stable key order. Loader hooks (such as `NODE_OPTIONS` and `LD_PRELOAD`) and
invalid child bindings are omitted from enumeration, while individual reads
remain subject to environment disclosure policy. A secret disclosure allowlist
permits individual reads without enabling implicit secret forwarding; custom
secret rules also filter the inherited child snapshot. Explicitly supplying unsafe
child bindings is rejected. A global or per-extension environment denial
exposes no snapshot keys or values. Pi
advertises its configuration directory through `PI_CONFIG_DIR` metadata while
preserving an explicitly configured value. This lets child integrations
identify their host without receiving read access to a parent data directory.
Context-mode's MCP server still needs a real Node or Bun executable on PATH.
Its process launch and environment snapshot require the existing `exec` and
`env` permissions; installing a package or granting storage does not enable
those capabilities. See the [extension security operator handbook](security/operator-handbook.md#dangerous-capability-opt-in)
for the existing opt-in and policy precedence rules.
An allowed external process runs with the host user's OS authority, as with
the existing `exec` capability; the extension's data-directory grants do not
constitute a sandbox for external executables.

---

## 4. Virtual npm Module Stubs

Extensions that import popular npm packages get virtual stubs that expose the
package's public API shape. These stubs allow extensions to load and register
without runtime errors, even when the real package is not installed.

### Pi Framework Modules

| Package | Key exports |
|---------|-------------|
| `@mariozechner/pi-coding-agent` | `ExtensionAPI`, `Tool`, `SlashCommand`, `EventHook` |
| `@mariozechner/pi-ai` | `AI`, `Message`, `StreamEvent` |
| `@mariozechner/pi-tui` | `TUI`, `Widget`, `Layout` |
| `@sinclair/typebox` | `Type`, `Static`, `TSchema` |

### Protocol and Framework Modules

| Package | Key exports |
|---------|-------------|
| `@modelcontextprotocol/sdk/*` | MCP client/server/transport types |
| `vscode-languageserver-protocol/*` | LSP types and protocol definitions |
| `jsonwebtoken` | `decode`, HS256/HS384/HS512 `sign`/`verify` |
| `uuid` | `v4`, `v5`, `v7`, `NIL` |
| `dotenv` | `config`, `parse` |
| `shell-quote` | `parse`, `quote` |
| `ms` | Duration parsing |
| `diff` | `diffChars`, `diffLines`, `createPatch` |
| `glob` | `glob`, `globSync` |
| `undici` | `fetch`, `Headers`, `Response`; explicit unsupported `Agent` constructor |

`jsonwebtoken` support is intentionally bounded to HMAC JWTs. RSA/ECDSA
algorithms and unsupported verification options fail closed with explicit
diagnostics instead of silently accepting tokens.

### pi-freeflow 1.39.0 and optional Undici dispatchers

The current pi-freeflow release imports `Agent` from `undici`, even though
connection pooling is optional. PiJS registers a narrow `undici` compatibility
module so this import resolves. Its fetch exports use the same capability-gated
transport as global `fetch`. PiJS does not advertise `process.versions.undici`:
Freeflow's version check therefore selects its built-in-fetch fallback.
Constructing an `Agent` or passing a custom fetch dispatcher fails explicitly;
there is no npm package loading, custom socket transport, or alternate TLS
authority behind this module.

Import resolution alone is insufficient for this extension. Freeflow also
starts a local proxy and registers an OpenAI-compatible provider against it.
PiJS implements `http.createServer(listener).listen(port, host)` as a private
in-memory route when `host` is explicitly `127.0.0.1`, `localhost`, or `::1`.
Routes belong to the extension that registered them. A provider registered by
that same extension can use its route through the existing native provider
request/response codecs; no real port is bound and other extensions cannot
intercept the route. Unsupported listen forms and non-loopback hosts fail.
Freeflow's detached daemon spawn options remain unsupported, allowing its
existing in-process fallback to run.

The provider keeps its originally granted extension/origin binding when that
extension closes and replaces the listener on the same port. Each new request
resolves the current owned listener; a provider that had no route when it was
registered cannot acquire one later without registration. URL construction
resolves root-relative health and control paths against the origin, allowing
Freeflow's health, client attachment, shutdown, and recovery operations to use
their intended endpoints.

Provider-level and model-level `compat` settings survive registration and merge
with model settings taking precedence. A model's top-level `thinkingLevelMap`
overrides the compatibility map. String values remap effort names; explicit
`null` values disable those tiers, while omitted keys retain existing defaults.
The native model picker and request serializers honor that distinction.
OpenAI-compatible providers may explicitly enable the plain `reasoning_effort`
field with `supportsReasoningEffort: true`; declared OpenRouter formats retain
their `reasoning` object. Undeclared providers keep their existing request shape.

Outbound fetch and Node HTTP calls still require the extension's HTTP
capability and the connector's TLS/host policy. Responses expose headers before
streaming body bytes; reader cancellation, request destruction, abort signals,
header deadlines, body idle timeouts, and runtime task deadlines retain owned
transport cleanup. `AbortSignal.any()` composes caller and timeout signals.
`Readable.fromWeb()` and `StringDecoder` preserve byte and split UTF-8 behavior.
JavaScript tasks awaiting deferred HTTP work yield to the native executor so
socket readiness, other tasks, and timers can progress within the root deadline.
Limits include 16 virtual servers and 64 active virtual requests per realm,
1 MiB of queued virtual response data, a 10 MiB virtual request body, and the
configured connector response limit. Custom TLS/socket overrides and streaming
fetch uploads are rejected rather than acquiring additional authority.

Regression coverage is in `tests/node_http_shim.rs`,
`tests/virtual_http_server.rs`, and the provider/runtime unit tests. The optional
`scripts/freeflow_compat_probe.mjs` reads the actual embedded JavaScript and
loads a pinned, clean upstream checkout at
`c4370df7fd4d186d00a33e4fc177ea340abf08d3` with controlled HTTP responses.
It covers the import, dispatcher fallback, activation, model metadata, relay
behavior, proxy streaming and recovery, health/control requests, and
Windows/POSIX path projections. The script documents setup and
usage. These V8-based diagnostics do not replace QuickJS/Rust execution or a
native Windows run; use `dsr quality --tool pi_agent_rust` for the authoritative
repository gate.

### Runtime API Compatibility Modules

| Package | Key exports |
|---------|-------------|
| `openai` | `OpenAI`, default `OpenAI`, `chat.completions.create` |
| `adm-zip` | default `AdmZip`, `getEntries`, `readAsText`, `extractAllTo`, `addFile`, `writeZip` |
| `linkedom` | `parseHTML` with document/window shape used by corpus extensions |
| `@sourcegraph/scip-typescript` | `scip.Index`, default `{ scip }` |
| `@sourcegraph/scip-typescript/dist/src/scip.js` | `scip.Index`, default `{ scip }` |
| `@sourcegraph/scip-typescript/dist/src/main.js` | `main`, `run`, default `main` |

### Terminal and UI Modules

| Package | Key exports |
|---------|-------------|
| `node-pty` | `spawn` (returns PTY stub) |
| `chokidar` | `watch` (returns watcher stub) |
| `@xterm/headless` | `Terminal` |
| `@xterm/addon-serialize` | `SerializeAddon` |
| `turndown` | `TurndownService` |
| `turndown-plugin-gfm` | `gfm`, `tables`, `strikethrough` |
| `@mozilla/readability` | `Readability`, `isProbablyReaderable` |
| `beautiful-mermaid` | `render` |
| `jsdom` | `JSDOM` |

### Observability Modules

| Package | Key exports |
|---------|-------------|
| `@opentelemetry/api` | `trace`, `context`, `propagation`, `SpanStatusCode` |
| `@opentelemetry/sdk-trace-base` | `BasicTracerProvider`, `SimpleSpanProcessor` |
| `@opentelemetry/resources` | `Resource` |
| `@opentelemetry/exporter-trace-otlp-http` | `OTLPTraceExporter` |
| `@opentelemetry/semantic-conventions` | `SEMRESATTRS_*` constants |

---

## 5. Extension API Surface (Pi Protocol)

The core Pi extension API is fully supported. This is the primary API that
extensions use.

### Registration

```javascript
export default function activate(pi) {
  // Register tools
  pi.tool({ name: "my-tool", description: "...", schema: {}, run: async (input) => { ... } });

  // Register slash commands
  pi.slashCommand({ name: "/my-cmd", description: "...", run: async (args) => { ... } });

  // Register event hooks
  pi.on("onMessage", async (event) => { ... });
  pi.on("onToolResult", async (event) => { ... });

  // Register flags
  pi.flag({ name: "my-flag", description: "...", default: false });

  // Register shortcuts
  pi.shortcut({ name: "my-shortcut", key: "ctrl+k", run: async () => { ... } });

  // Register providers
  pi.registerProvider({ name: "my-provider", models: [...], streamSimple: async (model, context) => { ... } });
}
```

### Session and State APIs

| API | Description |
|-----|-------------|
| `pi.session.getState()` | Get current session state |
| `pi.session.getMessages()` | Get conversation messages |
| `pi.session.getName()` / `setName()` | Session name |
| `pi.session.getModel()` / `setModel()` | Active model |
| `pi.session.setLabel(key, value)` | Set status line labels |
| `pi.session.getThinkingLevel()` / `setThinkingLevel()` | Thinking mode |
| `pi.events(op, payload)` | Dispatch lifecycle events |

### Host Tools

| API | Description |
|-----|-------------|
| `pi.tool(name, input)` | Call built-in tools (read/write/edit/bash/grep/glob/ls) |
| `pi.exec(command, args, options)` | Run commands (capability-gated) |
| `pi.http(request)` | HTTP client (policy-controlled) |
| `pi.log({level, event, message})` | Structured logging |

---

## 6. Capability Policies

Extension access to sensitive APIs is governed by capability policies.

| Policy | `exec` | `http` | `fs` (outside root) | `env` |
|--------|--------|--------|---------------------|-------|
| `safe` | Denied | Denied | Denied | Denied |
| `balanced` (default) | Allowed | Allowed | Warned | Allowed |
| `permissive` | Allowed | Allowed | Allowed | Allowed |

Use `pi doctor <path> --policy <profile>` to check extension compatibility
under a specific policy.

### Persistent application state

Ordinary extension writes retain their existing virtual-filesystem behavior.
Persistent data directories are separate host-assigned capabilities and are
never added to the JavaScript module resolver's search roots.

For a verified installed npm extension, the loader can assign
`~/.pi/<package-name>` (or `~/.pi/@scope/<package-name>`). Verification requires
the installed `node_modules` path, manifest package name, and the declared
`pi.extensions` entrypoint to agree. Thus `npm:context-mode` can create and
reopen `~/.pi/context-mode/sessions` without receiving access to all of HOME or
all of `.pi`. Pi's own configuration/authentication directories and unsafe
namespace components are reserved. Ambiguous package origins or shared legacy
extension IDs suppress automatic grants and emit diagnostics; unrelated
extensions retain their own grants. Invalid or linked automatic roots remain
deny-only boundaries.

Each owner's storage paths are resolved once using that owner's runtime
configuration. Every realm receives the same protected path boundaries before
extension factories run, including when environment permissions differ or a
configured home directory lies beneath the workspace.

The host embedding API `PiJsRuntime::add_persistent_storage_root` can assign an
explicit absolute directory to one extension. Guest code, package metadata,
and environment changes cannot add arbitrary paths. A custom
`CONTEXT_MODE_DATA_DIR` outside the assigned namespace therefore requires a
corresponding host grant.

Persistent reads and mutations check ownership and read/write policy. Mutations
use no-follow directory handles on Unix and pinned, reparse-checked directory
handles on Windows. Traversal, linked targets, Windows device paths/names,
alternate data streams, and overlapping grants to different owners are
rejected. Checks inspect original inputs and virtual symlink targets before
normalization can discard forbidden components. These directories support
durable mkdir/mkdtemp, file reads/writes/appends, copy, rename, unlink, and
empty-directory removal. Read-only descriptors belong to the extension that
opened them and can still be closed by that owner after a path is revoked.
`realpath` and `realpathSync.native` resolve authorized existing host paths,
including project aliases, with a fallback for genuine virtual files.
Writable descriptors,
symlink creation, recursive directory deletion, and cross-filesystem rename
are explicitly unsupported within a persistent grant.

---

## 7. Preflight Analysis

The `pi doctor` command performs static analysis on extensions before loading:

```bash
# Text output (default)
pi doctor /path/to/extension

# JSON for automation
pi doctor /path/to/extension --format json

# Markdown for documentation
pi doctor /path/to/extension --format markdown

# Check against specific policy
pi doctor /path/to/extension --policy safe
```

The report includes:
- **Verdict**: PASS / WARN / FAIL
- **Confidence score**: 0-100 numeric rating
- **Risk banner**: Human-readable summary
- **Findings**: Per-issue category, severity, message, and line number

---

## 8. Known Limitations

### Module Resolution

- **Bare package specifiers** (`import foo from "some-package"`) must resolve
  to an installed package within the allowed extension roots or a registered
  virtual module. Resolution does not supply native addons or complete Node/Bun
  API compatibility.
- **Relative imports** (`import ./utils`) work within bundled extensions but may
  fail for multi-file extensions that were not bundled.
- **Network imports** (`import "https://..."`) are rejected.

### Runtime Constraints

- **JavaScript execution**: No guest `worker_threads`; bounded native workers
  handle SQLite and subprocess I/O.
- **No native addons**: C/C++ Node addons cannot be loaded. Use hostcalls or
  WASM instead.
- **Sandbox boundary**: `createServer`, `listen`, and other server-side APIs are
  blocked. Extensions are clients, not servers.
- **Filesystem scope**: Extension/workspace access retains its existing policy.
  Persistent application namespaces are separate, owner-scoped data grants;
  they do not authorize arbitrary HOME access or native addon loading.

### Remaining Failure Buckets (18/223)

| Category | Count | Root cause |
|----------|------:|------------|
| Multi-file relative specifiers | 4 | Unbundled multi-file extensions |
| Package module specifiers | 5 | npm packages without virtual stubs |
| Host-read policy denials | 4 | Read access outside extension root |
| Runtime shape/load errors | 4 | Extension structure mismatch |
| Test fixture artifacts | 1 | Not a real extension |

---

## 9. Verifying Compatibility

### For Extension Authors

```bash
# Check your extension
pi doctor /path/to/your-extension

# Check with strict policy
pi doctor /path/to/your-extension --policy safe

# View supported policy modes
pi --explain-extension-policy
```

### For CI Integration

```bash
# JSON output for automated checks
pi doctor /path/to/extension --format json | jq '.verdict'

# Exit code is always 0 (verdict is in output, not exit code)
# Parse the JSON verdict field for pass/fail decisions
```

---

## 10. Reporting Issues

If your extension fails to load and you believe it should be compatible:

1. Run `pi doctor /path/to/extension --format json` and capture the output.
2. Check the findings array for specific error messages.
3. If the failure is a missing module stub or shim gap, it may be eligible for
   a fix in the PiJS runtime.
