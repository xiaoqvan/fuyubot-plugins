//! SM3 哈希（GB/T 32905-2016），与参考实现逐位一致。
//!
//! a_bogus 用它哈希 query / body / UA。国标测试向量在
//! [`tests::matches_the_published_vectors`]，其余向量从 Python 参考实现对拍
//! （见 crate 根的 `sign-vectors.json`）。

const MASK: u32 = u32::MAX;

/// SM3 初始向量。
const IV: [u32; 8] = [
    0x7380_166F,
    0x4914_B2B9,
    0x1724_42D7,
    0xDA8A_0600,
    0xA96F_30BC,
    0x1631_38AA,
    0xE38D_EE4D,
    0xB0FB_0E4E,
];

const T0: u32 = 0x79CC_4519;
const T1: u32 = 0x7A87_9D8A;

fn rotl(value: u32, bits: u32) -> u32 {
    value.rotate_left(bits % 32)
}

fn ff(index: usize, x: u32, y: u32, z: u32) -> u32 {
    if index < 16 {
        x ^ y ^ z
    } else {
        (x & y) | (x & z) | (y & z)
    }
}

fn gg(index: usize, x: u32, y: u32, z: u32) -> u32 {
    if index < 16 {
        x ^ y ^ z
    } else {
        (x & y) | ((!x & MASK) & z)
    }
}

/// 消息扩展：16 个大端字 → 68 个 `w` + 64 个 `w1`。
fn expand(block: &[u8]) -> ([u32; 68], [u32; 64]) {
    let mut w = [0u32; 68];
    for (index, word) in w.iter_mut().take(16).enumerate() {
        *word = u32::from_be_bytes([
            block[index * 4],
            block[index * 4 + 1],
            block[index * 4 + 2],
            block[index * 4 + 3],
        ]);
    }
    for index in 16..68 {
        let mut x = w[index - 16] ^ w[index - 9] ^ rotl(w[index - 3], 15);
        x = x ^ rotl(x, 15) ^ rotl(x, 23);
        w[index] = (x ^ rotl(w[index - 13], 7) ^ w[index - 6]) & MASK;
    }
    let mut w1 = [0u32; 64];
    for index in 0..64 {
        w1[index] = (w[index] ^ w[index + 4]) & MASK;
    }
    (w, w1)
}

fn compress(state: [u32; 8], block: &[u8]) -> [u32; 8] {
    let (w, w1) = expand(block);
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
    for j in 0..64 {
        let t = if j < 16 { T0 } else { T1 };
        let ss1 = rotl(
            (rotl(a, 12).wrapping_add(e).wrapping_add(rotl(t, j as u32))) & MASK,
            7,
        );
        let ss2 = ss1 ^ rotl(a, 12);
        let tt1 = (ff(j, a, b, c)
            .wrapping_add(d)
            .wrapping_add(ss2)
            .wrapping_add(w1[j]))
            & MASK;
        let tt2 = (gg(j, e, f, g)
            .wrapping_add(h)
            .wrapping_add(ss1)
            .wrapping_add(w[j]))
            & MASK;
        d = c;
        c = rotl(b, 9);
        b = a;
        a = tt1;
        h = g;
        g = rotl(f, 19);
        f = e;
        e = (tt2 ^ rotl(tt2, 9) ^ rotl(tt2, 17)) & MASK;
    }
    let mixed = [a, b, c, d, e, f, g, h];
    let mut out = [0u32; 8];
    for index in 0..8 {
        out[index] = (state[index] ^ mixed[index]) & MASK;
    }
    out
}

/// 填充：`0x80` + 补零到 56 mod 64 + 8 字节大端比特长度。
fn pad(message: &[u8]) -> Vec<u8> {
    let bit_length = (message.len() as u64) * 8;
    let mut padded = message.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_length.to_be_bytes());
    padded
}

/// 32 字节 SM3 摘要。
pub fn sm3_hash(message: &[u8]) -> [u8; 32] {
    let mut state = IV;
    let padded = pad(message);
    for block in padded.as_chunks::<64>().0 {
        state = compress(state, block);
    }
    let mut out = [0u8; 32];
    for (index, word) in state.iter().enumerate() {
        out[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// 十六进制（小写）摘要。
pub fn sm3_hexdigest(message: &[u8]) -> String {
    sm3_hash(message)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 与 Python `sm3_to_array` 相同：str 按 UTF-8 哈希，返回 32 个整数。
pub fn sm3_to_array(data: &[u8]) -> [u8; 32] {
    sm3_hash(data)
}

#[cfg(test)]
mod tests {
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
}
