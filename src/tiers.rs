//! Which fusevm execution tier a program's bytecode actually reaches.
//!
//! Enabling the JIT is not the same as being compiled by it, and the only
//! honest way to tell the two apart is to ask the VM. This module runs a
//! program and then queries fusevm's own eligibility and cache predicates —
//! `is_block_eligible`, `block_jit_is_compiled`, `trace_is_compiled`,
//! `find_jit_region` — so the answer comes from the compiler that would have
//! done the work rather than from an assumption about it.
//!
//! `kotlin --tiers Main.kt` prints the report.

use std::collections::BTreeMap;

use fusevm::{Chunk, ChunkBuilder, JitCompiler, Op};

/// A loop header — the target of a backward branch — and what became of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loop {
    /// Op index of the loop header the backward branch jumps to.
    pub anchor: usize,
    /// Whether fusevm would accept this loop's body as a trace. Asked of
    /// `is_trace_eligible` with the body's ops — the same predicate the
    /// recorder applies to what it recorded, which for a loop whose body has
    /// no early exit is the same op sequence.
    pub trace_eligible: bool,
    /// Whether a compiled trace is installed for this header after the run.
    pub traced: bool,
    /// Whether the tracing JIT gave up on this header.
    pub blacklisted: bool,
}

/// What the tiers did with one chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkTiers {
    /// Which chunk this is — `main` for a whole Kotlin program.
    pub name: String,
    /// Ops in the compiled chunk.
    pub ops: usize,
    /// Whether every op in the chunk is block-JIT eligible, which is what the
    /// whole-chunk block tier requires.
    pub block_eligible: bool,
    /// Whether the block tier holds compiled native code for this chunk.
    pub block_compiled: bool,
    /// The largest contiguous block-eligible op range, if any is large enough
    /// for fusevm to consider it worth compiling.
    pub largest_eligible_region: Option<(usize, usize)>,
    /// Every loop header, and whether the tracing JIT compiled it.
    pub loops: Vec<Loop>,
    /// Op kinds the **block** tier refuses, by occurrence count — what keeps
    /// the whole chunk from being compiled in one piece.
    ///
    /// Not the same question as whether a loop is traced: the tracing tier
    /// takes `GetVar` / `SetVar` (fusevm promotes a referenced global to a
    /// register at trace entry and spills it at every exit), so a chunk can
    /// list those here and still reach native code through a trace.
    pub ineligible: BTreeMap<String, usize>,
}

impl ChunkTiers {
    /// Whether any tier holds compiled native code for this chunk.
    pub fn reaches_native(&self) -> bool {
        self.block_compiled || self.loops.iter().any(|l| l.traced)
    }
}

/// What the tiers did with one program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Every chunk the program compiled to, in the order they were lowered.
    pub chunks: Vec<ChunkTiers>,
}

impl Report {
    /// Whether any tier holds compiled native code for any of the program's
    /// chunks.
    pub fn reaches_native(&self) -> bool {
        self.chunks.iter().any(|c| c.reaches_native())
    }
}

impl std::fmt::Display for ChunkTiers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "ops                     {}", self.ops)?;
        writeln!(f, "block-JIT eligible      {}", self.block_eligible)?;
        writeln!(f, "block-JIT compiled      {}", self.block_compiled)?;
        match self.largest_eligible_region {
            Some((s, e)) => writeln!(f, "largest eligible region {s}..{e} ({} ops)", e - s)?,
            None => writeln!(f, "largest eligible region none")?,
        }
        if self.loops.is_empty() {
            writeln!(f, "loops                   none")?;
        }
        for l in &self.loops {
            writeln!(
                f,
                "loop @{:<4}             trace-eligible={} traced={} blacklisted={}",
                l.anchor, l.trace_eligible, l.traced, l.blacklisted
            )?;
        }
        if self.ineligible.is_empty() {
            writeln!(f, "block-ineligible ops    none")?;
        } else {
            writeln!(f, "block-ineligible ops")?;
            for (name, count) in &self.ineligible {
                writeln!(f, "  {name:<22}{count}")?;
            }
        }
        Ok(())
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A single-chunk program needs no section headers; the fleet's
        // multi-chunk frontends label each one.
        let label = self.chunks.len() > 1;
        for c in &self.chunks {
            if label {
                writeln!(f, "== {} ==", c.name)?;
            }
            write!(f, "{c}")?;
        }
        write!(f, "reaches native code     {}", self.reaches_native())
    }
}

/// Compile and run `src`, then report which tiers took it.
///
/// The program is run because tier membership is a runtime fact: the block tier
/// compiles after its warmup threshold and the tracing tier only after a loop
/// has gone round enough times to be recorded. kotlinrs writes program output
/// straight to the process stdout, so the program's own output precedes the
/// report — what is measured is what an ordinary run does.
///
/// THE RUN AND THE INSPECTION HAPPEN ON THE SAME THREAD, and that is load-
/// bearing rather than tidy: fusevm keeps its compiled-block and compiled-
/// trace caches in `thread_local!` tables (`BLOCK_CACHE_TLS`, `TRACE_CACHE_TLS`
/// in fusevm `src/jit.rs`), and kotlinrs runs every program on a freshly
/// spawned interpreter thread (see [`crate::runtime::on_interpreter_thread`]).
/// Reporting from the SPAWNING thread therefore read tables the run never
/// touched, so `traced` and `block-JIT compiled` could only ever come back
/// `false` — not because the tiers declined the program but because the
/// question went to the wrong thread. A report that can only answer one way is
/// the one failure a tier report must not have, whatever the true answer is.
pub fn report(src: &str) -> Result<Report, String> {
    let owned = src.to_string();
    let measure = |src: &str| {
        // The chunk lowered here is the same bytecode the run below executes,
        // and fusevm keys its compiled code by the chunk's op hash, so asking
        // this copy is asking about the run that just happened.
        let chunk = crate::runtime::compile(src)?;
        crate::runtime::run_source_on_this_thread(src)?;
        Ok(inspect(&chunk))
    };
    crate::runtime::on_interpreter_thread(move || measure(&owned), || measure(src))
}

/// Report on an already-executed program chunk.
pub fn inspect(chunk: &Chunk) -> Report {
    Report {
        chunks: vec![inspect_chunk("main", chunk)],
    }
}

/// Report on one already-executed chunk.
pub fn inspect_chunk(name: &str, chunk: &Chunk) -> ChunkTiers {
    let jit = JitCompiler::new();
    let loops = loop_anchors(&chunk.ops)
        .into_iter()
        .map(|anchor| Loop {
            anchor,
            trace_eligible: body_of(&chunk.ops, anchor)
                .is_some_and(|body| jit.is_trace_eligible(body, anchor)),
            traced: jit.trace_is_compiled(chunk, anchor),
            blacklisted: jit.trace_is_blacklisted(chunk, anchor),
        })
        .collect();

    let mut ineligible: BTreeMap<String, usize> = BTreeMap::new();
    for op in &chunk.ops {
        if !op_is_eligible(&jit, op) {
            *ineligible.entry(op_name(op)).or_default() += 1;
        }
    }

    ChunkTiers {
        name: name.to_string(),
        ops: chunk.ops.len(),
        block_eligible: jit.is_block_eligible(chunk),
        block_compiled: jit.block_jit_is_compiled(chunk),
        largest_eligible_region: jit.find_jit_region(chunk),
        loops,
        ineligible,
    }
}

/// Every op index a backward branch jumps to — fusevm anchors a trace at each.
fn loop_anchors(ops: &[Op]) -> Vec<usize> {
    let mut anchors: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter_map(|(ip, op)| match op {
            Op::Jump(t)
            | Op::JumpIfTrue(t)
            | Op::JumpIfFalse(t)
            | Op::JumpIfTrueKeep(t)
            | Op::JumpIfFalseKeep(t)
                if *t <= ip =>
            {
                Some(*t)
            }
            _ => None,
        })
        .collect();
    anchors.sort_unstable();
    anchors.dedup();
    anchors
}

/// The op sequence one iteration of the loop at `anchor` runs: from the header
/// through the backward branch that closes it. `None` when nothing closes it.
fn body_of(ops: &[Op], anchor: usize) -> Option<&[Op]> {
    let close = ops.iter().enumerate().position(|(ip, op)| {
        ip >= anchor
            && matches!(
                op,
                Op::Jump(t) | Op::JumpIfTrue(t) | Op::JumpIfFalse(t)
                    if *t == anchor
            )
    })?;
    Some(&ops[anchor..=close])
}

/// Whether fusevm's block tier accepts this op, asked by handing the JIT a
/// chunk holding just that op. Whole-chunk eligibility is the conjunction of
/// the per-op decision, so a one-op chunk isolates it.
fn op_is_eligible(jit: &JitCompiler, op: &Op) -> bool {
    let mut b = ChunkBuilder::new();
    b.emit(op.clone(), 1);
    jit.is_block_eligible(&b.build())
}

/// An op's variant name, without its operands, so occurrences group.
fn op_name(op: &Op) -> String {
    let text = format!("{op:?}");
    match text.split_once('(') {
        Some((name, _)) => name.to_string(),
        None => text,
    }
}

/// The counted loop this module's tests measure, in the frontend's own syntax.
#[cfg(test)]
const PROGRAM: &str = "fun f(n: Int): Int {\n var t = 0\n var i = 0\n while (i < n) { t += i; i += 1 }\n return t\n}\nfun main() { f(200000) }";

#[cfg(test)]
mod tests {
    use super::*;

    /// The report can say yes. A counted loop built by hand in the rotated
    /// shape — entered at its body, closed by a conditional backward branch —
    /// is the one shape fusevm's trace compiler accepts (`jit.rs` compiles a
    /// `JumpIfTrue`/`JumpIfFalse` close and bails on an unconditional `Jump`),
    /// and after a run the report finds the installed trace. Without this,
    /// every "not traced" below could be a report that only ever says no.
    #[test]
    fn a_rotated_slot_loop_reaches_a_compiled_trace() {
        let mut b = ChunkBuilder::new();
        b.emit(Op::LoadInt(0), 1);
        b.emit(Op::SetSlot(0), 1);
        let enter = b.emit(Op::Jump(usize::MAX), 1);
        let body = b.current_pos();
        b.emit(Op::GetSlot(0), 1);
        b.emit(Op::LoadInt(1), 1);
        b.emit(Op::Add, 1);
        b.emit(Op::SetSlot(0), 1);
        let cond = b.current_pos();
        b.patch_jump(enter, cond);
        b.emit(Op::GetSlot(0), 1);
        b.emit(Op::LoadInt(200_000), 1);
        b.emit(Op::NumLt, 1);
        b.emit(Op::JumpIfTrue(body), 1);
        b.emit(Op::GetSlot(0), 1);
        let chunk = b.build();

        let mut vm = fusevm::VM::new(chunk.clone());
        vm.enable_tracing_jit();
        vm.run();

        let report = inspect(&chunk);
        assert_eq!(report.chunks[0].loops.len(), 1, "{report}");
        assert!(report.chunks[0].loops[0].traced, "{report}");
        assert!(report.reaches_native(), "{report}");
    }

    /// A counted loop written in the frontend's own syntax reaches a compiled
    /// trace. This used to assert the opposite, and said so — the loop was
    /// lowered with a forward `JumpIfFalse` exit closed by an unconditional
    /// backward `Jump`, which the trace compiler records and then declines, so
    /// the hottest shape a Kotlin program has stayed in the interpreter however
    /// hot it got. `Compiler::compile_stmt_inner` now emits every loop rotated:
    /// the test duplicated as an entry guard and a conditional backward branch,
    /// which is the shape [`a_rotated_slot_loop_reaches_a_compiled_trace`]
    /// proves fusevm accepts.
    ///
    /// It is ALSO the pin on where the report is taken from. fusevm's trace
    /// cache is a `thread_local!` and kotlinrs runs every program on a spawned
    /// interpreter thread, so [`report`] reading that cache from the spawning
    /// thread found it empty and answered `traced=false` for every program on
    /// earth. Move the inspection back off the interpreter thread and this
    /// fails.
    #[test]
    fn a_counted_loop_reaches_a_compiled_trace() {
        let report = report(PROGRAM).expect("runs");
        let counted = report.chunks[0]
            .loops
            .iter()
            .find(|l| l.trace_eligible)
            .unwrap_or_else(|| panic!("a trace-eligible loop: {report}"));
        assert!(counted.traced, "{report}");
        assert!(!counted.blacklisted, "{report}");
        assert!(report.reaches_native(), "{report}");
    }

    /// A loop body holding an `if` STATEMENT still reaches native code.
    ///
    /// Statement position used to compile the `if` for its value and pop it,
    /// which puts a `LoadUndef` on the else path — an op fusevm's JIT refuses —
    /// so one `if` anywhere in a loop body reported `trace-eligible=false` and
    /// took the whole loop out of the tracing tier. Since that is what most
    /// loops worth compiling contain, the rotation above would have bought
    /// almost nothing on its own.
    #[test]
    fn a_loop_whose_body_branches_reaches_a_compiled_trace() {
        let report = report(
            "fun f(n: Int): Int {\n var t = 0\n var i = 0\n \
             while (i < n) { t = t + 3; if (t > 1000) { t = 0 }; i = i + 1 }\n \
             return t\n}\nfun main() { f(200000) }",
        )
        .expect("runs");
        let counted = report.chunks[0]
            .loops
            .iter()
            .find(|l| l.trace_eligible)
            .unwrap_or_else(|| panic!("a trace-eligible loop: {report}"));
        assert!(counted.traced, "{report}");
        assert!(report.reaches_native(), "{report}");
    }

    /// The same for a `when` statement, whose non-exhaustive form emitted the
    /// same refused `LoadUndef` as the `if` above.
    #[test]
    fn a_loop_whose_body_holds_a_when_statement_reaches_a_compiled_trace() {
        let report = report(
            "fun f(n: Int): Int {\n var t = 0\n var i = 0\n \
             while (i < n) { when (t) { 1000 -> { t = 0 } }; t = t + 3; i = i + 1 }\n \
             return t\n}\nfun main() { f(200000) }",
        )
        .expect("runs");
        let counted = report.chunks[0]
            .loops
            .iter()
            .find(|l| l.trace_eligible)
            .unwrap_or_else(|| panic!("a trace-eligible loop: {report}"));
        assert!(counted.traced, "{report}");
        assert!(report.reaches_native(), "{report}");
    }
}
