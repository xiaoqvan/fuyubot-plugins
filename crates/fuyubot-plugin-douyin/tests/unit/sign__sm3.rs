use super::*;

/// GB/T 32905-2016 附录 A 的公开测试向量。
#[test]
fn matches_the_published_vectors() {
    assert_eq!(
        sm3_hexdigest(b"abc"),
        "66c7f0f462eeedd9d1f2d46bdc10e4e24167c4875cf2f7a2297da02b8f4ba8e0"
    );
    assert_eq!(
        sm3_hexdigest(&b"abcd".repeat(16)),
        "debe9ff92275b8a138604889c18e5a4d6fdb70e5387e5765293dcba39c0c5732"
    );
}

/// 55 / 56 / 64 字节走三种填充路径，不能撞摘要。
#[test]
fn pads_across_a_block_boundary() {
    for size in [0usize, 55, 56, 57, 63, 64, 65, 119, 120] {
        assert_eq!(sm3_hash(&b"x".repeat(size)).len(), 32);
    }
    assert_ne!(sm3_hash(&b"x".repeat(56)), sm3_hash(&b"x".repeat(57)));
}
