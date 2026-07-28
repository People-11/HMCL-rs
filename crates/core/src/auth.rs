use md5::{Digest, Md5};
use uuid::Uuid;

pub mod authlib_injector;
pub mod microsoft;

/// 对应 Java `UUID.nameUUIDFromBytes(("OfflinePlayer:" + name).getBytes(UTF_8))`。
///
/// 注意：这**不是**标准的 UUID v3（标准 v3 是 `MD5(namespace_uuid_bytes ++ name)`）。
/// Java 这个方法直接对传入的字节数组算 MD5，不会拼 namespace UUID——所以不能用
/// `uuid` crate 的 `Uuid::new_v3(namespace, name)`，得自己算 MD5 之后手动按
/// RFC 4122 设置 version/variant 位。必须和 Java 位对位一致，否则同一个用户名在
/// 两边启动器里生成的 UUID 不一样。
pub fn offline_player_uuid(username: &str) -> Uuid {
    let mut hasher = Md5::new();
    hasher.update(format!("OfflinePlayer:{username}").as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest);
    bytes[6] = (bytes[6] & 0x0f) | 0x30; // version 3
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // variant RFC 4122
    Uuid::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_a_deterministic_rfc4122_v3_uuid() {
        let uuid = offline_player_uuid("Steve");
        assert_eq!(uuid.get_version_num(), 3);
        assert_eq!(
            uuid.as_bytes()[8] & 0xc0,
            0x80,
            "variant bits must be RFC 4122 (10xxxxxx)"
        );
        assert_eq!(
            uuid,
            offline_player_uuid("Steve"),
            "same username must always derive the same uuid"
        );
        assert_ne!(
            uuid,
            offline_player_uuid("Alex"),
            "different usernames must derive different uuids"
        );
    }

    #[test]
    fn matches_independently_computed_md5_with_version_bits_applied() {
        assert_eq!(
            offline_player_uuid("Steve").to_string(),
            "5627dd98-e6be-3c21-b8a8-e92344183641"
        );
    }
}
