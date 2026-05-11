//! DeepSeekHashV1 + PoW solver。
//!
//! 移植自 ds2api `pow/deepseek_hash.go` 与 `pow/deepseek_pow.go`：
//! Keccak-f[1600] 但只跑 round 1..23（跳过 round 0），rate=136。
//!
//! 用法：
//! ```ignore
//! let challenge = DeepseekChallenge { algorithm, challenge, salt, signature, target_path, expire_at, difficulty };
//! let header = solve_and_build_header(&challenge)?;  // 直接用作 x-ds-pow-response 头
//! ```
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use common::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use serde_json::json;

const RC: [u64; 24] = [
    0x0000000000000001,
    0x0000000000008082,
    0x800000000000808A,
    0x8000000080008000,
    0x000000000000808B,
    0x0000000080000001,
    0x8000000080008081,
    0x8000000000008009,
    0x000000000000008A,
    0x0000000000000088,
    0x0000000080008009,
    0x000000008000000A,
    0x000000008000808B,
    0x800000000000008B,
    0x8000000000008089,
    0x8000000000008003,
    0x8000000000008002,
    0x8000000000000080,
    0x000000000000800A,
    0x800000008000000A,
    0x8000000080008081,
    0x8000000000008080,
    0x0000000080000001,
    0x8000000080008008,
];

#[inline(always)]
fn rotl64(v: u64, k: u32) -> u64 {
    v.rotate_left(k)
}

/// Keccak-f[1600] with rounds 1..24（跳过 round 0），与 DeepSeek WASM 的实现一致。
fn keccak_f23(s: &mut [u64; 25]) {
    let (mut a0, mut a1, mut a2, mut a3, mut a4) = (s[0], s[1], s[2], s[3], s[4]);
    let (mut a5, mut a6, mut a7, mut a8, mut a9) = (s[5], s[6], s[7], s[8], s[9]);
    let (mut a10, mut a11, mut a12, mut a13, mut a14) = (s[10], s[11], s[12], s[13], s[14]);
    let (mut a15, mut a16, mut a17, mut a18, mut a19) = (s[15], s[16], s[17], s[18], s[19]);
    let (mut a20, mut a21, mut a22, mut a23, mut a24) = (s[20], s[21], s[22], s[23], s[24]);

    for r in 1..24 {
        let c0 = a0 ^ a5 ^ a10 ^ a15 ^ a20;
        let c1 = a1 ^ a6 ^ a11 ^ a16 ^ a21;
        let c2 = a2 ^ a7 ^ a12 ^ a17 ^ a22;
        let c3 = a3 ^ a8 ^ a13 ^ a18 ^ a23;
        let c4 = a4 ^ a9 ^ a14 ^ a19 ^ a24;
        let d0 = c4 ^ rotl64(c1, 1);
        let d1 = c0 ^ rotl64(c2, 1);
        let d2 = c1 ^ rotl64(c3, 1);
        let d3 = c2 ^ rotl64(c4, 1);
        let d4 = c3 ^ rotl64(c0, 1);
        a0 ^= d0;
        a5 ^= d0;
        a10 ^= d0;
        a15 ^= d0;
        a20 ^= d0;
        a1 ^= d1;
        a6 ^= d1;
        a11 ^= d1;
        a16 ^= d1;
        a21 ^= d1;
        a2 ^= d2;
        a7 ^= d2;
        a12 ^= d2;
        a17 ^= d2;
        a22 ^= d2;
        a3 ^= d3;
        a8 ^= d3;
        a13 ^= d3;
        a18 ^= d3;
        a23 ^= d3;
        a4 ^= d4;
        a9 ^= d4;
        a14 ^= d4;
        a19 ^= d4;
        a24 ^= d4;

        let b0 = a0;
        let b10 = rotl64(a1, 1);
        let b20 = rotl64(a2, 62);
        let b5 = rotl64(a3, 28);
        let b15 = rotl64(a4, 27);
        let b16 = rotl64(a5, 36);
        let b1 = rotl64(a6, 44);
        let b11 = rotl64(a7, 6);
        let b21 = rotl64(a8, 55);
        let b6 = rotl64(a9, 20);
        let b7 = rotl64(a10, 3);
        let b17 = rotl64(a11, 10);
        let b2 = rotl64(a12, 43);
        let b12 = rotl64(a13, 25);
        let b22 = rotl64(a14, 39);
        let b23 = rotl64(a15, 41);
        let b8 = rotl64(a16, 45);
        let b18 = rotl64(a17, 15);
        let b3 = rotl64(a18, 21);
        let b13 = rotl64(a19, 8);
        let b14 = rotl64(a20, 18);
        let b24 = rotl64(a21, 2);
        let b9 = rotl64(a22, 61);
        let b19 = rotl64(a23, 56);
        let b4 = rotl64(a24, 14);

        a0 = b0 ^ (!b1 & b2);
        a1 = b1 ^ (!b2 & b3);
        a2 = b2 ^ (!b3 & b4);
        a3 = b3 ^ (!b4 & b0);
        a4 = b4 ^ (!b0 & b1);
        a5 = b5 ^ (!b6 & b7);
        a6 = b6 ^ (!b7 & b8);
        a7 = b7 ^ (!b8 & b9);
        a8 = b8 ^ (!b9 & b5);
        a9 = b9 ^ (!b5 & b6);
        a10 = b10 ^ (!b11 & b12);
        a11 = b11 ^ (!b12 & b13);
        a12 = b12 ^ (!b13 & b14);
        a13 = b13 ^ (!b14 & b10);
        a14 = b14 ^ (!b10 & b11);
        a15 = b15 ^ (!b16 & b17);
        a16 = b16 ^ (!b17 & b18);
        a17 = b17 ^ (!b18 & b19);
        a18 = b18 ^ (!b19 & b15);
        a19 = b19 ^ (!b15 & b16);
        a20 = b20 ^ (!b21 & b22);
        a21 = b21 ^ (!b22 & b23);
        a22 = b22 ^ (!b23 & b24);
        a23 = b23 ^ (!b24 & b20);
        a24 = b24 ^ (!b20 & b21);

        a0 ^= RC[r];
    }

    s[0] = a0;
    s[1] = a1;
    s[2] = a2;
    s[3] = a3;
    s[4] = a4;
    s[5] = a5;
    s[6] = a6;
    s[7] = a7;
    s[8] = a8;
    s[9] = a9;
    s[10] = a10;
    s[11] = a11;
    s[12] = a12;
    s[13] = a13;
    s[14] = a14;
    s[15] = a15;
    s[16] = a16;
    s[17] = a17;
    s[18] = a18;
    s[19] = a19;
    s[20] = a20;
    s[21] = a21;
    s[22] = a22;
    s[23] = a23;
    s[24] = a24;
}

const RATE: usize = 136;

/// 求解 PoW：找到 nonce ∈ [0, difficulty) 使 DeepSeekHashV1(salt + "_" + expire_at + "_" + str(nonce)) 前 32 字节等于 challenge。
pub fn solve_pow(
    challenge_hex: &str,
    salt: &str,
    expire_at: i64,
    difficulty: i64,
) -> AppResult<i64> {
    if challenge_hex.len() != 64 {
        return Err(AppError::msg(format!(
            "pow: challenge 应为 64 位 hex，实际 {}",
            challenge_hex.len()
        )));
    }
    let target = decode_hex(challenge_hex)?;
    let t0 = u64::from_le_bytes(target[0..8].try_into().unwrap());
    let t1 = u64::from_le_bytes(target[8..16].try_into().unwrap());
    let t2 = u64::from_le_bytes(target[16..24].try_into().unwrap());
    let t3 = u64::from_le_bytes(target[24..32].try_into().unwrap());

    let prefix = format!("{salt}_{expire_at}_");
    let prefix_bytes = prefix.as_bytes();

    // 预吸收完整的 rate 倍前缀。
    let mut base_state = [0u64; 25];
    let mut off = 0usize;
    while off + RATE <= prefix_bytes.len() {
        for i in 0..RATE / 8 {
            let chunk = u64::from_le_bytes(prefix_bytes[off + i * 8..off + i * 8 + 8].try_into().unwrap());
            base_state[i] ^= chunk;
        }
        keccak_f23(&mut base_state);
        off += RATE;
    }
    let tail_len = prefix_bytes.len() - off;
    let mut tail = [0u8; RATE];
    tail[..tail_len].copy_from_slice(&prefix_bytes[off..]);

    let mut num_buf = [0u8; 20]; // 19 位整数足够装下 i64

    for n in 0..difficulty {
        // 把 n 写成 ascii 数字（右对齐到 num_buf 末尾）
        let mut v = n as u64;
        let mut pos = 20usize;
        if v == 0 {
            pos -= 1;
            num_buf[pos] = b'0';
        } else {
            while v > 0 {
                pos -= 1;
                num_buf[pos] = b'0' + (v % 10) as u8;
                v /= 10;
            }
        }
        let num_len = 20 - pos;

        let mut s = base_state;
        let total_tail = tail_len + num_len;
        if total_tail < RATE {
            let mut buf = [0u8; RATE];
            buf[..tail_len].copy_from_slice(&tail[..tail_len]);
            buf[tail_len..total_tail].copy_from_slice(&num_buf[pos..]);
            buf[total_tail] = 0x06;
            buf[RATE - 1] |= 0x80;
            for i in 0..RATE / 8 {
                let chunk = u64::from_le_bytes(buf[i * 8..i * 8 + 8].try_into().unwrap());
                s[i] ^= chunk;
            }
            keccak_f23(&mut s);
        } else {
            let mut buf = [0u8; RATE];
            buf[..tail_len].copy_from_slice(&tail[..tail_len]);
            let cap = RATE - tail_len;
            buf[tail_len..RATE].copy_from_slice(&num_buf[pos..pos + cap]);
            for i in 0..RATE / 8 {
                let chunk = u64::from_le_bytes(buf[i * 8..i * 8 + 8].try_into().unwrap());
                s[i] ^= chunk;
            }
            keccak_f23(&mut s);

            let mut buf2 = [0u8; RATE];
            let rem = total_tail - RATE;
            buf2[..rem].copy_from_slice(&num_buf[pos + cap..pos + cap + rem]);
            buf2[rem] = 0x06;
            buf2[RATE - 1] |= 0x80;
            for i in 0..RATE / 8 {
                let chunk = u64::from_le_bytes(buf2[i * 8..i * 8 + 8].try_into().unwrap());
                s[i] ^= chunk;
            }
            keccak_f23(&mut s);
        }

        if s[0] == t0 && s[1] == t1 && s[2] == t2 && s[3] == t3 {
            return Ok(n);
        }
    }

    Err(AppError::msg("pow: 在 difficulty 范围内未找到解"))
}

fn decode_hex(s: &str) -> AppResult<[u8; 32]> {
    let bytes = s.as_bytes();
    if bytes.len() != 64 {
        return Err(AppError::msg("pow: challenge 长度不正确"));
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        let hi = hex_val(bytes[i * 2])?;
        let lo = hex_val(bytes[i * 2 + 1])?;
        out[i] = (hi << 4) | lo;
    }
    Ok(out)
}

fn hex_val(b: u8) -> AppResult<u8> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(AppError::msg("pow: challenge 非法 hex 字符")),
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DeepseekChallenge {
    pub algorithm: String,
    pub challenge: String,
    pub salt: String,
    pub signature: String,
    pub target_path: String,
    pub expire_at: i64,
    #[serde(default)]
    pub difficulty: i64,
}

/// 直接计算 DeepSeekHashV1（用于自检 PoW 求解器是否对得上 Go 实现）。
#[doc(hidden)]
pub fn deepseek_hash_v1(data: &[u8]) -> [u8; 32] {
    let mut s = [0u64; 25];
    let mut off = 0usize;
    while off + RATE <= data.len() {
        for i in 0..RATE / 8 {
            let chunk = u64::from_le_bytes(data[off + i * 8..off + i * 8 + 8].try_into().unwrap());
            s[i] ^= chunk;
        }
        keccak_f23(&mut s);
        off += RATE;
    }
    let mut final_block = [0u8; RATE];
    final_block[..data.len() - off].copy_from_slice(&data[off..]);
    final_block[data.len() - off] = 0x06;
    final_block[RATE - 1] |= 0x80;
    for i in 0..RATE / 8 {
        let chunk = u64::from_le_bytes(final_block[i * 8..i * 8 + 8].try_into().unwrap());
        s[i] ^= chunk;
    }
    keccak_f23(&mut s);
    let mut out = [0u8; 32];
    out[0..8].copy_from_slice(&s[0].to_le_bytes());
    out[8..16].copy_from_slice(&s[1].to_le_bytes());
    out[16..24].copy_from_slice(&s[2].to_le_bytes());
    out[24..32].copy_from_slice(&s[3].to_le_bytes());
    out
}

/// `Challenge` → `x-ds-pow-response` 头值（base64(JSON)）。
pub fn solve_and_build_header(c: &DeepseekChallenge) -> AppResult<String> {
    if c.algorithm != "DeepSeekHashV1" {
        return Err(AppError::msg(format!(
            "pow: 不支持的算法 {}",
            c.algorithm
        )));
    }
    let difficulty = if c.difficulty == 0 { 144_000 } else { c.difficulty };
    let answer = solve_pow(&c.challenge, &c.salt, c.expire_at, difficulty)?;
    let body = json!({
        "algorithm": c.algorithm,
        "challenge": c.challenge,
        "salt": c.salt,
        "answer": answer,
        "signature": c.signature,
        "target_path": c.target_path,
    });
    let bytes = serde_json::to_vec(&body)?;
    Ok(STANDARD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex_encode(bytes: &[u8; 32]) -> String {
        let mut s = String::with_capacity(64);
        for b in bytes {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }

    #[test]
    fn solver_finds_known_nonce() {
        // 自构造一个低难度 challenge：对每个候选 nonce 算 hash，取 nonce=37 的 hash 当 challenge。
        let salt = "abc";
        let expire_at: i64 = 1_700_000_000;
        let known_nonce: i64 = 37;
        let probe = format!("{salt}_{expire_at}_{known_nonce}");
        let target = deepseek_hash_v1(probe.as_bytes());
        let challenge_hex = hex_encode(&target);

        let n = solve_pow(&challenge_hex, salt, expire_at, 1_000).unwrap();
        assert_eq!(n, known_nonce);
    }
}
