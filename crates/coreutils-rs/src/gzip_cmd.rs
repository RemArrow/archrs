//! `gzip`/`gunzip`/`zcat`, built directly on the `flate2` crate (already
//! a dependency of `alpm-rs` for reading real package archives). No
//! vendor target exists here either — GNU gzip is its own upstream
//! project, unrelated to `uutils/coreutils`.
//!
//! Scope: compress/decompress a list of files in place (removing the
//! original unless `-k`), or stream through stdin/stdout with `-c` or no
//! file arguments, matching gzip's own argv[0]-based aliasing (`gunzip`
//! and `zcat` imply decompression, `zcat` also implies `-c`). Not
//! implemented: multi-member gzip streams, `.Z`/`.zip` formats, and
//! gzip's various '--rsyncable'/'--best' tuning flags beyond `-1`..`-9`.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::vec::IntoIter;

struct Args {
    decompress: bool,
    stdout: bool,
    keep: bool,
    force: bool,
    level: u32,
    files: Vec<PathBuf>,
}

fn parse_args(
    argv: &[String],
    decompress_by_default: bool,
    stdout_by_default: bool,
) -> Result<Args, String> {
    let mut args = Args {
        decompress: decompress_by_default,
        stdout: stdout_by_default,
        keep: false,
        force: false,
        level: 6,
        files: Vec::new(),
    };

    for arg in argv.iter().skip(1) {
        if let Some(bundle) = arg.strip_prefix('-').filter(|s| !s.starts_with('-')) {
            for c in bundle.chars() {
                match c {
                    'd' => args.decompress = true,
                    'c' => args.stdout = true,
                    'k' => args.keep = true,
                    'f' => args.force = true,
                    'v' => {} // verbose: accepted, not implemented
                    '1'..='9' => args.level = c.to_digit(10).unwrap(),
                    other => return Err(format!("unsupported gzip flag: -{other}")),
                }
            }
        } else {
            match arg.as_str() {
                "--decompress" | "--uncompress" => args.decompress = true,
                "--stdout" | "--to-stdout" => args.stdout = true,
                "--keep" => args.keep = true,
                "--force" => args.force = true,
                _ if arg.starts_with('-') => return Err(format!("unsupported gzip flag: {arg}")),
                _ => args.files.push(PathBuf::from(arg)),
            }
        }
    }

    Ok(args)
}

fn compressed_name(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".gz");
    PathBuf::from(name)
}

fn decompressed_name(path: &Path) -> Result<PathBuf, String> {
    let name = path.to_string_lossy();
    name.strip_suffix(".gz")
        .map(PathBuf::from)
        .ok_or_else(|| format!("{}: unknown suffix -- ignored", path.display()))
}

fn compress_one(path: &Path, args: &Args) -> io::Result<()> {
    let mut input = File::open(path)?;
    if args.stdout {
        let stdout = io::stdout();
        let mut encoder =
            flate2::write::GzEncoder::new(stdout.lock(), flate2::Compression::new(args.level));
        io::copy(&mut input, &mut encoder)?;
        let _ = encoder.finish()?;
        return Ok(());
    }

    let dest = compressed_name(path);
    if dest.exists() && !args.force {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists", dest.display()),
        ));
    }
    let out = File::create(&dest)?;
    let mut encoder = flate2::write::GzEncoder::new(out, flate2::Compression::new(args.level));
    io::copy(&mut input, &mut encoder)?;
    encoder.finish()?;
    if !args.keep {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn decompress_one(path: &Path, args: &Args) -> Result<(), String> {
    let input = File::open(path).map_err(|e| e.to_string())?;
    let mut decoder = flate2::read::GzDecoder::new(input);

    if args.stdout {
        let mut stdout = io::stdout();
        io::copy(&mut decoder, &mut stdout).map_err(|e| e.to_string())?;
        return Ok(());
    }

    let dest = decompressed_name(path)?;
    if dest.exists() && !args.force {
        return Err(format!("{} already exists", dest.display()));
    }
    let mut out = File::create(&dest).map_err(|e| e.to_string())?;
    io::copy(&mut decoder, &mut out).map_err(|e| e.to_string())?;
    if !args.keep {
        fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn run_stdin_stdout(args: &Args) -> io::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    if args.decompress {
        let mut decoder = flate2::read::GzDecoder::new(stdin.lock());
        io::copy(&mut decoder, &mut stdout.lock())?;
    } else {
        let mut encoder =
            flate2::write::GzEncoder::new(stdout.lock(), flate2::Compression::new(args.level));
        io::copy(&mut stdin.lock(), &mut encoder)?;
        let _ = encoder.finish()?;
    }
    Ok(())
}

fn run_with(argv: Vec<String>, decompress_by_default: bool, stdout_by_default: bool) -> i32 {
    let args = match parse_args(&argv, decompress_by_default, stdout_by_default) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("gzip: {e}");
            return 1;
        }
    };

    if args.files.is_empty() {
        return match run_stdin_stdout(&args) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("gzip: {e}");
                1
            }
        };
    }

    let mut status = 0;
    for path in &args.files {
        let result = if args.decompress {
            decompress_one(path, &args)
        } else {
            compress_one(path, &args).map_err(|e| e.to_string())
        };
        if let Err(e) = result {
            eprintln!("gzip: {e}");
            status = 1;
        }
    }
    status
}

pub fn run(args: IntoIter<OsString>) -> i32 {
    let argv: Vec<String> = args.map(|s| s.to_string_lossy().into_owned()).collect();
    run_with(argv, false, false)
}

pub fn run_gunzip(args: IntoIter<OsString>) -> i32 {
    let argv: Vec<String> = args.map(|s| s.to_string_lossy().into_owned()).collect();
    run_with(argv, true, false)
}

pub fn run_zcat(args: IntoIter<OsString>) -> i32 {
    let argv: Vec<String> = args.map(|s| s.to_string_lossy().into_owned()).collect();
    run_with(argv, true, true)
}
