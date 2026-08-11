//! Dhrystone 2.1 — integer ALU, branches, procedure calls, and string work.
//!
//! A faithful Rust port of Reinhold Weicker's C version (May 1988), measuring
//! the same statement mix: integer arithmetic on small values, pointer and
//! record manipulation, `strcpy`/`strcmp` over 30-character buffers, and a lot
//! of procedure calls with by-reference parameters.
//!
//! # Why port it instead of linking the C
//!
//! The original C sources have three properties that make them unusable inside
//! a threaded harness:
//!
//! * **Global state.** Every record, array, and flag is a file-scope global, so
//!   two threads running Dhrystone corrupt each other. The previous version of
//!   this suite worked around that with a process-wide mutex, which meant
//!   `--threads 14` serialised and measured nothing parallel.
//! * **A built-in `main` and timing loop.** Bolting on an external harness
//!   meant `#define main dhry_unused_main` and calling the whole of `main` as
//!   though it were one iteration — so every "iteration" re-ran the benchmark's
//!   own initialisation, `malloc`s, and `times()` calls.
//! * **K&R declarations.** Compiling it needs `-std=gnu89` and eight warning
//!   suppressions, and it still emits dozens of diagnostics.
//!
//! Porting fixes all three: state lives in a struct, so threads are naturally
//! independent; the measured loop is exactly the measured loop; and there is no
//! C toolchain in the build at all.
//!
//! # Faithfulness
//!
//! The port is verified against the reference implementation's documented final
//! values — `Int_Glob`, `Arr_2_Glob[8][7]`, both records' fields, and every
//! local the original prints — in `final_state_matches_the_reference`.
//! Those constants come from the published expected output, not from this
//! implementation, so the test is a genuine check rather than a snapshot of
//! whatever the code happens to do.
//!
//! Two deliberate departures, both documented at their use sites: the two
//! records live in a two-element array addressed by index rather than by raw
//! pointer, and the `variant` union is flattened to a struct. Neither changes
//! the statement mix; the second moves a slightly larger record on each
//! structure assignment.
//!
//! # Ground rules
//!
//! Dhrystone's rules require that procedures not be merged into their callers,
//! since the call overhead is part of what the benchmark measures. Every `Proc_`
//! and `Func_` here is `#[inline(never)]` to enforce that — which also stops
//! LLVM from collapsing the whole loop after proving that iterations past the
//! first reach a fixed point.
//!
//! # Interpreting the number
//!
//! One "Dhrystone" is one pass of the main loop. The conventional DMIPS figure
//! divides Dhrystones per second by 1757, the VAX 11/780's rate. Dhrystone is a
//! tiny working set that lives entirely in L1 and says nothing about memory,
//! floating point, or vector units — which is precisely why this suite reports
//! five other workloads next to it.

use threadstone_core::kernel::{
    Footprint, Kernel, KernelInfo, KernelState, Scaling, SetupCtx, Unit,
};

/// Dhrystones per second on a VAX 11/780, the historical DMIPS divisor.
pub const VAX_DHRYSTONES_PER_SEC: f64 = 1757.0;

/// Fixed-size string type from the original (`typedef char Str_30[31]`).
type Str30 = [u8; 31];

const STR_1: &[u8] = b"DHRYSTONE PROGRAM, 1'ST STRING";
const STR_2: &[u8] = b"DHRYSTONE PROGRAM, 2'ND STRING";
const STR_3: &[u8] = b"DHRYSTONE PROGRAM, 3'RD STRING";
const STR_SOME: &[u8] = b"DHRYSTONE PROGRAM, SOME STRING";

/// The original's `Enumeration` type.
///
/// `Ident5` is never constructed by the benchmark, but it exists in the C
/// original's type and `Proc_6` has a case for it. Dropping it would change the
/// shape of that `match` and make the port less faithful for no gain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
#[allow(dead_code)]
enum Enumeration {
    Ident1 = 0,
    Ident2 = 1,
    Ident3 = 2,
    Ident4 = 3,
    Ident5 = 4,
}

/// The original's `Rec_Type`.
///
/// `ptr_comp` is an index into [`Dhrystone::rec`] rather than a raw pointer.
/// The original's records form a self-referential cycle — `Ptr_Glob->Ptr_Comp`
/// points at `Next_Ptr_Glob`, and a structure assignment copies that pointer
/// into the record it points at — which raw references cannot express in safe
/// Rust. Indices preserve the aliasing exactly, with the same amount of data
/// movement, and need no `unsafe`.
///
/// The three-way `variant` union is flattened: only `var_1` is ever written, so
/// the other arms would be dead. The cost is that a structure assignment moves
/// a slightly larger record than in C.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Record {
    ptr_comp: usize,
    discr: Enumeration,
    enum_comp: Enumeration,
    int_comp: i32,
    str_comp: Str30,
}

impl Default for Record {
    fn default() -> Record {
        Record {
            ptr_comp: NULL_REC,
            discr: Enumeration::Ident1,
            enum_comp: Enumeration::Ident1,
            int_comp: 0,
            str_comp: [0; 31],
        }
    }
}

/// Stands in for the original's null `Rec_Pointer`.
const NULL_REC: usize = usize::MAX;

/// Index of `Ptr_Glob` in the record arena.
const PTR_GLOB: usize = 0;
/// Index of `Next_Ptr_Glob` in the record arena.
const NEXT_PTR_GLOB: usize = 1;

/// `strcpy` for the fixed-size buffers, including the terminating NUL.
fn str_copy(dst: &mut Str30, src: &[u8]) {
    debug_assert!(src.len() < dst.len(), "source must leave room for the NUL");
    dst[..src.len()].copy_from_slice(src);
    dst[src.len()] = 0;
}

/// `strcmp`: compares NUL-terminated contents, returning the sign convention of
/// the C original.
fn str_cmp(a: &Str30, b: &Str30) -> i32 {
    for i in 0..a.len() {
        let (x, y) = (a[i], b[i]);
        if x != y {
            return i32::from(x) - i32::from(y);
        }
        if x == 0 {
            return 0;
        }
    }
    0
}

/// One thread's complete Dhrystone state.
///
/// Everything the original declared at file scope lives here, which is what
/// makes concurrent execution safe. The `*_loc` fields are the main loop's
/// locals, retained after the loop so the reference-comparison test can inspect
/// them; the original prints the same values at the end of its run.
struct Dhrystone {
    int_glob: i32,
    bool_glob: bool,
    ch_1_glob: u8,
    ch_2_glob: u8,
    arr_1_glob: [i32; 50],
    arr_2_glob: [[i32; 50]; 50],
    rec: [Record; 2],

    int_1_loc: i32,
    int_2_loc: i32,
    int_3_loc: i32,
    enum_loc: Enumeration,
    str_1_loc: Str30,
    str_2_loc: Str30,
}

impl Dhrystone {
    /// Set up exactly as the original's `main` does before its timing loop.
    fn new() -> Dhrystone {
        let mut rec = [Record::default(); 2];
        rec[PTR_GLOB].ptr_comp = NEXT_PTR_GLOB;
        rec[PTR_GLOB].discr = Enumeration::Ident1;
        rec[PTR_GLOB].enum_comp = Enumeration::Ident3;
        rec[PTR_GLOB].int_comp = 40;
        str_copy(&mut rec[PTR_GLOB].str_comp, STR_SOME);

        let mut arr_2_glob = [[0i32; 50]; 50];
        arr_2_glob[8][7] = 10;

        let mut str_1_loc = [0u8; 31];
        str_copy(&mut str_1_loc, STR_1);

        Dhrystone {
            int_glob: 0,
            bool_glob: false,
            ch_1_glob: b'\0',
            ch_2_glob: b'\0',
            arr_1_glob: [0; 50],
            arr_2_glob,
            rec,
            int_1_loc: 0,
            int_2_loc: 0,
            int_3_loc: 0,
            enum_loc: Enumeration::Ident1,
            str_1_loc,
            str_2_loc: [0; 31],
        }
    }

    /// One pass of the original's main measurement loop.
    ///
    /// `run_index` is the 1-based iteration number, matching `Run_Index`.
    fn iteration(&mut self, run_index: i32) {
        self.proc_5();
        self.proc_4();

        self.int_1_loc = 2;
        self.int_2_loc = 3;
        let mut str_2 = self.str_2_loc;
        str_copy(&mut str_2, STR_2);
        self.str_2_loc = str_2;
        self.enum_loc = Enumeration::Ident2;

        let (s1, s2) = (self.str_1_loc, self.str_2_loc);
        self.bool_glob = !self.func_2(&s1, &s2);

        while self.int_1_loc < self.int_2_loc {
            self.int_3_loc = 5 * self.int_1_loc - self.int_2_loc;
            let mut out = self.int_3_loc;
            self.proc_7(self.int_1_loc, self.int_2_loc, &mut out);
            self.int_3_loc = out;
            self.int_1_loc += 1;
        }

        self.proc_8(self.int_1_loc, self.int_3_loc);
        self.proc_1(PTR_GLOB);

        let mut ch_index = b'A';
        while ch_index <= self.ch_2_glob {
            // `Func_1` can assign to `Ch_1_Glob`, so it needs `&mut self` and
            // the comparand must be read first.
            let enum_loc = self.enum_loc;
            if enum_loc == self.func_1(ch_index, b'C') {
                let mut enum_out = self.enum_loc;
                self.proc_6(Enumeration::Ident1, &mut enum_out);
                self.enum_loc = enum_out;
                let mut str_2 = self.str_2_loc;
                str_copy(&mut str_2, STR_3);
                self.str_2_loc = str_2;
                self.int_2_loc = run_index;
                self.int_glob = run_index;
            }
            ch_index += 1;
        }

        self.int_2_loc = self.int_2_loc.wrapping_mul(self.int_1_loc);
        self.int_1_loc = self.int_2_loc / self.int_3_loc;
        self.int_2_loc = 7 * (self.int_2_loc - self.int_3_loc) - self.int_1_loc;
        let mut out = self.int_1_loc;
        self.proc_2(&mut out);
        self.int_1_loc = out;
    }

    #[inline(never)]
    fn proc_1(&mut self, ptr_val_par: usize) {
        let next_record = self.rec[ptr_val_par].ptr_comp;
        // structassign(*Ptr_Val_Par->Ptr_Comp, *Ptr_Glob)
        self.rec[next_record] = self.rec[PTR_GLOB];
        self.rec[ptr_val_par].int_comp = 5;
        self.rec[next_record].int_comp = self.rec[ptr_val_par].int_comp;
        self.rec[next_record].ptr_comp = self.rec[ptr_val_par].ptr_comp;

        let mut ptr_ref = self.rec[next_record].ptr_comp;
        self.proc_3(&mut ptr_ref);
        self.rec[next_record].ptr_comp = ptr_ref;

        if self.rec[next_record].discr == Enumeration::Ident1 {
            self.rec[next_record].int_comp = 6;
            let enum_val = self.rec[ptr_val_par].enum_comp;
            let mut enum_out = self.rec[next_record].enum_comp;
            self.proc_6(enum_val, &mut enum_out);
            self.rec[next_record].enum_comp = enum_out;
            self.rec[next_record].ptr_comp = self.rec[PTR_GLOB].ptr_comp;
            let mut int_out = self.rec[next_record].int_comp;
            self.proc_7(self.rec[next_record].int_comp, 10, &mut int_out);
            self.rec[next_record].int_comp = int_out;
        } else {
            let source = self.rec[ptr_val_par].ptr_comp;
            self.rec[ptr_val_par] = self.rec[source];
        }
    }

    #[inline(never)]
    fn proc_2(&mut self, int_par_ref: &mut i32) {
        let mut int_loc = *int_par_ref + 10;
        // The original leaves `Enum_Loc` uninitialised and relies on the body
        // running at least once to set it; `Ch_1_Glob` is always 'A' here
        // because `Proc_5` set it earlier in the same iteration. Seeding it to
        // Ident_1 keeps the behaviour identical while making a hostile state
        // terminate instead of spinning forever.
        let mut enum_loc = Enumeration::Ident1;
        loop {
            if self.ch_1_glob == b'A' {
                int_loc -= 1;
                *int_par_ref = int_loc - self.int_glob;
                enum_loc = Enumeration::Ident1;
            }
            if enum_loc == Enumeration::Ident1 {
                break;
            }
        }
    }

    #[inline(never)]
    fn proc_3(&mut self, ptr_ref_par: &mut usize) {
        if PTR_GLOB != NULL_REC {
            *ptr_ref_par = self.rec[PTR_GLOB].ptr_comp;
        }
        let mut out = self.rec[PTR_GLOB].int_comp;
        self.proc_7(10, self.int_glob, &mut out);
        self.rec[PTR_GLOB].int_comp = out;
    }

    #[inline(never)]
    fn proc_4(&mut self) {
        let bool_loc = self.ch_1_glob == b'A';
        self.bool_glob |= bool_loc;
        self.ch_2_glob = b'B';
    }

    #[inline(never)]
    fn proc_5(&mut self) {
        self.ch_1_glob = b'A';
        self.bool_glob = false;
    }

    #[inline(never)]
    fn proc_6(&mut self, enum_val_par: Enumeration, enum_ref_par: &mut Enumeration) {
        *enum_ref_par = enum_val_par;
        if !self.func_3(enum_val_par) {
            *enum_ref_par = Enumeration::Ident4;
        }
        match enum_val_par {
            Enumeration::Ident1 => *enum_ref_par = Enumeration::Ident1,
            Enumeration::Ident2 => {
                *enum_ref_par = if self.int_glob > 100 {
                    Enumeration::Ident1
                } else {
                    Enumeration::Ident4
                }
            }
            Enumeration::Ident3 => *enum_ref_par = Enumeration::Ident2,
            Enumeration::Ident4 => {}
            Enumeration::Ident5 => *enum_ref_par = Enumeration::Ident3,
        }
    }

    #[inline(never)]
    fn proc_7(&self, int_1_par_val: i32, int_2_par_val: i32, int_par_ref: &mut i32) {
        let int_loc = int_1_par_val + 2;
        *int_par_ref = int_2_par_val + int_loc;
    }

    /// The original passes `Arr_1_Glob` and `Arr_2_Glob` as parameters; here
    /// they are reached through `self`, which is the same aliasing.
    #[inline(never)]
    fn proc_8(&mut self, int_1_par_val: i32, int_2_par_val: i32) {
        let int_loc = (int_1_par_val + 5) as usize;
        self.arr_1_glob[int_loc] = int_2_par_val;
        self.arr_1_glob[int_loc + 1] = self.arr_1_glob[int_loc];
        self.arr_1_glob[int_loc + 30] = int_loc as i32;
        for int_index in int_loc..=int_loc + 1 {
            self.arr_2_glob[int_loc][int_index] = int_loc as i32;
        }
        // Increments once per iteration for the whole life of this state, so it
        // is the one value here that can run away; wrapping matches C.
        self.arr_2_glob[int_loc][int_loc - 1] =
            self.arr_2_glob[int_loc][int_loc - 1].wrapping_add(1);
        self.arr_2_glob[int_loc + 20][int_loc] = self.arr_1_glob[int_loc];
        self.int_glob = 5;
    }

    #[inline(never)]
    fn func_1(&mut self, ch_1_par_val: u8, ch_2_par_val: u8) -> Enumeration {
        let ch_1_loc = ch_1_par_val;
        let ch_2_loc = ch_1_loc;
        if ch_2_loc != ch_2_par_val {
            Enumeration::Ident1
        } else {
            self.ch_1_glob = ch_1_loc;
            Enumeration::Ident2
        }
    }

    #[inline(never)]
    fn func_2(&mut self, str_1_par_ref: &Str30, str_2_par_ref: &Str30) -> bool {
        let mut int_loc: i32 = 2;
        // Uninitialised in the original; the loop below always runs at least
        // once and assigns it before any read.
        let mut ch_loc: u8 = 0;
        while int_loc <= 2 {
            if self.func_1(
                str_1_par_ref[int_loc as usize],
                str_2_par_ref[int_loc as usize + 1],
            ) == Enumeration::Ident1
            {
                ch_loc = b'A';
                int_loc += 1;
            }
        }
        if (b'W'..b'Z').contains(&ch_loc) {
            int_loc = 7;
        }
        if ch_loc == b'R' {
            true
        } else if str_cmp(str_1_par_ref, str_2_par_ref) > 0 {
            int_loc += 7;
            self.int_glob = int_loc;
            true
        } else {
            false
        }
    }

    #[inline(never)]
    fn func_3(&self, enum_par_val: Enumeration) -> bool {
        enum_par_val == Enumeration::Ident3
    }

    /// Fold the whole state into one value.
    ///
    /// Returned to the runner, which black-boxes it. Without a data dependency
    /// on real results, LLVM is free to delete the entire loop.
    fn checksum(&self) -> u64 {
        let mut sum = self.int_glob as i64 as u64;
        sum ^= u64::from(self.bool_glob) << 8;
        sum ^= u64::from(self.ch_1_glob) << 16;
        sum ^= u64::from(self.ch_2_glob) << 24;
        sum = sum.wrapping_add(self.arr_1_glob[8] as i64 as u64);
        sum = sum.wrapping_add(self.arr_2_glob[8][7] as i64 as u64);
        sum = sum.wrapping_add(self.rec[PTR_GLOB].int_comp as i64 as u64);
        sum = sum.wrapping_add(self.rec[NEXT_PTR_GLOB].int_comp as i64 as u64);
        sum ^= (self.int_1_loc as i64 as u64) << 32;
        sum ^= (self.int_2_loc as i64 as u64) << 40;
        sum ^= self.str_2_loc[0] as u64;
        sum
    }
}

impl KernelState for Dhrystone {
    fn run(&mut self, iters: u64) -> u64 {
        for run_index in 1..=iters {
            self.iteration(run_index as i32);
        }
        self.checksum()
    }
}

/// The Dhrystone 2.1 workload.
pub struct DhrystoneKernel;

impl Kernel for DhrystoneKernel {
    fn info(&self) -> KernelInfo {
        KernelInfo {
            id: "dhrystone",
            name: "Dhrystone 2.1",
            summary: "Integer arithmetic, branches, procedure calls, and short string operations",
            unit: Unit::DhrystonesPerSec,
            footprint: Footprint::PerThread,
            scaling: Scaling::Scales,
            // The reference core runs the loop in roughly 200 cycles at 3 GHz.
            reference: 15_000_000.0,
        }
    }

    fn setup(&self, _ctx: &SetupCtx) -> Box<dyn KernelState> {
        Box::new(Dhrystone::new())
    }

    fn rate(&self, iters_per_thread: u64, threads: usize, secs: f64) -> f64 {
        iters_per_thread as f64 * threads as f64 / secs
    }
}

/// Convert a Dhrystones-per-second figure to the conventional DMIPS scale.
pub fn dmips(dhrystones_per_sec: f64) -> f64 {
    dhrystones_per_sec / VAX_DHRYSTONES_PER_SEC
}

#[cfg(test)]
mod tests {
    use super::*;

    fn as_str(buf: &Str30) -> &str {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        std::str::from_utf8(&buf[..end]).expect("Dhrystone strings are ASCII")
    }

    /// The whole point of the port: reproduce the reference implementation's
    /// documented final state exactly.
    ///
    /// Every expected value below is from Dhrystone 2.1's published output, not
    /// from this implementation. If the port drifts, this fails.
    #[test]
    fn final_state_matches_the_reference() {
        const RUNS: u64 = 500;
        let mut d = Dhrystone::new();
        d.run(RUNS);

        assert_eq!(d.int_glob, 5, "Int_Glob");
        assert!(d.bool_glob, "Bool_Glob");
        assert_eq!(d.ch_1_glob, b'A', "Ch_1_Glob");
        assert_eq!(d.ch_2_glob, b'B', "Ch_2_Glob");
        assert_eq!(d.arr_1_glob[8], 7, "Arr_1_Glob[8]");
        assert_eq!(
            d.arr_2_glob[8][7],
            RUNS as i32 + 10,
            "Arr_2_Glob[8][7] should be Number_Of_Runs + 10"
        );

        assert_eq!(
            d.rec[PTR_GLOB].discr,
            Enumeration::Ident1,
            "Ptr_Glob->Discr"
        );
        assert_eq!(
            d.rec[PTR_GLOB].enum_comp,
            Enumeration::Ident3,
            "Ptr_Glob->Enum_Comp"
        );
        assert_eq!(d.rec[PTR_GLOB].int_comp, 17, "Ptr_Glob->Int_Comp");
        assert_eq!(
            as_str(&d.rec[PTR_GLOB].str_comp),
            "DHRYSTONE PROGRAM, SOME STRING"
        );

        assert_eq!(
            d.rec[NEXT_PTR_GLOB].discr,
            Enumeration::Ident1,
            "Next_Ptr_Glob->Discr"
        );
        assert_eq!(
            d.rec[NEXT_PTR_GLOB].enum_comp,
            Enumeration::Ident2,
            "Next_Ptr_Glob->Enum_Comp"
        );
        assert_eq!(d.rec[NEXT_PTR_GLOB].int_comp, 18, "Next_Ptr_Glob->Int_Comp");
        assert_eq!(
            as_str(&d.rec[NEXT_PTR_GLOB].str_comp),
            "DHRYSTONE PROGRAM, SOME STRING"
        );

        assert_eq!(d.int_1_loc, 5, "Int_1_Loc");
        assert_eq!(d.int_2_loc, 13, "Int_2_Loc");
        assert_eq!(d.int_3_loc, 7, "Int_3_Loc");
        assert_eq!(d.enum_loc, Enumeration::Ident2, "Enum_Loc");
        assert_eq!(as_str(&d.str_1_loc), "DHRYSTONE PROGRAM, 1'ST STRING");
        assert_eq!(as_str(&d.str_2_loc), "DHRYSTONE PROGRAM, 2'ND STRING");
    }

    #[test]
    fn final_state_is_independent_of_run_count() {
        // Every documented value except Arr_2_Glob[8][7] reaches a fixed point
        // after the first iteration.
        let mut a = Dhrystone::new();
        a.run(1);
        let mut b = Dhrystone::new();
        b.run(1000);
        assert_eq!(a.int_1_loc, b.int_1_loc);
        assert_eq!(a.int_2_loc, b.int_2_loc);
        assert_eq!(a.rec[PTR_GLOB], b.rec[PTR_GLOB]);
        assert_eq!(a.arr_2_glob[8][7] + 999, b.arr_2_glob[8][7]);
    }

    #[test]
    fn threads_do_not_share_state() {
        // The defect that forced the old implementation to hold a global mutex:
        // concurrent runs must reach the same state as a lone run.
        let solo = {
            let mut d = Dhrystone::new();
            d.run(200);
            d.checksum()
        };
        let checksums: Vec<u64> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    s.spawn(|| {
                        let mut d = Dhrystone::new();
                        d.run(200);
                        d.checksum()
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(
            checksums.iter().all(|&c| c == solo),
            "concurrent runs diverged from the single-threaded result"
        );
    }

    #[test]
    fn str_copy_terminates_and_str_cmp_orders() {
        let mut a: Str30 = [0xFF; 31];
        str_copy(&mut a, STR_1);
        assert_eq!(as_str(&a), "DHRYSTONE PROGRAM, 1'ST STRING");
        assert_eq!(a[30], 0, "must be NUL-terminated");

        let mut b: Str30 = [0; 31];
        str_copy(&mut b, STR_2);
        assert!(str_cmp(&a, &b) < 0, "'1'ST' sorts before '2'ND'");
        assert!(str_cmp(&b, &a) > 0);
        assert_eq!(str_cmp(&a, &a), 0);
    }

    #[test]
    fn str_cmp_stops_at_the_terminator() {
        let mut a: Str30 = [0; 31];
        let mut b: Str30 = [0; 31];
        str_copy(&mut a, b"AB");
        str_copy(&mut b, b"AB");
        // Differing bytes past the NUL must not affect the comparison.
        a[5] = b'X';
        b[5] = b'Y';
        assert_eq!(str_cmp(&a, &b), 0);
    }

    #[test]
    fn kernel_reports_a_positive_rate() {
        let k = DhrystoneKernel;
        let mut state = k.setup(&SetupCtx {
            threads: 1,
            thread_index: 0,
        });
        let checksum = state.run(10_000);
        assert_ne!(checksum, 0, "checksum must depend on real work");
        assert!((k.rate(10_000, 1, 0.5) - 20_000.0).abs() < 1e-9);
        assert!(
            (k.rate(10_000, 4, 0.5) - 80_000.0).abs() < 1e-9,
            "per-thread work must be summed across threads"
        );
    }

    #[test]
    fn dmips_uses_the_vax_divisor() {
        assert!((dmips(1757.0) - 1.0).abs() < 1e-12);
        assert!((dmips(17_570_000.0) - 10_000.0).abs() < 1e-9);
    }
}
