//! Integer helpers for the trailing filename numbers, backed by num-prime.

use std::collections::BTreeMap;

/// Greatest common divisor.
pub fn gcd(a: u64, b: u64) -> u64 {
    num_integer::gcd(a, b)
}

/// Primality test.
pub fn is_prime(n: u64) -> bool {
    num_prime::nt_funcs::is_prime64(n)
}

/// Factor `n` into `{prime: exponent}` (empty for n <= 1).
pub fn factorize(n: u64) -> BTreeMap<u64, usize> {
    if n <= 1 {
        return BTreeMap::new();
    }
    num_prime::nt_funcs::factorize64(n)
}

/// Render a factorisation like `5^3 · 43 · 277603`.
pub fn format_factors(factors: &BTreeMap<u64, usize>) -> String {
    if factors.is_empty() {
        return "1".into();
    }
    factors
        .iter()
        .map(|(&p, &e)| {
            if e > 1 {
                format!("{p}^{e}")
            } else {
                p.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factors_carrier_signature() {
        let f = factorize(1_492_116_125); // 5^3 · 43 · 277603
        assert_eq!(f.get(&5), Some(&3));
        assert_eq!(f.get(&43), Some(&1));
        assert_eq!(f.get(&277_603), Some(&1));
        assert_eq!(f.len(), 3);
    }

    #[test]
    fn primality() {
        assert!(is_prime(277_603));
        assert!(is_prime(154_921_957));
        assert!(!is_prime(1_492_116_125));
        assert!(is_prime(3_616_442_437));
        assert!(!is_prime(1));
    }

    #[test]
    fn factors_the_largest_number() {
        let n = 18_303_940_925_429_378_347u64; // largest in the dataset
        let f = factorize(n);
        let product: u128 = f.iter().map(|(p, e)| (*p as u128).pow(*e as u32)).product();
        assert_eq!(product, n as u128);
        for p in f.keys() {
            assert!(is_prime(*p));
        }
    }

    #[test]
    fn gcd_basic() {
        assert_eq!(gcd(85_523, 85_523 * 7), 85_523);
        assert_eq!(gcd(0, 9), 9);
    }
}
