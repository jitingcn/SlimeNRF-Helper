use serde::{
    Deserialize,
    de::{
        self, DeserializeSeed, Deserializer, EnumAccess, MapAccess, SeqAccess, VariantAccess,
        Visitor,
    },
};
use std::{collections::BTreeSet, fmt};

/// Input events are bounded separately from the expanded, typed document.
pub(super) const MAX_INPUT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy)]
struct Limits {
    nodes: usize,
    text_bytes: usize,
    depth: usize,
}

// Allow substantial external catalogs and ordinary aliases without permitting a
// small YAML source to retain an arbitrarily large expanded tree.
const LIMITS: Limits = Limits {
    nodes: 16 * 1024,
    text_bytes: 2 * 1024 * 1024,
    depth: 32,
};

struct Budget {
    nodes_left: usize,
    text_left: usize,
    max_depth: usize,
}

impl Budget {
    fn new(limits: Limits) -> Self {
        Self {
            nodes_left: limits.nodes,
            text_left: limits.text_bytes,
            max_depth: limits.depth,
        }
    }

    fn check_depth<E: de::Error>(&self, depth: usize) -> Result<(), E> {
        if depth > self.max_depth {
            return Err(E::custom("configuration nesting depth exceeds limit"));
        }
        Ok(())
    }

    fn claim<E: de::Error>(&mut self, depth: usize, text_bytes: usize) -> Result<(), E> {
        self.check_depth(depth)?;
        if self.nodes_left == 0 {
            return Err(E::custom("configuration expanded node count exceeds limit"));
        }
        if text_bytes > self.text_left {
            return Err(E::custom("configuration expanded text exceeds limit"));
        }
        self.nodes_left -= 1;
        self.text_left -= text_bytes;
        Ok(())
    }
}

pub(super) fn from_slice<'de, T: Deserialize<'de>>(
    bytes: &'de [u8],
) -> Result<T, serde_yaml::Error> {
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(de::Error::custom("configuration input size exceeds limit"));
    }
    let mut budget = Budget::new(LIMITS);
    T::deserialize(BoundedDeserializer {
        inner: serde_yaml::Deserializer::from_slice(bytes),
        budget: &mut budget,
        depth: 0,
        keys: None,
    })
}

// Keep typed YAML deserialization: an intermediate Value changes scalar/tag
// coercions, and deserialize_any preflight loses numeric scalars' lexical size.
// YAML supplies borrowed scalar visits; charge before String, PathBuf or Serde's
// untagged/flatten Content visitors can retain copies, including alias replays.
struct BoundedDeserializer<'budget, D> {
    inner: D,
    budget: &'budget mut Budget,
    depth: usize,
    keys: Option<&'budget mut BTreeSet<String>>,
}

macro_rules! forward_deserialize {
    ($($method:ident $(($($arg:ident: $ty:ty),+))?;)*) => {
        $(fn $method<V: Visitor<'de>>(self, $($($arg: $ty,)*)? visitor: V) -> Result<V::Value, D::Error> {
            self.budget.check_depth(self.depth)?;
            self.inner.$method($($($arg,)*)? BoundedVisitor {
                inner: visitor,
                budget: self.budget,
                depth: self.depth,
                keys: self.keys,
            })
        })*
    };
}

impl<'de, D: Deserializer<'de>> Deserializer<'de> for BoundedDeserializer<'_, D> {
    type Error = D::Error;

    forward_deserialize! {
        deserialize_any;
        deserialize_bool;
        deserialize_i8;
        deserialize_i16;
        deserialize_i32;
        deserialize_i64;
        deserialize_i128;
        deserialize_u8;
        deserialize_u16;
        deserialize_u32;
        deserialize_u64;
        deserialize_u128;
        deserialize_f32;
        deserialize_f64;
        deserialize_char;
        deserialize_str;
        deserialize_string;
        deserialize_bytes;
        deserialize_byte_buf;
        deserialize_option;
        deserialize_unit;
        deserialize_unit_struct(name: &'static str);
        deserialize_newtype_struct(name: &'static str);
        deserialize_seq;
        deserialize_tuple(len: usize);
        deserialize_tuple_struct(name: &'static str, len: usize);
        deserialize_map;
        deserialize_struct(name: &'static str, fields: &'static [&'static str]);
        deserialize_enum(name: &'static str, variants: &'static [&'static str]);
        deserialize_identifier;
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, D::Error> {
        // The YAML deserializer otherwise skips an entire subtree without
        // visiting it, bypassing depth/node accounting for unknown fields.
        self.deserialize_any(visitor)
    }

    fn is_human_readable(&self) -> bool {
        self.inner.is_human_readable()
    }
}

struct BoundedVisitor<'budget, V> {
    inner: V,
    budget: &'budget mut Budget,
    depth: usize,
    keys: Option<&'budget mut BTreeSet<String>>,
}

impl<V> BoundedVisitor<'_, V> {
    fn claim_non_key<E: de::Error>(&mut self, text_bytes: usize) -> Result<(), E> {
        self.budget.claim(self.depth, text_bytes)?;
        if self.keys.is_some() {
            return Err(E::custom("configuration mapping keys must be strings"));
        }
        Ok(())
    }

    fn claim_string<E: de::Error>(&mut self, value: &str) -> Result<(), E> {
        self.budget.claim(self.depth, value.len())?;
        if let Some(keys) = &mut self.keys {
            if keys.contains(value) {
                return Err(E::custom("duplicate mapping key in configuration"));
            }
            keys.insert(value.to_owned());
        }
        Ok(())
    }
}

macro_rules! visit_primitive {
    ($($method:ident($ty:ty);)*) => {
        $(fn $method<E: de::Error>(mut self, value: $ty) -> Result<V::Value, E> {
            self.claim_non_key(0)?;
            self.inner.$method(value)
        })*
    };
}

impl<'de, V: Visitor<'de>> Visitor<'de> for BoundedVisitor<'_, V> {
    type Value = V::Value;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        self.inner.expecting(formatter)
    }

    visit_primitive! {
        visit_bool(bool);
        visit_i8(i8);
        visit_i16(i16);
        visit_i32(i32);
        visit_i64(i64);
        visit_i128(i128);
        visit_u8(u8);
        visit_u16(u16);
        visit_u32(u32);
        visit_u64(u64);
        visit_u128(u128);
        visit_f32(f32);
        visit_f64(f64);
        visit_char(char);
    }

    fn visit_str<E: de::Error>(mut self, value: &str) -> Result<V::Value, E> {
        self.claim_string(value)?;
        self.inner.visit_str(value)
    }

    fn visit_borrowed_str<E: de::Error>(mut self, value: &'de str) -> Result<V::Value, E> {
        self.claim_string(value)?;
        self.inner.visit_borrowed_str(value)
    }

    fn visit_string<E: de::Error>(mut self, value: String) -> Result<V::Value, E> {
        self.claim_string(&value)?;
        self.inner.visit_string(value)
    }

    fn visit_bytes<E: de::Error>(mut self, value: &[u8]) -> Result<V::Value, E> {
        self.claim_non_key(value.len())?;
        self.inner.visit_bytes(value)
    }

    fn visit_borrowed_bytes<E: de::Error>(mut self, value: &'de [u8]) -> Result<V::Value, E> {
        self.claim_non_key(value.len())?;
        self.inner.visit_borrowed_bytes(value)
    }

    fn visit_byte_buf<E: de::Error>(mut self, value: Vec<u8>) -> Result<V::Value, E> {
        self.claim_non_key(value.len())?;
        self.inner.visit_byte_buf(value)
    }

    fn visit_none<E: de::Error>(mut self) -> Result<V::Value, E> {
        self.claim_non_key(0)?;
        self.inner.visit_none()
    }

    fn visit_unit<E: de::Error>(mut self) -> Result<V::Value, E> {
        self.claim_non_key(0)?;
        self.inner.visit_unit()
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<V::Value, D::Error> {
        self.inner.visit_some(BoundedDeserializer {
            inner: deserializer,
            budget: self.budget,
            depth: self.depth,
            keys: self.keys,
        })
    }

    fn visit_newtype_struct<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<V::Value, D::Error> {
        self.inner.visit_newtype_struct(BoundedDeserializer {
            inner: deserializer,
            budget: self.budget,
            depth: self.depth,
            keys: self.keys,
        })
    }

    fn visit_seq<A: SeqAccess<'de>>(mut self, sequence: A) -> Result<V::Value, A::Error> {
        self.claim_non_key(0)?;
        self.inner.visit_seq(BoundedSeqAccess {
            inner: sequence,
            budget: self.budget,
            depth: self.depth + 1,
        })
    }

    fn visit_map<A: MapAccess<'de>>(mut self, mapping: A) -> Result<V::Value, A::Error> {
        self.claim_non_key(0)?;
        self.inner.visit_map(BoundedMapAccess {
            inner: mapping,
            budget: self.budget,
            depth: self.depth + 1,
            keys: BTreeSet::new(),
        })
    }

    fn visit_enum<A: EnumAccess<'de>>(self, value: A) -> Result<V::Value, A::Error> {
        if self.keys.is_some() {
            return Err(de::Error::custom(
                "configuration mapping keys must be strings",
            ));
        }
        self.inner.visit_enum(BoundedEnumAccess {
            inner: value,
            budget: self.budget,
            depth: self.depth,
        })
    }
}

struct BoundedSeed<'budget, S> {
    inner: S,
    budget: &'budget mut Budget,
    depth: usize,
    keys: Option<&'budget mut BTreeSet<String>>,
}

impl<'de, S: DeserializeSeed<'de>> DeserializeSeed<'de> for BoundedSeed<'_, S> {
    type Value = S::Value;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<S::Value, D::Error> {
        self.budget.check_depth(self.depth)?;
        self.inner.deserialize(BoundedDeserializer {
            inner: deserializer,
            budget: self.budget,
            depth: self.depth,
            keys: self.keys,
        })
    }
}

struct BoundedSeqAccess<'budget, A> {
    inner: A,
    budget: &'budget mut Budget,
    depth: usize,
}

impl<'de, A: SeqAccess<'de>> SeqAccess<'de> for BoundedSeqAccess<'_, A> {
    type Error = A::Error;

    fn next_element_seed<S: DeserializeSeed<'de>>(
        &mut self,
        seed: S,
    ) -> Result<Option<S::Value>, A::Error> {
        self.inner.next_element_seed(BoundedSeed {
            inner: seed,
            budget: self.budget,
            depth: self.depth,
            keys: None,
        })
    }

    fn size_hint(&self) -> Option<usize> {
        None
    }
}

struct BoundedMapAccess<'budget, A> {
    inner: A,
    budget: &'budget mut Budget,
    depth: usize,
    keys: BTreeSet<String>,
}

impl<'de, A: MapAccess<'de>> MapAccess<'de> for BoundedMapAccess<'_, A> {
    type Error = A::Error;

    fn next_key_seed<S: DeserializeSeed<'de>>(
        &mut self,
        seed: S,
    ) -> Result<Option<S::Value>, A::Error> {
        self.inner.next_key_seed(BoundedSeed {
            inner: seed,
            budget: self.budget,
            depth: self.depth,
            keys: Some(&mut self.keys),
        })
    }

    fn next_value_seed<S: DeserializeSeed<'de>>(&mut self, seed: S) -> Result<S::Value, A::Error> {
        self.inner.next_value_seed(BoundedSeed {
            inner: seed,
            budget: self.budget,
            depth: self.depth,
            keys: None,
        })
    }

    fn size_hint(&self) -> Option<usize> {
        None
    }
}

struct BoundedEnumAccess<'budget, A> {
    inner: A,
    budget: &'budget mut Budget,
    depth: usize,
}

impl<'de, 'budget, A: EnumAccess<'de>> EnumAccess<'de> for BoundedEnumAccess<'budget, A> {
    type Error = A::Error;
    type Variant = BoundedVariantAccess<'budget, A::Variant>;

    fn variant_seed<S: DeserializeSeed<'de>>(
        self,
        seed: S,
    ) -> Result<(S::Value, Self::Variant), A::Error> {
        let (value, variant) = self.inner.variant_seed(BoundedSeed {
            inner: seed,
            budget: &mut *self.budget,
            depth: self.depth,
            keys: None,
        })?;
        Ok((
            value,
            BoundedVariantAccess {
                inner: variant,
                budget: self.budget,
                depth: self.depth + 1,
            },
        ))
    }
}

struct BoundedVariantAccess<'budget, A> {
    inner: A,
    budget: &'budget mut Budget,
    depth: usize,
}

impl<'de, A: VariantAccess<'de>> VariantAccess<'de> for BoundedVariantAccess<'_, A> {
    type Error = A::Error;

    fn unit_variant(self) -> Result<(), A::Error> {
        self.inner.unit_variant()
    }

    fn newtype_variant_seed<S: DeserializeSeed<'de>>(self, seed: S) -> Result<S::Value, A::Error> {
        self.inner.newtype_variant_seed(BoundedSeed {
            inner: seed,
            budget: self.budget,
            depth: self.depth,
            keys: None,
        })
    }

    fn tuple_variant<V: Visitor<'de>>(self, len: usize, visitor: V) -> Result<V::Value, A::Error> {
        self.budget.check_depth(self.depth)?;
        self.inner.tuple_variant(
            len,
            BoundedVisitor {
                inner: visitor,
                budget: self.budget,
                depth: self.depth,
                keys: None,
            },
        )
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, A::Error> {
        self.budget.check_depth(self.depth)?;
        self.inner.struct_variant(
            fields,
            BoundedVisitor {
                inner: visitor,
                budget: self.budget,
                depth: self.depth,
                keys: None,
            },
        )
    }
}

#[cfg(test)]
mod tests;
