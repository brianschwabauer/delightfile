//! XXH3-128, scalar, seed 0, default secret — the one function yazi's thumbnail
//! cache name is made of.
//!
//! This exists for exactly one reason: [`super::cache`] has to reproduce a file
//! name yazi wrote, byte for byte, and yazi names its cache files with the hex
//! of an XXH3-128 digest. There is no way to be "close enough" — a digest that
//! differs in one bit is a cache miss, which is the same as not having a cache
//! at all.
//!
//! ## Why it is hand-written
//!
//! df-core's dependency list is four crates and PLAN §1's bar for a fifth is
//! "rewriting is impractical". This is ~200 lines of arithmetic with no
//! allocation, no platform code and a published set of test vectors, so it does
//! not clear that bar. The port is scalar-only: the SIMD paths in the reference
//! implementation exist to hash gigabytes, and everything hashed here is a file
//! *name* plus four integers — under 300 bytes, hashed once per previewed file.
//!
//! ## Faithfulness
//!
//! Transcribed from the `twox-hash` 2.1.3 scalar implementation (itself a port
//! of Cyan4973's xxHash), specialised to seed 0 and the 192-byte default
//! secret, which is all yazi ever uses. The five length brackets the reference
//! splits on (0, 1–3, 4–8, 9–16, 17–128, 129–240, 241+) are kept as separate
//! functions with the same names so a future reader can diff them against the
//! reference. The known-answer vectors in the tests below are copied from
//! `twox-hash`'s own test suite, including the boundary lengths on either side
//! of every bracket — those are what prove the transcription, and
//! [`super::cache`]'s tests then prove it against files yazi actually wrote.

/// The default 192-byte secret. XXH3 with seed 0 uses it verbatim; a seeded
/// hash derives from it, which yazi never does.
#[rustfmt::skip]
const SECRET: [u8; 192] = [
    0xb8, 0xfe, 0x6c, 0x39, 0x23, 0xa4, 0x4b, 0xbe, 0x7c, 0x01, 0x81, 0x2c, 0xf7, 0x21, 0xad, 0x1c,
    0xde, 0xd4, 0x6d, 0xe9, 0x83, 0x90, 0x97, 0xdb, 0x72, 0x40, 0xa4, 0xa4, 0xb7, 0xb3, 0x67, 0x1f,
    0xcb, 0x79, 0xe6, 0x4e, 0xcc, 0xc0, 0xe5, 0x78, 0x82, 0x5a, 0xd0, 0x7d, 0xcc, 0xff, 0x72, 0x21,
    0xb8, 0x08, 0x46, 0x74, 0xf7, 0x43, 0x24, 0x8e, 0xe0, 0x35, 0x90, 0xe6, 0x81, 0x3a, 0x26, 0x4c,
    0x3c, 0x28, 0x52, 0xbb, 0x91, 0xc3, 0x00, 0xcb, 0x88, 0xd0, 0x65, 0x8b, 0x1b, 0x53, 0x2e, 0xa3,
    0x71, 0x64, 0x48, 0x97, 0xa2, 0x0d, 0xf9, 0x4e, 0x38, 0x19, 0xef, 0x46, 0xa9, 0xde, 0xac, 0xd8,
    0xa8, 0xfa, 0x76, 0x3f, 0xe3, 0x9c, 0x34, 0x3f, 0xf9, 0xdc, 0xbb, 0xc7, 0xc7, 0x0b, 0x4f, 0x1d,
    0x8a, 0x51, 0xe0, 0x4b, 0xcd, 0xb4, 0x59, 0x31, 0xc8, 0x9f, 0x7e, 0xc9, 0xd9, 0x78, 0x73, 0x64,
    0xea, 0xc5, 0xac, 0x83, 0x34, 0xd3, 0xeb, 0xc3, 0xc5, 0x81, 0xa0, 0xff, 0xfa, 0x13, 0x63, 0xeb,
    0x17, 0x0d, 0xdd, 0x51, 0xb7, 0xf0, 0xda, 0x49, 0xd3, 0x16, 0x55, 0x26, 0x29, 0xd4, 0x68, 0x9e,
    0x2b, 0x16, 0xbe, 0x58, 0x7d, 0x47, 0xa1, 0xfc, 0x8f, 0xf8, 0xb8, 0xd1, 0x7a, 0xd0, 0x31, 0xce,
    0x45, 0xcb, 0x3a, 0x8f, 0x95, 0x16, 0x04, 0x28, 0xaf, 0xd7, 0xfb, 0xca, 0xbb, 0x4b, 0x40, 0x7e,
];

const PRIME32_1: u64 = 0x9E37_79B1;
const PRIME32_2: u64 = 0x85EB_CA77;
const PRIME32_3: u64 = 0xC2B2_AE3D;
const PRIME64_1: u64 = 0x9E37_79B1_85EB_CA87;
const PRIME64_2: u64 = 0xC2B2_AE3D_27D4_EB4F;
const PRIME64_3: u64 = 0x1656_67B1_9E37_79F9;
const PRIME64_4: u64 = 0x85EB_CA77_C2B2_AE63;
const PRIME64_5: u64 = 0x27D4_EB2F_1656_67C5;
const PRIME_MX1: u64 = 0x1656_6791_9E37_79F9;
const PRIME_MX2: u64 = 0x9FB2_1C65_1E98_DF25;

/// Where the "long input" algorithm takes over. Below it the digest is a few
/// multiplications of the ends of the input; above it, the 8-accumulator
/// striped loop. Inputs from [`super::cache`] land on both sides of this — a
/// short path with a short name is under it, a deep one is over — so both
/// halves are load-bearing and both are tested.
const CUTOFF: usize = 240;

/// The accumulator seed for inputs past [`CUTOFF`].
#[rustfmt::skip]
const INITIAL_ACCUMULATORS: [u64; 8] = [
    PRIME32_3, PRIME64_1, PRIME64_2, PRIME64_3,
    PRIME64_4, PRIME32_2, PRIME64_5, PRIME32_1,
];

/// One 64-byte stripe per 8 secret bytes, so the 192-byte secret yields 16.
const STRIPES_PER_BLOCK: usize = (SECRET.len() - 64) / 8;

/// 1 KiB. The unit the long-input loop scrambles after.
const BLOCK_SIZE: usize = 64 * STRIPES_PER_BLOCK;

/// The 128-bit digest of `input`, seed 0.
pub fn xxh3_128(input: &[u8]) -> u128 {
    match input.len() {
        0 => impl_0_bytes(),
        1..=3 => impl_1_to_3_bytes(input),
        4..=8 => impl_4_to_8_bytes(input),
        9..=16 => impl_9_to_16_bytes(input),
        17..=128 => impl_17_to_128_bytes(input),
        129..=CUTOFF => impl_129_to_240_bytes(input),
        _ => impl_241_plus_bytes(input),
    }
}

/// Little-endian `u64` at `off`. Every read in XXH3 is little-endian
/// regardless of the host, which is what makes the digest portable — the
/// *input* to it, on the other hand, is not (see [`super::cache`]).
fn u64le(bytes: &[u8], off: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&bytes[off..off + 8]);
    u64::from_le_bytes(b)
}

fn u32le(bytes: &[u8], off: usize) -> u32 {
    let mut b = [0u8; 4];
    b.copy_from_slice(&bytes[off..off + 4]);
    u32::from_le_bytes(b)
}

fn secret_u64(off: usize) -> u64 {
    u64le(&SECRET, off)
}

fn secret_u32(off: usize) -> u32 {
    u32le(&SECRET, off)
}

fn compose(low: u64, high: u64) -> u128 {
    (high as u128) << 64 | low as u128
}

fn lower(x: u128) -> u64 {
    x as u64
}

fn upper(x: u128) -> u64 {
    (x >> 64) as u64
}

fn avalanche(mut x: u64) -> u64 {
    x ^= x >> 37;
    x = x.wrapping_mul(PRIME_MX1);
    x ^= x >> 32;
    x
}

fn avalanche_xxh64(mut x: u64) -> u64 {
    x ^= x >> 33;
    x = x.wrapping_mul(PRIME64_2);
    x ^= x >> 29;
    x = x.wrapping_mul(PRIME64_3);
    x ^= x >> 32;
    x
}

fn impl_0_bytes() -> u128 {
    let low = avalanche_xxh64(secret_u64(64) ^ secret_u64(72));
    let high = avalanche_xxh64(secret_u64(80) ^ secret_u64(88));
    compose(low, high)
}

fn impl_1_to_3_bytes(input: &[u8]) -> u128 {
    let len = input.len();
    let combined = u32::from(input[len - 1])
        | (len as u32) << 8
        | u32::from(input[0]) << 16
        | u32::from(input[len >> 1]) << 24;

    let low = u64::from(secret_u32(0) ^ secret_u32(4)) ^ u64::from(combined);
    let high = u64::from(secret_u32(8) ^ secret_u32(12))
        ^ u64::from(combined.swap_bytes().rotate_left(13));

    compose(avalanche_xxh64(low), avalanche_xxh64(high))
}

fn impl_4_to_8_bytes(input: &[u8]) -> u128 {
    let len = input.len();
    let first = u64::from(u32le(input, 0));
    let last = u64::from(u32le(input, len - 4));

    let combined = first | last << 32;
    let lhs = (secret_u64(16) ^ secret_u64(24)) ^ combined;
    let rhs = PRIME64_1.wrapping_add((len as u64) << 2);
    let mul = (lhs as u128).wrapping_mul(rhs as u128);

    let mut high = upper(mul);
    let mut low = lower(mul);

    high = high.wrapping_add(low << 1);
    low ^= high >> 3;
    low ^= low >> 35;
    low = low.wrapping_mul(PRIME_MX2);
    low ^= low >> 28;

    compose(low, avalanche(high))
}

fn impl_9_to_16_bytes(input: &[u8]) -> u128 {
    let len = input.len();
    let first = u64le(input, 0);
    let last = u64le(input, len - 8);

    let val1 = (secret_u64(32) ^ secret_u64(40)) ^ first ^ last;
    let val2 = (secret_u64(48) ^ secret_u64(56)) ^ last;
    let mul = (val1 as u128).wrapping_mul(PRIME64_1 as u128);

    let low = lower(mul).wrapping_add(((len - 1) as u64) << 54);
    let high = upper(mul)
        .wrapping_add(u64::from((val2 >> 32) as u32) << 32)
        .wrapping_add(u64::from(val2 as u32).wrapping_mul(PRIME32_2));

    let low = low ^ high.swap_bytes();
    let q = compose(low, high).wrapping_mul(PRIME64_2 as u128);

    compose(avalanche(lower(q)), avalanche(upper(q)))
}

/// One 16-byte chunk against one 16-byte secret word. `seed` is always 0 here
/// except for the last chunk of the 129–240 bracket, where the reference
/// negates it — with seed 0 that is still 0, but the parameter stays so the
/// shape matches the reference.
fn mix_step(data: &[u8], data_off: usize, secret_off: usize, seed: u64) -> u64 {
    let d0 = u64le(data, data_off);
    let d1 = u64le(data, data_off + 8);
    let a = (d0 ^ secret_u64(secret_off).wrapping_add(seed)) as u128;
    let b = (d1 ^ secret_u64(secret_off + 8).wrapping_sub(seed)) as u128;
    let mul = a.wrapping_mul(b);
    lower(mul) ^ upper(mul)
}

fn mix_two_chunks(
    acc: &mut [u64; 2],
    data: &[u8],
    off1: usize,
    off2: usize,
    secret_off: usize,
    seed: u64,
) {
    acc[0] = acc[0].wrapping_add(mix_step(data, off1, secret_off, seed));
    acc[1] = acc[1].wrapping_add(mix_step(data, off2, secret_off + 16, seed));
    acc[0] ^= u64le(data, off2).wrapping_add(u64le(data, off2 + 8));
    acc[1] ^= u64le(data, off1).wrapping_add(u64le(data, off1 + 8));
}

fn finalize_medium(acc: [u64; 2], input_len: u64) -> u128 {
    let low = acc[0].wrapping_add(acc[1]);
    let high = acc[0]
        .wrapping_mul(PRIME64_1)
        .wrapping_add(acc[1].wrapping_mul(PRIME64_4))
        .wrapping_add(input_len.wrapping_mul(PRIME64_2));

    compose(avalanche(low), avalanche(high).wrapping_neg())
}

fn impl_17_to_128_bytes(input: &[u8]) -> u128 {
    let len = input.len();
    let mut acc = [(len as u64).wrapping_mul(PRIME64_1), 0];

    // The reference walks the pairs from the outside in, and the accumulator
    // mixes with both `+` and `^`, so the order is part of the answer.
    let pairs = if len > 96 {
        4
    } else if len > 64 {
        3
    } else if len > 32 {
        2
    } else {
        1
    };
    for k in (0..pairs).rev() {
        mix_two_chunks(&mut acc, input, 16 * k, len - 16 * (k + 1), 32 * k, 0);
    }

    finalize_medium(acc, len as u64)
}

fn impl_129_to_240_bytes(input: &[u8]) -> u128 {
    let len = input.len();
    let mut acc = [(len as u64).wrapping_mul(PRIME64_1), 0];
    let chunks = len / 32;

    for i in 0..4 {
        mix_two_chunks(&mut acc, input, 32 * i, 32 * i + 16, 32 * i, 0);
    }
    acc = [avalanche(acc[0]), avalanche(acc[1])];

    // The second pass reads the secret from a 3-byte offset — that shift is
    // what keeps the two passes from cancelling.
    for i in 4..chunks {
        mix_two_chunks(&mut acc, input, 32 * i, 32 * i + 16, 3 + 32 * (i - 4), 0);
    }

    // The final 32 bytes, halves swapped and the seed negated.
    mix_two_chunks(
        &mut acc,
        input,
        len - 16,
        len - 32,
        103,
        0u64.wrapping_neg(),
    );

    finalize_medium(acc, len as u64)
}

fn accumulate(acc: &mut [u64; 8], stripe: &[u8], stripe_off: usize, secret_off: usize) {
    for i in 0..8 {
        let st = u64le(stripe, stripe_off + 8 * i);
        let se = secret_u64(secret_off + 8 * i);
        let value = st ^ se;
        acc[i ^ 1] = acc[i ^ 1].wrapping_add(st);
        // A 32×32→64 multiply, not a 64×64: the truncation is deliberate and
        // is where most of XXH3's speed comes from.
        let product = u64::from(value as u32).wrapping_mul(u64::from((value >> 32) as u32));
        acc[i] = acc[i].wrapping_add(product);
    }
}

fn round_scramble(acc: &mut [u64; 8]) {
    for (i, a) in acc.iter_mut().enumerate() {
        *a ^= *a >> 47;
        *a ^= secret_u64(128 + 8 * i);
        *a = a.wrapping_mul(PRIME32_1);
    }
}

fn final_merge(acc: &[u64; 8], init: u64, secret_off: usize) -> u64 {
    let mut result = init;
    for i in 0..4 {
        let a = (acc[i * 2] ^ secret_u64(secret_off + 16 * i)) as u128;
        let b = (acc[i * 2 + 1] ^ secret_u64(secret_off + 16 * i + 8)) as u128;
        let mul = a.wrapping_mul(b);
        result = result.wrapping_add(lower(mul) ^ upper(mul));
    }
    avalanche(result)
}

fn impl_241_plus_bytes(input: &[u8]) -> u128 {
    let len = input.len();
    let mut acc = INITIAL_ACCUMULATORS;

    let full_blocks = len / BLOCK_SIZE;
    let remainder = len % BLOCK_SIZE;
    // The last block is never accumulated in the main loop — it is finished by
    // `last_round`, which treats its final stripe specially. When the input
    // divides evenly the last *full* block plays that part.
    let (rounds, last_block_start) = if remainder == 0 {
        (full_blocks - 1, (full_blocks - 1) * BLOCK_SIZE)
    } else {
        (full_blocks, full_blocks * BLOCK_SIZE)
    };

    for b in 0..rounds {
        let block = &input[b * BLOCK_SIZE..(b + 1) * BLOCK_SIZE];
        for s in 0..STRIPES_PER_BLOCK {
            accumulate(&mut acc, block, 64 * s, 8 * s);
        }
        round_scramble(&mut acc);
    }

    let last_block = &input[last_block_start..];
    // Every whole stripe of the last block except its final one, then the
    // input's last 64 bytes — which may overlap what was just accumulated, and
    // is meant to.
    let whole = last_block.len() / 64;
    let stripes = if last_block.len().is_multiple_of(64) {
        whole.saturating_sub(1)
    } else {
        whole
    };
    for s in 0..stripes {
        accumulate(&mut acc, last_block, 64 * s, 8 * s);
    }
    accumulate(&mut acc, input, len - 64, 121);

    let len64 = len as u64;
    let low = final_merge(&acc, len64.wrapping_mul(PRIME64_1), 11);
    let high = final_merge(&acc, !len64.wrapping_mul(PRIME64_2), 117);

    compose(low, high)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference suite's input generator: 251 is prime, so the pattern
    /// never lines up with a power-of-two stripe boundary.
    fn gen_bytes(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i % 251) as u8).collect()
    }

    #[track_caller]
    fn check(cases: &[(usize, u128)]) {
        for &(len, expected) in cases {
            let got = xxh3_128(&gen_bytes(len));
            assert_eq!(got, expected, "length {len}: {got:032x} != {expected:032x}");
        }
    }

    // Every expectation below is copied from `twox-hash` 2.1.3's own test
    // suite, which in turn matches the C reference. They are grouped by the
    // length bracket they exercise, with the boundary lengths on both sides.

    #[test]
    fn empty_input() {
        assert_eq!(xxh3_128(&[]), 0x99aa_06d3_0147_98d8_6001_c324_468d_497f);
    }

    #[test]
    fn one_to_three_bytes() {
        check(&[
            (1, 0xa6cd_5e93_9200_0f6a_c44b_dff4_074e_ecdb),
            (2, 0x6a4a_5274_c1b0_d3ad_d664_5fc3_051a_9457),
            (3, 0xe3b5_5f57_945a_17cf_5f42_99fc_161c_9cbb),
        ]);
    }

    #[test]
    fn four_to_eight_bytes() {
        check(&[
            (4, 0xeb70_bf5f_c779_e9e6_a611_1d53_e80a_3db5),
            (5, 0x9434_5321_06a7_c141_c920_d234_7a85_929b),
            (6, 0x545f_093d_32b1_68fe_a6b5_2f4d_ea38_96a3),
            (7, 0x61ce_291b_c3a4_357d_dbb2_0782_1e6d_5efe),
            (8, 0xe1e4_432a_6221_7fe4_cfd5_0c61_c8bb_98c1),
        ]);
    }

    #[test]
    fn nine_to_sixteen_bytes() {
        check(&[
            (9, 0x16c7_69d8_3e4a_ebce_9079_3197_9dca_3746),
            (10, 0xbd93_0669_a87b_4b37_e67b_f1ad_8dcf_73a8),
            (11, 0xacad_8071_8f47_d494_7d67_cfc1_730f_22a3),
            (12, 0x38f9_2247_a7f7_3cc5_7780_eb31_198f_13ca),
            (13, 0xae92_e123_e947_2408_bd79_5526_1902_66c0),
            (14, 0x5f91_e6bf_7418_cfaa_55d6_5715_e2a5_7c31),
            (15, 0x301a_9f75_4e8f_569a_0017_ea4b_e19b_c787),
            (16, 0x7295_0631_8276_07e2_8428_12cc_870d_cae2),
        ]);
    }

    #[test]
    fn seventeen_to_128_bytes() {
        check(&[
            (17, 0x685b_c458_b37d_057f_c06e_233d_f772_9217),
            (18, 0x87ce_996b_b557_6d8d_e3a3_c96b_b0af_2c23),
            (19, 0x7619_bcef_2e31_1cd8_c47d_dc58_8737_93df),
            (31, 0x4ed3_946d_393b_687b_b54d_e399_3874_ed20),
            (32, 0x25e7_c9b3_424c_eed2_457d_9566_b6fc_d697),
            (33, 0x0217_5c3a_abb0_0637_e08d_8495_1339_de86),
            (126, 0x0abc_2062_87ce_2afe_5181_0be2_9323_2106),
            (127, 0xd5ad_d870_c9c9_e00f_060c_2e3d_df0f_2fb9),
            (128, 0x1479_2fc3_af88_dc6c_0532_1a0b_64d6_7b41),
        ]);
    }

    #[test]
    fn one_hundred_twenty_nine_to_240_bytes() {
        check(&[
            (129, 0xdd5e_74ac_6b45_f54e_bc30_b633_82b0_9a3b),
            (130, 0x6cd2_e56a_10f1_e707_3ec5_f135_d0a7_d28f),
            (131, 0x6da7_92f1_702d_4494_5609_cfc7_9dba_18fd),
            (238, 0x73a9_e8f7_bd32_83c8_2a9b_ddd0_e5c4_014c),
            (239, 0x9843_ab31_a06b_e0df_fe21_3746_28fc_c539),
            (240, 0x65b5_be86_da55_40e7_c92b_68e1_6f83_bbb6),
        ]);
    }

    #[test]
    fn two_hundred_forty_one_plus_bytes() {
        // 1024 and 10240 are the block-boundary cases: an input that divides
        // evenly into 1 KiB blocks takes the branch where the last full block
        // is held back from the main loop.
        check(&[
            (241, 0x1da1_cb61_bcb8_a2a1_02e8_cd95_421c_6d02),
            (242, 0x1623_84cb_44d1_d806_ddcb_33c4_9405_1832),
            (243, 0xbd2e_9fcf_378c_35e9_8835_f952_9193_e3dc),
            (244, 0x3ff4_93d7_a813_7ab6_bc17_c91e_c3cf_8d7f),
            (1024, 0xd0ac_1f7b_93bf_57b9_e5d7_8baf_a45b_2aa5),
            (10240, 0x4f63_75cc_a7ec_e1e1_bcd6_3266_df6e_2244),
        ]);
    }
}
