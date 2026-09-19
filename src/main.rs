#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
mod cli {
    use a76probe::{env::Env, pmu::Pmu, selftest};
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
