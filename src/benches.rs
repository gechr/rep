//! Nightly micro-benchmarks for the pure, CPU-bound hot paths: expression
//! application, colored diff rendering, tokenization, and SGR assembly. These
//! complement the end-to-end `scripts/bench.sh` harness, which covers the
//! I/O-bound walk/scan path that no in-memory bench can model.
//!
//! Run with `make bench` (or `RUSTFLAGS="--cfg rep_bench" cargo +nightly
//! bench`). The whole module is gated behind the `rep_bench` cfg so stable
//! builds never pull in the unstable `test` crate.

use clap::Parser as _;
use test::Bencher;
use test::black_box;

use crate::Cli;
use crate::diff::{self, DiffHints};
use crate::expressions::{CompiledExpression, apply_compiled_expressions, compile_expressions};
use crate::theme::StyleSpec;
use crate::ui::Styles;

/// A ~2000-line source-like buffer with a match on roughly every fifth line,
/// mirroring the `many-small` corpus a single file at a time.
fn corpus() -> String {
    (0..2000)
        .map(|i| {
            if i % 5 == 0 {
                format!("let needle_{i} = compute(some_input, other_input);  // hit\n")
            } else {
                format!("let value_{i} = compute(some_input, other_input);  // plain\n")
            }
        })
        .collect()
}

/// Compile expressions the way the run path does, straight from argv.
fn compile(args: &[&str]) -> Vec<CompiledExpression> {
    let processed =
        crate::preprocess_expression_args(args.iter().map(|s| (*s).to_string()).collect());
    let cli = Cli::parse_from(processed);
    compile_expressions(&cli).expect("benchmark expressions compile")
}

#[bench]
fn apply_single_expression(b: &mut Bencher) {
    let text = corpus();
    let exprs = compile(&["rep", "needle", "replaced"]);
    b.iter(|| {
        let (out, count, _) = apply_compiled_expressions(black_box(text.as_bytes()), &exprs, false);
        black_box((out, count));
    });
}

#[bench]
fn apply_single_expression_with_spans(b: &mut Bencher) {
    let text = corpus();
    let exprs = compile(&["rep", "needle", "replaced"]);
    b.iter(|| {
        let (out, count, spans) =
            apply_compiled_expressions(black_box(text.as_bytes()), &exprs, true);
        black_box((out, count, spans));
    });
}

#[bench]
fn apply_multi_expression(b: &mut Bencher) {
    let text = corpus();
    let exprs = compile(&["rep", "-e", "needle", "replaced", "-e", "compute", "derive"]);
    b.iter(|| {
        let (out, count, _) = apply_compiled_expressions(black_box(text.as_bytes()), &exprs, false);
        black_box((out, count));
    });
}

#[bench]
fn render_colored_diff(b: &mut Bencher) {
    let text = corpus();
    let exprs = compile(&["rep", "needle", "replaced"]);
    let (out, _, spans) = apply_compiled_expressions(text.as_bytes(), &exprs, true);
    let new = String::from_utf8(out.into_owned()).expect("utf8 output");
    let hints = DiffHints {
        spans: &spans,
        linewise: true,
        multiline_spans: false,
    };
    let styles = Styles::ansi();
    let mut sink = String::with_capacity(text.len() * 2);
    b.iter(|| {
        sink.clear();
        diff::print_file_line_diff(
            black_box(&text),
            black_box(&new),
            hints,
            styles,
            None,
            "",
            None,
            None,
            crate::ui::Color::Red,
            &mut sink,
        );
        black_box(&sink);
    });
}

#[bench]
fn tokenize_code_line(b: &mut Bencher) {
    let line = "    let mut some_camelCaseName = compute_value(foo_bar, BazQux::new(42));";
    b.iter(|| black_box(diff::tokenize(black_box(line))));
}

#[bench]
fn sgr_open_into(b: &mut Bencher) {
    // fg + dim + underline: the multi-parameter shape the diff line styles
    // emit, exercising the single-CSI fold.
    let spec = StyleSpec {
        fg: Some(crossterm::style::Color::DarkRed),
        dim: true,
        underline: true,
        ..Default::default()
    };
    let styles = Styles::ansi();
    let mut out = String::with_capacity(32);
    b.iter(|| {
        out.clear();
        spec.open_into(black_box(&mut out), styles);
        black_box(&out);
    });
}
