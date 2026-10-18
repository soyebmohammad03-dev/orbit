//! Implementation identity and fingerprint semantics.

mod common;

use orbit_core::ProjectionKey;
use orbit_core::projection::examples::Counter;
use orbit_lab::identity::RUST_PROJECTION_ADAPTER;
use orbit_lab::variants::CounterFloorAtZero;
use orbit_lab::{
    Catalog, Determinism, ImplementationDescriptor, ImplementationId, LabError,
    ProjectionImplementation,
};
use serde_json::{Value, json};
use std::sync::Arc;

fn descriptor(
    id: &str,
    name: &str,
    ty: &str,
    config: Value,
    determinism: Determinism,
) -> ImplementationDescriptor {
    ImplementationDescriptor::new(
        ImplementationId::new(id).unwrap(),
        name,
        ProjectionKey::new("counter", 1).unwrap(),
        RUST_PROJECTION_ADAPTER,
        ty,
        config,
        determinism,
    )
    .unwrap()
}

#[test]
fn same_name_and_version_with_different_implementations_are_distinguished() {
    let catalog = Catalog::builtin();
    let core = catalog.find("orbit.counter").unwrap();
    let variant = catalog.find("example.counter-floor-at-zero").unwrap();
    let (a, b) = (core.descriptor(), variant.descriptor());
    assert_eq!(a.name, b.name, "both are called Counter");
    assert_eq!(a.projection, b.projection, "both declare counter@1");
    assert_ne!(a.id, b.id);
    assert_ne!(a.implementation_type, b.implementation_type);
    assert_ne!(a.fingerprint, b.fingerprint);
    assert!(a.implementation_type.ends_with("examples::Counter"));
    assert!(
        b.implementation_type
            .ends_with("variants::CounterFloorAtZero")
    );
}

#[test]
fn fingerprints_are_stable_under_equivalent_configuration() {
    let one: Value = serde_json::from_str(r#"{"limit": 10, "mode": {"b": 2, "a": 1}}"#).unwrap();
    let two: Value = serde_json::from_str(r#"{"mode":{"a":1,"b":2},"limit":10}"#).unwrap();
    let a = descriptor("x.counter", "Counter", "T", one, Determinism::Pure);
    let b = descriptor("x.counter", "Counter", "T", two, Determinism::Pure);
    assert_eq!(a.config_digest, b.config_digest);
    assert_eq!(a.fingerprint, b.fingerprint);
    assert_eq!(a.fingerprint, a.computed_fingerprint());
    // Built twice in one process, the built-in identities are identical.
    assert_eq!(
        Catalog::builtin()
            .find("orbit.counter")
            .unwrap()
            .descriptor(),
        Catalog::builtin()
            .find("orbit.counter")
            .unwrap()
            .descriptor()
    );
}

#[test]
fn every_identity_field_changes_the_fingerprint() {
    let base = descriptor("x.counter", "Counter", "T", json!({}), Determinism::Pure);
    let variants = [
        descriptor("x.counter-2", "Counter", "T", json!({}), Determinism::Pure),
        descriptor("x.counter", "Counter 2", "T", json!({}), Determinism::Pure),
        descriptor("x.counter", "Counter", "U", json!({}), Determinism::Pure),
        descriptor(
            "x.counter",
            "Counter",
            "T",
            json!({ "limit": 1 }),
            Determinism::Pure,
        ),
        descriptor(
            "x.counter",
            "Counter",
            "T",
            json!({}),
            Determinism::Uncontrolled,
        ),
        ImplementationDescriptor::new(
            ImplementationId::new("x.counter").unwrap(),
            "Counter",
            ProjectionKey::new("counter", 2).unwrap(),
            RUST_PROJECTION_ADAPTER,
            "T",
            json!({}),
            Determinism::Pure,
        )
        .unwrap(),
        ImplementationDescriptor::new(
            ImplementationId::new("x.counter").unwrap(),
            "Counter",
            ProjectionKey::new("counter", 1).unwrap(),
            "other-adapter",
            "T",
            json!({}),
            Determinism::Pure,
        )
        .unwrap(),
    ];
    for v in &variants {
        assert_ne!(v.fingerprint, base.fingerprint, "{v:?}");
    }
    // A tampered descriptor no longer matches its stored fingerprint.
    let mut tampered = base.clone();
    tampered.name = "Other".to_owned();
    assert_ne!(tampered.computed_fingerprint(), tampered.fingerprint);
}

#[test]
fn the_fingerprint_does_not_see_code_bodies() {
    // Documented limitation: identical registration metadata gives an
    // identical fingerprint, whatever the code does. Here two different
    // projections are registered under the same id, name and *type name*
    // string, which only a hand-built descriptor can do.
    let a = descriptor(
        "x.counter",
        "Counter",
        "same::Type",
        json!({}),
        Determinism::Pure,
    );
    let b = descriptor(
        "x.counter",
        "Counter",
        "same::Type",
        json!({}),
        Determinism::Pure,
    );
    assert_eq!(a.fingerprint, b.fingerprint);
}

#[test]
fn identifiers_are_validated_and_unique_in_a_catalog() {
    for bad in ["", "Upper", "-lead", "has space", "a/b", &"x".repeat(129)] {
        assert!(ImplementationId::new(bad).is_err(), "{bad:?}");
    }
    for good in ["a", "orbit.counter", "x_1-2.y", &"x".repeat(128)] {
        assert!(ImplementationId::new(good).is_ok(), "{good:?}");
    }
    assert!(ProjectionImplementation::new("ok", " ", Counter, Determinism::Pure).is_err());
    let mut catalog = Catalog::builtin();
    let dup = ProjectionImplementation::new(
        "orbit.counter",
        "Counter",
        CounterFloorAtZero,
        Determinism::Pure,
    )
    .unwrap();
    assert!(matches!(
        catalog.register(Arc::new(dup)),
        Err(LabError::DuplicateImplementation(_))
    ));
    assert_eq!(catalog.all().len(), 5);
}

#[test]
fn descriptors_round_trip_through_json() {
    let catalog = Catalog::builtin();
    for implementation in catalog.all() {
        let d = implementation.descriptor();
        let back: ImplementationDescriptor =
            serde_json::from_value(serde_json::to_value(d).unwrap()).unwrap();
        assert_eq!(&back, d);
        assert_eq!(back.computed_fingerprint(), d.fingerprint);
    }
}

#[test]
fn spec_digests_ignore_member_order_and_written_out_defaults() {
    let a = orbit_lab::DifferentialSpec::from_json(
        serde_json::from_str(r#"{"right":"b","left":"a","stream":"s"}"#).unwrap(),
    )
    .unwrap();
    let b = orbit_lab::DifferentialSpec::from_json(json!({
        "stream": "s", "left": "a", "right": "b", "from": 0, "to": null,
        "reference": null, "determinism_check": true
    }))
    .unwrap();
    assert_eq!(a.digest(), b.digest());
    let mut c = a.clone();
    c.determinism_check = false;
    assert_ne!(a.digest(), c.digest());
    let mut swapped = a.clone();
    std::mem::swap(&mut swapped.left, &mut swapped.right);
    assert_ne!(a.digest(), swapped.digest(), "sides are ordered");
}
