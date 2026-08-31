//! Generated project names (§12: `amber-falcon.dv` — naming never blocks
//! starting). Pure: the caller supplies entropy (e.g. nanoseconds since the
//! epoch), so dv-core needs no RNG dependency and tests are deterministic.

const ADJECTIVES: &[&str] = &[
    "amber", "brisk", "calm", "coral", "crisp", "dusky", "eager", "fable", "fleet", "gilded",
    "hazel", "indigo", "jade", "keen", "lucid", "mellow", "noble", "ochre", "pale", "quick",
    "rustic", "sable", "tidal", "umber", "vivid", "warm", "young", "zesty", "bold", "clear",
    "deft", "early",
];

const NOUNS: &[&str] = &[
    "falcon", "badger", "cedar", "delta", "ember", "fjord", "grove", "heron", "island", "juniper",
    "kestrel", "lagoon", "meadow", "nimbus", "otter", "prairie", "quartz", "raven", "summit",
    "thicket", "upland", "valley", "willow", "yarrow", "zephyr", "aspen", "brook", "canyon",
    "dune", "eddy", "finch", "glacier",
];

/// A `adjective-noun` name from a caller-provided seed.
pub fn generate(seed: u64) -> String {
    // Split the seed so adjacent seeds don't walk the two lists in lockstep.
    let a = (seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32) as usize % ADJECTIVES.len();
    let n = (seed.wrapping_mul(0xC2B2_AE3D_27D4_EB4F) >> 32) as usize % NOUNS.len();
    format!("{}-{}", ADJECTIVES[a], NOUNS[n])
}

/// First generated name (from `seed`, probing upward) whose `.dv` file does
/// not already exist in `dir`.
pub fn generate_untaken(dir: &std::path::Path, seed: u64) -> String {
    for i in 0..1024 {
        let name = generate(seed.wrapping_add(i));
        if !dir.join(format!("{name}.dv")).exists() {
            return name;
        }
    }
    // Astronomically unlikely (32×32 combos × 1024 probes): fall back to a
    // seed-suffixed name that cannot collide with generated ones.
    format!("project-{seed}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_shaped() {
        let n = generate(42);
        assert_eq!(n, generate(42));
        let (a, b) = n.split_once('-').expect("hyphenated");
        assert!(ADJECTIVES.contains(&a));
        assert!(NOUNS.contains(&b));
    }

    #[test]
    fn untaken_skips_existing() {
        let dir = std::env::temp_dir().join("dv-core-names-test");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let taken = generate(7);
        std::fs::write(dir.join(format!("{taken}.dv")), b"x").expect("write");
        let got = generate_untaken(&dir, 7);
        assert_ne!(got, taken);
    }
}
