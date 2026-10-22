# Frozen pre-selective journal evidence

Captured on 22 October 2026 from the schema-2 serializer/renderer at plan base
`0ec1661`, with the existing dirty CIDR and daemon-generation fixes present.
The synthetic fixture has no domain rules, so those fixes do not change its
wire representation. No host profile, root journal, network, or secret was read.

`capture.rs.txt` is the exact temporary test inserted into
`src/backend/privileged.rs`'s existing `tests` module. In an isolated checkout of
the pre-selective source, insert that function and run:

```bash
rtk proxy cargo test --lib backend::privileged::tests::capture_legacy_v2_evidence -- --exact --ignored --nocapture
```

The test only uses `RuntimeFixture`'s fake executor, in-memory store and fake
environment. The `LEGACY_CAPTURE=` JSON supplies each checked-in `.json` file.
`committed` is the actual first successful activation record; `pending` uses the
same schema-2 pending-expectation builder with generation 2; `cleanup-pending`
models the exact cleanup intent before removal. `active-nft` is the original
renderer document. `evidence` contains its literal hash, marker, fingerprint and
routing tuples. Formatting object keys does not change typed serialization.

Do not regenerate these expected values with a modern renderer or convert the
frozen profile/plan to modern types. These fixtures authorize no live cleanup:
live resources must still pass exact ownership and prefix validation.
