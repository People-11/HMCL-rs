use std::fmt;

use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TypedId {
    pub prefix: &'static str,
    pub uuid: Uuid,
}

impl TypedId {
    pub fn new(prefix: &'static str, uuid: Uuid) -> TypedId {
        TypedId { prefix, uuid }
    }

    pub fn generate(prefix: &'static str) -> TypedId {
        TypedId {
            prefix,
            uuid: Uuid::now_v7(),
        }
    }

    pub fn parse(prefix: &'static str, s: &str) -> Option<TypedId> {
        let rest = s.strip_prefix(prefix)?.strip_prefix(':')?;
        Uuid::parse_str(rest)
            .ok()
            .map(|uuid| TypedId { prefix, uuid })
    }
}

impl fmt::Display for TypedId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.prefix, self.uuid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_display_and_parse() {
        let id = TypedId::generate("account");
        let text = id.to_string();
        assert!(text.starts_with("account:"));
        assert_eq!(TypedId::parse("account", &text), Some(id));
    }

    #[test]
    fn rejects_wrong_prefix_or_malformed_uuid() {
        assert_eq!(
            TypedId::parse("account", "game-directory:not-a-real-uuid-at-all"),
            None
        );
        let real = TypedId::generate("game-directory").to_string();
        assert_eq!(
            TypedId::parse("account", &real),
            None,
            "prefix mismatch must fail even with a valid uuid"
        );
    }

    #[test]
    fn generated_ids_are_v7() {
        let id = TypedId::generate("account");
        assert_eq!(id.uuid.get_version_num(), 7);
    }
}
