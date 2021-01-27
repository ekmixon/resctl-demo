// Copyright (c) Facebook, Inc. and its affiliates.
use anyhow::{bail, Result};
use log::error;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::process::exit;
use util::*;

use super::JobSpec;
use rd_agent_intf;

lazy_static::lazy_static! {
    static ref TOP_ARGS_STR: String = format!(
        "<RESULTFILE>           'Record the bench results into the specified json file'
         -d, --dir=[TOPDIR]     'Top-level dir for operation and scratch files (default: {dfl_dir})'
         -D, --dev=[DEVICE]     'Scratch device override (e.g. nvme0n1)'
         -l, --linux=[PATH]     'Path to linux.tar, downloaded automatically if not specified'
         -R, --rep-retention=[SECS] '1s report retention in seconds (default: {dfl_rep_ret:.1}h)'
         -a, --args=[FILE]      'Load base command line arguments from FILE'
         -c, --iocost-from-sys  'Use iocost parameters from io.cost.{{model,qos}} instead of bench.json'
             --keep-reports     'Don't delete expired report files'
             --clear-reports    'Remove existing report files'
             --test             'Test mode for development'
         -v...                  'Sets the level of verbosity'",
        dfl_dir = Args::default().dir,
        dfl_rep_ret = Args::default().rep_retention,
    );
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Args {
    pub dir: String,
    pub dev: Option<String>,
    pub linux_tar: Option<String>,
    pub rep_retention: u64,
    pub job_specs: Vec<JobSpec>,

    #[serde(skip)]
    pub result: String,
    #[serde(skip)]
    pub iocost_from_sys: bool,
    #[serde(skip)]
    pub keep_reports: bool,
    #[serde(skip)]
    pub clear_reports: bool,
    #[serde(skip)]
    pub test: bool,
    #[serde(skip)]
    pub verbosity: u32,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            dir: rd_agent_intf::Args::default().dir.clone(),
            dev: None,
            linux_tar: None,
            result: "".into(),
            job_specs: Default::default(),
            rep_retention: 24 * 3600,
            iocost_from_sys: false,
            keep_reports: false,
            clear_reports: false,
            test: false,
            verbosity: 0,
        }
    }
}

impl Args {
    fn parse_job_spec(spec: &str) -> Result<JobSpec> {
        let mut groups = spec.split(':');

        let kind = match groups.next() {
            Some(v) => v,
            None => bail!("invalid job type"),
        };

        let mut properties = vec![];
        let mut id = None;

        for group in groups {
            let mut props = BTreeMap::<String, String>::new();
            for tok in group.split(',') {
                if tok.len() == 0 {
                    continue;
                }

                // Allow key-only properties.
                let mut kv = tok.splitn(2, '=').collect::<Vec<&str>>();
                while kv.len() < 2 {
                    kv.push("");
                }

                match kv[0] {
                    "id" => id = Some(kv[1]),
                    key => {
                        props.insert(key.into(), kv[1].into());
                    }
                }
            }
            properties.push(props);
        }

        // Make sure there always is the first group.
        if properties.len() == 0 {
            properties.push(Default::default());
        }

        Ok(JobSpec::new(
            kind.into(),
            id.map(str::to_string),
            properties,
        ))
    }

    fn parse_job_specs(subm: &clap::ArgMatches) -> Result<Vec<JobSpec>> {
        let mut jobsets = BTreeMap::<usize, Vec<JobSpec>>::new();

        match (subm.indices_of("spec"), subm.values_of("spec")) {
            (Some(idxs), Some(specs)) => {
                for (idx, spec) in idxs.zip(specs) {
                    match Self::parse_job_spec(spec) {
                        Ok(v) => {
                            jobsets.insert(idx, vec![v]);
                        }
                        Err(e) => bail!("spec {:?}: {}", spec, &e),
                    }
                }
            }
            _ => {}
        }

        match (subm.indices_of("file"), subm.values_of("file")) {
            (Some(idxs), Some(fnames)) => {
                for (idx, fname) in idxs.zip(fnames) {
                    match Self::load(fname) {
                        Ok(v) => {
                            jobsets.insert(idx, v.job_specs);
                        }
                        Err(e) => bail!("file {:?}: {}", fname, &e),
                    }
                }
            }
            _ => {}
        }

        let mut job_specs = Vec::new();
        if jobsets.len() > 0 {
            for jobset in jobsets.values_mut() {
                job_specs.append(jobset);
            }
        }
        Ok(job_specs)
    }
}

impl JsonLoad for Args {}
impl JsonSave for Args {}

impl JsonArgs for Args {
    fn match_cmdline() -> clap::ArgMatches<'static> {
        clap::App::new("resctl-bench")
            .version(clap::crate_version!())
            .author(clap::crate_authors!("\n"))
            .about("Facebook Resoruce Control Benchmarks")
            .setting(clap::AppSettings::UnifiedHelpMessage)
            .setting(clap::AppSettings::DeriveDisplayOrder)
            .args_from_usage(&TOP_ARGS_STR)
            .subcommand(
                clap::SubCommand::with_name("run")
                    .about("Run benchmarks")
                    .arg(
                        clap::Arg::with_name("file")
                            .long("job")
                            .short("j")
                            .multiple(true)
                            .takes_value(true)
                            .number_of_values(1)
                            .help("Benchmark job file"),
                    )
                    .arg(
                        clap::Arg::with_name("spec")
                            .multiple(true)
                            .help("Benchmark job spec - \"BENCH_TYPE[:KEY=VAL...]\""),
                    ),
            )
            .get_matches()
    }

    fn verbosity(matches: &clap::ArgMatches) -> u32 {
        matches.occurrences_of("v") as u32
    }

    fn process_cmdline(&mut self, matches: &clap::ArgMatches) -> bool {
        let dfl = Args::default();
        let mut updated = false;

        if let Some(v) = matches.value_of("dir") {
            self.dir = if v.len() > 0 {
                v.to_string()
            } else {
                dfl.dir.clone()
            };
            updated = true;
        }
        if let Some(v) = matches.value_of("dev") {
            self.dev = if v.len() > 0 {
                Some(v.to_string())
            } else {
                None
            };
            updated = true;
        }
        if let Some(v) = matches.value_of("linux") {
            self.linux_tar = if v.len() > 0 {
                Some(v.to_string())
            } else {
                None
            };
            updated = true;
        }
        if let Some(v) = matches.value_of("rep-retention") {
            self.rep_retention = if v.len() > 0 {
                v.parse::<u64>().unwrap()
            } else {
                dfl.rep_retention
            };
            updated = true;
        }

        self.result = matches.value_of("RESULTFILE").unwrap().into();
        self.iocost_from_sys = matches.is_present("iocost-from-sys");
        self.keep_reports = matches.is_present("keep-reports");
        self.clear_reports = matches.is_present("clear-reports");
        self.test = matches.is_present("test");
        self.verbosity = Self::verbosity(matches);

        match matches.subcommand() {
            ("run", Some(subm)) => match Self::parse_job_specs(subm) {
                Ok(job_specs) => {
                    if job_specs.len() > 0 {
                        self.job_specs = job_specs;
                        updated = true;
                    }
                }
                Err(e) => {
                    error!("{}", &e);
                    exit(1);
                }
            },
            _ => {}
        }

        updated
    }
}
