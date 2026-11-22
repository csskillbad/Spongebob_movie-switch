//! Pure-Rust SHA-256 and AES-128 (ECB / CTR / Nintendo XTS).
//! Zero external crates, zero hardware-ISA assumptions (works on every x86_64 CPU).

// ============================================================================
// SHA-256 (FIPS 180-4)
// ============================================================================

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

#[derive(Clone)]
pub struct Sha256 {
    state: [u32; 8],
    buf: [u8; 64],
    buf_len: usize,
    total_bits: u64,
}

impl Sha256 {
    pub fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
                0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
            ],
            buf: [0u8; 64],
            buf_len: 0,
            total_bits: 0,
        }
    }

    #[inline(always)]
    fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total_bits = self.total_bits.wrapping_add((data.len() as u64) * 8);

        if self.buf_len > 0 {
            let take = (64 - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len == 64 {
                let blk = self.buf;
                Self::compress(&mut self.state, &blk);
                self.buf_len = 0;
            }
        }

        while data.len() >= 64 {
            let blk: &[u8; 64] = data[..64].try_into().unwrap();
            Self::compress(&mut self.state, blk);
            data = &data[64..];
        }

        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buf_len = data.len();
        }
    }

    pub fn finalize(mut self) -> [u8; 32] {
        let bits = self.total_bits;
        self.buf[self.buf_len] = 0x80;
        self.buf_len += 1;

        if self.buf_len > 56 {
            self.buf[self.buf_len..64].fill(0);
            let blk = self.buf;
            Self::compress(&mut self.state, &blk);
            self.buf_len = 0;
        }
        self.buf[self.buf_len..56].fill(0);
        self.buf[56..64].copy_from_slice(&bits.to_be_bytes());
        let blk = self.buf;
        Self::compress(&mut self.state, &blk);

        let mut out = [0u8; 32];
        for i in 0..8 {
            out[i * 4..i * 4 + 4].copy_from_slice(&self.state[i].to_be_bytes());
        }
        out
    }

    pub fn digest(data: &[u8]) -> [u8; 32] {
        let mut h = Self::new();
        h.update(data);
        h.finalize()
    }
}

pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

pub fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return Err(format!("Odd hex string length: {}", s.len()));
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for i in (0..bytes.len()).step_by(2) {
        let hi = hex_nibble(bytes[i])?;
        let lo = hex_nibble(bytes[i + 1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_nibble(b: u8) -> Result<u8, String> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(format!("Invalid hex character: {:?}", b as char)),
    }
}

// ============================================================================
// AES-128 (FIPS 197) — T-Table Accelerated
// ============================================================================

const SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76,
    0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0,
    0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15,
    0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75,
    0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84,
    0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf,
    0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8,
    0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2,
    0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
    0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb,
    0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79,
    0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08,
    0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a,
    0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e,
    0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16,
];

const fn make_inv_sbox() -> [u8; 256] {
    let mut inv = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        inv[SBOX[i] as usize] = i as u8;
        i += 1;
    }
    inv
}

const INV_SBOX: [u8; 256] = make_inv_sbox();

const fn xtime(x: u8) -> u8 {
    let hi = (x & 0x80) != 0;
    let shifted = x << 1;
    if hi { shifted ^ 0x1b } else { shifted }
}

const fn gf_mul(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0u8;
    let mut i = 0;
    while i < 8 {
        if (b & 1) != 0 {
            p ^= a;
        }
        a = xtime(a);
        b >>= 1;
        i += 1;
    }
    p
}

const fn make_te0() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let s = SBOX[i];
        let s2 = xtime(s);
        let s3 = s2 ^ s;
        t[i] = ((s2 as u32) << 24) | ((s as u32) << 16) | ((s as u32) << 8) | (s3 as u32);
        i += 1;
    }
    t
}

const fn make_te_rot(src: [u32; 256], rot: u32) -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = src[i].rotate_right(rot);
        i += 1;
    }
    t
}

const TE0: [u32; 256] = make_te0();
const TE1: [u32; 256] = make_te_rot(TE0, 8);
const TE2: [u32; 256] = make_te_rot(TE0, 16);
const TE3: [u32; 256] = make_te_rot(TE0, 24);

const RCON: [u32; 10] = [
    0x01000000, 0x02000000, 0x04000000, 0x08000000, 0x10000000,
    0x20000000, 0x40000000, 0x80000000, 0x1b000000, 0x36000000,
];

#[derive(Clone)]
pub struct Aes128 {
    rk: [u32; 44],
}

impl Aes128 {
    pub fn new(key: &[u8; 16]) -> Self {
        let mut rk = [0u32; 44];
        for i in 0..4 {
            rk[i] = u32::from_be_bytes([
                key[i * 4],
                key[i * 4 + 1],
                key[i * 4 + 2],
                key[i * 4 + 3],
            ]);
        }
        for i in 4..44 {
            let mut temp = rk[i - 1];
            if i % 4 == 0 {
                temp = ((SBOX[((temp >> 16) & 0xff) as usize] as u32) << 24)
                    | ((SBOX[((temp >> 8) & 0xff) as usize] as u32) << 16)
                    | ((SBOX[(temp & 0xff) as usize] as u32) << 8)
                    | (SBOX[((temp >> 24) & 0xff) as usize] as u32);
                temp ^= RCON[i / 4 - 1];
            }
            rk[i] = rk[i - 4] ^ temp;
        }
        Self { rk }
    }

    #[inline(always)]
    pub fn encrypt_block(&self, input: &[u8; 16]) -> [u8; 16] {
        let mut s0 = u32::from_be_bytes([input[0], input[1], input[2], input[3]]) ^ self.rk[0];
        let mut s1 = u32::from_be_bytes([input[4], input[5], input[6], input[7]]) ^ self.rk[1];
        let mut s2 = u32::from_be_bytes([input[8], input[9], input[10], input[11]]) ^ self.rk[2];
        let mut s3 = u32::from_be_bytes([input[12], input[13], input[14], input[15]]) ^ self.rk[3];

        for r in 1..10 {
            let b = r * 4;
            let t0 = TE0[(s0 >> 24) as usize]
                ^ TE1[((s1 >> 16) & 0xff) as usize]
                ^ TE2[((s2 >> 8) & 0xff) as usize]
                ^ TE3[(s3 & 0xff) as usize]
                ^ self.rk[b];
            let t1 = TE0[(s1 >> 24) as usize]
                ^ TE1[((s2 >> 16) & 0xff) as usize]
                ^ TE2[((s3 >> 8) & 0xff) as usize]
                ^ TE3[(s0 & 0xff) as usize]
                ^ self.rk[b + 1];
            let t2 = TE0[(s2 >> 24) as usize]
                ^ TE1[((s3 >> 16) & 0xff) as usize]
                ^ TE2[((s0 >> 8) & 0xff) as usize]
                ^ TE3[(s1 & 0xff) as usize]
                ^ self.rk[b + 2];
            let t3 = TE0[(s3 >> 24) as usize]
                ^ TE1[((s0 >> 16) & 0xff) as usize]
                ^ TE2[((s1 >> 8) & 0xff) as usize]
                ^ TE3[(s2 & 0xff) as usize]
                ^ self.rk[b + 3];
            s0 = t0;
            s1 = t1;
            s2 = t2;
            s3 = t3;
        }

        let o0 = (((SBOX[(s0 >> 24) as usize] as u32) << 24)
            | ((SBOX[((s1 >> 16) & 0xff) as usize] as u32) << 16)
            | ((SBOX[((s2 >> 8) & 0xff) as usize] as u32) << 8)
            | (SBOX[(s3 & 0xff) as usize] as u32))
            ^ self.rk[40];
        let o1 = (((SBOX[(s1 >> 24) as usize] as u32) << 24)
            | ((SBOX[((s2 >> 16) & 0xff) as usize] as u32) << 16)
            | ((SBOX[((s3 >> 8) & 0xff) as usize] as u32) << 8)
            | (SBOX[(s0 & 0xff) as usize] as u32))
            ^ self.rk[41];
        let o2 = (((SBOX[(s2 >> 24) as usize] as u32) << 24)
            | ((SBOX[((s3 >> 16) & 0xff) as usize] as u32) << 16)
            | ((SBOX[((s0 >> 8) & 0xff) as usize] as u32) << 8)
            | (SBOX[(s1 & 0xff) as usize] as u32))
            ^ self.rk[42];
        let o3 = (((SBOX[(s3 >> 24) as usize] as u32) << 24)
            | ((SBOX[((s0 >> 16) & 0xff) as usize] as u32) << 16)
            | ((SBOX[((s1 >> 8) & 0xff) as usize] as u32) << 8)
            | (SBOX[(s2 & 0xff) as usize] as u32))
            ^ self.rk[43];

        let mut out = [0u8; 16];
        out[0..4].copy_from_slice(&o0.to_be_bytes());
        out[4..8].copy_from_slice(&o1.to_be_bytes());
        out[8..12].copy_from_slice(&o2.to_be_bytes());
        out[12..16].copy_from_slice(&o3.to_be_bytes());
        out
    }

    pub fn decrypt_block(&self, input: &[u8; 16]) -> [u8; 16] {
        let mut s = *input;

        // AddRoundKey(10)
        for c in 0..4 {
            let k = self.rk[40 + c].to_be_bytes();
            for r in 0..4 {
                s[c * 4 + r] ^= k[r];
            }
        }

        for round in (1..10).rev() {
            inv_shift_rows(&mut s);
            for b in &mut s {
                *b = INV_SBOX[*b as usize];
            }
            for c in 0..4 {
                let k = self.rk[round * 4 + c].to_be_bytes();
                for r in 0..4 {
                    s[c * 4 + r] ^= k[r];
                }
            }
            inv_mix_columns(&mut s);
        }

        inv_shift_rows(&mut s);
        for b in &mut s {
            *b = INV_SBOX[*b as usize];
        }
        for c in 0..4 {
            let k = self.rk[c].to_be_bytes();
            for r in 0..4 {
                s[c * 4 + r] ^= k[r];
            }
        }
        s
    }
}

fn inv_shift_rows(s: &mut [u8; 16]) {
    // Row 1: right shift by 1
    let tmp = s[13];
    s[13] = s[9];
    s[9] = s[5];
    s[5] = s[1];
    s[1] = tmp;
    // Row 2: right shift by 2
    s.swap(2, 10);
    s.swap(6, 14);
    // Row 3: right shift by 3 (= left shift by 1)
    let tmp = s[3];
    s[3] = s[7];
    s[7] = s[11];
    s[11] = s[15];
    s[15] = tmp;
}

fn inv_mix_columns(s: &mut [u8; 16]) {
    for c in 0..4 {
        let i = c * 4;
        let a0 = s[i];
        let a1 = s[i + 1];
        let a2 = s[i + 2];
        let a3 = s[i + 3];
        s[i] = gf_mul(a0, 14) ^ gf_mul(a1, 11) ^ gf_mul(a2, 13) ^ gf_mul(a3, 9);
        s[i + 1] = gf_mul(a0, 9) ^ gf_mul(a1, 14) ^ gf_mul(a2, 11) ^ gf_mul(a3, 13);
        s[i + 2] = gf_mul(a0, 13) ^ gf_mul(a1, 9) ^ gf_mul(a2, 14) ^ gf_mul(a3, 11);
        s[i + 3] = gf_mul(a0, 11) ^ gf_mul(a1, 13) ^ gf_mul(a2, 9) ^ gf_mul(a3, 14);
    }
}

/// Nintendo Switch AES-128-CTR stream cipher.
/// Tracks the exact byte offset within the NCA so arbitrary chunk sizes work seamlessly.
pub struct SwitchAesCtr {
    aes: Aes128,
    ctr_hi: u64,
    byte_offset: u64,
    cached_blk_idx: u64,
    keystream: [u8; 16],
}

impl SwitchAesCtr {
    pub fn new(key: &[u8; 16], ctr_hi: u64, start_byte_offset: u64) -> Self {
        let aes = Aes128::new(key);
        Self {
            aes,
            ctr_hi,
            byte_offset: start_byte_offset,
            cached_blk_idx: u64::MAX,
            keystream: [0u8; 16],
        }
    }

    pub fn apply(&mut self, data: &mut [u8]) {
        let mut i = 0;
        let len = data.len();

        // Fast path when 16-byte aligned.
        while i < len {
            let blk_idx = self.byte_offset >> 4;
            let in_blk = (self.byte_offset & 15) as usize;
            if in_blk == 0 && len - i >= 16 {
                let mut iv = [0u8; 16];
                iv[0..8].copy_from_slice(&self.ctr_hi.to_be_bytes());
                iv[8..16].copy_from_slice(&blk_idx.to_be_bytes());
                let ks = self.aes.encrypt_block(&iv);
                for k in 0..16 {
                    data[i + k] ^= ks[k];
                }
                i += 16;
                self.byte_offset += 16;
            } else {
                if blk_idx != self.cached_blk_idx {
                    let mut iv = [0u8; 16];
                    iv[0..8].copy_from_slice(&self.ctr_hi.to_be_bytes());
                    iv[8..16].copy_from_slice(&blk_idx.to_be_bytes());
                    self.keystream = self.aes.encrypt_block(&iv);
                    self.cached_blk_idx = blk_idx;
                }
                let take = (16 - in_blk).min(len - i);
                for k in 0..take {
                    data[i + k] ^= self.keystream[in_blk + k];
                }
                i += take;
                self.byte_offset += take as u64;
            }
        }
    }
}

/// Nintendo Switch AES-128-XTS (uses big-endian sector number tweak).
pub fn nintendo_xts_crypt(key: &[u8; 32], data: &mut [u8], encrypt: bool) {
    assert!(data.len() % 0x200 == 0);
    let aes1 = Aes128::new(key[0..16].try_into().unwrap());
    let aes2 = Aes128::new(key[16..32].try_into().unwrap());

    for (sec, sector) in data.chunks_exact_mut(0x200).enumerate() {
        let tweak_in = (sec as u128).to_be_bytes();
        let mut t = aes2.encrypt_block(&tweak_in);
        for blk in sector.chunks_exact_mut(16) {
            let mut x = [0u8; 16];
            for j in 0..16 {
                x[j] = blk[j] ^ t[j];
            }
            let y = if encrypt {
                aes1.encrypt_block(&x)
            } else {
                aes1.decrypt_block(&x)
            };
            for j in 0..16 {
                blk[j] = y[j] ^ t[j];
            }
            // GF(2^128) multiply by x
            let mut carry = 0u8;
            for b in &mut t {
                let nc = (*b >> 7) & 1;
                *b = (*b << 1) | carry;
                carry = nc;
            }
            if carry != 0 {
                t[0] ^= 0x87;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha256_nist() {
        assert_eq!(
            hex_encode(&Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex_encode(&Sha256::digest(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn test_aes128_fips197() {
        let key: [u8; 16] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
            0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
        ];
        let pt: [u8; 16] = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77,
            0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
        ];
        let expected_ct: [u8; 16] = [
            0x69, 0xc4, 0xe0, 0xd8, 0x6a, 0x7b, 0x04, 0x30,
            0xd8, 0xcd, 0xb7, 0x80, 0x70, 0xb4, 0xc5, 0x5a,
        ];
        let aes = Aes128::new(&key);
        let ct = aes.encrypt_block(&pt);
        assert_eq!(ct, expected_ct);
        assert_eq!(aes.decrypt_block(&ct), pt);
    }

    #[test]
    fn test_xts_roundtrip() {
        let key = [0x42u8; 32];
        let orig = vec![0xABu8; 0xC00];
        let mut buf = orig.clone();
        nintendo_xts_crypt(&key, &mut buf, true);
        assert_ne!(buf, orig);
        nintendo_xts_crypt(&key, &mut buf, false);
        assert_eq!(buf, orig);
    }
}
