# FLAC archival audio — issue #761

## Scope and authority

Implements #761 under #758 with existing ADR 0114 settings/facts. The frozen
WORK:SCOPE token is `q761-12d2cc3a`. Other codecs, aggregate acceptance (#764),
real-library runs (#339), selection/default changes, same-channel companions,
new containers and byte-ownership migration remain with their recorded owners.
No ownership transition, serialized field, profile knob, dependency or protocol
version change is needed. Protocol 5 deployment ordering remains ADR 0114's.

## Behavior

`transcode audio to flac` and existing explicit `synthesize audio` with `codec flac`
use MKV and the default profile. The FFmpeg encoder is `flac`, with explicit
compression level 5, s16 or s32 format, raw precision, source rate and layout.
There is no lossy bitrate. Missing FLAC in the worker's discovered encoder set
fails before file creation and suggests choosing a worker build with that encoder;
it is not a startup prerequisite and never falls back.

Ordinary archival conversion resolves every selected stream independently using
`LosslessAudioSettings::resolve` and actual FFprobe source facts. Known signed
16/24-bit integer PCM representations, positive rates up to 655350 Hz, and matching
mono/1, stereo/2, 5.1/6, 5.1(side)/6, 7.1/8 layouts are supported. Rate, exact layout
and precision are preserved by default; existing explicit 16-to-24 widening is
allowed and reduction rejected. Missing/float/unsupported facts fail before
starting the encoder. Decoder storage width never substitutes for source precision.
Side and back layouts are distinct and never silently normalized.

Synthesis retains strict downmix and existing companion identities, copied originals,
metadata and non-default behavior. FLAC companions support target 1/2/6 channels
with explicit mono/stereo/5.1 layout, and preserve the source rate and resolved
precision. This is a requested transform; it is not archival sample equality.
All selected source facts and target settings are validated before output creation.

After encoding, actual MKV codec/rate/layout/precision facts must match per-stream
resolved expectations before success. Existing metadata and companion checks remain.
Source immutability and transaction/publication ownership remain unchanged. Production
verification uses bounded probe facts; canonical decoded equality is generated-test
proof through #760's helper, not a new production byte-reading path.

## Implementation seams

Extend the policy's two closed target sets and protocol target predicate/constant.
Keep the lossy bitrate lookup returning None for FLAC. In the worker, reuse one
internal per-stream settings function for transcode and synthesis argv, and one
resolution function for preflight and post-output comparison. Existing source facts
and output facts already carry all evidence. Existing plan and dispatch carry the
canonical token unchanged; focused tests prove those paths and no-op behavior.
No generalized codec framework or serialized comparison object is introduced.

## Success and validation

- Focused policy/protocol/planner/dispatch tests accept FLAC and preserve stable IDs,
  target token, existing lossy profiles and bounded compatibility semantics.
- Worker unit tests check exact compression/no bitrate and missing encoder rejection;
  invalid/missing facts, precision reduction, excessive rate/channels/layout and
  output fact mismatches fail. Each new behavioral test gets a controlled fault.
- Required real-worker tests generate per-channel distinct integer fixtures in
  supported MKV layouts for both 16/24-bit precision and multiple rates. Actual
  output facts and canonical decoded samples compare equal; source bytes and copied
  streams/metadata/dispositions/lineage are checked. Tools missing locally or in CI
  fail loudly; no silent skips or new opt-in tests.
- Real synthesis tests independently generate the requested reference downmix,
  compare transformed samples, verify originals and companion metadata/defaults,
  and reject same-channel/upmix or unsupported targets before output mutation.
- Run focused tests, source checks, formatting, all-target/all-feature lint,
  mandatory pre-commit hooks, final `just ci`, then remote CI on the exact head.

## Failure model

- Actors and deployments: authenticated local or owner-node workers, operators,
  and Linux/macOS CI generated-media jobs using their installed FFmpeg builds.
- Invariants and assets: immutable source bytes; selected stream sample/rate/layout/
  precision; unaffected streams, metadata and lineage; strict non-default downmix.
  Missing/unsupported input facts and missing encoders fail before output creation;
  actual output mismatches fail before success. Validate the complete selection
  before launch, including a later invalid stream. Required tests exercise actual
  mux behavior across supported layouts and precision on CI.
- Accepted failure classes: none for the named FLAC contracts. Existing staging,
  no-overwrite and input-hash guards bound source/output hazards; this change does
  not add crash-recovery behavior or promise cross-build encoded-byte identity.
- Covered elsewhere: upstream fidelity loss and arbitrary codec families (#758),
  byte-ownership migration (ADR 0050/#416–#425), aggregate all-target CLI acceptance
  (#764) and real-library execution (#339). ADR 0114 owns settings/facts and the
  distinction between production output-fact verification and generated-test sample
  equality. Existing transaction/publication guards remain unchanged.

## Threat model

- Boundary inventory: no new transport or authority boundary. Existing authenticated
  worker requests widen the target token to FLAC; untrusted media probe facts feed
  integer settings; resolved settings and confined input/output paths feed argv;
  observed output facts cross back into worker results and existing publication.
- Actor model: authenticated operators choose policy/work and supply media. Media
  bytes and their probe facts remain untrusted. The configured FFmpeg/FFprobe
  executable and existing authenticated worker/owner deployment remain trusted.
- Controls per boundary: existing HTTP authentication/lease checks govern requests;
  closed target/default-profile validation and per-stream checked fact resolution
  govern FLAC settings before process launch. Argv uses separate arguments, bounded
  numeric values and an exact layout set. Existing staging confinement, option-like
  path rejection, no-overwrite, source hash and process deadlines remain. Exact
  output codec/rate/layout/precision verification precedes success; failure reports
  diagnostic field/codec facts without adding credentials or subprocess logging.
- Explicitly out of scope: replacing existing transport, process containment or
  executable provisioning; library execution (#339) and ownership migration
  (ADR 0050/#416–#425). No new permission, network listener or secret is introduced.
