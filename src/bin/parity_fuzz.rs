//! Differential parity fuzzer: the real Kotlin toolchain vs this frontend.
//!
//! Generates grammar-driven, deterministic-output Kotlin programs, runs each
//! through a real `kotlinc` + `kotlin` pair and through our `kotlin`, and reports
//! every case whose stdout OR success/failure diverges. Each program is produced
//! from a per-index seed so any divergence replays exactly:
//! `parity-fuzz --seed <N> --once`.
//!
//! The oracle is two steps — `kotlinc T.kt -d <out>` then `kotlin -classpath
//! <out> TKt` — because the `kotlin` launcher cannot run a `.kt` carrying
//! `fun main` directly. That compile dominates the runtime, so every program
//! packs many independent probe statements (`--probes`, default 40) into one
//! `main`; a single compile therefore exercises dozens of probes. On divergence,
//! [`minimize`] bisects the probe list down to the single offending probe.
//!
//! The generator is biased toward where a from-scratch Kotlin frontend goes
//! wrong: `Int`-vs-`Double` division dispatch (`7/2==3`, `7/2.0==3.5`), IEEE
//! division by zero, `Double.toString` notation (the decimal/scientific
//! threshold), string templates, and `String` member dispatch.
//!
//! Scope + determinism invariants (mirroring the javars/scalars harnesses):
//!   * Only constructs kotlinrs actually implements are emitted — an unsupported
//!     construct would be a known gap, not a parity signal. In particular
//!     `Math.*`/`kotlin.math.*`, ranges (`1..3`) and `arrayOf` are NOT generated.
//!   * No nondeterministic output (no `Random`, no time, no identity hashes, no
//!     unordered collections). Every probe's output is a pure function of source.
//!   * Integer operands stay well inside range so 32-bit overflow — a documented
//!     gap — is never the thing under test, and integer divisors are never zero
//!     (Kotlin throws there; that is a fault-path test, not a value test).
//!
//! Subprocess-only: this binary never links the kotlinrs library.
//!
//! Build:  cargo build --bin parity-fuzz
//! Run:    ./target/debug/parity-fuzz --iters 20
//!         ./target/debug/parity-fuzz --seed 12345 --once

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(if seed == 0 { 0x9E3779B97F4A7C15 } else { seed })
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn pick<'a, T>(rng: &mut Rng, xs: &'a [T]) -> &'a T {
    &xs[rng.below(xs.len())]
}

const INTS: &[&str] = &["0", "1", "2", "3", "7", "10", "42", "100", "-1", "-7", "-42"];
const DIVS: &[&str] = &["1", "2", "3", "4", "5", "7", "-2", "-3"];
const DBLS: &[&str] = &[
    "0.0", "1.0", "0.5", "2.5", "3.14", "-1.5", "100.0", "1e3", "1e-3", "0.1", "1234567.0",
    "1.0e7", "1.0e-7", "123456789.0", "9.999e-4",
];
const ZDIVS: &[&str] = &["0.0", "-0.0"];
const STRS: &[&str] = &["\"\"", "\"a\"", "\"abc\"", "\"Hello\"", "\" x \"", "\"AbC\""];
const BOOLS: &[&str] = &["true", "false"];
const AOPS: &[&str] = &["+", "-", "*"];
const CMPOPS: &[&str] = &["==", "!=", "<", ">", "<=", ">="];
const LOGOPS: &[&str] = &["&&", "||"];

fn p(body: String) -> String {
    format!("println({body})")
}

fn g_arith(r: &mut Rng) -> String {
    p(format!(
        "({} {} {}) {} {}",
        pick(r, INTS),
        pick(r, AOPS),
        pick(r, INTS),
        pick(r, AOPS),
        pick(r, INTS)
    ))
}

/// `Int` division/modulo truncate toward zero; divisors are never 0.
fn g_intdiv(r: &mut Rng) -> String {
    let op = if r.below(2) == 0 { "/" } else { "%" };
    p(format!("{} {op} {}", pick(r, INTS), pick(r, DIVS)))
}

fn g_doublearith(r: &mut Rng) -> String {
    p(format!(
        "{} {} {}",
        pick(r, DBLS),
        pick(r, &["+", "-", "*", "/"]),
        pick(r, DBLS)
    ))
}

/// Mixed operands — the promotion that decides `/` dispatch.
fn g_mixeddiv(r: &mut Rng) -> String {
    if r.below(2) == 0 {
        p(format!("{} / {}", pick(r, INTS), pick(r, DBLS)))
    } else {
        p(format!("{} / {}", pick(r, DBLS), pick(r, INTS)))
    }
}

/// IEEE division by zero: signed infinities and NaN, never a fault.
fn g_divzero(r: &mut Rng) -> String {
    p(format!("{} / {}", pick(r, DBLS), pick(r, ZDIVS)))
}

/// `Double.toString` notation — the decimal/scientific threshold.
fn g_doublefmt(r: &mut Rng) -> String {
    p((*pick(r, DBLS)).to_string())
}

fn g_concat(r: &mut Rng) -> String {
    let s = pick(r, STRS);
    match r.below(4) {
        0 => p(format!("{s} + {}", pick(r, INTS))),
        1 => p(format!("{s} + {}", pick(r, DBLS))),
        2 => p(format!("{s} + {}", pick(r, BOOLS))),
        _ => p(format!("{s} + {} + {}", pick(r, INTS), pick(r, STRS))),
    }
}

/// String templates — `$name` and `${expr}`. A declared name carries the probe
/// index so probes stay independent when packed into one `main`.
fn g_template(r: &mut Rng, idx: usize) -> String {
    let a = pick(r, INTS);
    let b = pick(r, INTS);
    match r.below(3) {
        0 => format!("val t{idx} = {a}; println(\"v=$t{idx}\")"),
        1 => p(format!("\"sum=${{{a} + {b}}}\"")),
        _ => format!("val t{idx} = {}; println(\"s=$t{idx}!\")", pick(r, STRS)),
    }
}

fn g_compare(r: &mut Rng) -> String {
    let op = pick(r, CMPOPS);
    if r.below(2) == 0 {
        p(format!("{} {op} {}", pick(r, INTS), pick(r, INTS)))
    } else {
        p(format!("{} {op} {}", pick(r, DBLS), pick(r, DBLS)))
    }
}

fn g_bool(r: &mut Rng) -> String {
    p(format!(
        "({} < {}) {} ({} > {})",
        pick(r, INTS),
        pick(r, INTS),
        pick(r, LOGOPS),
        pick(r, INTS),
        pick(r, INTS)
    ))
}

/// `if` as an expression.
fn g_ifexpr(r: &mut Rng) -> String {
    p(format!(
        "if ({} < {}) {} else {}",
        pick(r, INTS),
        pick(r, INTS),
        pick(r, INTS),
        pick(r, INTS)
    ))
}

/// `String` members kotlinrs dispatches (ASCII receivers only).
fn g_strmember(r: &mut Rng) -> String {
    let s = pick(r, STRS);
    match r.below(9) {
        0 => p(format!("{s}.length")),
        1 => p(format!("{s}.uppercase()")),
        2 => p(format!("{s}.lowercase()")),
        3 => p(format!("{s}.trim()")),
        4 => p(format!("{s}.isEmpty()")),
        5 => p(format!("{s}.contains(\"b\")")),
        6 => p(format!("{s}.startsWith(\"a\")")),
        7 => p(format!("{s}.indexOf(\"b\")")),
        _ => p(format!("{s}.replace(\"b\", \"-\")")),
    }
}

/// A counted loop with an accumulator, exercising the loop lowering.
///
fn g_loop(r: &mut Rng, idx: usize) -> String {
    let n = 2 + r.below(5);
    let step = pick(r, &["1", "2", "3"]);
    format!(
        "var a{idx} = 0; var i{idx} = 0; while (i{idx} < {n}) {{ a{idx} += i{idx} * {step}; i{idx}++ }}; println(a{idx})"
    )
}

/// `listOf` and a member read; list printing is order-deterministic.
fn g_list(r: &mut Rng) -> String {
    let a = pick(r, INTS);
    let b = pick(r, INTS);
    if r.below(2) == 0 {
        p(format!("listOf({a}, {b})"))
    } else {
        p(format!("listOf({a}, {b}).size"))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    All,
    Arith,
    IntDiv,
    DoubleArith,
    MixedDiv,
    DivZero,
    DoubleFmt,
    Concat,
    Template,
    Compare,
    Bool,
    IfExpr,
    StrMember,
    Loop,
    List,
}

const CONCRETE: &[Mode] = &[
    Mode::Arith,
    Mode::IntDiv,
    Mode::DoubleArith,
    Mode::MixedDiv,
    Mode::DivZero,
    Mode::DoubleFmt,
    Mode::Concat,
    Mode::Template,
    Mode::Compare,
    Mode::Bool,
    Mode::IfExpr,
    Mode::StrMember,
    Mode::Loop,
    Mode::List,
];

fn mode_name(m: Mode) -> &'static str {
    match m {
        Mode::All => "all",
        Mode::Arith => "arith",
        Mode::IntDiv => "intdiv",
        Mode::DoubleArith => "doublearith",
        Mode::MixedDiv => "mixeddiv",
        Mode::DivZero => "divzero",
        Mode::DoubleFmt => "doublefmt",
        Mode::Concat => "concat",
        Mode::Template => "template",
        Mode::Compare => "compare",
        Mode::Bool => "bool",
        Mode::IfExpr => "ifexpr",
        Mode::StrMember => "strmember",
        Mode::Loop => "loop",
        Mode::List => "list",
    }
}

fn parse_mode(s: &str) -> Option<Mode> {
    if s == "all" {
        return Some(Mode::All);
    }
    CONCRETE.iter().copied().find(|m| mode_name(*m) == s)
}

fn gen_probe(r: &mut Rng, mode: Mode, idx: usize) -> String {
    let m = if mode == Mode::All {
        *pick(r, CONCRETE)
    } else {
        mode
    };
    match m {
        Mode::Arith => g_arith(r),
        Mode::IntDiv => g_intdiv(r),
        Mode::DoubleArith => g_doublearith(r),
        Mode::MixedDiv => g_mixeddiv(r),
        Mode::DivZero => g_divzero(r),
        Mode::DoubleFmt => g_doublefmt(r),
        Mode::Concat => g_concat(r),
        Mode::Template => g_template(r, idx),
        Mode::Compare => g_compare(r),
        Mode::Bool => g_bool(r),
        Mode::IfExpr => g_ifexpr(r),
        Mode::StrMember => g_strmember(r),
        Mode::Loop => g_loop(r, idx),
        Mode::List => g_list(r),
        Mode::All => unreachable!("resolved above"),
    }
}

fn gen_probes(seed: u64, mode: Mode, n: usize) -> Vec<String> {
    let mut r = Rng::new(seed);
    (0..n).map(|i| gen_probe(&mut r, mode, i)).collect()
}

/// Probes are emitted straight into `main`. Any probe that declares a name
/// suffixes it with the probe index, so packing many into one `main` keeps them
/// independent for [`minimize`] without a block wrapper — kotlinrs has no
/// `run { }` scope function, so wrapping would test the harness, not the
/// frontend.
fn build_program(probes: &[String]) -> String {
    let mut s = String::from("fun main() {\n");
    for probe in probes {
        s.push_str("    ");
        s.push_str(probe);
        s.push('\n');
    }
    s.push_str("}\n");
    s
}

struct RunOut {
    stdout: Vec<u8>,
    ok: bool,
}

static TMP_CTR: AtomicU64 = AtomicU64::new(0);

fn workdir() -> PathBuf {
    let n = TMP_CTR.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("kotlinrs_parity_{}_{n}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    d
}

fn capture(cmd: &mut Command, timeout: Duration) -> Option<(Vec<u8>, bool)> {
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => break None,
        }
    };
    let out = child.wait_with_output().ok()?;
    Some((out.stdout, status.map(|s| s.success()).unwrap_or(false)))
}

/// Run through our frontend: one command, the `.kt` straight in.
fn run_ours(ours: &Path, src: &str, timeout: Duration) -> RunOut {
    let dir = workdir();
    let path = dir.join("T.kt");
    let _ = std::fs::write(&path, src);
    let mut cmd = Command::new(ours);
    cmd.arg(&path);
    let r = capture(&mut cmd, timeout);
    let _ = std::fs::remove_dir_all(&dir);
    match r {
        Some((stdout, ok)) => RunOut { stdout, ok },
        None => RunOut {
            stdout: Vec::new(),
            ok: false,
        },
    }
}

/// Run through the reference toolchain: compile with `kotlinc`, then run the
/// generated `TKt` class. A compile failure reports `ok == false` with no
/// stdout — exactly how a parse error surfaces on our side too.
fn run_oracle(kotlinc: &Path, kotlin: &Path, src: &str, timeout: Duration) -> RunOut {
    let dir = workdir();
    let path = dir.join("T.kt");
    let out = dir.join("out");
    let _ = std::fs::write(&path, src);

    let mut c = Command::new(kotlinc);
    c.arg(&path).arg("-d").arg(&out).current_dir(&dir);
    let compiled = matches!(capture(&mut c, timeout), Some((_, true)));
    if !compiled {
        let _ = std::fs::remove_dir_all(&dir);
        return RunOut {
            stdout: Vec::new(),
            ok: false,
        };
    }

    let mut r = Command::new(kotlin);
    r.arg("-classpath").arg(&out).arg("TKt").current_dir(&dir);
    let res = capture(&mut r, timeout);
    let _ = std::fs::remove_dir_all(&dir);
    match res {
        Some((stdout, ok)) => RunOut { stdout, ok },
        None => RunOut {
            stdout: Vec::new(),
            ok: false,
        },
    }
}

struct Tools {
    ours: PathBuf,
    kotlinc: PathBuf,
    kotlin: PathBuf,
}

fn differs(oracle: &RunOut, ours: &RunOut) -> bool {
    oracle.stdout != ours.stdout || oracle.ok != ours.ok
}

fn render(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace('\n', "\\n")
}

fn diverges(probes: &[String], t: &Tools, timeout: Duration) -> bool {
    let src = build_program(probes);
    let a = run_oracle(&t.kotlinc, &t.kotlin, &src, timeout);
    let b = run_ours(&t.ours, &src, timeout);
    differs(&a, &b)
}

fn minimize(probes: &[String], t: &Tools, timeout: Duration) -> Vec<String> {
    let mut cur = probes.to_vec();
    let mut chunk = cur.len() / 2;
    while chunk >= 1 {
        let mut i = 0;
        while i < cur.len() {
            let mut trial = cur.clone();
            let end = (i + chunk).min(trial.len());
            trial.drain(i..end);
            if !trial.is_empty() && diverges(&trial, t, timeout) {
                cur = trial;
            } else {
                i += chunk;
            }
        }
        if chunk == 1 {
            break;
        }
        chunk /= 2;
    }
    cur
}

fn ours_bin() -> PathBuf {
    if let Ok(p) = std::env::var("KOTLINRS_BIN") {
        return PathBuf::from(p);
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for profile in ["debug", "release"] {
        let c = root.join("target").join(profile).join("kotlin");
        if c.exists() {
            return c;
        }
    }
    root.join("target/debug/kotlin")
}

/// Find a reference tool on PATH, skipping our own binary of the same name.
fn on_path(name: &str, skip: &Path) -> Option<PathBuf> {
    let skip = skip.canonicalize().ok();
    let path = std::env::var("PATH").ok()?;
    for dir in path.split(':') {
        let cand = Path::new(dir).join(name);
        if cand.exists() && cand.canonicalize().ok() != skip {
            return Some(cand);
        }
    }
    None
}

struct Args {
    iters: usize,
    probes: usize,
    seed: Option<u64>,
    once: bool,
    mode: Mode,
    timeout: Duration,
    verbose: bool,
}

fn parse_args() -> Args {
    let mut a = Args {
        iters: 20,
        probes: 40,
        seed: None,
        once: false,
        mode: Mode::All,
        timeout: Duration::from_secs(300),
        verbose: false,
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].clone();
        let mut take = |i: &mut usize| -> String {
            *i += 1;
            argv.get(*i).cloned().unwrap_or_default()
        };
        match arg.as_str() {
            "--iters" => a.iters = take(&mut i).parse().unwrap_or(a.iters),
            "--probes" => a.probes = take(&mut i).parse().unwrap_or(a.probes),
            "--seed" => a.seed = take(&mut i).parse().ok(),
            "--once" => a.once = true,
            "--verbose" | "-v" => a.verbose = true,
            "--timeout" => a.timeout = Duration::from_secs(take(&mut i).parse().unwrap_or(300)),
            "--mode" => {
                let m = take(&mut i);
                match parse_mode(&m) {
                    Some(m) => a.mode = m,
                    None => {
                        eprintln!("parity-fuzz: unknown mode `{m}`");
                        std::process::exit(2);
                    }
                }
            }
            "--help" | "-h" => {
                println!("parity-fuzz [--iters N] [--probes N] [--seed N] [--once] [--mode M] [--timeout SECS] [-v]");
                println!(
                    "modes: all {}",
                    CONCRETE
                        .iter()
                        .map(|m| mode_name(*m))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
                std::process::exit(0);
            }
            other => {
                eprintln!("parity-fuzz: unknown option `{other}`");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    a
}

fn main() {
    let args = parse_args();
    let ours = ours_bin();
    if !ours.exists() {
        eprintln!("parity-fuzz: {} not built (cargo build)", ours.display());
        std::process::exit(2);
    }
    let kotlinc = match std::env::var("KOTLINC")
        .map(PathBuf::from)
        .ok()
        .or_else(|| on_path("kotlinc", &ours))
    {
        Some(p) => p,
        None => {
            eprintln!("parity-fuzz: no `kotlinc` on PATH (set KOTLINC=/path/to/kotlinc)");
            std::process::exit(2);
        }
    };
    let kotlin = match std::env::var("KOTLIN_ORACLE")
        .map(PathBuf::from)
        .ok()
        .or_else(|| on_path("kotlin", &ours))
    {
        Some(p) => p,
        None => {
            eprintln!("parity-fuzz: no reference `kotlin` on PATH (set KOTLIN_ORACLE=...)");
            std::process::exit(2);
        }
    };

    let t = Tools {
        ours,
        kotlinc,
        kotlin,
    };
    eprintln!(
        "parity-fuzz: oracle={} + {} ours={} mode={} probes={}",
        t.kotlinc.display(),
        t.kotlin.display(),
        t.ours.display(),
        mode_name(args.mode),
        args.probes
    );

    let iters = if args.once { 1 } else { args.iters };
    let base = args.seed.unwrap_or(0x5EED);
    let mut failures = 0usize;
    let mut probes_run = 0usize;

    for k in 0..iters {
        let seed = if args.once {
            base
        } else {
            base.wrapping_add(k as u64)
        };
        let probes = gen_probes(seed, args.mode, args.probes);
        probes_run += probes.len();
        if !diverges(&probes, &t, args.timeout) {
            if args.verbose {
                eprintln!("seed {seed}: ok ({} probes)", probes.len());
            }
            continue;
        }
        failures += 1;
        let minimal = minimize(&probes, &t, args.timeout);
        let src = build_program(&minimal);
        let a = run_oracle(&t.kotlinc, &t.kotlin, &src, args.timeout);
        let b = run_ours(&t.ours, &src, args.timeout);
        println!("=== DIVERGENCE seed {seed} (replay: --seed {seed} --once) ===");
        for probe in &minimal {
            println!("  {probe}");
        }
        println!("  oracle: ok={} out={}", a.ok, render(&a.stdout));
        println!("  ours  : ok={} out={}", b.ok, render(&b.stdout));
    }

    eprintln!("parity-fuzz: {iters} program(s), {probes_run} probe(s), {failures} divergence(s)");
    if failures > 0 {
        std::process::exit(1);
    }
}
