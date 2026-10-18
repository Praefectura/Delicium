//! SHA-256 as specified by FIPS 180-4.
//!
//! The updater hashes every downloaded payload while it streams to disk and
//! then hashes it again from disk before installation. The implementation is
//! plain safe Rust: no platform API, no C dependency, and it is checked
//! against the published NIST vectors in the tests below.

/// Length of a SHA-256 digest in bytes.
pub const DIGEST_BYTES: usize = 32;

/// Length of a SHA-256 digest in lowercase hexadecimal characters.
pub const HEX_LEN: usize = DIGEST_BYTES * 2;

/// First 32 bits of the fractional parts of the cube roots of the first 64
/// primes (FIPS 180-4 section 4.2.2).
const K: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

/// First 32 bits of the fractional parts of the square roots of the first
/// eight primes (FIPS 180-4 section 5.3.3).
const INITIAL: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

/// A streaming SHA-256 hasher.
///
/// Feed arbitrary chunks with [`Sha256::update`] and read the digest once
/// with [`Sha256::finish`]; the chunk boundaries do not affect the result.
#[derive(Clone, Debug)]
pub struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    buffered: usize,
    length: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    /// A hasher for the empty input.
    pub fn new() -> Self {
        Self {
            state: INITIAL,
            buffer: [0; 64],
            buffered: 0,
            length: 0,
        }
    }

    /// Feeds the next chunk. Chunks need not be aligned to anything.
    pub fn update(&mut self, mut data: &[u8]) {
        self.length = self.length.wrapping_add(data.len() as u64);
        if self.buffered > 0 {
            let take = (64 - self.buffered).min(data.len());
            self.buffer[self.buffered..self.buffered + take].copy_from_slice(&data[..take]);
            self.buffered += take;
            data = &data[take..];
            if self.buffered == 64 {
                let block = self.buffer;
                compress(&mut self.state, &block);
                self.buffered = 0;
            }
        }
        while data.len() >= 64 {
            let (block, rest) = data.split_at(64);
            // The split is exactly 64 bytes, so the conversion cannot fail.
            let block: &[u8; 64] = block.try_into().expect("a 64-byte block");
            compress(&mut self.state, block);
            data = rest;
        }
        if !data.is_empty() {
            self.buffer[..data.len()].copy_from_slice(data);
            self.buffered = data.len();
        }
    }

    /// The digest of everything fed so far.
    pub fn finish(mut self) -> [u8; DIGEST_BYTES] {
        let bits = self.length.wrapping_mul(8);
        // The padding is `0x80`, zeroes, then the big-endian bit length; it
        // occupies one 64-byte block when at most 55 bytes are buffered and
        // two otherwise.
        let mut tail = [0u8; 128];
        tail[..self.buffered].copy_from_slice(&self.buffer[..self.buffered]);
        tail[self.buffered] = 0x80;
        let end = if self.buffered < 56 { 64 } else { 128 };
        tail[end - 8..end].copy_from_slice(&bits.to_be_bytes());
        let (blocks, _) = tail[..end].as_chunks::<64>();
        for chunk in blocks {
            compress(&mut self.state, chunk);
        }
        let mut digest = [0u8; DIGEST_BYTES];
        for (slot, word) in digest.as_chunks_mut::<4>().0.iter_mut().zip(self.state) {
            slot.copy_from_slice(&word.to_be_bytes());
        }
        digest
    }
}

/// The digest of one buffer.
pub fn digest(data: &[u8]) -> [u8; DIGEST_BYTES] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finish()
}

/// Lowercase hexadecimal, the form the manifest uses.
pub fn to_hex(digest: &[u8; DIGEST_BYTES]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(HEX_LEN);
    for byte in digest {
        text.push(DIGITS[(byte >> 4) as usize] as char);
        text.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    text
}

/// Parses exactly 64 hexadecimal characters, in either case.
pub fn parse_hex(text: &str) -> Option<[u8; DIGEST_BYTES]> {
    if text.len() != HEX_LEN {
        return None;
    }
    let mut digest = [0u8; DIGEST_BYTES];
    let (pairs, _) = text.as_bytes().as_chunks::<2>();
    for (slot, pair) in digest.iter_mut().zip(pairs) {
        let high = hex_value(pair[0])?;
        let low = hex_value(pair[1])?;
        *slot = (high << 4) | low;
    }
    Some(digest)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut schedule = [0u32; 64];
    let (words, _) = block.as_chunks::<4>();
    for (word, bytes) in schedule.iter_mut().zip(words) {
        *word = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    }
    for index in 16..64 {
        let s0 = schedule[index - 15].rotate_right(7)
            ^ schedule[index - 15].rotate_right(18)
            ^ (schedule[index - 15] >> 3);
        let s1 = schedule[index - 2].rotate_right(17)
            ^ schedule[index - 2].rotate_right(19)
            ^ (schedule[index - 2] >> 10);
        schedule[index] = schedule[index - 16]
            .wrapping_add(s0)
            .wrapping_add(schedule[index - 7])
            .wrapping_add(s1);
    }

    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for index in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let choice = (e & f) ^ (!e & g);
        let temp1 = h
            .wrapping_add(s1)
            .wrapping_add(choice)
            .wrapping_add(K[index])
            .wrapping_add(schedule[index]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let majority = (a & b) ^ (a & c) ^ (b & c);
        let temp2 = s0.wrapping_add(majority);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(data: &[u8]) -> String {
        to_hex(&digest(data))
    }

    #[test]
    fn nist_vectors() {
        // FIPS 180-4 / NIST CAVP examples.
        assert_eq!(
            hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // The two-block 896-bit message.
        assert_eq!(
            hex(b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu"),
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1"
        );
        let million = vec![b'a'; 1_000_000];
        assert_eq!(
            hex(&million),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn chunk_boundaries_do_not_change_the_digest() {
        let data: Vec<u8> = (0..1000u32).map(|value| (value % 251) as u8).collect();
        let expected = digest(&data);
        for chunk in [1usize, 3, 55, 56, 57, 63, 64, 65, 127, 128, 129, 999] {
            let mut hasher = Sha256::new();
            for piece in data.chunks(chunk) {
                hasher.update(piece);
            }
            assert_eq!(hasher.finish(), expected, "chunk size {chunk}");
        }
        // Empty updates are no-ops.
        let mut hasher = Sha256::new();
        hasher.update(b"");
        hasher.update(b"abc");
        hasher.update(b"");
        assert_eq!(hasher.finish(), digest(b"abc"));
    }

    #[test]
    fn hex_round_trips_and_rejects_bad_input() {
        let digest = digest(b"delicium");
        let text = to_hex(&digest);
        assert_eq!(text.len(), HEX_LEN);
        assert_eq!(parse_hex(&text), Some(digest));
        assert_eq!(parse_hex(&text.to_uppercase()), Some(digest));
        assert_eq!(parse_hex(""), None);
        assert_eq!(parse_hex(&text[..HEX_LEN - 1]), None);
        assert_eq!(parse_hex(&format!("{text}0")), None);
        assert_eq!(parse_hex(&"z".repeat(HEX_LEN)), None);
    }
}
