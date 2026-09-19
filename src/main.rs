#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
mod cli {
    use a76probe::{analysis, cache_geom, env::Env, harness::Session, mem_bw, mem_lat, pmu::Pmu, selftest};
    use anyhow::Result;
    use clap::{Parser, Subcommand};
    use std::path::PathBuf;

    #[derive(Parser)]
    #[command(name = "a76probe", version, about = "Cortex-A76 (Raspberry Pi 5) microarchitecture explorer")]
    struct Cli {
        #[command(subcommand)]
        cmd: Cmd,
    }

    #[derive(Subcommand)]
    enum Cmd {
        /// Print the environment snapshot as JSON (optionally also write it to a file).
        Env {
            #[arg(long)]
            out: Option<PathBuf>,
        },
        /// Validate the timing and PMU measurement chain.
        Selftest {
            #[arg(long, default_value_t = 1)]
            core: usize,
            #[arg(long, default_value_t = 30)]
            repeat: usize,
            #[arg(long, default_value = "results")]
            out_dir: PathBuf,
        },
        /// Run experiments (phase 1: latency, tlb, bandwidth; phase 2: cache, needs root for pagemap).
        Run {
            /// Run every implemented experiment.
            #[arg(long)]
            all: bool,
            /// Experiment(s) to run: latency, tlb, bandwidth, cache.
            #[arg(long = "exp", value_name = "NAME")]
            exps: Vec<String>,
            #[arg(long, default_value_t = 1)]
            core: usize,
            #[arg(long, default_value_t = 30)]
            repeat: usize,
            #[arg(long, default_value = "results")]
            out_dir: PathBuf,
        },
        /// Re-score the replacement-policy signatures stored in a raw cache JSONL file (no hardware access).
        AnalyzeCache {
            #[arg(long)]
            raw: PathBuf,
            #[arg(long, default_value_t = 4)]
            l1_ways: usize,
            #[arg(long, default_value_t = 8)]
            l2_ways: usize,
        },
        /// List PMU events exposed by the kernel for this CPU.
        PmuList,
    }

    pub fn main() -> Result<()> {
        match Cli::parse().cmd {
            Cmd::Env { out } => {
                let json = serde_json::to_string_pretty(&Env::collect())?;
                if let Some(p) = out {
                    std::fs::write(p, &json)?;
                }
                println!("{json}");
            }
            Cmd::Selftest { core, repeat, out_dir } => selftest::run(&selftest::Opts { core, repeat, out_dir })?,
            Cmd::Run { all, exps, core, repeat, out_dir } => {
                let known = ["latency", "tlb", "bandwidth", "cache"];
                let wanted: Vec<String> = if all { known.iter().map(|s| s.to_string()).collect() } else { exps };
                if wanted.is_empty() {
                    anyhow::bail!("nothing to run: pass --all or --exp <{}>", known.join("|"));
                }
                if let Some(bad) = wanted.iter().find(|w| !known.contains(&w.as_str())) {
                    anyhow::bail!("unknown experiment '{bad}' (known: {})", known.join(", "));
                }
                let label = if wanted.iter().any(|w| w == "cache") { "phase2" } else { "phase1" };
                let sess = Session::start(core, repeat, &out_dir, label)?;
                println!("core {core}, expected freq {} kHz, CNTFRQ {} Hz, {repeat} reps", sess.max_khz, sess.frq);
                for w in &wanted {
                    match w.as_str() {
                        "latency" => mem_lat::run_latency(&sess)?,
                        "tlb" => mem_lat::run_tlb(&sess)?,
                        "bandwidth" => mem_bw::run_bw(&sess)?,
                        "cache" => cache_geom::run_cache(&sess)?,
                        _ => unreachable!(),
                    }
                }
            }
            Cmd::AnalyzeCache { raw, l1_ways, l2_ways } => {
                for (level, ways) in [("L1D", l1_ways), ("L2", l2_ways)] {
                    let vals = analysis::load_pattern_values(&raw, level, ways)?;
                    let scores = analysis::score_policies(ways, &vals);
                    println!("\n== {level}, W = {ways}");
                    cache_geom::print_scores(level, &a76probe::sim::patterns(ways), &vals, &scores);
                }
            }
            Cmd::PmuList => {
                let p = Pmu::discover()?;
                println!("device {} type {} cpus {}", p.device, p.type_, p.cpus);
                for (name, cfg) in &p.events {
                    println!("{name:<24} event={cfg:#06x}");
                }
            }
        }
        Ok(())
    }
}

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
fn main() -> anyhow::Result<()> {
    cli::main()
}

#[cfg(not(all(target_os = "linux", target_arch = "aarch64")))]
fn main() {
    eprintln!("a76probe only runs on aarch64 Linux; cross-compile with the target from .cargo/config.toml");
    std::process::exit(2);
}
