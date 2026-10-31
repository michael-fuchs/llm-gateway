//! The services allowed to call the gateway, each with its own key.

use sha2::{Digest, Sha256};

/// The callers and their keys. Keys are kept only as SHA-256 digests, and a presented key is
/// compared by its digest, so the comparison reveals nothing about a stored key.
#[derive(Clone, Default)]
pub struct Callers(Vec<(String, [u8; 32])>);

impl Callers {
    /// Parses callers like `mnemograph=<key>`. Names must be unique, and so must keys, so that a
    /// key identifies exactly one caller.
    pub fn parse<S: AsRef<str>>(entries: &[S]) -> Result<Self, String> {
        let mut callers = Callers::default();
        for entry in entries {
            let (name, key) = entry
                .as_ref()
                .split_once('=')
                .ok_or("a caller must be given as <name>=<key>")?;
            let name = name.trim();
            if !crate::is_name(name) {
                return Err(format!(
                    "{name:?} is not a caller name (lowercase letters, digits, - and _)"
                ));
            }
            if key.len() < 16 {
                return Err(format!(
                    "the key of caller {name:?} is shorter than 16 characters"
                ));
            }
            let digest = digest(key);
            if callers.0.iter().any(|(n, _)| n == name) {
                return Err(format!("the caller {name:?} is given more than once"));
            }
            if callers.0.iter().any(|(_, d)| *d == digest) {
                return Err(format!("the caller {name:?} shares its key with another"));
            }
            callers.0.push((name.to_string(), digest));
        }
        Ok(callers)
    }

    /// The name of the caller whose key this is.
    pub fn authenticate(&self, key: &str) -> Option<&str> {
        let digest = digest(key);
        self.0
            .iter()
            .find(|(_, d)| *d == digest)
            .map(|(name, _)| name.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for Callers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.0.iter().map(|(n, _)| n))
            .finish()
    }
}

fn digest(key: &str) -> [u8; 32] {
    Sha256::digest(key.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authenticates_by_key() {
        let callers =
            Callers::parse(&["mnemograph=0123456789abcdef", "documents=fedcba9876543210"]).unwrap();
        assert_eq!(callers.authenticate("0123456789abcdef"), Some("mnemograph"));
        assert_eq!(callers.authenticate("fedcba9876543210"), Some("documents"));
        assert_eq!(callers.authenticate("0123456789abcdeX"), None);
        assert_eq!(callers.authenticate(""), None);
    }

    #[test]
    fn rejects_bad_callers() {
        assert!(Callers::parse(&["mnemograph"]).is_err());
        assert!(Callers::parse(&["mnemograph=short"]).is_err());
        assert!(Callers::parse(&["Mnemo Graph=0123456789abcdef"]).is_err());
        assert!(Callers::parse(&["a=0123456789abcdef", "a=fedcba9876543210"]).is_err());
        assert!(Callers::parse(&["a=0123456789abcdef", "b=0123456789abcdef"]).is_err());
    }

    #[test]
    fn debug_shows_no_keys() {
        let callers = Callers::parse(&["mnemograph=0123456789abcdef"]).unwrap();
        assert_eq!(format!("{callers:?}"), r#"["mnemograph"]"#);
    }
}
