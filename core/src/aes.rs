//! AES-128-CBC 解密（HLS 的 `EXT-X-KEY:METHOD=AES-128` 就是它）。
//!
//! 为什么自己写：内核"零第三方依赖"的约束。引 `aes` + `cbc` 两个 crate 会让安卓交叉编译
//! 多背两个包，而这里只需要"解"，不需要"加"——固定 128 位、CBC、单向解密，百来行就够，
//! 而且有 FIPS-197 / SP 800-38A 的官方向量兜底。
//!
//! 安全性说明：这是**格式解密**，不是安全边界。HLS 的密钥本来就是明文交给客户端的，
//! 实现里不做任何"抵侧信道"处理（S 盒是现算的，不做常量时间查表）。

use crate::error::{Error, Result};

/// GF(2^8) 乘法，模多项式 0x11B
fn gf_mul(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0u8;
    for _ in 0..8 {
        if b & 1 != 0 {
            p ^= a;
        }
        let hi = a & 0x80;
        a <<= 1;
        if hi != 0 {
            a ^= 0x1b;
        }
        b >>= 1;
    }
    p
}

/// GF(2^8) 求逆：a^254 == a^-1（费马小定理），a=0 时定义为 0
fn gf_inv(a: u8) -> u8 {
    if a == 0 {
        return 0;
    }
    let mut r = 1u8;
    let mut base = a;
    let mut e = 254u32;
    while e > 0 {
        if e & 1 != 0 {
            r = gf_mul(r, base);
        }
        base = gf_mul(base, base);
        e >>= 1;
    }
    r
}

fn rotl8(x: u8, n: u32) -> u8 {
    (x << n) | (x >> (8 - n))
}

/// S 盒按定义现算，省掉 256 个魔数常量
fn build_tables() -> ([u8; 256], [u8; 256]) {
    let mut sbox = [0u8; 256];
    let mut inv = [0u8; 256];
    for x in 0..=255u8 {
        let a = gf_inv(x);
        let s = a ^ rotl8(a, 1) ^ rotl8(a, 2) ^ rotl8(a, 3) ^ rotl8(a, 4) ^ 0x63;
        sbox[x as usize] = s;
        inv[s as usize] = x;
    }
    (sbox, inv)
}

const RCON: [u8; 11] = [0x00, 0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36];

/// AES-128 密钥扩展：11 组 16 字节轮密钥
fn expand_key(key: &[u8; 16], sbox: &[u8; 256]) -> [[u8; 16]; 11] {
    let mut w = [[0u8; 4]; 44];
    for i in 0..4 {
        w[i] = [key[4 * i], key[4 * i + 1], key[4 * i + 2], key[4 * i + 3]];
    }
    for i in 4..44 {
        let mut t = w[i - 1];
        if i % 4 == 0 {
            t = [t[1], t[2], t[3], t[0]]; // RotWord
            for b in t.iter_mut() {
                *b = sbox[*b as usize]; // SubWord
            }
            t[0] ^= RCON[i / 4];
        }
        for j in 0..4 {
            w[i][j] = w[i - 4][j] ^ t[j];
        }
    }
    let mut rk = [[0u8; 16]; 11];
    for r in 0..11 {
        for j in 0..4 {
            for k in 0..4 {
                rk[r][4 * j + k] = w[4 * r + j][k];
            }
        }
    }
    rk
}

fn add_round_key(state: &mut [u8; 16], rk: &[u8; 16]) {
    for i in 0..16 {
        state[i] ^= rk[i];
    }
}

fn inv_sub_bytes(state: &mut [u8; 16], inv: &[u8; 256]) {
    for b in state.iter_mut() {
        *b = inv[*b as usize];
    }
}

/// 状态按列存：state[c*4 + r]
fn inv_shift_rows(state: &mut [u8; 16]) {
    let s = *state;
    for c in 0..4 {
        for r in 0..4 {
            state[c * 4 + r] = s[((c + 4 - r) % 4) * 4 + r];
        }
    }
}

fn inv_mix_columns(state: &mut [u8; 16]) {
    for c in 0..4 {
        let a = [state[c * 4], state[c * 4 + 1], state[c * 4 + 2], state[c * 4 + 3]];
        let r0 = gf_mul(a[0], 0x0e) ^ gf_mul(a[1], 0x0b) ^ gf_mul(a[2], 0x0d) ^ gf_mul(a[3], 0x09);
        let r1 = gf_mul(a[0], 0x09) ^ gf_mul(a[1], 0x0e) ^ gf_mul(a[2], 0x0b) ^ gf_mul(a[3], 0x0d);
        let r2 = gf_mul(a[0], 0x0d) ^ gf_mul(a[1], 0x09) ^ gf_mul(a[2], 0x0e) ^ gf_mul(a[3], 0x0b);
        let r3 = gf_mul(a[0], 0x0b) ^ gf_mul(a[1], 0x0d) ^ gf_mul(a[2], 0x09) ^ gf_mul(a[3], 0x0e);
        state[c * 4] = r0;
        state[c * 4 + 1] = r1;
        state[c * 4 + 2] = r2;
        state[c * 4 + 3] = r3;
    }
}

/// 单分组解密
pub fn decrypt_block(block: &[u8; 16], round_keys: &[[u8; 16]; 11], inv: &[u8; 256]) -> [u8; 16] {
    let mut s = *block;
    add_round_key(&mut s, &round_keys[10]);
    for r in (1..10).rev() {
        inv_shift_rows(&mut s);
        inv_sub_bytes(&mut s, inv);
        add_round_key(&mut s, &round_keys[r]);
        inv_mix_columns(&mut s);
    }
    inv_shift_rows(&mut s);
    inv_sub_bytes(&mut s, inv);
    add_round_key(&mut s, &round_keys[0]);
    s
}

/// CBC 解密，不去填充（调用方自己决定怎么处理尾部）
pub fn cbc_decrypt_raw(key: &[u8; 16], iv: &[u8; 16], data: &[u8]) -> Result<Vec<u8>> {
    if data.is_empty() || data.len() % 16 != 0 {
        return Err(Error::Platform(format!(
            "AES-128 密文长度必须是 16 的倍数（收到 {} 字节）",
            data.len()
        )));
    }
    let (sbox, inv) = build_tables();
    let rk = expand_key(key, &sbox);
    let mut out = Vec::with_capacity(data.len());
    let mut prev = *iv;
    for chunk in data.chunks(16) {
        let mut block = [0u8; 16];
        block.copy_from_slice(chunk);
        let plain = decrypt_block(&block, &rk, &inv);
        for i in 0..16 {
            out.push(plain[i] ^ prev[i]);
        }
        prev = block;
    }
    Ok(out)
}

/// CBC 解密 + 去 PKCS#7 填充。
/// 尾部没填充时不报错（HLS 里确实存在没填充的切片），只把非法填充当作"原样返回"。
pub fn cbc_decrypt(key: &[u8; 16], iv: &[u8; 16], data: &[u8]) -> Result<Vec<u8>> {
    let mut out = cbc_decrypt_raw(key, iv, data)?;
    if let Some(&pad) = out.last() {
        let n = pad as usize;
        if n >= 1 && n <= 16 && n <= out.len() && out[out.len() - n..].iter().all(|b| *b == pad) {
            out.truncate(out.len() - n);
        }
    }
    Ok(out)
}

/// HLS 的 IV 约定：`EXT-X-KEY` 给了 IV 就用它（16 字节十六进制，允许 0x 前缀），
/// 没给就用分片的**媒体序号**当 IV（RFC 8216 §5.2）。
pub fn parse_iv(hex: &str) -> Option<[u8; 16]> {
    let s = hex.trim().trim_start_matches("0x").trim_start_matches("0X");
    if s.is_empty() || s.len() > 32 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let padded = format!("{:0>32}", s);
    let mut iv = [0u8; 16];
    for i in 0..16 {
        iv[i] = u8::from_str_radix(&padded[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(iv)
}

pub fn sequence_iv(seq: u64) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[8..].copy_from_slice(&seq.to_be_bytes());
    iv
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        let clean: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        clean.as_bytes()
            .chunks(2)
            .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
            .collect()
    }

    /// FIPS-197 附录 C.1 的官方 AES-128 向量
    #[test]
    fn decrypts_fips197_vector() {
        let key: [u8; 16] = hex("000102030405060708090a0b0c0d0e0f").try_into().unwrap();
        let block: [u8; 16] = hex("69c4e0d86a7b0430d8cdb78070b4c55a").try_into().unwrap();
        let (sbox, inv) = build_tables();
        let rk = expand_key(&key, &sbox);
        let plain = decrypt_block(&block, &rk, &inv);
        assert_eq!(plain.to_vec(), hex("00112233445566778899aabbccddeeff"));
    }

    /// NIST SP 800-38A F.2.2 —— AES-128-CBC 四分组
    #[test]
    fn decrypts_sp800_38a_cbc_vector() {
        let key: [u8; 16] = hex("2b7e151628aed2a6abf7158809cf4f3c").try_into().unwrap();
        let iv: [u8; 16] = hex("000102030405060708090a0b0c0d0e0f").try_into().unwrap();
        let ct = hex("7649abac8119b246cee98e9b12e9197d5086cb9b507219ee95db113a917678b273bed6b8e3c1743b7116e69e222295163ff1caa1681fac09120eca307586e1a7");
        let raw = cbc_decrypt_raw(&key, &iv, &ct).unwrap();
        let expected = hex("6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e5130c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c3710");
        assert_eq!(raw, expected);
    }

    /// 单分组 CBC 向量走完整入口（含填充处理）
    #[test]
    fn decrypts_single_block_via_public_api() {
        let key: [u8; 16] = hex("2b7e151628aed2a6abf7158809cf4f3c").try_into().unwrap();
        let iv: [u8; 16] = hex("000102030405060708090a0b0c0d0e0f").try_into().unwrap();
        let a = cbc_decrypt(&key, &iv, &hex("7649abac8119b246cee98e9b12e9197d")).unwrap();
        assert_eq!(a, hex("6bc1bee22e409f96e93d7e117393172a"));
    }

    #[test]
    fn rejects_bad_length() {
        let key = [0u8; 16];
        let iv = [0u8; 16];
        assert!(cbc_decrypt(&key, &iv, &[1, 2, 3]).is_err());
        assert!(cbc_decrypt(&key, &iv, &[]).is_err());
    }

    #[test]
    fn parses_iv_hex_variants() {
        assert_eq!(parse_iv("0x0102"), Some(sequence_iv(0x0102)));
        assert_eq!(parse_iv("0102"), Some(sequence_iv(0x0102)));
        assert_eq!(parse_iv(""), None);
        assert_eq!(parse_iv("not-hex"), None);
        assert_eq!(
            parse_iv("0x000102030405060708090a0b0c0d0e0f"),
            Some([0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f])
        );
    }

    #[test]
    fn sequence_iv_is_big_endian_in_tail() {
        let iv = sequence_iv(258);
        assert_eq!(&iv[..14], &[0u8; 14]);
        assert_eq!(&iv[14..], &[0x01, 0x02]);
    }
}
