# ADR 0002: Canonical JSON storage and a per-stream hash chain

- Status: accepted
- Date: 2026-10-18

## Context

Each stored event needs a digest that detects modification. Hashing
`serde_json` output directly is fragile: key order and number formatting
depend on crate features (`preserve_order`) and versions. Hashing several
fields by plain concatenation is ambiguous (`"ab" + "c"` is the same bytes as
`"a" + "bc"`).

## Decision

1. Convert payload and metadata to a **canonical JSON** text at ingest:
   compact, with object keys sorted bytewise by ORBIT's own writer. **Store that
   text**, and compute the digest over the stored bytes.
2. Compute the digest with SHA-256 over a **domain-separated,
   length-prefixed** encoding of all event fields (`ORBIT-EVENT-DIGEST-V1`,
   specified in `STORAGE_DESIGN.md`).
3. Include the previous event's digest in each event's digest, with 32 zero
   bytes as the genesis value. Store the head sequence and head digest on the
   stream row.

## Rationale

- Because the digest covers the stored bytes, verification never has to
  re-serialize. A future change in `serde_json` number formatting cannot
  invalidate digests of existing records.
- Length prefixes and fixed-width integers make the encoding injective.
- The chain detects deletion and reordering inside a stream, and the stored
  head detects tail truncation. Both are cheap: one extra 32-byte column per event.

## Consequences

- Payloads are not stored byte-for-byte as submitted. Whitespace, key order
  and number spelling are normalised, and size limits apply to the canonical
  form.
- Floats round-trip exactly (`serde_json/float_roundtrip`). Integers outside
  the `i64`/`u64` range are parsed as `f64` by `serde_json`, which loses
  precision before ORBIT sees the value. Callers who need exact large numbers
  should encode them as strings.
- Digests are unkeyed, which is an explicit, documented limitation. Keyed
  MACs or signatures belong in a later phase if they are needed.
