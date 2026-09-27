//! Ports of the fdlibm routines `java.lang.StrictMath` specifies — and, on the
//! reference JVM, `java.lang.Math` answers bit-for-bit — for the `kotlin.math`
//! functions that delegate to them. Each is a line-for-line transliteration of
//! the C original (Sun fdlibm 5.3), so every intermediate rounding matches.

/// The high 32 bits of `x`, as fdlibm's `__HI` reads them (signed).
fn hi(x: f64) -> i32 {
    (x.to_bits() >> 32) as i32
}

/// The low 32 bits of `x`, as fdlibm's `__LO` reads them.
fn lo(x: f64) -> u32 {
    x.to_bits() as u32
}

/// `x` with its high word replaced, as an fdlibm `__HI(x) = h` store.
fn with_hi(x: f64, h: i32) -> f64 {
    f64::from_bits((u64::from(h as u32) << 32) | u64::from(lo(x)))
}

// Every constant below is the shortest decimal that parses to the same IEEE
// word as fdlibm's own spelling, which is quoted above it — the same bits, so
// no intermediate rounding moves. `the_constants_are_fdlibms_words` pins each
// one to the hex word the fdlibm source gives beside it.

/// `0.33333333333333333`
const THIRD: f64 = 3.333333333333333e-1;
/// `0.66666666666666666`
const TWO_THIRDS: f64 = 6.666666666666666e-1;

/// `6.93147180369123816490e-01`
const LN2_HI: f64 = 6.931471803691238e-1;
/// `1.90821492927058770002e-10`
const LN2_LO: f64 = 1.9082149292705877e-10;
/// `1.80143985094819840000e+16`
const TWO54: f64 = 1.8014398509481984e16;
/// `6.666666666666735130e-01`
const LG1: f64 = 6.666666666666735e-1;
/// `3.999999999940941908e-01`
const LG2: f64 = 3.999999999940942e-1;
/// `2.857142874366239149e-01`
const LG3: f64 = 2.857142874366239e-1;
/// `2.222219843214978396e-01`
const LG4: f64 = 2.2222198432149784e-1;
/// `1.818357216161805012e-01`
const LG5: f64 = 1.818357216161805e-1;
/// `1.531383769920937332e-01`
const LG6: f64 = 1.5313837699209373e-1;
/// `1.479819860511658591e-01`
const LG7: f64 = 1.4798198605116586e-1;

/// `e_log.c`: the natural logarithm.
pub fn log(mut x: f64) -> f64 {
    let mut hx = hi(x);
    let lx = lo(x);
    let mut k: i32 = 0;
    if hx < 0x0010_0000 {
        if ((hx & 0x7fff_ffff) as u32 | lx) == 0 {
            return -TWO54 / 0.0;
        }
        if hx < 0 {
            return f64::NAN; // fdlibm: (x - x) / zero
        }
        k -= 54;
        x *= TWO54;
        hx = hi(x);
    }
    if hx >= 0x7ff0_0000 {
        return x + x;
    }
    k += (hx >> 20) - 1023;
    hx &= 0x000f_ffff;
    let i = (hx + 0x95f64) & 0x0010_0000;
    x = with_hi(x, hx | (i ^ 0x3ff0_0000));
    k += i >> 20;
    let f = x - 1.0;
    if (0x000f_ffff & (2 + hx)) < 3 {
        if f == 0.0 {
            if k == 0 {
                return 0.0;
            }
            let dk = f64::from(k);
            return dk * LN2_HI + dk * LN2_LO;
        }
        let r = f * f * (0.5 - THIRD * f);
        if k == 0 {
            return f - r;
        }
        let dk = f64::from(k);
        return dk * LN2_HI - ((r - dk * LN2_LO) - f);
    }
    let s = f / (2.0 + f);
    let dk = f64::from(k);
    let z = s * s;
    let mut i = hx - 0x6147a;
    let w = z * z;
    let j = 0x6b851 - hx;
    let t1 = w * (LG2 + w * (LG4 + w * LG6));
    let t2 = z * (LG1 + w * (LG3 + w * (LG5 + w * LG7)));
    i |= j;
    let r = t2 + t1;
    if i > 0 {
        let hfsq = 0.5 * f * f;
        if k == 0 {
            f - (hfsq - s * (hfsq + r))
        } else {
            dk * LN2_HI - ((hfsq - (s * (hfsq + r) + dk * LN2_LO)) - f)
        }
    } else if k == 0 {
        f - s * (f - r)
    } else {
        dk * LN2_HI - ((s * (f - r) - dk * LN2_LO) - f)
    }
}

/// `4.34294481903251816668e-01`
// fdlibm's `ivln10` is the same word as `LOG10_E`.
const IVLN10: f64 = std::f64::consts::LOG10_E;
/// `3.01029995663611771306e-01`
const LOG10_2HI: f64 = 3.0102999566361177e-1;
/// `3.69423907715893078616e-13`
const LOG10_2LO: f64 = 3.694239077158931e-13;

/// `e_log10.c`: the base-10 logarithm.
pub fn log10(mut x: f64) -> f64 {
    let mut hx = hi(x);
    let lx = lo(x);
    let mut k: i32 = 0;
    if hx < 0x0010_0000 {
        if ((hx & 0x7fff_ffff) as u32 | lx) == 0 {
            return -TWO54 / 0.0;
        }
        if hx < 0 {
            return f64::NAN; // fdlibm: (x - x) / zero
        }
        k -= 54;
        x *= TWO54;
        hx = hi(x);
    }
    if hx >= 0x7ff0_0000 {
        return x + x;
    }
    k += (hx >> 20) - 1023;
    let i = ((k as u32) & 0x8000_0000) >> 31;
    let i = i as i32;
    hx = (hx & 0x000f_ffff) | ((0x3ff - i) << 20);
    let y = f64::from(k + i);
    x = with_hi(x, hx);
    let z = y * LOG10_2LO + IVLN10 * log(x);
    z + y * LOG10_2HI
}

/// `6.666666666666735130e-01`
const LP1: f64 = 6.666666666666735e-1;
/// `3.999999999940941908e-01`
const LP2: f64 = 3.999999999940942e-1;
/// `2.857142874366239149e-01`
const LP3: f64 = 2.857142874366239e-1;
/// `2.222219843214978396e-01`
const LP4: f64 = 2.2222198432149784e-1;
/// `1.818357216161805012e-01`
const LP5: f64 = 1.818357216161805e-1;
/// `1.531383769920937332e-01`
const LP6: f64 = 1.5313837699209373e-1;
/// `1.479819860511658591e-01`
const LP7: f64 = 1.4798198605116586e-1;

/// `s_log1p.c`: `ln(1 + x)`, accurate for `x` near zero.
pub fn log1p(x: f64) -> f64 {
    let hx = hi(x);
    let ax = hx & 0x7fff_ffff;
    let mut k: i32 = 1;
    let mut f = 0.0;
    let mut hu: i32 = 0;
    let mut c = 0.0;
    if hx < 0x3FDA_827A {
        if ax >= 0x3ff0_0000 {
            if x == -1.0 {
                return -TWO54 / 0.0;
            }
            return f64::NAN; // fdlibm: (x - x) / (x - x)
        }
        if ax < 0x3e20_0000 {
            if TWO54 + x > 0.0 && ax < 0x3c90_0000 {
                return x;
            }
            return x - x * x * 0.5;
        }
        if hx > 0 || hx <= 0xbfd2_bec3_u32 as i32 {
            k = 0;
            f = x;
            hu = 1;
        }
    }
    if hx >= 0x7ff0_0000 {
        return x + x;
    }
    if k != 0 {
        let mut u;
        if hx < 0x4340_0000 {
            u = 1.0 + x;
            hu = hi(u);
            k = (hu >> 20) - 1023;
            c = if k > 0 { 1.0 - (u - x) } else { x - (u - 1.0) };
            c /= u;
        } else {
            u = x;
            hu = hi(u);
            k = (hu >> 20) - 1023;
            c = 0.0;
        }
        hu &= 0x000f_ffff;
        if hu < 0x6a09e {
            u = with_hi(u, hu | 0x3ff0_0000);
        } else {
            k += 1;
            u = with_hi(u, hu | 0x3fe0_0000);
            hu = (0x0010_0000 - hu) >> 2;
        }
        f = u - 1.0;
    }
    let hfsq = 0.5 * f * f;
    let dk = f64::from(k);
    if hu == 0 {
        if f == 0.0 {
            if k == 0 {
                return 0.0;
            }
            c += dk * LN2_LO;
            return dk * LN2_HI + c;
        }
        let r = hfsq * (1.0 - TWO_THIRDS * f);
        if k == 0 {
            return f - r;
        }
        return dk * LN2_HI - ((r - (dk * LN2_LO + c)) - f);
    }
    let s = f / (2.0 + f);
    let z = s * s;
    let r = z * (LP1 + z * (LP2 + z * (LP3 + z * (LP4 + z * (LP5 + z * (LP6 + z * LP7))))));
    if k == 0 {
        f - (hfsq - s * (hfsq + r))
    } else {
        dk * LN2_HI - ((hfsq - (s * (hfsq + r) + (dk * LN2_LO + c))) - f)
    }
}

/// `7.09782712893383973096e+02`
const O_THRESHOLD: f64 = 7.09782712893384e2;
/// `-7.45133219101941108420e+02`
const U_THRESHOLD: f64 = -7.451332191019411e2;
const HUGE: f64 = 1.0e+300;
/// `9.33263618503218878990e-302`
const TWOM1000: f64 = 9.332636185032189e-302;
/// `1.44269504088896338700e+00`
// fdlibm's `invln2` is the same word as `LOG2_E`.
const INVLN2: f64 = std::f64::consts::LOG2_E;
/// `1.66666666666666019037e-01`
const P1: f64 = 1.6666666666666602e-1;
/// `-2.77777777770155933842e-03`
const P2: f64 = -2.7777777777015593e-3;
/// `6.61375632143793436117e-05`
const P3: f64 = 6.613756321437934e-5;
/// `-1.65339022054652515390e-06`
const P4: f64 = -1.6533902205465252e-6;
/// `4.13813679705723846039e-08`
const P5: f64 = 4.1381367970572385e-8;

/// `e_exp.c`: `e` raised to `x`.
pub fn exp(mut x: f64) -> f64 {
    let ln2hi = [LN2_HI, -LN2_HI];
    let ln2lo = [LN2_LO, -LN2_LO];
    let half = [0.5, -0.5];
    let mut hx = hi(x) as u32;
    let xsb = ((hx >> 31) & 1) as usize;
    hx &= 0x7fff_ffff;
    if hx >= 0x4086_2E42 {
        if hx >= 0x7ff0_0000 {
            if ((hx & 0xfffff) | lo(x)) != 0 {
                return x + x;
            }
            return if xsb == 0 { x } else { 0.0 };
        }
        if x > O_THRESHOLD {
            return HUGE * HUGE;
        }
        if x < U_THRESHOLD {
            return TWOM1000 * TWOM1000;
        }
    }
    let (mut hi_part, mut lo_part, mut k) = (0.0, 0.0, 0i32);
    if hx > 0x3fd6_2e42 {
        if hx < 0x3FF0_A2B2 {
            hi_part = x - ln2hi[xsb];
            lo_part = ln2lo[xsb];
            k = 1 - xsb as i32 - xsb as i32;
        } else {
            k = (INVLN2 * x + half[xsb]) as i32;
            let t = f64::from(k);
            hi_part = x - t * ln2hi[0];
            lo_part = t * ln2lo[0];
        }
        x = hi_part - lo_part;
    } else if hx < 0x3e30_0000 && HUGE + x > 1.0 {
        // `|x| < 2**-28`: fdlibm's `huge + x > one` raises inexact and is
        // always true here.
        return 1.0 + x;
    }
    let t = x * x;
    let c = x - t * (P1 + t * (P2 + t * (P3 + t * (P4 + t * P5))));
    if k == 0 {
        return 1.0 - ((x * c) / (c - 2.0) - x);
    }
    let y = 1.0 - ((lo_part - (x * c) / (2.0 - c)) - hi_part);
    if k >= -1021 {
        with_hi(y, hi(y).wrapping_add(k << 20))
    } else {
        with_hi(y, hi(y).wrapping_add((k + 1000) << 20)) * TWOM1000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each constant against the IEEE word fdlibm 5.3 prints beside it.
    #[test]
    fn the_constants_are_fdlibms_words() {
        for (v, word) in [
            (THIRD, 0x3FD5_5555_5555_5555u64),
            (TWO_THIRDS, 0x3FE5_5555_5555_5555),
            (LN2_HI, 0x3FE6_2E42_FEE0_0000),
            (LN2_LO, 0x3DEA_39EF_3579_3C76),
            (TWO54, 0x4350_0000_0000_0000),
            (LG1, 0x3FE5_5555_5555_5593),
            (LG2, 0x3FD9_9999_9997_FA04),
            (LG3, 0x3FD2_4924_9422_9359),
            (LG4, 0x3FCC_71C5_1D8E_78AF),
            (LG5, 0x3FC7_4664_96CB_03DE),
            (LG6, 0x3FC3_9A09_D078_C69F),
            (LG7, 0x3FC2_F112_DF3E_5244),
            (IVLN10, 0x3FDB_CB7B_1526_E50E),
            (LOG10_2HI, 0x3FD3_4413_509F_6000),
            (LOG10_2LO, 0x3D59_FEF3_11F1_2B36),
            (LP1, 0x3FE5_5555_5555_5593),
            (LP7, 0x3FC2_F112_DF3E_5244),
            (O_THRESHOLD, 0x4086_2E42_FEFA_39EF),
            (U_THRESHOLD, 0xC087_4910_D52D_3051),
            (TWOM1000, 0x0170_0000_0000_0000),
            (INVLN2, 0x3FF7_1547_652B_82FE),
            (P1, 0x3FC5_5555_5555_553E),
            (P2, 0xBF66_C16C_16BE_BD93),
            (P3, 0x3F11_566A_AF25_DE2C),
            (P4, 0xBEBB_BD41_C5D2_6BF1),
            (P5, 0x3E66_3769_72BE_A4D0),
        ] {
            assert_eq!(v.to_bits(), word, "{v:e}");
        }
    }

    /// Answers the reference JVM gives where a correctly rounded result does
    /// not (measured on JDK 21: `Math.exp(1.0)`, `Math.log10(1.0 / 7.0)`).
    #[test]
    fn last_place_answers_match_the_jvm() {
        assert_eq!(exp(1.0), 2.7182818284590455);
        assert_eq!(log10(1.0 / 7.0), -0.8450980400142569);
        assert_eq!(log(0.0), f64::NEG_INFINITY);
        assert!(log(-1.0).is_nan());
        assert_eq!(log1p(-1.0), f64::NEG_INFINITY);
        assert_eq!(exp(-746.0), 0.0);
        assert_eq!(exp(710.0), f64::INFINITY);
    }
}
