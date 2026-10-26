use super::{BoundedDeserializer, Budget, Limits, from_slice};
use serde::de::{self, DeserializeSeed, Deserializer, SeqAccess, Visitor};
use std::{cell::Cell, fmt};

fn with_budget<'de, S: DeserializeSeed<'de>>(
    bytes: &'de [u8],
    seed: S,
    limits: Limits,
) -> Result<S::Value, serde_yaml::Error> {
    let mut budget = Budget::new(limits);
    seed.deserialize(BoundedDeserializer {
        inner: serde_yaml::Deserializer::from_slice(bytes),
        budget: &mut budget,
        depth: 0,
        keys: None,
    })
}

struct StringCopies<'a>(&'a Cell<usize>);

impl<'de> DeserializeSeed<'de> for StringCopies<'_> {
    type Value = String;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<String, D::Error> {
        deserializer.deserialize_str(self)
    }
}

impl<'de> Visitor<'de> for StringCopies<'_> {
    type Value = String;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a string whose ownership is counted")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<String, E> {
        self.0.set(self.0.get() + 1);
        Ok(value.to_owned())
    }
}

struct ListCopies<'a>(&'a Cell<usize>);

impl<'de> DeserializeSeed<'de> for ListCopies<'_> {
    type Value = Vec<String>;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for ListCopies<'_> {
    type Value = Vec<String>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a sequence of counted strings")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        assert_eq!(
            sequence.size_hint(),
            None,
            "do not reserve an unchecked length"
        );
        let mut strings = Vec::new();
        while let Some(string) = sequence.next_element_seed(StringCopies(self.0))? {
            strings.push(string);
        }
        Ok(strings)
    }
}

#[test]
fn alias_text_is_checked_before_the_next_owning_string_visitor() {
    let scalar = "x".repeat(32 * 1024);
    let yaml = format!("[&large '{scalar}', {}]", ["*large"; 1023].join(", "));
    let copies = Cell::new(0);
    let error = with_budget(
        yaml.as_bytes(),
        ListCopies(&copies),
        Limits {
            nodes: 4096,
            text_bytes: scalar.len() * 2,
            depth: 8,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("expanded text"), "{error}");
    assert_eq!(
        copies.get(),
        2,
        "the first over-budget alias must never be copied"
    );
}

#[test]
fn numeric_looking_scalar_text_is_charged_as_the_requested_string() {
    // deserialize_any would turn this long lexical scalar into a small number,
    // undercounting the String allocation made by typed deserialization.
    let scalar = format!("0x{}1", "0".repeat(32 * 1024));
    let yaml = format!("[&large {scalar}, *large, *large]");
    let copies = Cell::new(0);
    let error = with_budget(
        yaml.as_bytes(),
        ListCopies(&copies),
        Limits {
            nodes: 64,
            text_bytes: scalar.len(),
            depth: 8,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("expanded text"), "{error}");
    assert_eq!(
        copies.get(),
        1,
        "numeric coercion must not bypass the byte budget"
    );
}

#[test]
fn alias_nodes_are_checked_before_forwarding_the_next_item() {
    let copies = Cell::new(0);
    let error = with_budget(
        b"[&item text, *item, *item, *item]",
        ListCopies(&copies),
        Limits {
            nodes: 3,
            text_bytes: 1024,
            depth: 8,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("node count"), "{error}");
    assert_eq!(copies.get(), 2, "the sequence itself consumes one node");
}

#[test]
fn exact_text_and_node_limits_are_admitted() {
    let copies = Cell::new(0);
    let strings = with_budget(
        b"[&item text, *item]",
        ListCopies(&copies),
        Limits {
            nodes: 3,
            text_bytes: 8,
            depth: 1,
        },
    )
    .unwrap();
    assert_eq!(strings, ["text", "text"]);
    assert_eq!(copies.get(), 2);
}

#[test]
fn all_mapping_keys_and_ignored_subtrees_share_the_budget() {
    #[derive(serde::Deserialize, Debug)]
    struct Empty {}
    let error = with_budget(
        b"ignored: [[[[null]]]]",
        std::marker::PhantomData::<Empty>,
        Limits {
            nodes: 128,
            text_bytes: 1024,
            depth: 3,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("nesting depth"), "{error}");
    let error = with_budget(
        b"a: [null, null, null, null]",
        std::marker::PhantomData::<Empty>,
        Limits {
            nodes: 5,
            text_bytes: 1024,
            depth: 8,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("node count"), "{error}");
    let error = with_budget(
        b"long_mapping_key: null",
        std::marker::PhantomData::<Empty>,
        Limits {
            nodes: 128,
            text_bytes: 4,
            depth: 8,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("expanded text"), "{error}");
}

#[test]
fn duplicate_mapping_keys_are_rejected_before_their_values_are_deserialized() {
    let error = from_slice::<std::collections::BTreeMap<String, String>>(
        b"key: original\nkey: [not a string]",
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("duplicate mapping key"),
        "{error}"
    );
}

#[test]
fn typed_yaml_scalar_and_tag_admission_matches_the_existing_schema() {
    use super::super::RawCatalog;
    for raw in [
        "schema_version: 1\nfirmware_root: 123",
        "schema_version: 1\nfirmware_root: null",
        "schema_version: 1\nfirmware_root: !!int 123",
        "schema_version: 1\nfirmware_root: &root !!str .\ninfo_filename: *root",
        "schema_version: 1\nfirmware_root: .\nboard_targets: {board: {firmware: app.uf2, esb_ota_kind: tracker}}",
        "schema_version: 1\nfirmware_root: .\nboard_targets: {board: {firmware: app.uf2, esb_ota_kind: !tracker null}}",
        "schema_version: !unexpected 1\nfirmware_root: .",
        "schema_version: !!str 1\nfirmware_root: .",
        "schema_version: 1\nfirmware_root: .\nboard_volume_labels: [1]",
        "schema_version: 1\nfirmware_root: .\nremote_firmware: {unexpected: true}",
    ] {
        assert_eq!(
            from_slice::<RawCatalog>(raw.as_bytes()).is_ok(),
            serde_yaml::from_slice::<RawCatalog>(raw.as_bytes()).is_ok(),
            "the budget must not replace typed YAML schema semantics: {raw}"
        );
    }
}

#[test]
fn oversized_input_is_rejected_before_calling_the_schema_deserializer() {
    struct Unreachable;
    impl<'de> serde::Deserialize<'de> for Unreachable {
        fn deserialize<D: Deserializer<'de>>(_: D) -> Result<Self, D::Error> {
            panic!("oversized input must not reach typed deserialization");
        }
    }
    let bytes = vec![b' '; super::MAX_INPUT_BYTES + 1];
    let error = from_slice::<Unreachable>(&bytes).err().unwrap();
    assert!(error.to_string().contains("input size"), "{error}");
}
