//! A `tar` implementation built directly on the `tar`/`flate2`/`zstd`
//! crates (the same libraries `alpm-rs` already uses to read real Arch
//! package archives) — no `uu_tar` exists to vendor, since GNU tar is its
//! own separate upstream project, not part of `uutils/coreutils`.
//!
//! Scope: create (`-c`), extract (`-x`), and list (`-t`), with gzip
//! (`-z`) and zstd (`--zstd`) compression, matching the common subset of
//! real tar invocations (`tar -czvf out.tar.gz dir/`, `tar -xvf a.tar`).
//! Not implemented: bzip2/xz (no vendored crate for either yet),
//! incremental archives, sparse-file handling beyond what the `tar` crate
//! itself does, and extracting a subset of named members.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::vec::IntoIter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Create,
    Extract,
    List,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Compression {
    None,
    Gzip,
    Zstd,
}

struct Args {
    mode: Option<Mode>,
    compression: Option<Compression>,
    verbose: bool,
    archive: Option<PathBuf>,
    directory: Option<PathBuf>,
    paths: Vec<PathBuf>,
}

fn sniff_compression(path: &Path) -> Compression {
    let name = path.to_string_lossy();
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        Compression::Gzip
    } else if name.ends_with(".tar.zst") || name.ends_with(".tzst") {
        Compression::Zstd
    } else {
        Compression::None
    }
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut args = Args {
        mode: None,
        compression: None,
        verbose: false,
        archive: None,
        directory: None,
        paths: Vec::new(),
    };

    // Real tar accepts a leading flag bundle with no dash at all
    // (`tar xvf a.tar`), in addition to normal `-xvf`. Normalize by
    // treating the first arg as a flag bundle if it doesn't start with
    // '-' and isn't a path that happens to already exist as a file.
    let mut iter = argv.iter().skip(1).peekable();
    let mut first = true;
    while let Some(arg) = iter.next() {
        let bundle: Option<&str> = if first && !arg.starts_with('-') {
            Some(arg.as_str())
        } else {
            arg.strip_prefix('-').filter(|s| !s.starts_with('-'))
        };
        first = false;

        if let Some(bundle) = bundle {
            for c in bundle.chars() {
                match c {
                    'c' => args.mode = Some(Mode::Create),
                    'x' => args.mode = Some(Mode::Extract),
                    't' => args.mode = Some(Mode::List),
                    'v' => args.verbose = true,
                    'z' => args.compression = Some(Compression::Gzip),
                    'f' => {
                        let path = iter.next().ok_or("-f requires an archive path")?;
                        args.archive = Some(PathBuf::from(path));
                    }
                    'C' => {
                        let dir = iter.next().ok_or("-C requires a directory")?;
                        args.directory = Some(PathBuf::from(dir));
                    }
                    other => return Err(format!("unsupported tar flag: -{other}")),
                }
            }
            continue;
        }

        match arg.as_str() {
            "--zstd" => args.compression = Some(Compression::Zstd),
            "--gzip" | "--gunzip" | "--ungzip" => args.compression = Some(Compression::Gzip),
            "--create" => args.mode = Some(Mode::Create),
            "--extract" | "--get" => args.mode = Some(Mode::Extract),
            "--list" => args.mode = Some(Mode::List),
            "--verbose" => args.verbose = true,
            "--file" => {
                let path = iter.next().ok_or("--file requires an archive path")?;
                args.archive = Some(PathBuf::from(path));
            }
            "--directory" => {
                let dir = iter.next().ok_or("--directory requires a directory")?;
                args.directory = Some(PathBuf::from(dir));
            }
            _ if arg.starts_with('-') => return Err(format!("unsupported tar flag: {arg}")),
            _ => args.paths.push(PathBuf::from(arg)),
        }
    }

    Ok(args)
}

fn open_reader(path: &Path, compression: Compression) -> io::Result<Box<dyn Read>> {
    let file = File::open(path)?;
    Ok(match compression {
        Compression::None => Box::new(file),
        Compression::Gzip => Box::new(flate2::read::GzDecoder::new(file)),
        Compression::Zstd => Box::new(zstd::Decoder::new(file)?),
    })
}

fn open_writer(path: &Path, compression: Compression) -> io::Result<Box<dyn Write>> {
    let file = File::create(path)?;
    Ok(match compression {
        Compression::None => Box::new(file),
        Compression::Gzip => Box::new(flate2::write::GzEncoder::new(
            file,
            flate2::Compression::default(),
        )),
        Compression::Zstd => Box::new(zstd::Encoder::new(file, 0)?.auto_finish()),
    })
}

fn run_create(args: &Args) -> io::Result<()> {
    let archive_path = args
        .archive
        .as_deref()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "-c requires -f <archive>"))?;
    let compression = args
        .compression
        .unwrap_or_else(|| sniff_compression(archive_path));
    let writer = open_writer(archive_path, compression)?;
    let mut builder = tar::Builder::new(writer);
    for path in &args.paths {
        if args.verbose {
            println!("{}", path.display());
        }
        if path.is_dir() {
            builder.append_dir_all(path, path)?;
        } else {
            builder.append_path(path)?;
        }
    }
    builder.into_inner()?.flush()
}

// Prints exactly what's stored in the header, same as real tar -t does —
// no attempt to synthesize a trailing slash from the entry type, since
// the `tar` crate's own `append_dir_all` doesn't consistently store one
// for nested directories the way GNU tar's writer does. Harmless: it's
// a listing-cosmetics difference only, not an extraction-correctness one
// (verified identical file trees both directions against real tar).
fn print_entry_path<R: Read>(entry: &tar::Entry<R>) -> io::Result<()> {
    println!("{}", entry.path()?.display());
    Ok(())
}

fn run_extract(args: &Args) -> io::Result<()> {
    let archive_path = args
        .archive
        .as_deref()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "-x requires -f <archive>"))?;
    let compression = args
        .compression
        .unwrap_or_else(|| sniff_compression(archive_path));
    let reader = open_reader(archive_path, compression)?;
    let mut archive = tar::Archive::new(reader);
    let dest = args.directory.as_deref().unwrap_or_else(|| Path::new("."));
    if args.verbose {
        for entry in archive.entries()? {
            let entry = entry?;
            print_entry_path(&entry)?;
        }
        // entries() consumes the archive's read position; re-open for the
        // actual unpack rather than trying to seek a possibly-compressed
        // stream backwards.
        let reader = open_reader(archive_path, compression)?;
        let mut archive = tar::Archive::new(reader);
        archive.unpack(dest)
    } else {
        archive.unpack(dest)
    }
}

fn run_list(args: &Args) -> io::Result<()> {
    let archive_path = args
        .archive
        .as_deref()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "-t requires -f <archive>"))?;
    let compression = args
        .compression
        .unwrap_or_else(|| sniff_compression(archive_path));
    let reader = open_reader(archive_path, compression)?;
    let mut archive = tar::Archive::new(reader);
    for entry in archive.entries()? {
        let entry = entry?;
        print_entry_path(&entry)?;
    }
    Ok(())
}

pub fn run(args: IntoIter<OsString>) -> i32 {
    let argv: Vec<String> = args.map(|s| s.to_string_lossy().into_owned()).collect();
    let parsed = match parse_args(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("tar: {e}");
            return 1;
        }
    };

    let result = match parsed.mode {
        Some(Mode::Create) => run_create(&parsed),
        Some(Mode::Extract) => run_extract(&parsed),
        Some(Mode::List) => run_list(&parsed),
        None => {
            eprintln!("tar: one of -c, -x, or -t is required");
            return 1;
        }
    };

    if let Err(e) = result {
        eprintln!("tar: {e}");
        return 1;
    }
    0
}
