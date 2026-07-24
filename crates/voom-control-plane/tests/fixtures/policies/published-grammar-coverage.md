# Published V1/V1.1 grammar coverage

This directory's four `published-grammar-*.voom` files are the canonical
execution corpus for the grammar published in
`docs/specs/voom-control-plane-design.md`. They use published spellings only.
The other policy files in this directory remain planner fixtures and are not
grammar authority.

The corpus is intentionally representative rather than combinatorial. A row
for a comparator, codec, strategy, or target proves its distinct behavior; it
does not require every interchangeable permutation. Every row inherits the
input, expected mutation, and oracle assigned to its policy below.

## Policies and generated inputs

| Code | Policy file | Generated input |
|---|---|---|
| C | `published-grammar-core.voom` | C1 |
| T | `published-grammar-tracks.voom` | T1 |
| A | `published-grammar-audio.voom` | A1 |
| F | `published-grammar-control-flow.voom` | F1 |

### C1 — core source

An MP4 containing 1920x1080 H.264 video and one default English stereo AAC
track.

- Expected mutation: remux to MKV, then transcode video to HEVC with the named
  `default-hevc` profile.
- Oracle: the VOOM report records committed remux and transcode phases plus a
  successful artifact-verification phase; ffprobe reports Matroska and HEVC.

### T1 — track-rich source

T1a is a 1920x1080 MKV containing H.264 video; default English 5.1 E-AC-3,
English commentary AAC, and untagged stereo audio; forced English, untagged,
and titled English `Signs` subtitles; one font attachment and one non-font
attachment. T1b has the same tracks and attachments at 1024x576. T1c has the
same tracks and attachments at 512x288. The one default disposition belongs
to the English audio track, so the filtered head-order operation has exactly
one match.

- Expected mutation: remove the commentary, non-preferred subtitle, and
  non-font attachment; retain the selected tracks; then set deterministic
  order and defaults. T1a leaves `best` as the final subtitle strategy over two
  surviving subtitles. T1b separately makes `none` the final audio strategy
  and `preserve` the final subtitle strategy over those same two subtitles.
  T1c exercises the bare conditional removal and leaves no subtitle tracks.
- Oracle: the VOOM report records a committed remux and successful artifact
  verification; mkvtoolnix-compatible JSON proves the exact surviving track
  order, dispositions, languages, titles, and attachment MIME types.

### A1 — multi-audio source

An MKV containing H.264 video, default English 5.1 E-AC-3, English stereo AAC,
and Japanese commentary AAC.

- Expected mutation: exercise all three transcode targets, add stereo AAC,
  Opus, and E-AC-3 downmix companions, and extract both a filtered sidecar and
  the selected bare-extract set without output collisions.
- Oracle: the VOOM report records committed audio phases, sidecar lineage, and
  successful artifact verification. The filtered extract selects only the
  commentary AAC track; ffprobe proves codecs and channel counts, and hashes
  distinguish every sidecar.

### F1 — control-flow input set

The set contains these exact cases:

- F1a `modify.mp4`: 1920x1080 H.264 video at 2 Mbps, duration 2000 ms, two
  audio tracks, and two subtitle tracks. `inspect` modifies, `normalize`
  completes and modifies, and `organize` runs.
- F1b `already-normalized.mkv`: the same facts except Matroska and HEVC.
  `inspect` completes without mutation, `normalize` runs because `completed`
  is true but completes without mutation, and `organize` does not run because
  `modified` is false.
- F1c `fail.mp4`: byte-identical media facts to F1a. Before the same two-file
  batch containing F1a and F1c starts, create the exact final `fail.mkv`
  destination with different bytes. The existing-target commit guard makes
  F1c's `inspect` remux fail deterministically; its dependent phases do not
  run, while F1a continues and commits.
- Resume F1a after two durable boundaries: first after the `inspect` per-file
  summary is committed and before `normalize` dispatch, then after the
  `normalize` summary is committed and before `organize` dispatch. The #330
  coordinator test stops the driver at each named boundary, reopens the same
  database, and resumes the same job rather than compiling or planning anew.

- Expected mutation: for the passing source, the matching conditional remuxes
  to MKV, the first-rule phase transcodes video to HEVC, and the all-rules phase
  applies all matching track actions. The deliberate failure never prevents
  successful-file commits.
- Oracle: per-file VOOM phase summaries prove skip/rule decisions, true and
  false `completed` and `modified` gates, and identical decisions after resume.
  The batch report and exit status identify partial failure while retaining the
  successful file's committed output and artifact-verification result.

## Structure and control matrix

| ID | Published production or alternative | Policy | Input | Owner |
|---|---|---|---|---|
| S01 | `policy <quoted-name>` | C,T,A,F | all | #326 |
| S02 | `metadata requires_tools: [...]` | C | C1 | #327 |
| S03 | `config languages: [...]` | C,T,A,F | all | #328 |
| S04 | config `on_error: abort` | C,T,A | C1,T1,A1 | #335 |
| S05 | config `on_error: continue` | F | F1 | #335 |
| S06 | `phase <identifier>` | C,T,A,F | all | #326 |
| S07 | `depends_on: [<identifier>, ...]` | C,T,A,F | all | #326 |
| S08 | `run_if completed <identifier>` | F | F1 | #330 |
| S09 | `run_if modified <identifier>` | F | F1 | #330 |
| S10 | `skip when <condition>` | F | F1 | #329 |
| S11 | phase `on_error: abort` | F | F1 | #335 |
| S12 | phase `on_error: continue` | F | F1 | #335 |
| S13 | direct `<operation>` | C,T,A,F | all | #326 |
| S14 | `when <condition> { <operation> }` | T,F | T1,F1 | #329 |
| S15 | `rules first` | F | F1 | #329 |
| S16 | `rules all` | F | F1 | #329 |
| S17 | `rule <quoted-name> when <condition>` | F | F1 | #329 |

`Owner` names the issue that establishes the semantic oracle. #326 owns only
the corpus and coverage contract; a later owner may strengthen the focused
test without changing the canonical policy text.

## Operation matrix

| ID | Published production or alternative | Policy | Input | Execution witness | Owner |
|---|---|---|---|---|---|
| O01 | `container mkv` | C,F | C1,F1 | output container changes from MP4 to Matroska | existing |
| O02 | `transcode video to hevc` | F | F1a | video codec changes from H.264 to HEVC | existing |
| O03 | video `using profile <quoted-name>` | C | C1 | plan resolves `default-hevc`; output is HEVC | existing |
| O04 | `transcode audio to aac` without filter | A | A1 | every original audio track becomes AAC | existing |
| O05 | `transcode audio to opus where ...` | A | A1 | selected English and untagged tracks become Opus | existing |
| O06 | `transcode audio to eac3 where ...` | A | A1 | selected surround track becomes E-AC-3 | existing |
| O07 | `keep audio where ...` | T | T1a | untagged audio is absent after selection | #331 |
| O08 | `keep subtitle` with and without filter | T,F | T1a,F1a | only selected subtitle tracks survive | #331 |
| O09 | `keep attachment where ...` | T | T1a | only the font attachment survives | #331 |
| O10 | `remove audio where ...` | T | T1a | English commentary audio is removed before keep | #331 |
| O11 | `remove subtitle` with and without filter | T | T1a,T1c | `Signs` is removed; T1c removes all remaining subtitles | #331 |
| O12 | `remove attachment where ...` | T | T1a | non-font attachment is removed before keep | #331 |
| O13 | `order tracks [<track-target>, ...]` | T | T1a | final kind order is video, audio, subtitle, attachment | existing |
| O14 | defaults `first`, `best`, `none`, `preserve` | T | T1a,T1b | exact final default dispositions differ by variant | #336 |
| O15 | bare `extract audio` | A | A1 | one sidecar exists for every selected audio track | #99 |
| O16 | `extract audio where ...` | A | A1 | one commentary AAC sidecar exists | existing |
| O17 | `verify artifact` | C,T,A,F | all | each successful file records verified artifact facts | #334 |
| O18 | `defaults audio\|subtitle where ...` | T | T1a | named filtered tracks become defaults | #332 |
| O19 | `order tracks [<targets>] where ...` | T | T1a | sole default audio is first within grouped order | #332 |
| O20 | `order tracks where ...` | T | T1a | sole forced subtitle is first | #332 |
| O21 | synthesize target `codec aac` | A | A1 | added stereo AAC companion is present | #333 |
| O22 | synthesize target `codec opus` | A | A1 | added stereo Opus companion is present | #333 |
| O23 | synthesize target `codec eac3` | A | A1 | added stereo E-AC-3 companion is present | #333 |

The owner column is intentionally honest about published forms outside this
campaign. Their presence records the contract; it does not claim execution
acceptance before their listed issues land.

## Condition matrix

| ID | Published condition or alternative | Policy | Input | Owner |
|---|---|---|---|---|
| C01 | `video.codec == <token>` | F | F1 | existing |
| C02 | `media.container == <token>` | F | F1 | existing |
| C03 | `media.duration_millis <op> <number>` | F | F1 | existing |
| C04 | `video.width <op> <number>` | T,F | T1,F1 | existing |
| C05 | `video.height <op> <number>` | F | F1 | existing |
| C06 | `video.bitrate <op> <number>` | F | F1 | existing |
| C07 | `exists audio` in `skip` | F | F1 | #329 |
| C08 | `exists subtitle` in `when` and `rule` | T,F | T1,F1 | #329 |
| C09 | `count audio <op> <number>` in `skip` and `rule` | F | F1 | #329 |
| C10 | `count subtitle <op> <number>` in `when` and `rule` | T,F | T1,F1 | #329 |
| C11 | `not <condition>` | F | F1 | #329 |
| C12 | `<condition> and <condition>` | F | F1 | existing |
| C13 | `<condition> or <condition>` | F | F1 | existing |
| C14 | parenthesized Boolean condition | F | F1 | existing |
| C15 | comparators `==`, `!=`, `<`, `<=`, `>`, `>=` | T,F | T1,F1 | existing |

## Track-filter matrix

| ID | Published filter or alternative | Policy | Input | Owner |
|---|---|---|---|---|
| T01 | `language == <quoted-token>` | T | T1 | existing |
| T02 | `language in [<quoted-token>, ...]` | T,A | T1,A1 | existing |
| T03 | `codec in [<quoted-token>, ...]` | A | A1 | existing |
| T04 | `channels <op> <number>` | T,A | T1,A1 | existing |
| T05 | `commentary` | T,A | T1,A1 | #331 |
| T06 | `forced` | T,F | T1,F1 | #331 |
| T07 | `default` | T,F | T1,F1 | #332 |
| T08 | `font` | T | T1 | #331 |
| T09 | `title contains <quoted-string>` | T | T1 | #331 |
| T10 | `not <track-filter>` | T,A | T1,A1 | existing |
| T11 | `<track-filter> and <track-filter>` | T,A | T1,A1 | existing |
| T12 | `<track-filter> or <track-filter>` | T,F | T1,F1 | existing |
| T13 | parenthesized Boolean track filter | T | T1 | existing |

## Acceptance layers

1. #326 keeps the four sources compilation-clean apart from temporary
   non-error diagnostics owned by later rows. It proves every policy contains
   a mutation and `verify artifact`.
2. The semantic owner in each row adds focused planning or coordinator
   evidence. Existing compiled policy versions must remain readable.
3. #338 generates C1, T1, A1, and F1 and executes the corpus through
   `voom compliance execute`, asserting the concrete oracles above.
4. #339 uses only the separately chosen production-safe policy for canary and
   full-library acceptance; the exhaustive corpus is not a production policy.

Parser acceptance alone never satisfies a row. Unpublished `extends`, tag
operations, `actions clear`, forced-track mutation syntax, and grammar aliases
are excluded from this corpus.
