use super::*;

#[test]
fn frozen_rng_returns_configured_value_from_every_call() {
    let mut rng = FrozenRng::new(0);
    assert_eq!(rng.next_u32(), 0);
    assert_eq!(rng.next_u32(), 0);
    assert_eq!(rng.next_u64(), 0);

    let mut rng = FrozenRng::new(u32::MAX);
    assert_eq!(rng.next_u32(), u32::MAX);
    assert_eq!(rng.next_u32(), u32::MAX);
}

#[test]
fn frozen_rng_fill_bytes_writes_value_pattern() {
    let mut rng = FrozenRng::new(0x1234_5678);
    let mut buf = [0u8; 8];
    rng.fill_bytes(&mut buf);
    // little-endian bytes of 0x1234_5678 are [0x78, 0x56, 0x34, 0x12].
    assert_eq!(buf, [0x78, 0x56, 0x34, 0x12, 0x78, 0x56, 0x34, 0x12]);
}

#[test]
fn seeded_rng_is_reproducible() {
    let mut a = SeededRng::new(42);
    let mut b = SeededRng::new(42);
    for _ in 0..10 {
        assert_eq!(a.next_u32(), b.next_u32());
    }
}

#[test]
fn seeded_rng_different_seeds_diverge() {
    let mut a = SeededRng::new(1);
    let mut b = SeededRng::new(2);
    let mut diverged = false;
    for _ in 0..10 {
        if a.next_u32() != b.next_u32() {
            diverged = true;
            break;
        }
    }
    assert!(diverged, "different seeds should produce different streams");
}
