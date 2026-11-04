//! One address or several, in the order to try them.
//!
//! A leaf's `[home] address` and home's `[daemon] listen` both take either a
//! string or a list. A leaf lists where home is from wherever this machine
//! might be (the LAN name first, the tailnet address after); home lists each
//! interface it answers on. Trust never depends on the address: every
//! connection pins the peer's certificate, so an address is only routing.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Addresses(Vec<String>);

impl Addresses {
    pub fn one(address: impl Into<String>) -> Self {
        Self(vec![address.into()])
    }

    /// In the given order, duplicates dropped, the default port applied.
    /// Empty is an error: a list that names nowhere is a config mistake.
    pub fn new(list: Vec<String>) -> std::result::Result<Self, String> {
        let mut out: Vec<String> = Vec::with_capacity(list.len());
        for a in list {
            let a = a.trim();
            if a.is_empty() {
                return Err("an address must not be empty".into());
            }
            let a = super::with_port(a);
            if !out.contains(&a) {
                out.push(a);
            }
        }
        if out.is_empty() {
            return Err("at least one address is needed".into());
        }
        Ok(Self(out))
    }

    pub fn as_slice(&self) -> &[String] {
        &self.0
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The first one: what a single-address caller would have used.
    pub fn first(&self) -> &str {
        &self.0[0]
    }
}

impl std::fmt::Display for Addresses {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.join(", "))
    }
}

/// One address, as written: no port applied, no trimming.
impl From<&str> for Addresses {
    fn from(s: &str) -> Self {
        Self::one(s)
    }
}

impl From<String> for Addresses {
    fn from(s: String) -> Self {
        Self::one(s)
    }
}

/// On disk: a bare string when there is one, a list otherwise.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl Serialize for Addresses {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self.0.as_slice() {
            [one] => OneOrMany::One(one.clone()).serialize(s),
            many => OneOrMany::Many(many.to_vec()).serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for Addresses {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let list = match OneOrMany::deserialize(d)? {
            OneOrMany::One(s) => vec![s],
            OneOrMany::Many(v) => v,
        };
        Addresses::new(list).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Doc {
        address: Addresses,
    }

    #[test]
    fn a_string_or_a_list_both_read_and_write_back_the_same_shape() {
        let one: Doc = toml::from_str("address = \"home:7750\"\n").unwrap();
        assert_eq!(one.address, Addresses::one("home:7750"));
        assert_eq!(toml::to_string(&one).unwrap(), "address = \"home:7750\"\n");

        let many: Doc = toml::from_str("address = [\"a.local\", \"100.64.0.1:7751\"]\n").unwrap();
        assert_eq!(
            many.address.as_slice(),
            ["a.local:7750".to_string(), "100.64.0.1:7751".to_string()]
        );
        let text = toml::to_string(&many).unwrap();
        assert_eq!(text, "address = [\"a.local:7750\", \"100.64.0.1:7751\"]\n");
        assert_eq!(toml::from_str::<Doc>(&text).unwrap(), many);
    }

    #[test]
    fn order_is_kept_duplicates_dropped_and_empties_refused() {
        let a = Addresses::new(vec![
            "b:1".into(),
            " a ".into(),
            "b:1".into(),
            "a:7750".into(),
        ])
        .unwrap();
        assert_eq!(a.as_slice(), ["b:1".to_string(), "a:7750".to_string()]);
        assert_eq!(a.first(), "b:1");
        assert_eq!(a.to_string(), "b:1, a:7750");
        assert_eq!(a.len(), 2);
        assert!(!a.is_empty());

        assert!(Addresses::new(vec![]).unwrap_err().contains("at least one"));
        assert!(
            Addresses::new(vec!["".into()])
                .unwrap_err()
                .contains("empty")
        );
        let err = toml::from_str::<Doc>("address = []\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("at least one"), "{err}");
        let err = toml::from_str::<Doc>("address = 7\n")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("untagged") || err.contains("data did not match"),
            "{err}"
        );
    }
}
