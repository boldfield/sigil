# surl stdlib requirements — design

Status: draft (requirements/design for the agentask board)
Date: 2026-06-20
Target: networking primitives in the `sigil` repo (a `Net` builtin
effect + `std.net` / `std.url` / `std.http`), consumed by `surl` (a
curl-style HTTP client) in `sigil-programs`.

## Goal

Write **surl** — a small curl-style HTTP client — in Sigil. surl does
`surl <url>` → HTTP(S) GET → print the response body, growing toward
curl's common flags. Sigil has **no networking** today (no sockets, TLS,
or DNS — `language.md §13` lists `Net` as out of v1 scope), so surl's
requirements bottom out in a new runtime effect, not just stdlib modules.

**Decisions (settled):** TLS/https is in the first cut; HTTP and URL
parsing live in the **stdlib** (`std.http`, `std.url`), not surl-local.

## The requirements stack (build bottom-up)

1. **`Net` builtin effect** — runtime + compiler (the foundation).
2. **`std/net.sigil`** — Result-typed wrapper over `Net`.
3. **`std/url.sigil`** — URL parsing.
4. **`std/http.sigil`** — request serialization + response parsing.
5. **surl** — the CLI program (in `sigil-programs`).

Layer 1 is the most involved — a new builtin effect on the CPS arm-fn
ABI, runtime socket + TLS lifecycle, and the standard arm-fn GC discipline
for the `ByteArray`/tuple returns. But it is **not** a monolith: it clones
the existing `Process` effect end-to-end (runtime arm fn → handler
registration → typecheck `builtin_effects` table → codegen frame → `std`
wrapper → link — the same ~10-touchpoint template `std.process` used), and
that path decomposes into single-logical-unit increments (one arm at a
time, registration, codegen frame, wrapper, each test). Every task —
including Layer 1's — is cut for `haiku` (see decomposition below).

## Layer 1 — the `Net` builtin effect

**Handle model:** `connect` returns an opaque integer connection id; the
runtime holds the live socket/TLS state in a `Mutex`'d registry keyed by
that id (`HashMap<i64, Conn>`, `Conn = Plain(TcpStream) |
Tls(Box<rustls::ClientConnection>, TcpStream)`). `send`/`recv`/`close`
pass the id back. This keeps all OS/TLS resources Rust-side; the Sigil
value is just an `Int`. Caveat: a connection leaks if `close` is never
called (no GC finalizer — acceptable for v1, documented).

**Arms** (raw runtime tuple-return ABI, mirroring `Process.run`):

- `Net.connect(host: String, port: Int, tls: Bool) -> (Int, Int, String)`
  → `(error_tag, conn_id, error_msg)`. Resolves `host` (DNS via Rust
  `ToSocketAddrs`), opens TCP, and if `tls` performs the rustls
  handshake with webpki-roots cert verification.
- `Net.send(conn_id: Int, data: ByteArray) -> (Int, Int, String)`
  → `(error_tag, bytes_written, error_msg)` (encrypts through rustls for
  the `Tls` variant).
- `Net.recv(conn_id: Int, max: Int) -> (Int, ByteArray, String)`
  → `(error_tag, data, error_msg)`; `error_tag == 0` with an empty
  `data` means EOF.
- `Net.close(conn_id: Int) -> (Int, String)` → `(error_tag, error_msg)`;
  closes the socket and drops the registry entry.

**`error_tag` mapping:** 0 ok · 1 ResolveFailed · 2 ConnectionRefused ·
3 TlsError · 4 BadHandle/Closed · 5 Other.

**TLS:** `rustls` + `webpki-roots` (and `rustls-pemfile` only if custom
CAs are added later). Blocking sync I/O (matches `Process.run`'s
`Command::output()` model; no async). This is the single heaviest
requirement.

## Layer 2 — `std/net.sigil`

Wraps the raw arms into a Result API (the `std.process` pattern):

```
type Conn = Conn(Int)                       // opaque conn id
type NetError = ResolveFailed | ConnectionRefused | TlsError(String)
             | Closed | Other(String)

fn connect(host: String, port: Int, tls: Bool) -> Result[Conn, NetError] ![Net]
fn send(c: Conn, data: ByteArray)            -> Result[Int, NetError]  ![Net]
fn recv(c: Conn, max: Int)                   -> Result[ByteArray, NetError] ![Net]  // empty = EOF
fn close(c: Conn)                            -> Result[Unit, NetError] ![Net]
fn recv_all(c: Conn)                         -> Result[ByteArray, NetError] ![Net]  // loop recv to EOF
```

## Layer 3 — `std/url.sigil`

```
type Url = { scheme: String, host: String, port: Int, path: String, query: String }

fn parse_url(s: String) -> Result[Url, String] ![]
  // scheme://host[:port][/path][?query]
  // default port: 80 (http) / 443 (https); default path "/"; empty query allowed
```

Pure string work (`string_split`, `string_substring_opt`). No effects.

## Layer 4 — `std/http.sigil`

```
type Header   = (String, String)
type Request  = { method: String, url: Url, headers: List[Header], body: ByteArray }
type Response = { status: Int, reason: String, headers: List[Header], body: ByteArray }

fn get(url: Url, headers: List[Header]) -> Request          // method=GET, no body
fn serialize_request(req: Request) -> ByteArray ![Mem]
  // request-line + headers (auto Host, auto Content-Length if body) + CRLF CRLF + body
fn parse_response(bytes: ByteArray) -> Result[Response, String] ![Mem]
  // status line, headers, then body by Content-Length OR chunked transfer-decoding
```

Pure byte/string work over `byte_array_*` + `string_*`. Chunked decoding
is in the first cut (real servers use it).

## Layer 5 — surl (in `sigil-programs`)

```
surl [-X METHOD] [-H 'K: V']… [-d DATA] [-o FILE] [-I] [-L] <url>
  parse_url(url) → http.get/build request → net.connect(host, port, tls = scheme=="https")
    → send(serialize_request) → recv_all → parse_response → print body  (-I: headers)
  -L follows 3xx by re-parsing Location and reconnecting.
```

**MVP:** GET over http + https, print the body, matching `curl`'s body
output. Flags (`-X/-H/-d/-o/-I/-L`) are incremental follow-ons.

## Non-goals (first cut)

HTTP/2, keep-alive / connection reuse, gzip/deflate, cookies, auth,
proxies, IPv6 address-literal niceties, streaming (the whole body is
read into memory). Custom CA bundles (webpki-roots default only).

## Acceptance criteria

1. `Net` arms connect/send/recv/close work against a **loopback test
   server** for both plaintext and TLS; errors map to the right
   `NetError` variants.
2. `std.url.parse_url` round-trips a representative set
   (`http://h/p`, `https://h:8443/p?q=1`, bare host, no path).
3. `std.http` serializes a GET and parses a response with both
   Content-Length and chunked bodies.
4. **surl GETs a real `https://` URL and prints a body byte-identical to
   `curl`'s** on the same URL.
5. CI is green (the sigil-programs CI policy — built against a Sigil
   release that ships `Net`).

## Testing concern (call it out)

Networking tests need a server. The `Net` e2e should stand up a
**localhost listener inside the test** (Rust side) for deterministic
plaintext + TLS cases — not hit the public internet (flaky, and blocked
in many CI sandboxes). surl's oracle should likewise target a local
HTTP server fixture; any public-endpoint check is a separate,
non-gating smoke. A self-signed cert + a custom-root escape hatch may be
needed for the TLS loopback test (else webpki-roots rejects it) — decide
during Layer 1.

## Decomposition principle

**Every task is cut for `haiku` and is a single logical unit with exact,
mechanically-checkable acceptance criteria. Nothing is pre-assigned to a
larger model.** If a task turns out to be too much for haiku, the
escalation ladder handles it (haiku→sonnet→opus) — escalation is the
safety net, not a planning tool. Even Layer 1 decomposes this way; the
irreducibly-hard bits (TLS handshake, the registry) become their own
tiny, separately-tested units.

**Layer 1 (`Net` effect) — single-unit chain (illustrative):**

- runtime: `net.rs` connection registry + `connect` arm (plaintext TCP +
  DNS only), with a Rust unit test against a loopback listener.
- runtime: `send` arm (plaintext) + unit test.
- runtime: `recv` arm (plaintext, empty=EOF) + unit test.
- runtime: `close` arm (drop registry entry) + unit test.
- compiler: register `Net` in `typecheck.rs` `BUILTIN_EFFECT_NAMES` +
  `builtin_effects` (4 arm signatures, effect_id) + table tests.
- compiler: codegen frame — declare the 4 `sigil_net_*_arm` externs,
  install the effect frame, link.
- `std/net.sigil`: `Conn`/`NetError` + Result wrappers.
- e2e: plaintext connect→send→recv→close against a loopback server.
- runtime: add `Tls` variant + `rustls` handshake in `connect` (tls=true)
  + unit test (custom-root loopback).
- runtime: route `send`/`recv` through rustls for the `Tls` variant.
- runtime: test-only custom-CA path for the loopback TLS fixture.
- e2e: TLS connect→send→recv→close against a loopback TLS server.

**Layers 2–4 (`std.url`, `std.http`)** decompose per function (parse_url;
serialize_request; parse_response status line; headers; content-length
body; chunked body — each its own unit + test). **Layer 5 (surl)** lands
on the `sigil-programs` board as single-unit tasks, against a Sigil
release that includes `Net` — the same cross-repo release gate sjq had:
`Net` must ship in a tagged release before surl's pinned-toolchain CI can
build against it.
