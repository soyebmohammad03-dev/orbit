-- Lab registry (schema v1) created by orbit-lab at commit a44e62f (Sprint 7), dumped with sqlite3 .dump.
PRAGMA application_id = 1330793538;
PRAGMA user_version = 1;
PRAGMA foreign_keys=OFF;
BEGIN TRANSACTION;
CREATE TABLE experiments (
    id              BLOB PRIMARY KEY CHECK (length(id) = 16),
    kind            TEXT NOT NULL CHECK (kind IN ('differential_replay')),
    spec_version    INTEGER NOT NULL CHECK (spec_version >= 1),
    spec            TEXT NOT NULL,
    spec_digest     BLOB NOT NULL CHECK (length(spec_digest) = 32),
    stream_id       TEXT NOT NULL,
    left_impl       TEXT NOT NULL,
    right_impl      TEXT NOT NULL,
    implementations TEXT NOT NULL,
    orbit_version   TEXT NOT NULL,
    git_commit      TEXT,
    created_at      INTEGER NOT NULL,
    status          TEXT NOT NULL CHECK (status IN ('created', 'running', 'completed', 'failed')),
    started_at      INTEGER,
    finished_at     INTEGER,
    elapsed_micros  INTEGER,
    input           TEXT,
    outcome         TEXT,
    result          TEXT,
    result_digest   BLOB CHECK (result_digest IS NULL OR length(result_digest) = 32),
    reproducibility TEXT CHECK (reproducibility IS NULL
                                OR reproducibility IN ('reproducible', 'unverified', 'not_reproducible')),
    failure         TEXT,
    CHECK (CASE status
        WHEN 'created' THEN started_at IS NULL AND finished_at IS NULL
                            AND result IS NULL AND failure IS NULL
        WHEN 'running' THEN started_at IS NOT NULL AND finished_at IS NULL
                            AND result IS NULL AND failure IS NULL
        WHEN 'completed' THEN started_at IS NOT NULL AND finished_at IS NOT NULL
                            AND input IS NOT NULL AND outcome IS NOT NULL AND result IS NOT NULL
                            AND result_digest IS NOT NULL AND reproducibility IS NOT NULL
                            AND failure IS NULL
        WHEN 'failed' THEN finished_at IS NOT NULL AND failure IS NOT NULL AND result IS NULL
    END)
);
INSERT INTO experiments VALUES(x'01a14e0b788c731faae44bcffb646c18','differential_replay',1,'{"determinism_check":true,"from":0,"left":"orbit.counter","reference":"left","right":"example.counter-floor-at-zero","stream":"legacy-counter","to":null}',x'34c8c486ecbefb77c277a2dd82e3a21d05daf5251260bf265c919a6e4fa716a3','legacy-counter','orbit.counter','example.counter-floor-at-zero','[{"descriptor":{"adapter":"orbit-core/projection","config":{},"config_digest":"a48db2dfed9b774bc3b4ed780e2a9f58230eb01a3a254115531575c832542dca","descriptor_version":1,"determinism":"pure","fingerprint":"b55a1c4623f35631e5e0e4e4823129ce9b7796bada805435e1b4bc2a55522b48","id":"orbit.counter","implementation_type":"orbit_core::projection::examples::Counter","name":"Counter","projection":{"id":"counter","version":1}},"side":"left"},{"descriptor":{"adapter":"orbit-core/projection","config":{},"config_digest":"a48db2dfed9b774bc3b4ed780e2a9f58230eb01a3a254115531575c832542dca","descriptor_version":1,"determinism":"pure","fingerprint":"19c355a31cd75c2317a95051a16f7a19993117fd625a953b2203fcdc19cf10eb","id":"example.counter-floor-at-zero","implementation_type":"orbit_lab::variants::CounterFloorAtZero","name":"Counter","projection":{"id":"counter","version":1}},"side":"right"}]','0.1.0','a44e62fa93cb7ff624dbbdb5a152ea545ab74e74',1792310737036289,'completed',1792310737036984,1792310737050857,13670,'{"from":0,"from_event_digest":"0000000000000000000000000000000000000000000000000000000000000000","history_fingerprint":"e4cf0d7774b090aa8748375d5c70c540431b35a1b937add04094272477704883","stream":"legacy-counter","to":5,"to_event_digest":"f50f58845b8a5b51ce1757a4bb7b2abc7aa687796eecc768a35639c43020376a"}','diverged','{"boundaries_compared":6,"boundaries_differing":2,"determinism":{"checked":true,"consistent":true,"declared_uncontrolled":[],"inconsistency":null},"divergent_segments":[{"first":2,"last":2},{"first":5,"last":5}],"divergent_segments_total":2,"events_fed_digest":"249c08881f828829eaae034d200744fd92023e4ee6770bb503aa1d45a0671063","events_processed":5,"failures":[],"final_comparison":{"boundary":5,"changes":[{"from":-3,"op":"modified","path":"/value","to":0}],"changes_total":1,"equal":false,"event":{"event_id":"01a14e0b-55de-73a1-84cb-b7948e7b3fcd","event_type":"counter.decremented","schema_version":1,"sequence":5},"left":{"canonical_bytes":12,"digest":"83c96b890fb01d3f11d8477e2756773c4e90d1b168ab4972a4d61e0cd1f621d7"},"right":{"canonical_bytes":11,"digest":"e64c8fbc17c521b14774ff3885ea9b13e061f88a78f65ca984ae12bb5e8b6f61"}},"first_divergence":{"boundaries_matched_before":2,"boundary":2,"changes":[{"from":-3,"op":"modified","path":"/value","to":0}],"changes_total":1,"equal":false,"event":{"event_id":"01a14e0b-55af-7168-96de-243a3c82f0b7","event_type":"counter.decremented","schema_version":1,"sequence":2},"left":{"canonical_bytes":12,"digest":"83c96b890fb01d3f11d8477e2756773c4e90d1b168ab4972a4d61e0cd1f621d7"},"right":{"canonical_bytes":11,"digest":"e64c8fbc17c521b14774ff3885ea9b13e061f88a78f65ca984ae12bb5e8b6f61"}},"input":{"from":0,"from_event_digest":"0000000000000000000000000000000000000000000000000000000000000000","history_fingerprint":"e4cf0d7774b090aa8748375d5c70c540431b35a1b937add04094272477704883","stream":"legacy-counter","to":5,"to_event_digest":"f50f58845b8a5b51ce1757a4bb7b2abc7aa687796eecc768a35639c43020376a"},"left":{"fingerprint":"b55a1c4623f35631e5e0e4e4823129ce9b7796bada805435e1b4bc2a55522b48","implementation":"orbit.counter"},"outcome":"diverged","reference":"left","reproducibility":{"reasons":["input pinned by event digests; both implementations gave identical traces on two executions"],"status":"reproducible"},"right":{"fingerprint":"19c355a31cd75c2317a95051a16f7a19993117fd625a953b2203fcdc19cf10eb","implementation":"example.counter-floor-at-zero"}}',x'25b1234be94155f552d30e772bd87acb9a08988334f1f32e80b98f052925093d','reproducible',NULL);
INSERT INTO experiments VALUES(x'01a14e0b78ac74fdadedfdf070690083','differential_replay',1,'{"determinism_check":true,"from":0,"left":"orbit.counter","reference":null,"right":"orbit.counter","stream":"legacy-counter","to":null}',x'fc81749959c12110d2a1aec73d38e27b005148e9e1f5f1d29062e3d62a522fc9','legacy-counter','orbit.counter','orbit.counter','[{"descriptor":{"adapter":"orbit-core/projection","config":{},"config_digest":"a48db2dfed9b774bc3b4ed780e2a9f58230eb01a3a254115531575c832542dca","descriptor_version":1,"determinism":"pure","fingerprint":"b55a1c4623f35631e5e0e4e4823129ce9b7796bada805435e1b4bc2a55522b48","id":"orbit.counter","implementation_type":"orbit_core::projection::examples::Counter","name":"Counter","projection":{"id":"counter","version":1}},"side":"left"},{"descriptor":{"adapter":"orbit-core/projection","config":{},"config_digest":"a48db2dfed9b774bc3b4ed780e2a9f58230eb01a3a254115531575c832542dca","descriptor_version":1,"determinism":"pure","fingerprint":"b55a1c4623f35631e5e0e4e4823129ce9b7796bada805435e1b4bc2a55522b48","id":"orbit.counter","implementation_type":"orbit_core::projection::examples::Counter","name":"Counter","projection":{"id":"counter","version":1}},"side":"right"}]','0.1.0','a44e62fa93cb7ff624dbbdb5a152ea545ab74e74',1792310737068482,'completed',1792310737069241,1792310737074638,5072,'{"from":0,"from_event_digest":"0000000000000000000000000000000000000000000000000000000000000000","history_fingerprint":"e4cf0d7774b090aa8748375d5c70c540431b35a1b937add04094272477704883","stream":"legacy-counter","to":5,"to_event_digest":"f50f58845b8a5b51ce1757a4bb7b2abc7aa687796eecc768a35639c43020376a"}','equivalent','{"boundaries_compared":6,"boundaries_differing":0,"determinism":{"checked":true,"consistent":true,"declared_uncontrolled":[],"inconsistency":null},"divergent_segments":[],"divergent_segments_total":0,"events_fed_digest":"249c08881f828829eaae034d200744fd92023e4ee6770bb503aa1d45a0671063","events_processed":5,"failures":[],"final_comparison":{"boundary":5,"changes":[],"changes_total":0,"equal":true,"event":{"event_id":"01a14e0b-55de-73a1-84cb-b7948e7b3fcd","event_type":"counter.decremented","schema_version":1,"sequence":5},"left":{"canonical_bytes":12,"digest":"83c96b890fb01d3f11d8477e2756773c4e90d1b168ab4972a4d61e0cd1f621d7"},"right":{"canonical_bytes":12,"digest":"83c96b890fb01d3f11d8477e2756773c4e90d1b168ab4972a4d61e0cd1f621d7"}},"first_divergence":null,"input":{"from":0,"from_event_digest":"0000000000000000000000000000000000000000000000000000000000000000","history_fingerprint":"e4cf0d7774b090aa8748375d5c70c540431b35a1b937add04094272477704883","stream":"legacy-counter","to":5,"to_event_digest":"f50f58845b8a5b51ce1757a4bb7b2abc7aa687796eecc768a35639c43020376a"},"left":{"fingerprint":"b55a1c4623f35631e5e0e4e4823129ce9b7796bada805435e1b4bc2a55522b48","implementation":"orbit.counter"},"outcome":"equivalent","reference":null,"reproducibility":{"reasons":["input pinned by event digests; both implementations gave identical traces on two executions"],"status":"reproducible"},"right":{"fingerprint":"b55a1c4623f35631e5e0e4e4823129ce9b7796bada805435e1b4bc2a55522b48","implementation":"orbit.counter"}}',x'eed87be9c3c014a097c28e1b19c1826cf057eeb76a74d005d11ae9e0dca81ba4','reproducible',NULL);
INSERT INTO experiments VALUES(x'01a14e0b790a72849e98dba63604ef17','differential_replay',1,'{"determinism_check":true,"from":0,"left":"orbit.counter","reference":null,"right":"orbit.counter","stream":"missing","to":null}',x'b0a601aa0515f0f982c8a9d524a00277d60d61a41b5ab0b2e8650f2e616fa0dc','missing','orbit.counter','orbit.counter','[{"descriptor":{"adapter":"orbit-core/projection","config":{},"config_digest":"a48db2dfed9b774bc3b4ed780e2a9f58230eb01a3a254115531575c832542dca","descriptor_version":1,"determinism":"pure","fingerprint":"b55a1c4623f35631e5e0e4e4823129ce9b7796bada805435e1b4bc2a55522b48","id":"orbit.counter","implementation_type":"orbit_core::projection::examples::Counter","name":"Counter","projection":{"id":"counter","version":1}},"side":"left"},{"descriptor":{"adapter":"orbit-core/projection","config":{},"config_digest":"a48db2dfed9b774bc3b4ed780e2a9f58230eb01a3a254115531575c832542dca","descriptor_version":1,"determinism":"pure","fingerprint":"b55a1c4623f35631e5e0e4e4823129ce9b7796bada805435e1b4bc2a55522b48","id":"orbit.counter","implementation_type":"orbit_core::projection::examples::Counter","name":"Counter","projection":{"id":"counter","version":1}},"side":"right"}]','0.1.0','a44e62fa93cb7ff624dbbdb5a152ea545ab74e74',1792310737163011,'failed',1792310737164103,1792310737164346,52,NULL,NULL,NULL,NULL,NULL,'{"details":null,"kind":"history_unavailable","message":"stream missing does not exist"}');
CREATE TABLE artifacts (
    digest     BLOB PRIMARY KEY CHECK (length(digest) = 32),
    media_type TEXT NOT NULL,
    size       INTEGER NOT NULL CHECK (size >= 0),
    created_at INTEGER NOT NULL
);
INSERT INTO artifacts VALUES(x'5053f61cdc56ec2e7f574ba93b0a22eb688924c6fd2582720acb1c0bedfef440','application/octet-stream',192,1792310737050857);
INSERT INTO artifacts VALUES(x'e956f215a2f1f48c5e8817339d73727285bacbd6ac3d314481818ce1eef6747c','application/octet-stream',192,1792310737050857);
INSERT INTO artifacts VALUES(x'd4e7905d5c45bcc1f05a5abbfa26c1e150690502a2ebd073fe16fc282644c818','application/json',41,1792310737050857);
INSERT INTO artifacts VALUES(x'87a8f4dafdec3722af3fb04e9dd1afa8f83b761fb43882e535b9ddd68092090e','application/json',42,1792310737074638);
CREATE TABLE experiment_artifacts (
    experiment_id BLOB NOT NULL REFERENCES experiments (id),
    role          TEXT NOT NULL,
    digest        BLOB NOT NULL REFERENCES artifacts (digest),
    PRIMARY KEY (experiment_id, role)
);
INSERT INTO experiment_artifacts VALUES(x'01a14e0b788c731faae44bcffb646c18','left_trace',x'5053f61cdc56ec2e7f574ba93b0a22eb688924c6fd2582720acb1c0bedfef440');
INSERT INTO experiment_artifacts VALUES(x'01a14e0b788c731faae44bcffb646c18','right_trace',x'e956f215a2f1f48c5e8817339d73727285bacbd6ac3d314481818ce1eef6747c');
INSERT INTO experiment_artifacts VALUES(x'01a14e0b788c731faae44bcffb646c18','first_divergence_states',x'd4e7905d5c45bcc1f05a5abbfa26c1e150690502a2ebd073fe16fc282644c818');
INSERT INTO experiment_artifacts VALUES(x'01a14e0b788c731faae44bcffb646c18','final_states',x'd4e7905d5c45bcc1f05a5abbfa26c1e150690502a2ebd073fe16fc282644c818');
INSERT INTO experiment_artifacts VALUES(x'01a14e0b78ac74fdadedfdf070690083','left_trace',x'5053f61cdc56ec2e7f574ba93b0a22eb688924c6fd2582720acb1c0bedfef440');
INSERT INTO experiment_artifacts VALUES(x'01a14e0b78ac74fdadedfdf070690083','right_trace',x'5053f61cdc56ec2e7f574ba93b0a22eb688924c6fd2582720acb1c0bedfef440');
INSERT INTO experiment_artifacts VALUES(x'01a14e0b78ac74fdadedfdf070690083','final_states',x'87a8f4dafdec3722af3fb04e9dd1afa8f83b761fb43882e535b9ddd68092090e');
CREATE TABLE verifications (
    id            BLOB PRIMARY KEY CHECK (length(id) = 16),
    experiment_id BLOB NOT NULL REFERENCES experiments (id),
    kind          TEXT NOT NULL CHECK (kind IN ('artifacts', 'reproduction')),
    status        TEXT NOT NULL,
    detail        TEXT NOT NULL,
    verified_at   INTEGER NOT NULL
);
CREATE TRIGGER experiments_no_delete BEFORE DELETE ON experiments
BEGIN SELECT RAISE(ABORT, 'experiments are never deleted'); END;
CREATE TRIGGER experiments_lifecycle BEFORE UPDATE ON experiments
WHEN NOT ((OLD.status = 'created' AND NEW.status IN ('running', 'failed'))
       OR (OLD.status = 'running' AND NEW.status IN ('completed', 'failed')))
BEGIN SELECT RAISE(ABORT, 'invalid experiment status transition'); END;
CREATE TRIGGER experiments_identity_is_immutable BEFORE UPDATE ON experiments
WHEN NEW.id IS NOT OLD.id OR NEW.kind IS NOT OLD.kind
  OR NEW.spec_version IS NOT OLD.spec_version OR NEW.spec IS NOT OLD.spec
  OR NEW.spec_digest IS NOT OLD.spec_digest OR NEW.stream_id IS NOT OLD.stream_id
  OR NEW.left_impl IS NOT OLD.left_impl OR NEW.right_impl IS NOT OLD.right_impl
  OR NEW.implementations IS NOT OLD.implementations
  OR NEW.orbit_version IS NOT OLD.orbit_version OR NEW.git_commit IS NOT OLD.git_commit
  OR NEW.created_at IS NOT OLD.created_at
  OR (OLD.started_at IS NOT NULL AND NEW.started_at IS NOT OLD.started_at)
BEGIN SELECT RAISE(ABORT, 'experiment identity columns are immutable'); END;
CREATE TRIGGER artifacts_no_update BEFORE UPDATE ON artifacts
BEGIN SELECT RAISE(ABORT, 'artifacts are immutable'); END;
CREATE TRIGGER artifacts_no_delete BEFORE DELETE ON artifacts
BEGIN SELECT RAISE(ABORT, 'artifacts are never deleted'); END;
CREATE TRIGGER experiment_artifacts_only_while_running BEFORE INSERT ON experiment_artifacts
WHEN (SELECT status FROM experiments WHERE id = NEW.experiment_id) IS NOT 'running'
BEGIN SELECT RAISE(ABORT, 'artifacts can only be attached to a running experiment'); END;
CREATE TRIGGER experiment_artifacts_no_update BEFORE UPDATE ON experiment_artifacts
BEGIN SELECT RAISE(ABORT, 'artifact links are immutable'); END;
CREATE TRIGGER experiment_artifacts_no_delete BEFORE DELETE ON experiment_artifacts
BEGIN SELECT RAISE(ABORT, 'artifact links are never deleted'); END;
CREATE TRIGGER verifications_only_when_finished BEFORE INSERT ON verifications
WHEN (SELECT status FROM experiments WHERE id = NEW.experiment_id) NOT IN ('completed', 'failed')
BEGIN SELECT RAISE(ABORT, 'only finished experiments can be verified'); END;
CREATE TRIGGER verifications_no_update BEFORE UPDATE ON verifications
BEGIN SELECT RAISE(ABORT, 'verifications are immutable'); END;
CREATE TRIGGER verifications_no_delete BEFORE DELETE ON verifications
BEGIN SELECT RAISE(ABORT, 'verifications are never deleted'); END;
CREATE INDEX experiments_by_stream ON experiments (stream_id, id);
CREATE INDEX verifications_by_experiment ON verifications (experiment_id, id);
COMMIT;
