//! The bundled interface definitions: hashes against the published values,
//! and the checked-in generated code against the generator.

use std::path::Path;

use ruro_ros2::hash::{hashable_description, type_hash};
use ruro_ros2::idl::TypeName;

#[test]
fn every_bundled_hash_matches_the_published_value() {
    let interfaces = ruro_ros2::bundled_interfaces();
    let vectors = include_str!("data/jazzy_type_hashes.txt");
    let mut checked = 0;
    for line in vectors
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let (name, expected) = line.split_once(' ').unwrap();
        let name = TypeName::parse(name).unwrap();
        let actual = type_hash(&interfaces, &name).unwrap();
        assert_eq!(
            actual,
            expected,
            "{name}\n{}",
            hashable_description(&interfaces, &name).unwrap()
        );
        checked += 1;
    }
    assert_eq!(
        checked,
        interfaces.messages.len(),
        "a vector per bundled type"
    );
}

#[test]
fn generated_code_is_current() {
    let code = ruro_ros2::codegen::generate(
        &ruro_ros2::bundled_interfaces(),
        &ruro_ros2::bundled_codegen_options(),
    )
    .unwrap();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/msgs.rs");
    if std::env::var_os("RURO_REGEN").is_some() {
        std::fs::write(&path, &code).unwrap();
        return;
    }
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert!(
        on_disk == code,
        "src/msgs.rs is stale; regenerate with RURO_REGEN=1 cargo test -p ruro-ros2 --test bundled"
    );
}
