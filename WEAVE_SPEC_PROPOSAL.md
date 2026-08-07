## Unified Weave

`weave` is the single construct for task graphs, servers, and composable multithreaded processes. It replaces `task ... endt`, `wrun`, `srun`, and `server`.

### Task definitions

Tasks use `name:` — same label syntax as the rest of the language. Dependencies are `'name` references listed before the task name. Task bodies end at the next `name:` or `start`/`nostart`. Every task body must end with an explicit `ret` — missing `ret` before a boundary is a compile error.

```uf
weave
  a:
    ; no dependencies
    ...
    ret
  'a b:
    ; b depends on a; a's result is on the stack
    ...
    ret
  'a 16 c:
    ; c depends on a, 16 fanout workers
    ...
    ret
start
```

### DAG resolution — backward from the terminal node

`name: end` marks the terminal node — the entry point and return source. The runtime walks backwards from it to find what to run. Tasks not reachable from the terminal are **orphans** — they exist in scope, can be called by address, but don't auto-execute.

The parser distinguishes `name: end` (terminal marker, immediately after a label definition) from a bare `end` inside a body (shutdown signal) by position.

```uf
weave
  data:
    "input.csv" slurp "\n" split
    ret
  'data parse:
    ; transform lines into records
    ...
    ret
  'parse 'data summarize: end
    ; terminal: depends on parse and data
    ; result pushed to stack after start
    ...
    ret
start
```

Run order: `data:` → `parse:` → `summarize:`. Summary result on stack.

### Orphan tasks

```uf
weave
  setup: end
    ":8080" httplisten
    ret
  'setup 16 handle:
    dup "path" get routes@ getq 'fallback orelse call
    ret
  routes:                       ; orphan — not reachable from setup: end
    dict                        ; called by handle: at runtime
    ret
  fallback:                     ; orphan — default 404
    404 "not found" respond
    ret
start
```

`routes:` and `fallback:` never auto-run. `handle:` looks them up and calls them per request.

### `start` and `nostart`

`start` closes the block and runs it. For batch weaves it blocks until the DAG completes. For servers it blocks until `end` is called.

`nostart` closes the block and pushes the dispatcher as a first-class value — no execution.

```uf
weave
  square: end
    dup *
    ret
nostart sq!           ; save dispatcher, don't run
```

### `end` (bare) — graceful shutdown

Called inside any task body, signals the enclosing weave to drain and exit:

```uf
weave
  setup: end
    ":8080" httplisten
    ret
  'setup handle:
    dup "path" get "/shutdown" eq 'do_shutdown 'normal ifelse
    ret
  do_shutdown:
    drop
    end                       ; triggers shutdown
    ret
  normal:
    dup "path" get routes@ getq 'fallback orelse call
    ret
  routes:
    dict
    ret
  fallback:
    404 "not found" respond
    ret
start
```

### Inheritance

`parent@ weave ... start/nostart` — child overrides parent tasks by name. Everything not overridden is inherited.

Inside an overridden task, `'super` is a label-like reference that pushes a pointer to the parent's version of the current task onto the stack. You can `call` it immediately (`'super call`), or store it in a variable (`super!`) and pass it to another task or scope for deferred invocation. This allows wrapping or extending inherited behavior without copying it.

**Define the base (stdlib `http`):**

```uf
weave
  setup: end
    port@ httplisten
    ret
  'setup 16 handle:
    dup "path" get routes@ getq 'fallback orelse call
    ret
  routes:
    dict
    ret
  fallback:
    404 "not found" respond
    ret
nostart http!
```

**Extend with routes and handlers:**

```uf
8080 port!
http@ weave
  routes:
    dict r!
    "/health" 'health r@ set drop
    "/metrics" 'metrics r@ set drop
    r@
    ret
  health:
    200 "OK" respond
    ret
  metrics:
    200 get_data respond
    ret
start
```

The child inherits `setup:`, `handle:`, and `fallback:`. It overrides `routes:` and adds `health:` / `metrics:` as orphans that `handle:` dispatches to at runtime.

**Overriding with `super`:**

```uf
http@ weave
  handle:
    auth_check 0 eq 'deny if
    'super call                    ; call parent's handle after auth passes
    ret
  deny:
    401 "unauthorized" respond
    ret
nostart auth_http!

auth_http@ weave
  handle:
    rate_check 0 eq 'deny if
    'super call                    ; calls auth_http's handle → which calls http's handle
    ret
  deny:
    429 "rate limited" respond
    ret
  routes:
    {"/health" 'health "/api" 'api}
    ret
  health:
    200 "OK" respond
    ret
  api:
    200 get_data respond
    ret
start
```

`super` always refers to the immediate parent's implementation of the current task. Each level in the inheritance chain can call `'super call` to delegate upward, forming a natural middleware chain. Because `'super` pushes a pointer, it can be stored and passed to other scopes — e.g., saved to a variable and called from a helper task that lives outside the weave block.

### Middleware via callback handle

Cross-cutting behavior (auth, rate limiting, logging) is composed by passing a `next@` callback address into a gate task. No new language concepts — just `call` by address, which already works.

```uf
; reusable auth gate — validates token, calls next handler if valid
auth_gate:
  req@ "token" get validate_token 0 eq 'deny if
  next@ call                        ; token valid → call the next handler
  ret
deny:
  401 "unauthorized" respond
  ret
```

Usage — wrap the real dispatcher:

```uf
http@ weave
  handle:
    'real_handler next!             ; set the callback before calling the gate
    auth_gate                       ; auth_gate calls next@ on success
    ret
  real_handler:
    dup "path" get routes@ getq 'fallback orelse call
    ret
nostart secure_http!
```

Chains naturally — each gate calls `next@`, which can be another gate:

```uf
http@ weave
  handle:
    'final next!
    rate_gate                        ; rate_gate calls next@ → final
    ret
  final:
    'real_handler next!
    auth_gate                        ; auth_gate calls next@ → real_handler
    ret
  real_handler:
    dup "path" get routes@ getq 'fallback orelse call
    ret
nostart full_http!
```

Just functions calling functions through a `next@` convention. Pure postfix composition — no `super`, no magic, no new ops.

### Timer / state machine

```uf
weave
  setup: end
    0 counter!
    60 timertick
    ret
  'setup tick:
    counter@ 1 + counter!
    counter@ 100 eq 'end if
    ret
start
```

`setup:` opens the timer channel. `tick:` drains it one tick at a time. After 100 ticks, `end` shuts down the weave.

### Batch fanout

```uf
weave
  pages:
    "urls.txt" slurp "\n" split
    ret
  'pages 16 fetch: end
    curlget
    ret
start
```

16 workers drain the `pages` list, each calling `curlget` on one URL. Results collected in completion order and pushed to stack.

---

## Stdlib weaves

### Event source dispatchers

Three pre-built weaves that ship with the stdlib. They have no runtime privileges — they are written in µFlux itself and saved weaves you inherit from. The only C-level ops they depend on are the thin I/O primitives (`httplisten`, `timertick`, `fswatch`, `respond`, etc.). Everything else — routing, dispatch loops, defaults — is pure µFlux task code.

**`http`** — HTTP request listener. Opens a socket, produces request events, routes by path. Useful for webhooks, API endpoints, health checks, and agents that need to be callable by external systems.

**`timer`** — Interval tick producer. Emits events on a fixed cadence. Useful for polling, scheduled tasks, delayed execution, and periodic system checks.

**`fswatch`** — Filesystem change monitor. Emits events when files or directories change. Useful for log tailing, config reload triggers, and reacting to system state changes.

### Mixin tasks

Reusable tasks that wrap a handler via the callback-handle pattern (`next@ call`). They are pure µFlux — no C dependencies beyond what's already in the language. They work with any of the above dispatchers.

**HTTP-oriented:**
- **authenticate** — validate token/API key before dispatching to routes
- **ratelimit** — token bucket per IP or global, reject with 429
- **validate** — parse and check JSON body structure before route dispatch
- **cors** — inject CORS headers, handle OPTIONS preflight
- **timeout** — per-request deadline, return 504 if exceeded

**Timer-oriented:**
- **debounce** — ignore ticks that fire too close together
- **jitter** — randomize tick offset to avoid thundering herd
- **backoff** — increase interval when downstream fails, reset on success

**fswatch-oriented:**
- **filter** — pass only events matching a glob pattern
- **coalesce** — batch rapid event sequences into one
- **ignore** — skip events in `.git/`, temp dirs

**Cross-cutting (all sources):**
- **lock** — skip event if previous run is still active
- **guard** — only fire if a condition is met (flag set, file exists, queue non-empty)
- **log** — structured event logging with timestamp and context
- **audit** — append every event to a log file
- **retry** — re-run handler N times with backoff before giving up
- **catch** — catch handler failures, return error instead of crashing
- **circuit** — stop dispatching after N consecutive failures, probe recovery
- **trace** — assign correlation ID, propagate to downstream calls

