// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Checker trait for analysis callbacks.
//!
//! Mirrors the role of OCaml's `registerCheckers.ml` callback types, but uses
//! Rust traits instead of function pointers in mutable global lists.

use sil::procdesc::Procdesc;
use sil::procname::Procname;
use sil::source_file::SourceFile;
use sil::tenv::Tenv;

use crate::summary::SummaryStore;

/// Context passed to a checker during analysis.
pub struct AnalysisContext<'a, S> {
    /// Type environment for the module being analyzed.
    pub tenv: &'a Tenv,
    /// Summary store for inter-procedural lookups.
    pub summaries: &'a SummaryStore<S>,
    /// The Cfg for looking up procedure descriptions (needed for specialization).
    pub cfg: &'a sil::cfg::Cfg,
}

/// An intraprocedural checker that analyzes a single procedure in isolation.
///
/// This is the simplest checker type — no inter-procedural dependencies.
/// Examples: liveness, SIL validation.
pub trait IntraChecker: Send + Sync {
    /// The analysis result type for a single procedure.
    type Summary: Send + Sync + 'static;

    /// Unique identifier for this checker.
    fn id(&self) -> &str;

    /// Analyze a single procedure.
    fn analyze(&self, pdesc: &Procdesc, tenv: &Tenv) -> Self::Summary;
}

/// An interprocedural checker that can query callee summaries.
///
/// The analysis runner ensures callees are analyzed before callers
/// (bottom-up call graph order). When cycles exist, `ctx.summaries.get()`
/// returns `None` for in-progress callees.
pub trait InterChecker: Send + Sync {
    /// The analysis result type for a single procedure.
    type Summary: Send + Sync + Clone + 'static;

    /// Unique identifier for this checker.
    fn id(&self) -> &str;

    /// Analyze a single procedure with access to callee summaries.
    fn analyze(&self, pdesc: &Procdesc, ctx: &AnalysisContext<Self::Summary>) -> Self::Summary;

    /// Re-analyze a procedure with a specialization applied.
    ///
    /// Called when a caller detects that the callee needs specialization
    /// (e.g., function pointer with known target). The specialization
    /// is applied to the callee's initial state before re-analysis.
    /// Returns the specialized pre/post list.
    ///
    /// Cross-ref: OCaml ondemand.ml analyze_specialized.
    fn analyze_specialized(
        &self,
        _pdesc: &Procdesc,
        _ctx: &AnalysisContext<Self::Summary>,
        _specialization: &sil::specialization::PulseSpecialization,
    ) -> Self::Summary {
        // Default: no specialization support — re-analyze normally
        self.analyze(_pdesc, _ctx)
    }
}

/// A file-level checker that runs after all procedures in a file have been analyzed.
///
/// Mirrors OCaml's file callbacks (e.g. RacerD file-level race reporting,
/// Starvation's cross-procedure deadlock detection). These checkers see all
/// procedure summaries for a source file and produce file-scoped results.
pub trait FileChecker: Send + Sync {
    /// The per-procedure summary type this checker consumes.
    type ProcSummary: Send + Sync + Clone + 'static;
    /// The file-level result type.
    type FileSummary: Send + Sync + 'static;

    /// Unique identifier for this checker.
    fn id(&self) -> &str;

    /// Analyze all procedures in a source file.
    ///
    /// Called once per source file after all procedure-level analysis is complete.
    /// Receives the source file path and all procedure summaries belonging to it.
    fn analyze_file(
        &self,
        source_file: &SourceFile,
        proc_summaries: &[(&Procname, &Self::ProcSummary)],
        tenv: &Tenv,
    ) -> Self::FileSummary;
}
