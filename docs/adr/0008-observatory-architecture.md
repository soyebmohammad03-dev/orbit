# ADR 0008: A thin Axum API over the synchronous engine, and a client-rendered UI

- Status: accepted
- Date: 2026-10-18

## Context

Phase 4 exposes the engine over HTTP and adds a web interface. The engine is
synchronous (rusqlite), generic over projection types, and already produces
serializable reports.

## Decision

1. **`crates/orbit-api`** is a new crate: Axum 0.8 with Tokio. Every engine
   call runs in `spawn_blocking`. The existing `EventStore`, which is one
   connection behind a mutex and `Send + Sync`, is shared through an `Arc`.
2. **Runtime projection selection** goes through an object-safe `DynEngine`
   trait. It is implemented once, generically, for every `Projection`, and
   only delegates to `Reconstructor` and `ReplayLab`. A generic
   `merge-patch@v1` projection (RFC 7386) was added to the core so that
   arbitrary streams can be reconstructed and diffed.
3. **Resources get DTOs.** Engine reports are returned as the engine
   serializes them, so the transport cannot drift from the domain. The
   OpenAPI document is generated with utoipa, from the handlers.
4. **The UI is a Next.js App Router application whose pages render on the
   client.** They fetch from the API with a typed client, and no
   server-side data fetching or caching sits between the user and the engine.
   Each page is a static shell that renders a client view in `<Suspense>`.
5. **Local-only by default.** The server binds to loopback, refuses remote
   binding without an explicit opt-in, has narrow CORS, and adds no
   authentication.

## Consequences

- The API holds no domain logic. New engine capabilities appear by adding a
  handler and, when needed, a `DynEngine` method.
- Throughput is bounded by the single connection, which is fine for a local
  observability tool and documented. A read-connection pool can be added
  behind `EventStore` later.
- Report types are loosely typed in OpenAPI (`object`) and typed precisely in
  the frontend's `types.ts`. The end-to-end test checks the contract against a
  real server.
