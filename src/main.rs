// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Shaun Murphy

use clap::{ArgAction, Parser, ValueEnum, error::ErrorKind};
use log::{debug, warn};
use std::path::PathBuf;
use std::process;

use takeout_helper_gphotos::app::{self, AppConfig};
use takeout_helper_gphotos::organizer::OrganizeMode;
use takeout_helper_gphotos::progress::ProgressLogger;

/// The output layout `--organize` selects.
///
/// A `ValueEnum` rather than a free string so a typo is a CLI error instead of
/// a silent fallback to the default layout.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum Organize {
    /// `YYYY/MM/` (default).
    Date,
    /// `<album>/`, falling back to the date layout for files with no album.
    Album,
    /// Everything in the output root.
    Flat,
    /// `YYYY/MM/` plus an extra copy under `<album>/` for album members.
    #[clap(name = "date-album")]
    DateAlbum,
}

impl From<Organize> for OrganizeMode {
    fn from(value: Organize) -> Self {
        match value {
            Organize::Date => OrganizeMode::Date,
            Organize::Album => OrganizeMode::Album,
            Organize::Flat => OrganizeMode::Flat,
            Organize::DateAlbum => OrganizeMode::DateAlbum,
        }
    }
}

/// Logging verbosity.
///
/// A `ValueEnum` on purpose: an unparseable `env_logger` filter is treated as
/// "log nothing", so a typo used to silently disable *all* logging for a
/// multi-hour run. A typo is therefore a hard CLI error.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lower")]
enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    fn as_filter(self) -> &'static str {
        match self {
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
            LogLevel::Trace => "trace",
        }
    }
}

/// A tool to organize Google Photos takeout archives
#[derive(Parser, Debug)]
#[clap(version, about = "Organize Google Photos takeout archives", long_about = Some("A tool to process Google Photos Takeout .zip, .tgz and .tar.gz files into a clean, organized, metadata-corrected photo library.\n\nThis tool extracts your Google Photos Takeout archives, reads their metadata, corrects EXIF data, and organizes your photos into a chronological folder structure (YYYY/MM). Files with no trustworthy date are filed under unknown-date/ instead of being guessed at.\n\nExit codes: 0 = success, 1 = completed with errors (see takeout-helper-report.csv in the output directory), 130 = interrupted with Ctrl+C.\n\nBy default, only warnings and errors are logged. Use --log-level to change this behavior."))]
struct Args {
    /// Path to the directory containing Google Photos takeout archives
    #[clap(
        long,
        short = 'i',
        value_parser,
        help = "Input directory containing Google Photos takeout .zip/.tgz/.tar.gz files"
    )]
    input: PathBuf,

    /// Path to the destination directory for organized photos
    #[clap(
        long,
        short = 'o',
        value_parser,
        help = "Output directory where organized photos will be stored"
    )]
    output: PathBuf,

    /// Increase logging verbosity
    #[clap(
        short,
        long,
        action = ArgAction::Count,
        help = "Increase logging: -v info, -vv debug, -vvv trace (overrides --log-level)"
    )]
    verbose: u8,

    /// Set the logging level
    #[clap(
        long,
        value_enum,
        ignore_case = true,
        default_value = "warn",
        help = "Logging level. Precedence: RUST_LOG (if set) > --verbose > --log-level"
    )]
    log_level: LogLevel,

    /// Maximum uncompressed size per file (e.g., 100M, 10G, "200 GB")
    #[clap(
        long,
        help = "Maximum uncompressed size per file (default 50G). A parse error aborts the run"
    )]
    max_file_size: Option<String>,

    /// Maximum total uncompressed size per archive (e.g., 100M, 10G)
    #[clap(
        long,
        help = "Maximum total uncompressed size per archive (default 100G). A parse error aborts the run"
    )]
    max_archive_size: Option<String>,

    /// Maximum number of entries extracted from one archive
    #[clap(
        long,
        help = "Maximum number of entries extracted from a single archive (default 100000)"
    )]
    max_files: Option<u64>,

    /// Search for archive files recursively in subdirectories
    #[clap(
        long,
        short = 'r',
        action,
        help = "Search for archive files (.zip/.tgz/.tar.gz) recursively in subdirectories"
    )]
    recursive: bool,

    /// Path to the temporary directory for extraction
    #[clap(
        long,
        short = 't',
        value_parser,
        help = "Parent directory for extraction scratch space (default: <output>/temp). A uniquely named subdirectory is created inside it; only that subdirectory is deleted"
    )]
    temp_dir: Option<PathBuf>,

    /// Keep the temporary extraction directory after the run
    #[clap(
        long,
        action,
        help = "Do not delete the generated temporary extraction directory when the run ends"
    )]
    keep_temp: bool,

    /// Number of worker threads
    #[clap(
        long,
        short = 'j',
        help = "Number of worker threads (default: one per logical CPU)"
    )]
    jobs: Option<usize>,

    /// Show what the run would do without writing anything
    #[clap(
        long,
        action,
        help = "Plan the run without writing anything outside the scratch directory: no EXIF, no copies, no manifest, no report"
    )]
    dry_run: bool,

    /// Output layout
    #[clap(
        long,
        value_enum,
        ignore_case = true,
        default_value = "date",
        conflicts_with = "preserve_albums",
        help = "Output layout: date (YYYY/MM), album, flat, or date-album (YYYY/MM plus a copy under <album>/)"
    )]
    organize: Organize,

    /// Deprecated alias for --organize date-album
    #[clap(
        long,
        action,
        hide = true,
        help = "Deprecated alias for --organize date-album"
    )]
    preserve_albums: bool,

    /// Reprocess everything, ignoring the resume manifest
    #[clap(
        long,
        action,
        help = "Ignore .gphotos-manifest.json and reprocess every file"
    )]
    force: bool,

    /// Do not skip byte-identical duplicates
    #[clap(
        long,
        action,
        help = "Do not skip byte-identical duplicates (name collisions still get an _N suffix)"
    )]
    no_dedup: bool,

    /// Copy JSON sidecars next to the organized media
    #[clap(
        long,
        action,
        help = "Copy each media file's JSON sidecar next to the organized copy"
    )]
    copy_sidecars: bool,

    /// Skip Google-generated derivatives
    #[clap(
        long,
        action,
        help = "Skip Google-generated derivatives (-edited, -pano, -collage, ...). Off by default: the test is a name match, and it can misfire on your own files"
    )]
    skip_derivatives: bool,

    /// Re-hash the organized library when the run ends
    #[clap(
        long,
        action,
        help = "After organizing, re-hash every file the manifest records and report anything missing or changed"
    )]
    verify: bool,

    /// Timezone for the EXIF date tags
    #[clap(
        long,
        value_name = "TZ",
        help = "IANA timezone (e.g. America/New_York) to render EXIF dates in. Default: each file's own zone, derived from its GPS coordinates, falling back to UTC"
    )]
    timezone: Option<chrono_tz::Tz>,
}

impl Args {
    /// Convert the parsed CLI arguments into the library's configuration type.
    fn into_config(self) -> AppConfig {
        // `--preserve-albums` is the old name for this layout; `conflicts_with`
        // means it can never contradict an explicit `--organize`.
        let organize = if self.preserve_albums {
            OrganizeMode::DateAlbum
        } else {
            self.organize.into()
        };

        AppConfig {
            input: self.input,
            output: self.output,
            recursive: self.recursive,
            temp_dir: self.temp_dir,
            max_file_size: self.max_file_size,
            max_archive_size: self.max_archive_size,
            max_files: self.max_files,
            keep_temp: self.keep_temp,
            dry_run: self.dry_run,
            organize,
            force: self.force,
            no_dedup: self.no_dedup,
            copy_sidecars: self.copy_sidecars,
            skip_derivatives: self.skip_derivatives,
            verify: self.verify,
            timezone: self.timezone,
        }
    }
}

fn main() {
    let args = match Args::try_parse() {
        Ok(args) => args,
        Err(e) => {
            // Check if it's a missing required argument error
            if e.kind() == ErrorKind::MissingRequiredArgument {
                // Print a more descriptive error message
                eprintln!("Error: Missing required arguments.\n");
                eprintln!("This tool requires two directories as arguments:");
                eprintln!(
                    "  --input  <DIRECTORY>  Directory containing Google Photos takeout .zip/.tgz/.tar.gz files"
                );
                eprintln!(
                    "  --output <DIRECTORY>  Directory where organized photos will be stored\n"
                );
                eprintln!("Example usage:");
                // Keep the example platform-neutral.
                eprintln!("  takeout-helper-gphotos --input ./takeout --output ./organized\n");
                eprintln!("For more information, try '--help'");
                process::exit(2);
            } else {
                // For other errors, use the default clap error handling
                e.exit();
            }
        }
    };

    // Logging precedence: RUST_LOG (env_logger's own default) wins if set,
    // otherwise --verbose, otherwise --log-level.
    let log_level = match args.verbose {
        0 => args.log_level.as_filter(),
        1 => "info",
        2 => "debug",
        _ => "trace",
    };

    // Set up logging with filtering for the little_exif crate
    let mut builder =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(log_level));
    builder.filter_module("little_exif::metadata", log::LevelFilter::Off);
    let logger = builder.build();
    let max_level = logger.filter();
    takeout_helper_gphotos::progress::set_enabled(max_level < log::LevelFilter::Debug);
    log::set_boxed_logger(Box::new(ProgressLogger::new(logger)))
        .expect("logging should be initialized only once");
    log::set_max_level(max_level);

    if args.verbose > 0
        && args.log_level != LogLevel::Warn
        && std::env::var_os("RUST_LOG").is_none()
    {
        warn!(
            "--verbose overrides --log-level {}; using {}",
            args.log_level.as_filter(),
            log_level
        );
    }

    if args.preserve_albums {
        warn!("--preserve-albums is deprecated; use --organize date-album");
    }

    // Size the rayon pool before any parallel phase starts.
    if let Some(jobs) = args.jobs {
        if jobs == 0 {
            eprintln!("Error: --jobs must be at least 1");
            process::exit(2);
        }
        if let Err(e) = rayon::ThreadPoolBuilder::new()
            .num_threads(jobs)
            .build_global()
        {
            warn!("Could not set the worker thread count to {}: {}", jobs, e);
        }
    }

    debug!(
        "Starting Google Photos takeout helper with args: {:?}",
        args
    );

    match app::run(args.into_config()) {
        Ok(outcome) => process::exit(outcome.exit_code()),
        Err(e) => {
            eprintln!("Error: {}", e);
            process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, clap::Error> {
        Args::try_parse_from(args)
    }

    #[test]
    fn log_level_is_validated_and_case_insensitive() {
        let base = ["takeout-helper-gphotos", "-i", "in", "-o", "out"];

        let mut ok = base.to_vec();
        ok.extend_from_slice(&["--log-level", "INFO"]);
        assert_eq!(parse(&ok).unwrap().log_level, LogLevel::Info);

        let mut ok = base.to_vec();
        ok.extend_from_slice(&["--log-level", "trace"]);
        assert_eq!(parse(&ok).unwrap().log_level, LogLevel::Trace);

        // A typo is a hard error rather than silently disabling all logging.
        let mut bad = base.to_vec();
        bad.extend_from_slice(&["--log-level", "infp"]);
        let err = parse(&bad).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidValue);

        // The default is warn.
        assert_eq!(parse(&base).unwrap().log_level, LogLevel::Warn);
    }

    #[test]
    fn new_flags_parse() {
        let parsed = parse(&[
            "takeout-helper-gphotos",
            "-i",
            "in",
            "-o",
            "out",
            "--max-files",
            "500",
            "--keep-temp",
            "--jobs",
            "4",
            "--max-file-size",
            "200 GB",
        ])
        .unwrap();
        assert_eq!(parsed.max_files, Some(500));
        assert!(parsed.keep_temp);
        assert_eq!(parsed.jobs, Some(4));
        assert_eq!(parsed.max_file_size.as_deref(), Some("200 GB"));
    }

    #[test]
    fn existing_flags_still_work() {
        let parsed = parse(&[
            "takeout-helper-gphotos",
            "--input",
            "in",
            "--output",
            "out",
            "--recursive",
            "--verbose",
            "--temp-dir",
            "/scratch",
            "--max-archive-size",
            "10G",
        ])
        .unwrap();
        assert!(parsed.recursive);
        assert_eq!(parsed.verbose, 1);
        assert_eq!(parsed.temp_dir, Some(PathBuf::from("/scratch")));
        assert_eq!(parsed.max_archive_size.as_deref(), Some("10G"));
    }

    #[test]
    fn repeated_verbose_flags_increase_the_level() {
        let debug = parse(&["takeout-helper-gphotos", "-i", "in", "-o", "out", "-vv"]).unwrap();
        assert_eq!(debug.verbose, 2);

        let trace = parse(&["takeout-helper-gphotos", "-i", "in", "-o", "out", "-vvv"]).unwrap();
        assert_eq!(trace.verbose, 3);
    }

    #[test]
    fn feature_flags_reach_the_config() {
        let config = parse(&[
            "takeout-helper-gphotos",
            "-i",
            "in",
            "-o",
            "out",
            "--dry-run",
            "--force",
            "--no-dedup",
            "--copy-sidecars",
            "--skip-derivatives",
            "--verify",
            "--organize",
            "album",
        ])
        .unwrap()
        .into_config();

        assert!(config.dry_run);
        assert!(config.force);
        assert!(config.no_dedup);
        assert!(config.copy_sidecars);
        assert!(config.skip_derivatives);
        assert!(config.verify);
        assert_eq!(config.organize, OrganizeMode::Album);
    }

    /// The defaults must stay conservative: nothing is skipped, nothing extra
    /// is written, and the layout is the chronological one.
    #[test]
    fn feature_flags_default_to_off() {
        let config = parse(&["takeout-helper-gphotos", "-i", "in", "-o", "out"])
            .unwrap()
            .into_config();

        assert!(!config.dry_run);
        assert!(!config.force);
        assert!(!config.no_dedup);
        assert!(!config.copy_sidecars);
        assert!(
            !config.skip_derivatives,
            "derivative skipping must be opt-in"
        );
        assert!(!config.verify);
        assert_eq!(config.organize, OrganizeMode::Date);
    }

    #[test]
    fn organize_value_is_validated() {
        let base = ["takeout-helper-gphotos", "-i", "in", "-o", "out"];

        for (value, expected) in [
            ("date", OrganizeMode::Date),
            ("ALBUM", OrganizeMode::Album),
            ("flat", OrganizeMode::Flat),
            ("date-album", OrganizeMode::DateAlbum),
        ] {
            let mut args = base.to_vec();
            args.extend_from_slice(&["--organize", value]);
            assert_eq!(parse(&args).unwrap().into_config().organize, expected);
        }

        // A typo must be a hard error, not a silent fallback to `date`.
        let mut bad = base.to_vec();
        bad.extend_from_slice(&["--organize", "dates"]);
        assert_eq!(parse(&bad).unwrap_err().kind(), ErrorKind::InvalidValue);
    }

    #[test]
    fn preserve_albums_is_a_deprecated_alias() {
        let base = ["takeout-helper-gphotos", "-i", "in", "-o", "out"];

        let mut args = base.to_vec();
        args.push("--preserve-albums");
        assert_eq!(
            parse(&args).unwrap().into_config().organize,
            OrganizeMode::DateAlbum
        );

        // It can never contradict an explicit --organize.
        let mut conflicting = base.to_vec();
        conflicting.extend_from_slice(&["--preserve-albums", "--organize", "flat"]);
        assert_eq!(
            parse(&conflicting).unwrap_err().kind(),
            ErrorKind::ArgumentConflict
        );
    }
}
