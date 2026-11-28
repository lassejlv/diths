//! Deterministic transcendental functions.
//!
//! `f32::powf`, `exp`, `sin`, `cos`, `hypot` and `cbrt` call the platform's libm, and libms
//! disagree in the last bit: glibc, Apple's libm and the `libm` crate that wasm builds link return
//! different results for some inputs, and one flipped bit can move a pixel. Every transcendental
//! on dith's output path goes through this module instead.
//!
//! Each function evaluates in `f64` with only `+`, `-`, `*`, `/` and `sqrt`, which IEEE 754
//! specifies exactly, and never with a fused multiply-add, so it returns the same bits on every
//! platform. The `f32` functions stay within about 1e-14 relative of the exact value before their
//! single rounding to `f32`, so they round like a correctly rounded libm on all but a vanishing
//! fraction of inputs. The lookup tables are built at compile time from the same operations.

use std::f64::consts::{FRAC_1_PI, LN_2, LOG2_E};

/// Each octave of the `log2` table is split into `2^TABLE_BITS` steps.
const TABLE_BITS: u32 = 7;
const STEPS: usize = 1 << TABLE_BITS;
/// Each octave of the `exp2` table is split into `2^EXP_BITS` steps.
const EXP_BITS: u32 = 10;
const EXP_STEPS: usize = 1 << EXP_BITS;
/// Adding this to an `f64` below 2^51 in magnitude rounds it to an integer, which then sits in
/// the low bits of the sum: subtract the constant again for the float, or its bits for the integer.
const ROUND_SHIFT: f64 = 6_755_399_441_055_744.0;
const ROUND_SHIFT_BITS: u64 = ROUND_SHIFT.to_bits();
const MANTISSA: u64 = (1 << 52) - 1;
const ONE_BITS: u64 = 0x3FF0_0000_0000_0000;
/// `pi / 128` as a sum of two `f64`s (fdlibm's `pi/2` split, divided by 64), so the table's
/// multiples `k * PI_STEP_HI` are exact.
const PI_STEP_HI: f64 = f64::from_bits(0x3F99_21FB_5440_0000);
const PI_STEP_LO: f64 = f64::from_bits(0x3D70_B461_1A62_6331);

/// For `c = 1 + i / STEPS`, `i` in `0..=STEPS`: `1 / c` rounded, and `-log2` of that rounded
/// reciprocal (so the last entry is `(0.5, 1.0)`).
static LOG2_TABLE: [(f64, f64); STEPS + 1] = log2_table();
/// `2^(j / EXP_STEPS)`.
static EXP2_TABLE: [f64; EXP_STEPS] = exp2_table();

/// `ln(x)` for `x` in `[0.5, 2]` from the series `2 atanh((x - 1) / (x + 1))`.
const fn ln_series(x: f64) -> f64 {
    let s = (x - 1.0) / (x + 1.0);
    let s2 = s * s;
    let mut term = s;
    let mut sum = 0.0;
    let mut k = 0;
    while k < 40 {
        sum += term / (2 * k + 1) as f64;
        term *= s2;
        k += 1;
    }
    2.0 * sum
}

const fn log2_table() -> [(f64, f64); STEPS + 1] {
    let mut table = [(0.0, 0.0); STEPS + 1];
    let mut i = 0;
    while i <= STEPS {
        let inv_c = 1.0 / (1.0 + i as f64 / STEPS as f64);
        table[i] = (inv_c, -ln_series(inv_c) / LN_2);
        i += 1;
    }
    table
}

const fn exp2_table() -> [f64; EXP_STEPS] {
    let mut table = [0.0; EXP_STEPS];
    let mut j = 0;
    while j < EXP_STEPS {
        let z = j as f64 / EXP_STEPS as f64 * LN_2;
        let mut term = 1.0;
        let mut sum = 1.0;
        let mut k = 1;
        while k < 30 {
            term *= z / k as f64;
            sum += term;
            k += 1;
        }
        table[j] = sum;
        j += 1;
    }
    table
}

/// `log2(x)` for positive, finite, normal `x`, within about 1e-15 absolute.
#[inline]
fn log2(x: f64) -> f64 {
    const K1: f64 = LOG2_E;
    const K2: f64 = -LOG2_E / 2.0;
    const K3: f64 = LOG2_E / 3.0;
    const K4: f64 = -LOG2_E / 4.0;
    const K5: f64 = LOG2_E / 5.0;
    let bits = x.to_bits();
    let exponent = ((bits >> 52) as i64 - 1023) as f64;
    // The mantissa rounded to the nearest table step, `STEPS` meaning the top of the octave.
    let index = ((((bits >> (52 - TABLE_BITS - 1)) & (2 * STEPS as u64 - 1)) + 1) >> 1) as usize;
    let (inv_c, log2_c) = LOG2_TABLE[index];
    // |r| <= 1 / (2 * STEPS), so five terms of log2(1 + r) leave an error below 1e-15.
    let r = f64::from_bits(bits & MANTISSA | ONE_BITS) * inv_c - 1.0;
    let r2 = r * r;
    (exponent + log2_c) + (r * K1 + r2 * ((K2 + r * K3) + r2 * (K4 + r * K5)))
}

/// `2^(u / EXP_STEPS)`, within about 1e-15 relative. Taking the exponent in table steps saves the
/// callers' scaling a multiplication. Flushes to zero below `2^-1020` and overflows to infinity
/// above `2^1023`, both far outside anything an `f32` result can hold.
#[inline]
fn exp2_steps(u: f64) -> f64 {
    const D1: f64 = LN_2 / EXP_STEPS as f64;
    const D2: f64 = D1 * D1 / 2.0;
    const D3: f64 = D2 * D1 / 3.0;
    if u.is_nan() {
        return u;
    }
    if u > 1023.0 * EXP_STEPS as f64 {
        return f64::INFINITY;
    }
    if u < -1020.0 * EXP_STEPS as f64 {
        return 0.0;
    }
    let shifted = u + ROUND_SHIFT;
    let n = shifted
        .to_bits()
        .wrapping_sub(ROUND_SHIFT_BITS)
        .cast_signed();
    // |f| <= 1/2 and the subtraction is exact; 2^(f / EXP_STEPS) = e^(f * D1) with
    // |f * D1| <= 3.4e-4, so a cubic leaves an error below 6e-16.
    let f = u - (shifted - ROUND_SHIFT);
    // Multiplying by 2^(n >> EXP_BITS) is adding to the exponent bits; the range checks above
    // keep the result normal.
    let scaled = EXP2_TABLE[(n & (EXP_STEPS as i64 - 1)) as usize]
        .to_bits()
        .wrapping_add(((n >> EXP_BITS) << 52).cast_unsigned());
    f64::from_bits(scaled) * ((1.0 + f * D1) + (f * f) * (D2 + f * D3))
}

/// `x^y` for `x >= 0`. Negative or NaN `x` returns NaN, `powf(x, 0.0)` is `1.0`.
#[must_use]
#[inline]
pub fn powf(x: f32, y: f32) -> f32 {
    if y == 0.0 {
        return 1.0;
    }
    if x.is_nan() || y.is_nan() || x < 0.0 {
        return f32::NAN;
    }
    if x == 0.0 {
        return if y > 0.0 { 0.0 } else { f32::INFINITY };
    }
    if x.is_infinite() {
        return if y > 0.0 { f32::INFINITY } else { 0.0 };
    }
    exp2_steps(f64::from(y) * EXP_STEPS as f64 * log2(f64::from(x))) as f32
}

/// `e^x`.
#[must_use]
#[inline]
pub fn exp(x: f32) -> f32 {
    exp2_steps(f64::from(x) * (EXP_STEPS as f64 * LOG2_E)) as f32
}

/// `e^x` in `f64`, within about 1e-15 relative for `|x| < 10` (the error grows with `|x|`).
#[must_use]
pub fn exp_f64(x: f64) -> f64 {
    exp2_steps(x * (EXP_STEPS as f64 * LOG2_E))
}

/// Steps of `pi / SIN_STEPS` in the `sin`/`cos` table.
const SIN_STEPS: usize = 128;
/// `sin(k * pi / SIN_STEPS)` and `cos(k * pi / SIN_STEPS)` for `k` in `0..2 * SIN_STEPS`.
static SIN_COS_TABLE: [(f64, f64); 2 * SIN_STEPS] = sin_cos_table();

const fn sin_cos_table() -> [(f64, f64); 2 * SIN_STEPS] {
    let mut table = [(0.0, 0.0); 2 * SIN_STEPS];
    let mut k = 0;
    while k < 2 * SIN_STEPS {
        let a = k as f64 * PI_STEP_HI + k as f64 * PI_STEP_LO;
        let (mut s, mut c) = (0.0, 0.0);
        let mut term = 1.0;
        let mut i = 0;
        while i < 60 {
            if i % 2 == 0 {
                c += if i % 4 == 0 { term } else { -term };
            } else {
                s += if i % 4 == 1 { term } else { -term };
            }
            i += 1;
            term *= a / i as f64;
        }
        table[k] = (s, c);
        k += 1;
    }
    table
}

/// `sin(x)` and `cos(x)` in `f64` from `sin(a + r) = sin a cos r + cos a sin r`, with `a` the
/// nearest multiple of `pi / SIN_STEPS`, from the table, and Taylor series for the small `r`.
/// Measuring `x` in table steps with one rounded multiplication costs an absolute error of about
/// `|x| * 2e-16` radians, far below `f32` precision for every argument dith uses.
#[inline]
fn sin_cos_f64(x: f64) -> (f64, f64) {
    const STEP: f64 = PI_STEP_HI + PI_STEP_LO;
    const S1: f64 = STEP;
    const S3: f64 = -S1 * STEP * STEP / 6.0;
    const S5: f64 = -S3 * STEP * STEP / 20.0;
    const C2: f64 = -STEP * STEP / 2.0;
    const C4: f64 = -C2 * STEP * STEP / 12.0;
    const C6: f64 = -C4 * STEP * STEP / 30.0;
    let steps = x * (SIN_STEPS as f64 * FRAC_1_PI);
    let shifted = steps + ROUND_SHIFT;
    let n = shifted
        .to_bits()
        .wrapping_sub(ROUND_SHIFT_BITS)
        .cast_signed();
    // |f| <= 1/2 step and the subtraction is exact.
    let f = steps - (shifted - ROUND_SHIFT);
    let z = f * f;
    let sin_r = f * S1 + (f * z) * (S3 + z * S5);
    let cos_r = (1.0 + z * C2) + (z * z) * (C4 + z * C6);
    let (sin_a, cos_a) = SIN_COS_TABLE[(n & (2 * SIN_STEPS as i64 - 1)) as usize];
    (sin_a * cos_r + cos_a * sin_r, cos_a * cos_r - sin_a * sin_r)
}

/// `sin(x)`.
#[must_use]
#[inline]
pub fn sin(x: f32) -> f32 {
    sin_cos_f64(f64::from(x)).0 as f32
}

/// `cos(x)`.
#[must_use]
#[inline]
pub fn cos(x: f32) -> f32 {
    sin_cos_f64(f64::from(x)).1 as f32
}

/// `x^n` by repeated squaring in `f64`: one rounding per multiplication, so within about
/// `2 log2(n)` ulps of `f64` before the final rounding to `f32`.
#[must_use]
#[inline]
pub fn powi(x: f32, n: u32) -> f32 {
    let mut base = f64::from(x);
    let mut result = 1.0;
    let mut n = n;
    while n > 0 {
        if n & 1 == 1 {
            result *= base;
        }
        base *= base;
        n >>= 1;
    }
    result as f32
}

/// `sqrt(x^2 + y^2)`, computed in `f64` so neither the squares nor their sum overflow or round
/// early.
#[must_use]
#[inline]
pub fn hypot(x: f32, y: f32) -> f32 {
    let (x, y) = (f64::from(x), f64::from(y));
    (x * x + y * y).sqrt() as f32
}

/// Cube root in `f64`, within a few ulps.
#[must_use]
pub fn cbrt(x: f64) -> f64 {
    if x == 0.0 || !x.is_finite() {
        return x;
    }
    if x < 0.0 {
        return -cbrt(-x);
    }
    if x < f64::MIN_POSITIVE {
        // 2^54 has the exact cube root 2^18.
        return cbrt(x * 18_014_398_509_481_984.0) / 262_144.0;
    }
    let mut y = f64::from_bits(x.to_bits() / 3 + 0x2A9F_7893_782D_A1CE);
    for _ in 0..4 {
        y = (2.0 * y + x / (y * y)) / 3.0;
    }
    y
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Distance in representable `f32` steps between two finite values of the same sign.
    fn ulps(a: f32, b: f32) -> u32 {
        a.to_bits().abs_diff(b.to_bits())
    }

    /// Evenly spread positive `f32` values in `[lo, hi]`, stepping through the bit patterns.
    fn sweep(lo: f32, hi: f32, count: u32) -> impl Iterator<Item = f32> {
        let (a, b) = (lo.to_bits(), hi.to_bits());
        (0..=count).map(move |i| {
            f32::from_bits(a + ((u64::from(b - a) * u64::from(i)) / u64::from(count)) as u32)
        })
    }

    /// FNV-1a over the result bits, so a single changed bit anywhere changes the digest.
    fn digest(values: impl Iterator<Item = u32>) -> u64 {
        values.fold(0xcbf2_9ce4_8422_2325, |h, v| {
            (h ^ u64::from(v)).wrapping_mul(0x0100_0000_01b3)
        })
    }

    #[test]
    fn powf_is_faithful_against_the_f64_reference() {
        let mut worst = 0;
        for y in [2.4, 1.0 / 2.4, 0.1, 0.6, 1.6, 2.6, 5.0, 28.0, 0.9375] {
            for x in sweep(1e-6, 1.0, 200_000) {
                let reference = f64::from(x).powf(f64::from(y)) as f32;
                worst = worst.max(ulps(powf(x, y), reference));
            }
        }
        assert!(worst <= 1, "powf off by {worst} ulps");
    }

    #[test]
    fn powf_handles_identities_and_edges() {
        for x in sweep(1e-30, 1e30, 50_000) {
            assert_eq!(powf(x, 1.0), x, "powf({x}, 1)");
        }
        assert_eq!(powf(1.0, 28.0), 1.0);
        assert_eq!(powf(0.0, 2.4), 0.0);
        assert_eq!(powf(-0.0, 2.4), 0.0);
        assert_eq!(powf(0.0, -1.0), f32::INFINITY);
        assert_eq!(powf(0.25, 0.0), 1.0);
        assert_eq!(powf(0.5, 2.0), 0.25);
        assert!(powf(-0.5, 2.0).is_nan());
        assert_eq!(powf(1e-30, 28.0), 0.0);
        assert_eq!(powf(1e30, 28.0), f32::INFINITY);
    }

    #[test]
    fn powi_is_faithful_against_the_f64_reference() {
        for n in [0, 1, 2, 3, 7, 28, 31] {
            for x in sweep(1e-3, 1.0, 100_000) {
                let reference = f64::from(x).powf(f64::from(n)) as f32;
                assert!(ulps(powi(x, n), reference) <= 1, "powi({x}, {n})");
            }
        }
        assert_eq!(powi(0.0, 0), 1.0);
        assert_eq!(powi(0.0, 28), 0.0);
    }

    #[test]
    fn exp_is_faithful_against_the_f64_reference() {
        let mut worst = 0;
        for x in sweep(1e-8, 87.0, 200_000).flat_map(|x| [x, -x]) {
            let reference = f64::from(x).exp() as f32;
            worst = worst.max(ulps(exp(x), reference));
        }
        assert!(worst <= 1, "exp off by {worst} ulps");
        assert_eq!(exp(0.0), 1.0);
        assert_eq!(exp(-200.0), 0.0);
        assert_eq!(exp(200.0), f32::INFINITY);
        for i in -40i32..=40 {
            let x = f64::from(i) * 0.37;
            assert!(
                ((exp_f64(x) - x.exp()) / x.exp()).abs() < 1e-14,
                "exp_f64({x})"
            );
        }
    }

    #[test]
    fn sin_and_cos_are_faithful_against_the_f64_reference() {
        let mut worst = 0;
        for x in sweep(1e-6, 2_000.0, 300_000).flat_map(|x| [x, -x]) {
            let s = f64::from(x).sin() as f32;
            let c = f64::from(x).cos() as f32;
            // Near a zero the f32 result is tiny and ulps are meaningless; compare absolutely.
            if s.abs() > 1e-3 {
                worst = worst.max(ulps(sin(x), s));
            } else {
                assert!((sin(x) - s).abs() < 1e-10, "sin({x})");
            }
            if c.abs() > 1e-3 {
                worst = worst.max(ulps(cos(x), c));
            } else {
                assert!((cos(x) - c).abs() < 1e-10, "cos({x})");
            }
        }
        assert!(worst <= 1, "sin/cos off by {worst} ulps");
        assert_eq!(sin(0.0), 0.0);
        assert_eq!(cos(0.0), 1.0);
        assert!(sin(f32::INFINITY).is_nan());
    }

    #[test]
    fn hypot_and_cbrt_match_the_reference() {
        for x in sweep(1e-3, 4_000.0, 2_000) {
            for y in [0.0, 0.5, 3.0, 1_919.0] {
                let reference = f64::from(x).hypot(f64::from(y)) as f32;
                assert!(ulps(hypot(x, y), reference) <= 1, "hypot({x}, {y})");
            }
        }
        for x in sweep(1e-30, 1e30, 100_000) {
            let x = f64::from(x);
            assert!(((cbrt(x) - x.cbrt()) / x.cbrt()).abs() < 1e-15, "cbrt({x})");
        }
        for n in 1..=16u32 {
            let cube = f64::from(n * n * n);
            assert_eq!(cbrt(cube) as f32, n as f32);
        }
        assert_eq!(cbrt(0.0), 0.0);
        assert_eq!(cbrt(-27.0) as f32, -3.0);
        assert!((cbrt(1e-310) - 1e-310f64.cbrt()).abs() < 1e-118);
    }

    /// The exact bits on every platform: CI runs this on x86-64 Linux, arm64 Linux and macOS, and
    /// the wasm build gives the same digests.
    #[test]
    fn results_are_bit_identical_everywhere() {
        let inputs = || sweep(1e-7, 1.0, 100_000);
        let got = [
            digest(inputs().map(|x| powf(x, 2.4).to_bits())),
            digest(inputs().map(|x| powf(x, 1.0 / 2.4).to_bits())),
            digest(inputs().map(|x| exp(-40.0 * x).to_bits())),
            digest(inputs().map(|x| sin(900.0 * x).to_bits())),
            digest(inputs().map(|x| cos(900.0 * x).to_bits())),
            digest(inputs().map(|x| (cbrt(f64::from(x)) as f32).to_bits())),
        ];
        assert_eq!(
            got,
            [
                0x9a93_3ba9_e3cc_cd68,
                0x38f6_1ea3_7203_2aae,
                0x974d_de8f_a037_9964,
                0x4b5c_8f0f_e705_3a0e,
                0xf27c_db50_c740_02f2,
                0x2eb4_3c4a_51fd_ce0c,
            ]
        );
    }
}
