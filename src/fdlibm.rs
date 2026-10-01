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

// ── The trigonometric, hyperbolic and power routines ─────────────────────────
//
// Each is fdlibm 5.3's routine as `java.lang.StrictMath` runs it in JDK 21,
// where `java.lang.Math` answers the same bits for every one of these on the
// reference JVM (measured over 400 000 inputs per function). `sin` and `cos`
// are NOT here: the reference JVM's `Math.sin`/`Math.cos` are a platform
// intrinsic that disagrees with fdlibm in the last place on about one input in
// twenty, so no port of fdlibm answers what Kotlin answers (see BUGS.md).
//
// Constants are written as the shortest decimal that parses to fdlibm's word;
// `the_trig_constants_are_fdlibms_words` pins each one to its hex spelling.

/// `x` with its low word replaced, as an fdlibm `__LO(x) = l` store.
fn with_lo(x: f64, l: u32) -> f64 {
    f64::from_bits((x.to_bits() & 0xFFFF_FFFF_0000_0000) | u64::from(l))
}

/// The double whose high and low words are `h` and `l`.
fn from_words(h: i32, l: u32) -> f64 {
    f64::from_bits((u64::from(h as u32) << 32) | u64::from(l))
}

/// `2^k` for `k` in the normal exponent range, exactly.
fn two_to(k: i32) -> f64 {
    f64::from_bits(((k + 1023) as u64) << 52)
}

/// `java.lang.Math.scalb`: `d * 2^n` with a single rounding. The scale is
/// applied in steps of `2^±512` after a first step of `n mod 512`, so every
/// step but the last is exact — the order matters for a subnormal result.
fn scalb(mut d: f64, n: i32) -> f64 {
    const MAX_SCALE: i32 = 1023 + 1022 + 53 + 1;
    let (mut n, increment, delta) = if n < 0 {
        (n.max(-MAX_SCALE), -512, two_to(-512))
    } else {
        (n.min(MAX_SCALE), 512, two_to(512))
    };
    let t = ((n >> 8) as u32 >> 23) as i32;
    let adjust = ((n + t) & 511) - t;
    d *= two_to(adjust);
    n -= adjust;
    while n != 0 {
        d *= delta;
        n -= increment;
    }
    d
}

const TWO24: f64 = 16777216.0;
const TINY: f64 = 1.0e-300;

/// `pi/2` and `pi/4` split for the inverse functions.
const PIO2_HI: f64 = std::f64::consts::FRAC_PI_2;
const PIO2_LO: f64 = 6.123233995736766e-17;
const PIO4_HI: f64 = std::f64::consts::FRAC_PI_4;
const PI_LO: f64 = 1.2246467991473532e-16;

/// `s_atan.c`: the arctangent.
pub fn atan(mut x: f64) -> f64 {
    const ATANHI: [f64; 4] = [
        0.4636476090008061,
        std::f64::consts::FRAC_PI_4,
        0.982793723247329,
        std::f64::consts::FRAC_PI_2,
    ];
    const ATANLO: [f64; 4] = [
        2.2698777452961687e-17,
        3.061616997868383e-17,
        1.3903311031230998e-17,
        6.123233995736766e-17,
    ];
    const AT: [f64; 11] = [
        0.3333333333333293,
        -0.19999999999876483,
        0.14285714272503466,
        -0.11111110405462356,
        0.09090887133436507,
        -0.0769187620504483,
        0.06661073137387531,
        -0.058335701337905735,
        0.049768779946159324,
        -0.036531572744216916,
        0.016285820115365782,
    ];
    let hx = hi(x);
    let ix = hx & 0x7fff_ffff;
    let id: i32;
    if ix >= 0x4410_0000 {
        // |x| >= 2^66
        if ix > 0x7ff0_0000 || (ix == 0x7ff0_0000 && lo(x) != 0) {
            return x + x;
        }
        return if hx > 0 {
            ATANHI[3] + ATANLO[3]
        } else {
            -ATANHI[3] - ATANLO[3]
        };
    }
    if ix < 0x3fdc_0000 {
        // |x| < 0.4375
        if ix < 0x3e20_0000 && HUGE + x > 1.0 {
            return x;
        }
        id = -1;
    } else {
        x = x.abs();
        if ix < 0x3ff3_0000 {
            if ix < 0x3fe6_0000 {
                id = 0;
                x = (2.0 * x - 1.0) / (2.0 + x);
            } else {
                id = 1;
                x = (x - 1.0) / (x + 1.0);
            }
        } else if ix < 0x4003_8000 {
            id = 2;
            x = (x - 1.5) / (1.0 + 1.5 * x);
        } else {
            id = 3;
            x = -1.0 / x;
        }
    }
    let z = x * x;
    let w = z * z;
    let s1 = z * (AT[0] + w * (AT[2] + w * (AT[4] + w * (AT[6] + w * (AT[8] + w * AT[10])))));
    let s2 = w * (AT[1] + w * (AT[3] + w * (AT[5] + w * (AT[7] + w * AT[9]))));
    if id < 0 {
        return x - x * (s1 + s2);
    }
    let id = id as usize;
    let z = ATANHI[id] - ((x * (s1 + s2) - ATANLO[id]) - x);
    if hx < 0 {
        -z
    } else {
        z
    }
}

/// `e_atan2.c`: the angle of the point `(x, y)`.
pub fn atan2(y: f64, x: f64) -> f64 {
    const PI_O_4: f64 = std::f64::consts::FRAC_PI_4;
    const PI_O_2: f64 = std::f64::consts::FRAC_PI_2;
    let pi = std::f64::consts::PI;
    let (hx, lx) = (hi(x), lo(x));
    let ix = hx & 0x7fff_ffff;
    let (hy, ly) = (hi(y), lo(y));
    let iy = hy & 0x7fff_ffff;
    if x.is_nan() || y.is_nan() {
        return x + y;
    }
    if ((hx.wrapping_sub(0x3ff0_0000)) as u32 | lx) == 0 {
        return atan(y); // x == 1.0
    }
    let m = ((hy >> 31) & 1) | ((hx >> 30) & 2);
    if (iy as u32 | ly) == 0 {
        return match m {
            0 | 1 => y,
            2 => pi + TINY,
            _ => -pi - TINY,
        };
    }
    if (ix as u32 | lx) == 0 {
        return if hy < 0 {
            -PI_O_2 - TINY
        } else {
            PI_O_2 + TINY
        };
    }
    if ix == 0x7ff0_0000 {
        return if iy == 0x7ff0_0000 {
            match m {
                0 => PI_O_4 + TINY,
                1 => -PI_O_4 - TINY,
                2 => 3.0 * PI_O_4 + TINY,
                _ => -3.0 * PI_O_4 - TINY,
            }
        } else {
            match m {
                0 => 0.0,
                1 => -0.0,
                2 => pi + TINY,
                _ => -pi - TINY,
            }
        };
    }
    if iy == 0x7ff0_0000 {
        return if hy < 0 {
            -PI_O_2 - TINY
        } else {
            PI_O_2 + TINY
        };
    }
    let k = (iy - ix) >> 20;
    let z = if k > 60 {
        PI_O_2 + 0.5 * PI_LO
    } else if hx < 0 && k < -60 {
        0.0
    } else {
        atan((y / x).abs())
    };
    match m {
        0 => z,
        1 => -z,
        2 => pi - (z - PI_LO),
        _ => (z - PI_LO) - pi,
    }
}

const PS0: f64 = 0.16666666666666666;
const PS1: f64 = -0.3255658186224009;
const PS2: f64 = 0.20121253213486293;
const PS3: f64 = -0.04005553450067941;
const PS4: f64 = 7.915349942898145e-4;
const PS5: f64 = 3.479331075960212e-5;
const QS1: f64 = -2.403394911734414;
const QS2: f64 = 2.0209457602335057;
const QS3: f64 = -0.6882839716054533;
const QS4: f64 = 0.07703815055590194;

/// The rational approximation `asin`/`acos` share: `(p(t), q(t))`.
fn asin_pq(t: f64) -> (f64, f64) {
    let p = t * (PS0 + t * (PS1 + t * (PS2 + t * (PS3 + t * (PS4 + t * PS5)))));
    let q = 1.0 + t * (QS1 + t * (QS2 + t * (QS3 + t * QS4)));
    (p, q)
}

/// `e_asin.c`: the arcsine.
pub fn asin(x: f64) -> f64 {
    let hx = hi(x);
    let ix = hx & 0x7fff_ffff;
    if ix >= 0x3ff0_0000 {
        if ((ix - 0x3ff0_0000) as u32 | lo(x)) == 0 {
            return x * PIO2_HI + x * PIO2_LO; // asin(±1) = ±pi/2
        }
        return f64::NAN; // |x| > 1
    }
    if ix < 0x3fe0_0000 {
        // |x| < 0.5
        let mut t = 0.0;
        if ix < 0x3e40_0000 {
            if HUGE + x > 1.0 {
                return x;
            }
        } else {
            t = x * x;
        }
        let (p, q) = asin_pq(t);
        return x + x * (p / q);
    }
    let w = 1.0 - x.abs();
    let t = w * 0.5;
    let (p, q) = asin_pq(t);
    let s = t.sqrt();
    let t = if ix >= 0x3FEF_3333 {
        // |x| > 0.975
        PIO2_HI - (2.0 * (s + s * (p / q)) - PIO2_LO)
    } else {
        let w = with_lo(s, 0);
        let c = (t - w * w) / (s + w);
        let r = p / q;
        let p = 2.0 * s * r - (PIO2_LO - 2.0 * c);
        let q = PIO4_HI - 2.0 * w;
        PIO4_HI - (p - q)
    };
    if hx > 0 {
        t
    } else {
        -t
    }
}

/// `e_acos.c`: the arccosine.
pub fn acos(x: f64) -> f64 {
    let pi = std::f64::consts::PI;
    let hx = hi(x);
    let ix = hx & 0x7fff_ffff;
    if ix >= 0x3ff0_0000 {
        if ((ix - 0x3ff0_0000) as u32 | lo(x)) == 0 {
            return if hx > 0 { 0.0 } else { pi + 2.0 * PIO2_LO };
        }
        return f64::NAN; // |x| > 1
    }
    if ix < 0x3fe0_0000 {
        // |x| < 0.5
        if ix <= 0x3c60_0000 {
            return PIO2_HI + PIO2_LO;
        }
        let z = x * x;
        let (p, q) = asin_pq(z);
        let r = p / q;
        return PIO2_HI - (x - (PIO2_LO - x * r));
    }
    if hx < 0 {
        // x < -0.5
        let z = (1.0 + x) * 0.5;
        let (p, q) = asin_pq(z);
        let s = z.sqrt();
        let r = p / q;
        let w = r * s - PIO2_LO;
        return pi - 2.0 * (s + w);
    }
    // x > 0.5
    let z = (1.0 - x) * 0.5;
    let s = z.sqrt();
    let df = with_lo(s, 0);
    let c = (z - df * df) / (s + df);
    let (p, q) = asin_pq(z);
    let r = p / q;
    let w = r * s + c;
    2.0 * (df + w)
}

/// `s_cbrt.c`: the real cube root.
pub fn cbrt(x: f64) -> f64 {
    const B1: i32 = 715_094_163;
    const B2: i32 = 696_219_795;
    const C: f64 = 0.5428571428571428;
    const D: f64 = -0.7053061224489796;
    const E: f64 = 1.4142857142857144;
    const F: f64 = 1.6071428571428572;
    const G: f64 = 0.35714285714285715;
    if x == 0.0 || !x.is_finite() {
        return x;
    }
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let mut t = if x < f64::MIN_POSITIVE {
        let t = TWO54 * x;
        with_hi(t, hi(t) / 3 + B2)
    } else {
        with_hi(0.0, hi(x) / 3 + B1)
    };
    let r = t * t / x;
    let s = C + r * t;
    t *= G + F / (s + E + D / s);
    t = with_lo(t, 0);
    t = with_hi(t, hi(t) + 1);
    let s = t * t;
    let r = x / s;
    let w = t + t;
    let r = (r - t) / (w + r);
    t += t * r;
    sign * t
}

/// `e_hypot.c`: `sqrt(x*x + y*y)` without undue overflow or underflow.
pub fn hypot(x: f64, y: f64) -> f64 {
    let (mut a, mut b) = (x.abs(), y.abs());
    if !a.is_finite() || !b.is_finite() {
        return if a == f64::INFINITY || b == f64::INFINITY {
            f64::INFINITY
        } else {
            a + b // NaN
        };
    }
    if b > a {
        std::mem::swap(&mut a, &mut b);
    }
    let (mut ha, mut hb) = (hi(a), hi(b));
    if ha - hb > 0x3c0_0000 {
        return a + b; // a/b > 2^60
    }
    let mut k = 0;
    if a > 3.2733937296446915e150 {
        ha -= 0x2580_0000;
        hb -= 0x2580_0000;
        a *= two_to(-600);
        b *= two_to(-600);
        k += 600;
    }
    if b < two_to(-500) {
        if b < f64::MIN_POSITIVE {
            if b == 0.0 {
                return a;
            }
            let t1 = two_to(1022);
            b *= t1;
            a *= t1;
            k -= 1022;
        } else {
            ha += 0x2580_0000;
            hb += 0x2580_0000;
            a *= two_to(600);
            b *= two_to(600);
            k -= 600;
        }
    }
    let mut w = a - b;
    if w > b {
        let t1 = with_hi(0.0, ha);
        let t2 = a - t1;
        w = (t1 * t1 - (b * (-b) - t2 * (a + t1))).sqrt();
    } else {
        a += a;
        let y1 = with_hi(0.0, hb);
        let y2 = b - y1;
        let t1 = with_hi(0.0, ha + 0x0010_0000);
        let t2 = a - t1;
        w = (t1 * y1 - (w * (-w) - (t1 * y2 + t2 * b))).sqrt();
    }
    if k != 0 {
        two_to(k) * w
    } else {
        w
    }
}

/// `s_expm1.c`: `e^x - 1`, accurate near zero.
pub fn expm1(mut x: f64) -> f64 {
    const LN2_HI_M: f64 = 0.6931471803691238;
    const LN2_LO_M: f64 = 1.9082149292705877e-10;
    const Q1: f64 = -0.03333333333333313;
    const Q2: f64 = 0.0015873015872548146;
    const Q3: f64 = -7.93650757867488e-5;
    const Q4: f64 = 4.008217827329362e-6;
    const Q5: f64 = -2.0109921818362437e-7;
    let mut hx = hi(x);
    let xsb = hx as u32 & 0x8000_0000;
    hx &= 0x7fff_ffff;
    if hx >= 0x4043_687A {
        // |x| >= 56 ln2
        if hx >= 0x4086_2E42 {
            // |x| >= 709.78
            if hx >= 0x7ff0_0000 {
                if ((hx & 0xf_ffff) as u32 | lo(x)) != 0 {
                    return x + x;
                }
                return if xsb == 0 { x } else { -1.0 };
            }
            if x > O_THRESHOLD {
                return HUGE * HUGE;
            }
        }
        if xsb != 0 && x + TINY < 0.0 {
            return TINY - 1.0;
        }
    }
    let k: i32;
    let mut c = 0.0;
    if hx > 0x3fd6_2e42 {
        // |x| > 0.5 ln2
        let (hi_part, lo_part);
        if hx < 0x3FF0_A2B2 {
            if xsb == 0 {
                hi_part = x - LN2_HI_M;
                lo_part = LN2_LO_M;
                k = 1;
            } else {
                hi_part = x + LN2_HI_M;
                lo_part = -LN2_LO_M;
                k = -1;
            }
        } else {
            k = (INVLN2 * x + if xsb == 0 { 0.5 } else { -0.5 }) as i32;
            let t = f64::from(k);
            hi_part = x - t * LN2_HI_M;
            lo_part = t * LN2_LO_M;
        }
        x = hi_part - lo_part;
        c = (hi_part - x) - lo_part;
    } else if hx < 0x3c90_0000 {
        // |x| < 2^-54
        let t = HUGE + x;
        return x - (t - (HUGE + x));
    } else {
        k = 0;
    }
    let hfx = 0.5 * x;
    let hxs = x * hfx;
    let r1 = 1.0 + hxs * (Q1 + hxs * (Q2 + hxs * (Q3 + hxs * (Q4 + hxs * Q5))));
    let t = 3.0 - r1 * hfx;
    let mut e = hxs * ((r1 - t) / (6.0 - x * t));
    if k == 0 {
        return x - (x * e - hxs);
    }
    e = x * (e - c) - c;
    e -= hxs;
    if k == -1 {
        return 0.5 * (x - e) - 0.5;
    }
    if k == 1 {
        return if x < -0.25 {
            -2.0 * (e - (x + 0.5))
        } else {
            1.0 + 2.0 * (x - e)
        };
    }
    if k <= -2 || k > 56 {
        let y = 1.0 - (e - x);
        let y = with_hi(y, hi(y).wrapping_add(k << 20));
        return y - 1.0;
    }
    if k < 20 {
        let t = with_hi(1.0, 0x3ff0_0000 - (0x20_0000 >> k));
        let y = t - (e - x);
        with_hi(y, hi(y).wrapping_add(k << 20))
    } else {
        let t = with_hi(1.0, (0x3ff - k) << 20);
        let y = x - (e + t) + 1.0;
        with_hi(y, hi(y).wrapping_add(k << 20))
    }
}

/// `e_sinh.c`: the hyperbolic sine.
pub fn sinh(x: f64) -> f64 {
    const SHUGE: f64 = 1.0e307;
    let jx = hi(x);
    let ix = jx & 0x7fff_ffff;
    if ix >= 0x7ff0_0000 {
        return x + x;
    }
    let h = if jx < 0 { -0.5 } else { 0.5 };
    if ix < 0x4036_0000 {
        // |x| < 22
        if ix < 0x3e30_0000 && SHUGE + x > 1.0 {
            return x;
        }
        let t = expm1(x.abs());
        if ix < 0x3ff0_0000 {
            return h * (2.0 * t - t * t / (t + 1.0));
        }
        return h * (t + t / (t + 1.0));
    }
    if ix < 0x4086_2E42 {
        return h * exp(x.abs());
    }
    if ix < 0x4086_33CE || (ix == 0x4086_33ce && lo(x) <= 0x8fb9_f87d) {
        let w = exp(0.5 * x.abs());
        let t = h * w;
        return t * w;
    }
    x * SHUGE
}

/// `e_cosh.c`: the hyperbolic cosine.
pub fn cosh(x: f64) -> f64 {
    let ix = hi(x) & 0x7fff_ffff;
    if ix >= 0x7ff0_0000 {
        return x * x;
    }
    if ix < 0x3fd6_2e43 {
        // |x| < 0.5 ln2
        let t = expm1(x.abs());
        let w = 1.0 + t;
        if ix < 0x3c80_0000 {
            return w;
        }
        return 1.0 + (t * t) / (w + w);
    }
    if ix < 0x4036_0000 {
        let t = exp(x.abs());
        return 0.5 * t + 0.5 / t;
    }
    if ix < 0x4086_2E42 {
        return 0.5 * exp(x.abs());
    }
    if ix < 0x4086_33CE || (ix == 0x4086_33ce && lo(x) <= 0x8fb9_f87d) {
        let w = exp(0.5 * x.abs());
        let t = 0.5 * w;
        return t * w;
    }
    HUGE * HUGE
}

/// `s_tanh.c`: the hyperbolic tangent.
pub fn tanh(x: f64) -> f64 {
    let jx = hi(x);
    let ix = jx & 0x7fff_ffff;
    if ix >= 0x7ff0_0000 {
        return if jx >= 0 {
            1.0 / x + 1.0
        } else {
            1.0 / x - 1.0
        };
    }
    let z = if ix < 0x4036_0000 {
        // |x| < 22
        if ix < 0x3c80_0000 {
            return x * (1.0 + x);
        }
        if ix >= 0x3ff0_0000 {
            let t = expm1(2.0 * x.abs());
            1.0 - 2.0 / (t + 2.0)
        } else {
            let t = expm1(-2.0 * x.abs());
            -t / (t + 2.0)
        }
    } else {
        1.0 - TINY
    };
    if jx >= 0 {
        z
    } else {
        -z
    }
}

/// `s_tan.c`: the tangent.
pub fn tan(x: f64) -> f64 {
    let ix = hi(x) & 0x7fff_ffff;
    if ix <= 0x3fe9_21fb {
        return kernel_tan(x, 0.0, 1);
    }
    if ix >= 0x7ff0_0000 {
        return f64::NAN; // tan(±Inf), tan(NaN)
    }
    let (n, y0, y1) = rem_pio2(x);
    kernel_tan(y0, y1, 1 - ((n & 1) << 1))
}

/// `k_tan.c`: the tangent on `[-pi/4, pi/4]` of `x + y`, or `-1/tan` when
/// `iy` is -1.
fn kernel_tan(mut x: f64, mut y: f64, iy: i32) -> f64 {
    const PIO4: f64 = std::f64::consts::FRAC_PI_4;
    const PIO4LO: f64 = 3.061616997868383e-17;
    const T: [f64; 13] = [
        0.3333333333333341,
        0.13333333333320124,
        0.05396825397622605,
        0.021869488294859542,
        0.0088632398235993,
        0.0035920791075913124,
        0.0014562094543252903,
        5.880412408202641e-4,
        2.464631348184699e-4,
        7.817944429395571e-5,
        7.140724913826082e-5,
        -1.8558637485527546e-5,
        2.590730518636337e-5,
    ];
    let hx = hi(x);
    let ix = hx & 0x7fff_ffff;
    if ix < 0x3e30_0000 && (x as i32) == 0 {
        // |x| < 2^-28
        if ((ix as u32 | lo(x)) | (iy + 1) as u32) == 0 {
            return 1.0 / x.abs();
        }
        if iy == 1 {
            return x;
        }
        let w = x + y;
        let z = with_lo(w, 0);
        let v = y - (z - x);
        let a = -1.0 / w;
        let t = with_lo(a, 0);
        let s = 1.0 + t * z;
        return t + a * (s + t * v);
    }
    if ix >= 0x3FE5_9428 {
        // |x| >= 0.6744
        if hx < 0 {
            x = -x;
            y = -y;
        }
        let z = PIO4 - x;
        let w = PIO4LO - y;
        x = z + w;
        y = 0.0;
    }
    let z = x * x;
    let w = z * z;
    let mut r = T[1] + w * (T[3] + w * (T[5] + w * (T[7] + w * (T[9] + w * T[11]))));
    let v = z * (T[2] + w * (T[4] + w * (T[6] + w * (T[8] + w * (T[10] + w * T[12])))));
    let s = z * x;
    r = y + z * (s * (r + v) + y);
    r += T[0] * s;
    let w = x + r;
    if ix >= 0x3FE5_9428 {
        let v = f64::from(iy);
        return f64::from(1 - ((hx >> 30) & 2)) * (v - 2.0 * (x - (w * w / (w + v) - r)));
    }
    if iy == 1 {
        return w;
    }
    let z = with_lo(w, 0);
    let v = r - (z - x);
    let a = -1.0 / w;
    let t = with_lo(a, 0);
    let s = 1.0 + t * z;
    t + a * (s + t * v)
}

/// `e_rem_pio2.c`: `x` reduced modulo `pi/2`, as `(n, y0, y1)` with
/// `x = n*pi/2 + (y0 + y1)` and `|y0 + y1| <= pi/4`.
fn rem_pio2(x: f64) -> (i32, f64, f64) {
    const NPIO2_HW: [i32; 32] = [
        0x3FF921FB, 0x400921FB, 0x4012D97C, 0x401921FB, 0x401F6A7A, 0x4022D97C, 0x4025FDBB,
        0x402921FB, 0x402C463A, 0x402F6A7A, 0x4031475C, 0x4032D97C, 0x40346B9C, 0x4035FDBB,
        0x40378FDB, 0x403921FB, 0x403AB41B, 0x403C463A, 0x403DD85A, 0x403F6A7A, 0x40407E4C,
        0x4041475C, 0x4042106C, 0x4042D97C, 0x4043A28C, 0x40446B9C, 0x404534AC, 0x4045FDBB,
        0x4046C6CB, 0x40478FDB, 0x404858EB, 0x404921FB,
    ];
    const INVPIO2: f64 = std::f64::consts::FRAC_2_PI;
    const PIO2_1: f64 = 1.5707963267341256;
    const PIO2_1T: f64 = 6.077100506506192e-11;
    const PIO2_2: f64 = 6.077100506303966e-11;
    const PIO2_2T: f64 = 2.0222662487959506e-21;
    const PIO2_3: f64 = 2.0222662487111665e-21;
    const PIO2_3T: f64 = 8.4784276603689e-32;
    let hx = hi(x);
    let ix = hx & 0x7fff_ffff;
    if ix <= 0x3fe9_21fb {
        return (0, x, 0.0);
    }
    if ix < 0x4002_d97c {
        // |x| < 3pi/4
        return if hx > 0 {
            let mut z = x - PIO2_1;
            let (y0, y1);
            if ix != 0x3ff9_21fb {
                y0 = z - PIO2_1T;
                y1 = (z - y0) - PIO2_1T;
            } else {
                z -= PIO2_2;
                y0 = z - PIO2_2T;
                y1 = (z - y0) - PIO2_2T;
            }
            (1, y0, y1)
        } else {
            let mut z = x + PIO2_1;
            let (y0, y1);
            if ix != 0x3ff9_21fb {
                y0 = z + PIO2_1T;
                y1 = (z - y0) + PIO2_1T;
            } else {
                z += PIO2_2;
                y0 = z + PIO2_2T;
                y1 = (z - y0) + PIO2_2T;
            }
            (-1, y0, y1)
        };
    }
    if ix <= 0x4139_21fb {
        // |x| <= 2^19 pi/2: the medium-size reduction
        let t = x.abs();
        let n = (t * INVPIO2 + 0.5) as i32;
        let f_n = f64::from(n);
        let mut r = t - f_n * PIO2_1;
        let mut w = f_n * PIO2_1T;
        let mut y0 = r - w;
        if !(n < 32 && ix != NPIO2_HW[(n - 1) as usize]) {
            let j = ix >> 20;
            let mut i = j - ((hi(y0) >> 20) & 0x7ff);
            if i > 16 {
                let t = r;
                w = f_n * PIO2_2;
                r = t - w;
                w = f_n * PIO2_2T - ((t - r) - w);
                y0 = r - w;
                i = j - ((hi(y0) >> 20) & 0x7ff);
                if i > 49 {
                    let t = r;
                    w = f_n * PIO2_3;
                    r = t - w;
                    w = f_n * PIO2_3T - ((t - r) - w);
                    y0 = r - w;
                }
            }
        }
        let y1 = (r - y0) - w;
        return if hx < 0 { (-n, -y0, -y1) } else { (n, y0, y1) };
    }
    if ix >= 0x7ff0_0000 {
        return (0, f64::NAN, f64::NAN);
    }
    // The large reduction: x split into three 24-bit pieces.
    let e0 = (ix >> 20) - 1046;
    let mut z = from_words(ix - (e0 << 20), lo(x));
    let mut tx = [0.0f64; 3];
    for t in tx.iter_mut().take(2) {
        *t = f64::from(z as i32);
        z = (z - *t) * TWO24;
    }
    tx[2] = z;
    let mut nx = 3;
    while tx[nx - 1] == 0.0 {
        nx -= 1;
    }
    let (n, y0, y1) = kernel_rem_pio2(&tx[..nx], e0);
    if hx < 0 {
        (-n, -y0, -y1)
    } else {
        (n, y0, y1)
    }
}

/// `k_rem_pio2.c` at double precision (`prec` 2): the reduction of a huge
/// argument given as 24-bit pieces `x` with exponent `e0`, against the bits of
/// `2/pi`. Answers `(n mod 8, y0, y1)`.
fn kernel_rem_pio2(x: &[f64], e0: i32) -> (i32, f64, f64) {
    const TWO_OVER_PI: [i32; 66] = [
        0xA2F983, 0x6E4E44, 0x1529FC, 0x2757D1, 0xF534DD, 0xC0DB62, 0x95993C, 0x439041, 0xFE5163,
        0xABDEBB, 0xC561B7, 0x246E3A, 0x424DD2, 0xE00649, 0x2EEA09, 0xD1921C, 0xFE1DEB, 0x1CB129,
        0xA73EE8, 0x8235F5, 0x2EBB44, 0x84E99C, 0x7026B4, 0x5F7E41, 0x3991D6, 0x398353, 0x39F49C,
        0x845F8B, 0xBDF928, 0x3B1FF8, 0x97FFDE, 0x05980F, 0xEF2F11, 0x8B5A0A, 0x6D1F6D, 0x367ECF,
        0x27CB09, 0xB74F46, 0x3F669E, 0x5FEA2D, 0x7527BA, 0xC7EBE5, 0xF17B3D, 0x0739F7, 0x8A5292,
        0xEA6BFB, 0x5FB11F, 0x8D5D08, 0x560330, 0x46FC7B, 0x6BABF0, 0xCFBC20, 0x9AF436, 0x1DA9E3,
        0x91615E, 0xE61B08, 0x659985, 0x5F14A0, 0x68408D, 0xFFD880, 0x4D7327, 0x310606, 0x1556CA,
        0x73A8C9, 0x60E27B, 0xC08C6B,
    ];
    const PIO2: [f64; 8] = [
        1.570796251296997,
        7.549789415861596e-8,
        5.390302529957765e-15,
        3.282003415807913e-22,
        1.270655753080676e-29,
        1.2293330898111133e-36,
        2.7337005381646456e-44,
        2.1674168387780482e-51,
    ];
    let twon24 = two_to(-24);
    let mut iq = [0i32; 20];
    let mut f = [0.0f64; 20];
    let mut fq = [0.0f64; 20];
    let mut q = [0.0f64; 20];
    let jk: i32 = 4; // init_jk[prec] for prec 2
    let jp = jk;
    let jx = x.len() as i32 - 1;
    let jv = ((e0 - 3) / 24).max(0);
    let mut q0 = e0 - 24 * (jv + 1);
    // f[0..jx+jk] = 2/pi's 24-bit chunks, the first jv - jx of them zero
    for (i, fi) in f.iter_mut().take((jx + jk + 1) as usize).enumerate() {
        let j = jv - jx + i as i32;
        *fi = if j < 0 {
            0.0
        } else {
            f64::from(TWO_OVER_PI[j as usize])
        };
    }
    let product = |f: &[f64; 20], i: i32| -> f64 {
        let mut fw = 0.0;
        for j in 0..=jx {
            fw += x[j as usize] * f[(jx + i - j) as usize];
        }
        fw
    };
    for i in 0..=jk {
        q[i as usize] = product(&f, i);
    }
    let mut jz = jk;
    let (mut n, mut z, mut ih);
    loop {
        // distill q[] into iq[] in reverse
        let mut i = 0usize;
        let mut j = jz;
        z = q[jz as usize];
        while j > 0 {
            let fw = f64::from((twon24 * z) as i32);
            iq[i] = (z - TWO24 * fw) as i32;
            z = q[(j - 1) as usize] + fw;
            i += 1;
            j -= 1;
        }
        z = scalb(z, q0);
        z -= 8.0 * (z * 0.125).floor();
        n = z as i32;
        z -= f64::from(n);
        ih = 0;
        let top = (jz - 1) as usize;
        if q0 > 0 {
            let i = iq[top] >> (24 - q0);
            n += i;
            iq[top] -= i << (24 - q0);
            ih = iq[top] >> (23 - q0);
        } else if q0 == 0 {
            ih = iq[top] >> 23;
        } else if z >= 0.5 {
            ih = 2;
        }
        if ih > 0 {
            n += 1;
            let mut carry = 0;
            for v in iq.iter_mut().take(jz as usize) {
                let j = *v;
                if carry == 0 {
                    if j != 0 {
                        carry = 1;
                        *v = 0x100_0000 - j;
                    }
                } else {
                    *v = 0xff_ffff - j;
                }
            }
            match q0 {
                1 => iq[top] &= 0x7f_ffff,
                2 => iq[top] &= 0x3f_ffff,
                _ => {}
            }
            if ih == 2 {
                z = 1.0 - z;
                if carry != 0 {
                    z -= scalb(1.0, q0);
                }
            }
        }
        if z != 0.0 {
            break;
        }
        // a zero result may need more bits of 2/pi
        let mut j = 0;
        let mut i = jz - 1;
        while i >= jk {
            j |= iq[i as usize];
            i -= 1;
        }
        if j != 0 {
            break;
        }
        let mut k = 1;
        while iq[(jk - k) as usize] == 0 {
            k += 1;
        }
        for i in (jz + 1)..=(jz + k) {
            f[(jx + i) as usize] = f64::from(TWO_OVER_PI[(jv + i) as usize]);
            q[i as usize] = product(&f, i);
        }
        jz += k;
    }
    // chop off zero terms
    if z == 0.0 {
        jz -= 1;
        q0 -= 24;
        while iq[jz as usize] == 0 {
            jz -= 1;
            q0 -= 24;
        }
    } else {
        z = scalb(z, -q0);
        if z >= TWO24 {
            let fw = f64::from((twon24 * z) as i32);
            iq[jz as usize] = (z - TWO24 * fw) as i32;
            jz += 1;
            q0 += 24;
            iq[jz as usize] = fw as i32;
        } else {
            iq[jz as usize] = z as i32;
        }
    }
    // convert integer bit chunks to floating point
    let mut fw = scalb(1.0, q0);
    let mut i = jz;
    while i >= 0 {
        q[i as usize] = fw * f64::from(iq[i as usize]);
        fw *= twon24;
        i -= 1;
    }
    // PIo2[0..jp] * q[jz..0]
    let mut i = jz;
    while i >= 0 {
        let mut fw = 0.0;
        let mut k = 0;
        while k <= jp && k <= jz - i {
            fw += PIO2[k as usize] * q[(i + k) as usize];
            k += 1;
        }
        fq[(jz - i) as usize] = fw;
        i -= 1;
    }
    // compress fq[] into y[]
    let mut fw = 0.0;
    let mut i = jz;
    while i >= 0 {
        fw += fq[i as usize];
        i -= 1;
    }
    let y0 = if ih == 0 { fw } else { -fw };
    let mut fw = fq[0] - fw;
    for v in fq.iter().take(jz as usize + 1).skip(1) {
        fw += *v;
    }
    let y1 = if ih == 0 { fw } else { -fw };
    (n & 7, y0, y1)
}

/// `e_pow.c`: `x` raised to `y`, as JDK 21's `StrictMath.pow` computes it.
pub fn pow(x: f64, y: f64) -> f64 {
    if y == 0.0 {
        return 1.0;
    }
    if x.is_nan() || y.is_nan() {
        return x + y;
    }
    let y_abs = y.abs();
    let mut x_abs = x.abs();
    if y == 2.0 {
        return x * x;
    } else if y == 0.5 {
        if x >= -f64::MAX {
            return (x + 0.0).sqrt();
        }
    } else if y_abs == 1.0 {
        return if y == 1.0 { x } else { 1.0 / x };
    } else if y_abs == f64::INFINITY {
        return if x_abs == 1.0 {
            f64::NAN // (±1)**±Inf
        } else if x_abs > 1.0 {
            if y >= 0.0 {
                y
            } else {
                0.0
            }
        } else if y < 0.0 {
            -y
        } else {
            0.0
        };
    }
    let hx = hi(x);
    let mut ix = hx & 0x7fff_ffff;
    // y_is_int: 0 = not an integer, 1 = odd, 2 = even (for a negative x)
    let mut y_is_int = 0;
    if hx < 0 {
        if y_abs >= two_to(53) {
            y_is_int = 2;
        } else if y_abs >= 1.0 {
            let y_long = y_abs as i64;
            if y_long as f64 == y_abs {
                y_is_int = 2 - (y_long & 1) as i32;
            }
        }
    }
    if x_abs == 0.0 || x_abs == f64::INFINITY || x_abs == 1.0 {
        let mut z = x_abs;
        if y < 0.0 {
            z = 1.0 / z;
        }
        if hx < 0 {
            if ((ix - 0x3ff0_0000) | y_is_int) == 0 {
                z = f64::NAN; // (-1)**non-int
            } else if y_is_int == 1 {
                z = -z;
            }
        }
        return z;
    }
    let mut n = (hx >> 31) + 1;
    if (n | y_is_int) == 0 {
        return f64::NAN; // (x < 0)**(non-int)
    }
    let s = if (n | (y_is_int - 1)) == 0 { -1.0 } else { 1.0 };
    let (t1, t2);
    if y_abs > 2.1474856959999995e9 {
        // |y| is huge
        const INV_LN2: f64 = std::f64::consts::LOG2_E;
        const INV_LN2_H: f64 = 1.4426950216293335;
        const INV_LN2_L: f64 = 1.9259629911266175e-8;
        if x_abs < 0.9999995231628418 {
            return if y < 0.0 { s * f64::INFINITY } else { s * 0.0 };
        }
        if x_abs > 1.0000009536743162 {
            return if y > 0.0 { s * f64::INFINITY } else { s * 0.0 };
        }
        let t = x_abs - 1.0;
        let w = (t * t) * (0.5 - t * (0.3333333333333333 - t * 0.25));
        let u = INV_LN2_H * t;
        let v = t * INV_LN2_L - w * INV_LN2;
        t1 = with_lo(u + v, 0);
        t2 = v - (t1 - u);
    } else {
        const CP: f64 = 0.9617966939259756;
        const CP_H: f64 = 0.9617967009544373;
        const CP_L: f64 = -7.028461650952758e-9;
        const BP: [f64; 2] = [1.0, 1.5];
        const DP_H: [f64; 2] = [0.0, 0.5849624872207642];
        const DP_L: [f64; 2] = [0.0, 1.350039202129749e-8];
        const L1: f64 = 0.5999999999999946;
        const L2: f64 = 0.4285714285785502;
        const L3: f64 = 0.33333332981837743;
        const L4: f64 = 0.272728123808534;
        const L5: f64 = 0.23066074577556175;
        const L6: f64 = 0.20697501780033842;
        n = 0;
        if ix < 0x0010_0000 {
            x_abs *= two_to(53);
            n -= 53;
            ix = hi(x_abs);
        }
        n += (ix >> 20) - 0x3ff;
        let j = ix & 0x000f_ffff;
        ix = j | 0x3ff0_0000;
        let k = if j <= 0x3988E {
            0
        } else if j < 0xBB67A {
            1
        } else {
            n += 1;
            ix -= 0x0010_0000;
            0
        };
        x_abs = with_hi(x_abs, ix);
        let u = x_abs - BP[k];
        let v = 1.0 / (x_abs + BP[k]);
        let ss = u * v;
        let s_h = with_lo(ss, 0);
        let t_h = with_hi(
            0.0,
            ((ix >> 1) | 0x2000_0000) + 0x0008_0000 + ((k as i32) << 18),
        );
        let t_l = x_abs - (t_h - BP[k]);
        let s_l = v * ((u - s_h * t_h) - s_h * t_l);
        let s2 = ss * ss;
        let mut r = s2 * s2 * (L1 + s2 * (L2 + s2 * (L3 + s2 * (L4 + s2 * (L5 + s2 * L6)))));
        r += s_l * (s_h + ss);
        let s2 = s_h * s_h;
        let t_h = with_lo(3.0 + s2 + r, 0);
        let t_l = r - ((t_h - 3.0) - s2);
        let u = s_h * t_h;
        let v = s_l * t_h + t_l * ss;
        let p_h = with_lo(u + v, 0);
        let p_l = v - (p_h - u);
        let z_h = CP_H * p_h;
        let z_l = CP_L * p_h + p_l * CP + DP_L[k];
        let t = f64::from(n);
        t1 = with_lo(((z_h + z_l) + DP_H[k]) + t, 0);
        t2 = z_l - (((t1 - t) - DP_H[k]) - z_h);
    }
    // (y1 + y2) * (t1 + t2)
    let y1 = with_lo(y, 0);
    let p_l = (y - y1) * t1 + y * t2;
    let mut p_h = y1 * t1;
    let z = p_l + p_h;
    let j = hi(z);
    let i = lo(z);
    if j >= 0x4090_0000 {
        // z >= 1024
        if ((j - 0x4090_0000) as u32 | i) != 0 {
            return s * f64::INFINITY;
        }
        const OVT: f64 = 8.008566259537294e-17;
        if p_l + OVT > z - p_h {
            return s * f64::INFINITY;
        }
    } else if (j & 0x7fff_ffff) >= 0x4090_cc00 {
        // z <= -1075
        if ((j as u32).wrapping_sub(0xc090_cc00) | i) != 0 {
            return s * 0.0;
        }
        if p_l <= z - p_h {
            return s * 0.0;
        }
    }
    const LN2_FULL: f64 = std::f64::consts::LN_2;
    const LN2_H: f64 = 0.6931471824645996;
    const LN2_L: f64 = -1.904654299957768e-9;
    let i = j & 0x7fff_ffff;
    let mut k = (i >> 20) - 0x3ff;
    let mut n = 0;
    if i > 0x3fe0_0000 {
        // |z| > 0.5: n = [z + 0.5]
        n = j + (0x0010_0000 >> (k + 1));
        k = ((n & 0x7fff_ffff) >> 20) - 0x3ff;
        let t = with_hi(0.0, n & !(0x000f_ffff >> k));
        n = ((n & 0x000f_ffff) | 0x0010_0000) >> (20 - k);
        if j < 0 {
            n = -n;
        }
        p_h -= t;
    }
    let t = with_lo(p_l + p_h, 0);
    let u = t * LN2_H;
    let v = (p_l - (t - p_h)) * LN2_FULL + t * LN2_L;
    let mut z = u + v;
    let w = v - (z - u);
    let t = z * z;
    let t1 = z - t * (P1 + t * (P2 + t * (P3 + t * (P4 + t * P5))));
    let r = (z * t1) / (t1 - 2.0) - (w + z * w);
    z = 1.0 - (r - z);
    let j = hi(z).wrapping_add(n << 20);
    if (j >> 20) <= 0 {
        z = scalb(z, n); // subnormal output
    } else {
        z = with_hi(z, hi(z).wrapping_add(n << 20));
    }
    s * z
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
    /// Each constant of the trigonometric, hyperbolic and power routines —
    /// the decimal spelling the code uses — against the IEEE word fdlibm
    /// spells it as.
    #[test]
    fn the_trig_constants_are_fdlibms_words() {
        let pinned: &[(f64, u64)] = &[
            (-1.904654299957768e-9, 0xBE20_5C61_0CA8_6C39),
            (-0.03333333333333313, 0xBFA1_1111_1111_10F4),
            (-0.036531572744216916, 0xBFA2_B444_2C6A_6C2F),
            (-2.403394911734414, 0xC003_3A27_1C8A_2D4B),
            (-1.8558637485527546e-5, 0xBEF3_75CB_DB60_5373),
            (-0.0769187620504483, 0xBFB3_B0F2_AF74_9A6D),
            (-0.04005553450067941, 0xBFA4_8228_B568_8F3B),
            (-7.93650757867488e-5, 0xBF14_CE19_9EAA_DBB7),
            (-0.3255658186224009, 0xBFD4_D612_03EB_6F7D),
            (-0.6882839716054533, 0xBFE6_066C_1B8D_0159),
            (-0.7053061224489796, 0xBFE6_91DE_2532_C834),
            (-0.19999999999876483, 0xBFC9_9999_9998_EBC4),
            (-2.0109921818362437e-7, 0xBE8A_FDB7_6E09_C32D),
            (-0.11111110405462356, 0xBFBC_71C6_FE23_1671),
            (-0.058335701337905735, 0xBFAD_DE2D_52DE_FD9A),
            (-7.028461650952758e-9, 0xBE3E_2FE0_145B_01F5),
            (1.0000009536743162, 0x3FF0_0000_FFFF_FFFF),
            (2.1474856959999995e9, 0x41E0_0000_FFFF_FFFF),
            (3.2733937296446915e150, 0x5F30_0000_FFFF_FFFF),
            (1.3903311031230998e-17, 0x3C70_0788_7AF0_CBBD),
            (1.270655753080676e-29, 0x39F0_1B83_8000_0000),
            (2.464631348184699e-4, 0x3F30_26F7_1A8D_1068),
            (2.0209457602335057, 0x4000_2AE5_9C59_8AC8),
            (0.016285820115365782, 0x3F90_AD3A_E322_DA11),
            (6.077100506506192e-11, 0x3DD0_B461_1A62_6331),
            (6.077100506303966e-11, 0x3DD0_B461_1A60_0000),
            (4.008217827329362e-6, 0x3ED0_CFCA_86E6_5239),
            (0.06661073137387531, 0x3FB1_0D66_A0D0_3D51),
            (0.13333333333320124, 0x3FC1_1111_1110_FE7A),
            (0.5428571428571428, 0x3FE1_5F15_F15F_15F1),
            (0.272728123808534, 0x3FD1_7460_A91D_4101),
            (1.2246467991473532e-16, 0x3CA1_A626_3314_5C07),
            (6.123233995736766e-17, 0x3C91_A626_3314_5C07),
            (3.061616997868383e-17, 0x3C81_A626_3314_5C07),
            (0.0088632398235993, 0x3F82_26E3_E96E_8493),
            (3.479331075960212e-5, 0x3F02_3DE1_0DFD_F709),
            (0.14285714272503466, 0x3FC2_4924_9200_83FF),
            (0.5849624872207642, 0x3FE2_B803_4000_0000),
            (7.140724913826082e-5, 0x3F12_B80F_32F0_A7E9),
            (2.0222662487959506e-21, 0x3BA3_198A_2E03_7073),
            (2.0222662487111665e-21, 0x3BA3_198A_2E00_0000),
            (0.5999999999999946, 0x3FE3_3333_3333_3303),
            (5.880412408202641e-4, 0x3F43_44D8_F2F2_6501),
            (2.7337005381646456e-44, 0x36E3_8222_8000_0000),
            (0.07703815055590194, 0x3FB3_B8C5_B12E_9282),
            (7.549789415861596e-8, 0x3E74_442D_0000_0000),
            (std::f64::consts::FRAC_2_PI, 0x3FE4_5F30_6DC9_C883),
            (7.817944429395571e-5, 0x3F14_7E88_A037_92A6),
            (1.9259629911266175e-8, 0x3E54_AE0B_F85D_DF44),
            (0.33333332981837743, 0x3FD5_5555_518F_264D),
            (0.3333333333333293, 0x3FD5_5555_5555_550D),
            (0.16666666666666666, 0x3FC5_5555_5555_5555),
            (0.3333333333333341, 0x3FD5_5555_5555_5563),
            (0.6931471803691238, 0x3FE6_2E42_FEE0_0000),
            (std::f64::consts::LN_2, 0x3FE6_2E42_FEFA_39EF),
            (0.6931471824645996, 0x3FE6_2E43_0000_0000),
            (0.021869488294859542, 0x3F96_64F4_8406_D637),
            (1.4142857142857144, 0x3FF6_A0EA_0EA0_EA0F),
            (0.35714285714285715, 0x3FD6_DB6D_B6DB_6DB7),
            (std::f64::consts::LOG2_E, 0x3FF7_1547_652B_82FE),
            (1.4426950216293335, 0x3FF7_1547_6000_0000),
            (0.09090887133436507, 0x3FB7_45CD_C54C_206E),
            (0.0014562094543252903, 0x3F57_DBC8_FEE0_8315),
            (5.390302529957765e-15, 0x3CF8_4698_8000_0000),
            (3.282003415807913e-22, 0x3B78_CC51_6000_0000),
            (1.570796251296997, 0x3FF9_21FB_4000_0000),
            (std::f64::consts::FRAC_PI_4, 0x3FE9_21FB_5444_2D18),
            (std::f64::consts::FRAC_PI_2, 0x3FF9_21FB_5444_2D18),
            (1.5707963267341256, 0x3FF9_21FB_5440_0000),
            (0.049768779946159324, 0x3FA9_7B4B_2476_0DEB),
            (1.6071428571428572, 0x3FF9_B6DB_6DB6_DB6E),
            (0.20121253213486293, 0x3FC9_C155_0E88_4455),
            (7.915349942898145e-4, 0x3F49_EFE0_7501_B288),
            (2.1674168387780482e-51, 0x3569_F31D_0000_0000),
            (0.0015873015872548146, 0x3F5A_01A0_19FE_5585),
            (1.2293330898111133e-36, 0x387A_2520_4000_0000),
            (2.2698777452961687e-17, 0x3C7A_2B7F_222F_65E2),
            (1.9082149292705877e-10, 0x3DEA_39EF_3579_3C76),
            (0.20697501780033842, 0x3FCA_7E28_4A45_4EEF),
            (2.590730518636337e-5, 0x3EFB_2A70_74BF_7AD4),
            (0.4285714285785502, 0x3FDB_6DB6_DB6F_ABFF),
            (8.4784276603689e-32, 0x397B_839A_2520_49C1),
            (0.05396825397622605, 0x3FAB_A1BA_1BB3_41FE),
            (1.350039202129749e-8, 0x3E4C_FDEB_43CF_D006),
            (0.0035920791075913124, 0x3F6D_6D22_C956_0328),
            (0.23066074577556175, 0x3FCD_864A_93C9_DB65),
            (0.4636476090008061, 0x3FDD_AC67_0561_BB4F),
            (0.9617966939259756, 0x3FEE_C709_DC3A_03FD),
            (0.9617967009544373, 0x3FEE_C709_E000_0000),
            (0.982793723247329, 0x3FEF_730B_D281_F69B),
            (0.9999995231628418, 0x3FEF_FFFF_0000_0000),
        ];
        for &(v, word) in pinned {
            assert_eq!(v.to_bits(), word, "{v:e}");
        }
    }

    /// Answers the reference JVM's `Math` gives (JDK 21), including the
    /// last-place ones a correctly rounded function would not, the large
    /// `tan` reduction, and `pow`'s special cases.
    #[test]
    fn trig_and_pow_answers_match_the_jvm() {
        assert_eq!(tan(std::f64::consts::FRAC_PI_4), 0.9999999999999999);
        assert_eq!(tan(1e300).to_bits(), 0x3FF6_BE41_1F37_AC77);
        assert_eq!(atan2(1.0, -2.0), 2.677945044588987);
        assert_eq!(sinh(1.0), 1.1752011936438014);
        assert_eq!(cbrt(-0.001), -0.1);
        assert_eq!(expm1(1e-10), 1.00000000005e-10);
        assert!(pow(1.0, f64::INFINITY).is_nan());
        assert_eq!(pow(2.0, -1074.0), f64::from_bits(1));
        assert_eq!(pow(-8.0, 3.0), -512.0);
        assert!(pow(-8.0, 1.0 / 3.0).is_nan());
        assert_eq!(hypot(f64::NAN, f64::INFINITY), f64::INFINITY);
    }
}
