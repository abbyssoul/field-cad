//! Command line for the compute performance harness.
//!
//! Two audiences: a person reading a table, and a tool reading JSON. Both see
//! the same measurements, and both see which scene produced them.

use std::{path::PathBuf, process::ExitCode};

use fieldcad_bench::{
    RunConfig, report,
    report::{Change, Report},
    run, selected,
};

const USAGE: &str = "\
fieldcad-bench — headless compute performance harness

USAGE:
    fieldcad-bench [OPTIONS]

OPTIONS:
    --filter <SUBSTRING>   Run only benchmarks whose ID contains SUBSTRING
    --quick                Fewer samples and smaller sweeps, for iterating
    --format <table|json>  Output format (default: table)
    --list                 List benchmarks and their sweeps, without running
    --baseline <PATH>      Compare this run against a saved JSON report
    --save-baseline <PATH> Write this run's JSON report to PATH
    --fail-on-regression   Exit non-zero if a benchmark is slower than baseline,
                           or if any measured growth exceeds its declared O()
    -h, --help             Show this help

EXAMPLES:
    # What is slow, and does anything scale worse than it claims?
    fieldcad-bench

    # Iterate on one solver path
    fieldcad-bench --filter maxwell/step --quick

    # Record a baseline, then gate a change against it
    fieldcad-bench --save-baseline perf.json
    fieldcad-bench --baseline perf.json --fail-on-regression

    # Machine-readable
    fieldcad-bench --format json --quick
";

struct Options {
    config: RunConfig,
    format: Format,
    list: bool,
    baseline: Option<PathBuf>,
    save_baseline: Option<PathBuf>,
    fail_on_regression: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Table,
    Json,
}

fn parse() -> Result<Option<Options>, String> {
    let mut filter = None;
    let mut quick = false;
    let mut format = Format::Table;
    let mut list = false;
    let mut baseline = None;
    let mut save_baseline = None;
    let mut fail_on_regression = false;

    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(None);
            }
            "--quick" => quick = true,
            "--list" => list = true,
            "--fail-on-regression" => fail_on_regression = true,
            "--filter" => {
                filter = Some(args.next().ok_or("--filter needs a substring")?);
            }
            "--format" => {
                format = match args.next().ok_or("--format needs a value")?.as_str() {
                    "table" => Format::Table,
                    "json" => Format::Json,
                    other => return Err(format!("unknown format '{other}'")),
                };
            }
            "--baseline" => {
                baseline = Some(PathBuf::from(args.next().ok_or("--baseline needs a path")?));
            }
            "--save-baseline" => {
                save_baseline = Some(PathBuf::from(
                    args.next().ok_or("--save-baseline needs a path")?,
                ));
            }
            other => return Err(format!("unknown argument '{other}'\n\n{USAGE}")),
        }
    }

    Ok(Some(Options {
        config: RunConfig { filter, quick },
        format,
        list,
        baseline,
        save_baseline,
        fail_on_regression,
    }))
}

fn main() -> ExitCode {
    let options = match parse() {
        Ok(Some(options)) => options,
        Ok(None) => return ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };

    if options.list {
        list_benchmarks(&options.config);
        return ExitCode::SUCCESS;
    }

    let benchmarks = selected(&options.config);
    if benchmarks.is_empty() {
        eprintln!("error: no benchmark matched the filter");
        return ExitCode::FAILURE;
    }

    let baseline = match options.baseline.as_ref().map(load_baseline) {
        Some(Ok(baseline)) => Some(baseline),
        Some(Err(error)) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
        None => None,
    };

    let quiet = options.format == Format::Json;
    if !quiet {
        eprintln!(
            "running {} benchmark(s), {} scene(s) total\n",
            benchmarks.len(),
            benchmarks
                .iter()
                .map(|bench| bench.scenes.len())
                .sum::<usize>()
        );
    }
    let report = run(&options.config, |id, scene| {
        if !quiet {
            eprint!("\r  {id} :: {scene}                    ");
        }
    });
    if !quiet {
        eprintln!("\r{: <70}", "");
    }

    if let Some(path) = &options.save_baseline
        && let Err(error) = std::fs::write(path, report.to_json())
    {
        eprintln!(
            "error: could not write baseline to {}: {error}",
            path.display()
        );
        return ExitCode::FAILURE;
    }

    match options.format {
        Format::Json => println!("{}", report.to_json()),
        Format::Table => print_table(&report, baseline.as_ref()),
    }

    let regressed = baseline
        .as_ref()
        .map(|baseline| report::compare(baseline, &report))
        .is_some_and(|comparisons| {
            comparisons
                .iter()
                .any(|comparison| comparison.change == Change::Slower)
        });
    let scaling_worse = !report.attention().is_empty();

    if options.fail_on_regression && (regressed || scaling_worse) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn load_baseline(path: &PathBuf) -> Result<Report, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("could not read baseline {}: {error}", path.display()))?;
    let report = Report::from_json(&text)?;
    if report.schema_version != report::SCHEMA_VERSION {
        return Err(format!(
            "baseline uses schema version {}, this build expects {}",
            report.schema_version,
            report::SCHEMA_VERSION
        ));
    }
    Ok(report)
}

fn list_benchmarks(config: &RunConfig) {
    for bench in selected(config) {
        println!("{}  [{}]", bench.id, bench.declared.label());
        println!("    measures : {}", bench.what);
        println!("    matters  : {}", bench.why);
        println!(
            "    sweeps   : {} over {} scenes",
            bench.parameter.label(),
            bench.scenes.len()
        );
        for scene in &bench.scenes {
            println!("        {: <28} {}", scene.name, scene.size_label());
        }
        println!();
    }
}

fn print_table(report: &Report, baseline: Option<&Report>) {
    println!("Field CAD compute benchmarks  ({} profile)", report.profile);
    println!("{}\n", report.scope);

    let comparisons = baseline.map(|baseline| report::compare(baseline, report));

    for bench in &report.benchmarks {
        println!("{}", bench.id);
        println!("  measures : {}", bench.what);
        println!("  matters  : {}", bench.why);
        println!(
            "  declared : {} in {}",
            bench.declared_label, bench.parameter
        );

        println!(
            "  {: <24} {: >12} {: >12} {: >12} {: >9}{}",
            "scene",
            bench.parameter,
            "median",
            "per unit",
            "noise",
            vs_baseline_header(&comparisons)
        );
        for point in &bench.points {
            let comparison = comparisons.as_ref().and_then(|comparisons| {
                comparisons
                    .iter()
                    .find(|entry| entry.id == bench.id && entry.scene == point.scene)
            });
            println!(
                "  {: <24} {: >12} {: >12} {: >12} {: >8.1}%{}",
                point.scene,
                report::format_count(point.n),
                report::format_ns(point.timing.median_ns),
                report::format_ns(point.ns_per_unit),
                point.timing.noise() * 100.0,
                match comparison {
                    Some(comparison) if comparison.change == Change::New => "   new".to_owned(),
                    Some(comparison) => format!(
                        "  {: >+7.1}% {}",
                        comparison.delta * 100.0,
                        comparison.change.label()
                    ),
                    None => String::new(),
                }
            );
        }

        match (&bench.scaling, bench.verdict) {
            (Some(scaling), Some(verdict)) => {
                println!(
                    "  measured : O({}^{:.2})  scatter={:.3}  R²={:.3}  -> {}",
                    bench.parameter,
                    scaling.exponent,
                    scaling.log_residual,
                    scaling.r_squared,
                    verdict.label()
                );
            }
            _ => println!("  measured : sweep could not support a fit"),
        }

        // A scene summary belongs with the numbers, not in a separate document.
        if let Some(first) = bench.points.first() {
            println!("  scene    : {}", first.scene_summary);
        }
        println!();
    }

    let attention = report.attention();
    if attention.is_empty() {
        println!("No benchmark grew faster than its declared complexity.");
    } else {
        println!("Growing faster than declared — look here first:");
        for bench in attention {
            let scaling = bench.scaling.expect("a verdict implies a fit");
            println!(
                "  {: <34} declared {}, measured O({}^{:.2})",
                bench.id, bench.declared_label, bench.parameter, scaling.exponent
            );
        }
    }
}

fn vs_baseline_header(comparisons: &Option<Vec<report::Comparison>>) -> &'static str {
    if comparisons.is_some() {
        "   vs baseline"
    } else {
        ""
    }
}
