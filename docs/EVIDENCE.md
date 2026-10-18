# Portable evidence bundles

A bundle carries one finished experiment, and the evidence needed to check
it, out of the lab that produced it. Code:
`crates/orbit-lab/src/bundle.rs`. Tests: `crates/orbit-lab/tests/bundles.rs`
and `crates/orbit-api/tests/evidence.rs`. Fixtures:
`crates/orbit-lab/tests/fixtures/bundles/`.

## Format (version 1)

A bundle is one canonical JSON document: sorted keys, no whitespace,
UTF-8.

| Field | Content |
|---|---|
| `format` | `"orbit-evidence-bundle"` |
| `format_version` | `1` |
| `manifest` | `experiment_id`, `kind`, `exporter` (ORBIT version, git commit, executable digest), `document_digest`, and `entries`: `name`, `media_type`, `size`, `sha256` for every entry, sorted by name |
| `document` | `experiment`: the full record (spec and spec digest, implementation descriptors, build, lifecycle timestamps, pinned input, result and result digest, failure, artifact references). Also `verifications` (its history in the exporting lab), `history`, `provenance` and `limitations` |
| `entries` | base64 (RFC 4648, padded): one `artifacts/<role>` entry per artifact, plus `history/events.json` when the history is included |
| `signatures` | optional, at most 8: `{key_id, algorithm: "ed25519", public_key, signature}` (see [Signatures and trust](#signatures-and-trust)); not covered by the bundle digest, which they sign |

- **Bundle digest:** the `ORBIT-EVIDENCE-BUNDLE-V1` tagged SHA-256 of the
  canonical manifest. The manifest holds the document digest
  (`ORBIT-EVIDENCE-DOCUMENT-V1`) and every entry digest, so the bundle
  digest commits to the whole bundle.
- **Determinism:** the export contains no timestamp of its own. Exporting
  the same record (with the same verification history) from the same build
  gives identical bytes; a test checks this.
- **Input history:**
  - Included for differential replays when the exporting server's store
    still holds exactly the pinned events `1..=to`: the chain is
    re-verified, and must end at the recorded `to_event_digest`. Their
    canonical JSON must also fit within half the size limit.
  - Otherwise the bundle states why it is not included: no store available,
    the stream changed or is too short, too large, or the experiment
    failed before its input was resolved.
  - Performance and reliability experiments have no input event history.

## Validation

`validate` (CLI `orbit-lab validate FILE`, API `POST
/lab/bundles/validate`) trusts nothing until it has checked it, and never
writes or executes anything. Checks run in this order, each problem
reported with a stable code:

| Check | Code |
|---|---|
| at most 32 MiB | `too_large` |
| JSON nesting at most 64 levels (counted outside strings, before parsing) | `too_deeply_nested` |
| complete, well-formed JSON | `truncated`, `malformed_json` |
| `format` | `not_a_bundle` |
| `format_version` is 1 | `unsupported_version` |
| exact fields at every level (unknown or missing fields are errors), manifest and document in canonical form | `invalid_structure` |
| entry names: 1–128 characters of `[a-z0-9_.-]` in `/`-separated segments that start with a letter or digit (so no `..`, absolute paths, backslashes, control or non-ASCII characters) | `unsafe_entry_name` |
| at most 64 entries, each at most 16 MiB decoded | `too_many_entries`, `entry_too_large` |
| media type is `application/octet-stream`, `application/json` or `text/plain` | `unsupported_media_type` |
| strict base64 (only the canonical encoding) | `invalid_encoding` |
| every listed entry present, nothing unlisted | `missing_entry`, `unlisted_entry` |
| every entry's size and SHA-256; the document digest; the experiment's own spec and result digests | `digest_mismatch` |
| every artifact reference has a matching entry | `artifact_missing` |
| included history: every event digest recomputed (`ORBIT-EVENT-DIGEST-V1`), the chain from genesis, the pinned range and history fingerprint | `history_mismatch` |
| manifest and record agree; supported kind; only finished experiments | `inconsistent_record`, `unsupported_kind` |

The report also states:
- `integrity`: `verified` or `failed`;
- `scientific_correctness`: always `not_verified`;
- `history`: `included_and_verified`, `not_included` or `invalid`;
- `independently_replayable`, with the reason.

### What a valid bundle shows, and what it does not

- **Integrity:** every digest matches its content, and any change to the
  content changes the bundle digest. That makes it tamper-evident
  **relative to a bundle digest you obtained from a trusted channel**.
- **Not authenticity, unless signed by a key you trust.** Anyone can build
  a consistent bundle under a new digest. A test (`a_consistent_forgery_…`)
  forges a result, recomputes every digest, and gets a *valid* bundle;
  reproduction then reports `differs`. Authenticity is reported separately,
  as `unsigned`, `signed_untrusted` or `trusted`
  ([below](#signatures-and-trust)).
- **Not scientific correctness:** validation shows that the evidence is
  consistent, not that the experiment was right.
- A matching executable digest identifies the same binary, not
  semantically equivalent code.

A property test flips single bytes of the valid fixture (512 cases per
run). Every mutated bundle is either invalid, or valid with a *different*
bundle digest. The second case comes from editing a manifest field that
only the bundle digest itself covers, such as the exporter's commit.

## Signatures and trust

Integrity says the bundle is consistent; authenticity says **who** vouches
for it. ORBIT keeps the two apart.

- **Signing.** `orbit-lab keygen --key-id ID --out key.json --public-out
  trusted-keys.json` creates an Ed25519 key, with randomness from the
  operating system. The key file is written with owner-only permissions on
  Unix, an existing file is never overwritten, and the secret is never
  printed. `orbit-lab sign bundle.json --key key.json --out signed.json`
  signs a **valid** bundle: the signature covers
  `ORBIT-EVIDENCE-SIGNATURE-V1 ‖ bundle digest`. Signing does not change the
  bundle digest, and signing again with the same key id replaces that
  signature. Implementation: `ed25519-dalek` (strict verification), in
  `crates/orbit-lab/src/signing.rs`; no custom cryptography.
- **Verification** is part of validation. A malformed signature
  (`malformed_signature`: bad key id, algorithm, key or length, or a key id
  signing twice) or one that does not verify for this bundle's digest
  (`invalid_signature`) makes the bundle **invalid**. Altering any content
  after signing changes the bundle digest and so invalidates the signature,
  even if every content digest is recomputed.
- **Trust is explicit.** A valid signature counts only if its key id *and*
  public key are listed in a trusted-keys file that the verifier configured:
  `--trusted-keys FILE` for the CLI, `ORBIT_TRUSTED_KEYS` for the API.
  Otherwise the result is `signed_untrusted`, including for a key that
  reuses a trusted key's id. No key is trusted automatically, not even the
  one embedded in the signature. Without signatures the result is
  `unsigned`.
- **What `trusted` means:** the holder of that private key signed this exact
  bundle. It does not make the experiment correct, and it is only as good
  as your key management.
- **Key management is yours.** Keys are files: keep them out of
  repositories and bundles (ORBIT never embeds them), give the trusted-keys
  file only to verifiers who should trust those signers, and rotate by
  issuing a new key id and replacing the trusted-keys file. There is no key
  revocation list or expiry.

The report's `authenticity` field and the Observatory's Evidence page
show the status, the key ids, whether a trust store was configured, and
what the status establishes. Imports record the authenticity seen at
import time and keep the signatures.

Tests: `crates/orbit-lab/src/signing.rs` unit tests (key ids, hex, flipped
signatures, impostor keys, trust-store errors, key-file permissions and
no-overwrite); `tests/bundles.rs` (`signatures::…`: signed, trusted,
untrusted, impostor, altered after signing, malformed, unknown fields,
import and re-validation); `tests/cli.rs` (keygen, sign, validate with and
without a trust store); `orbit-api` `tests/evidence.rs` (trust only by
configuration).

## Import

`import` (CLI `orbit-lab import FILE`, API `POST /lab/bundles`) validates
first. **An invalid bundle is never imported**: the API answers 422
`invalid_bundle` with the report.

A valid bundle is stored as follows:
- **Entry bytes** go to the artifact store, addressed by content digest,
  never by name. An entry name can therefore never address a file, and
  existing content is never replaced by different content.
- **Manifest, document and validation report** go to the registry table
  `imported_bundles` (registry schema v4). That table is separate from
  `experiments` and append-only (triggers forbid updates and deletes).
- **No local experiment** is created or changed. When the lab has an
  experiment with the same id, the response says whether its spec and
  result digests match.
- **Importing twice** returns the existing row and changes nothing.

## Reproduction from a bundle

`reproduce-bundle DIGEST`, or `POST /lab/bundles/{digest}/reproduce`,
first rebuilds the bundle from storage and validates it again. It then
appends an attempt to `bundle_verifications`; the bundle itself never
changes.

| Bundle | What happens | Possible statuses |
|---|---|---|
| differential replay with history | re-executed over the bundle's verified events (an in-memory `MemoryHistory`), with this build's implementations, using exactly the rules of local reproduction: spec digest, comparison semantics version, implementations by id **and** fingerprint, pinned input, traces and result document | `reproduced`, `differs`, `implementation_unavailable`, `configuration_mismatch`, `history_changed`, `not_reproducible` |
| differential replay without history | not re-executed | `history_unavailable` |
| performance | the same spec is measured again here and compared (see [PERFORMANCE.md](PERFORMANCE.md)); in the API, in the background | `reproduced`, `differs`, `inconclusive`, `configuration_mismatch` |
| reliability | not executed: fault injection is only available in the `orbit-reliability` tool | `unsupported` |
| failed experiment | nothing to reproduce | `not_reproducible` |

**Exact reproduction from the exported evidence alone** is tested. A
differential replay is exported from one lab, imported into a fresh lab
with no access to the original store, and reproduced: `reproduced`, with
`compared: full_result`. The committed fixture `valid-differential.json`,
exported by an earlier build, also still reproduces exactly; this is
checked on every test run.

## Example (real output)

Release build, against the committed fixtures:

```text
valid-differential True verified included_and_verified True [] 5eeac6e060b07b055e052ed64268ac1875a722f36bfbcf2e903fac4124e45d94
tampered-history False failed invalid False ['history_mismatch'] 2b28e3aae407939fa7813032a49f406c901ce78e39c8c7da37e91f152d62cd9a
path-traversal False failed included_and_verified False ['unsafe_entry_name', 'unsafe_entry_name'] 540984bb…e660
truncated False failed invalid False ['truncated'] None
```

Columns: valid, integrity, history, independently replayable, error codes,
bundle digest. The path-traversal fixture reports the unsafe name twice:
once in the manifest and once among the entries. Importing the valid
fixture into an empty lab and reproducing it printed
`reproduced full_result []`.

```bash
cargo run -p orbit-lab -- --db data/orbit.db --lab data/lab export EXPERIMENT_ID --out bundle.json
```

```bash
cargo run -p orbit-lab -- --lab other-lab validate bundle.json
```

```bash
cargo run -p orbit-lab -- --lab other-lab import bundle.json
```

## Fixtures

`crates/orbit-lab/tests/fixtures/bundles/` holds:
- `valid-differential.json`;
- `truncated.json` (half of the valid file);
- `malformed.json`;
- `unsupported-version.json`;
- `tampered-artifact.json` (one flipped trace byte);
- `tampered-history.json` (an edited event payload with every entry digest
  recomputed);
- `path-traversal.json` (an entry named `../escape`).

They are regenerated, only after a format change, with:

```bash
cargo test -p orbit-lab --test bundles -- --ignored regenerate_fixtures
```

## Limits and limitations

- Signatures are optional. An unsigned bundle's origin is established only
  by obtaining its bundle digest from a trusted source. There is no key
  revocation, expiry or certificate chain: a trusted-keys file is a flat
  list.
- The history is included only while the exporting store still holds the
  pinned events, and only up to about 16 MiB of event JSON.
- Bundles are uncompressed, so there is no decompression step to bound.
  The size limit applies to the bytes as sent.
- Reliability results are carried and validated, but not reproduced from
  a bundle.
