# Engine v1.2: DNS + HTTPS so hardcoded endpoints reach the mock

Status: design, for owner approval. Scope is deliberately small: v1.1 (`fe27cc5a`) is what we want,
plus the ability to measure gateways that pin a real cloud endpoint. Nothing else changes.

## Why

Field `otb-20261109T204248Z` has 8 cells the engine never measured, each declared `untestable` by
hand in the gateway's manifest because the gateway pins a cloud host the plain-HTTP mock cannot
answer:

| Gateway | Cells | Reason |
|---|---|---|
| bifrost v2.5.0 | all 6 →bedrock | HTTPS Bedrock runtime URLs |
| one-api v0.6.10 | openai→bedrock | AWS SDK with no endpoint override (InvokeModel) |
| tensorzero 2026.6.0 | openai→gemini | hardcoded `googleapis.com` |

## Changes

1. **Catch-all DNS.** After setup and before the gateway launches, every name on the box resolves
   to the mock (a tiny responder on :53: every A query gets the mock's IP, AAAA gets none). No
   hostname list. Image pulls and installs keep real DNS. Every manifest uses `--network host`, so
   pointing the box resolver at the responder before `docker run` covers docker and native alike.
   The engine's own traffic is by IP and unaffected.

2. **HTTPS on the mock, kept light.** One extra listener on :443: TLS 1.3 only, one ECDSA P-256
   key, `rustls`; a leaf certificate is issued once per requested name (SNI) from a per-run
   certificate authority and cached; session resumption and keep-alive on, so pooled connections
   pay the handshake once. Routing stays by path, as today. The plain listener is untouched.

3. **The gateway trusts the run's certificate authority.** A bundle (system roots + the run's
   authority) mounted over `/etc/ssl/certs/ca-certificates.crt`, plus `SSL_CERT_FILE`,
   `REQUESTS_CA_BUNDLE`, `NODE_EXTRA_CA_CERTS`, `CURL_CA_BUNDLE` and `AWS_CA_BUNDLE`. AWS SDK
   gateways also get placeholder credentials and a region; the mock already accepts any key.

4. **Mock answers Bedrock InvokeModel** (one-api's call), with the same reply validation v1.1
   applies to every other dialect. Without it that cell would read red for a gap in our mock.

5. **Manifests drop their hand-written `untestable` lists** (bifrost, one-api, tensorzero). Every
   cell is probed; whatever happens is the engine's verdict, with its evidence.

6. **The cell records how it reached the mock** (`upstream_transport: "tls"`), shown verbatim, so a
   reader can see those numbers include an HTTPS hop.

Everything else is frozen at v1.1, including the still-excluded #1, #7 and #8.

## What to expect

- Up to 8 cells move from untestable to measured. A gateway that refuses our certificate (for
  example a Rust build with compiled-in roots) or needs the internet at start-up gets that as its
  recorded verdict instead.
- No gateway can reach the internet while it is measured, which makes runs more isolated.
- Because DNS changes for every gateway, v1.2 is a new instrument: one full 15-gateway field
  (about 5 hours), released as usual.

## Rollout

1. Build and test locally (targeted tests); CI runs `verify-engine`.
2. Freeze and calibrate the control.
3. Candidate check with per-cell placement: the 8 cells plus openai→openai on every gateway,
   to confirm plain cells did not move. About an hour of boxes.
4. Owner approval, full field, dev→qa→main.

## One decision

Those 8 cells' added latency includes the HTTPS hop to the mock. Leave it in, flagged by
`upstream_transport` (recommended, simplest, and real deployments pay it too), or also measure a
TLS direct baseline to subtract it (more engine work).
