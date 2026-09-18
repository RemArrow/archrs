//! `curl`, built directly on `ureq` — the same HTTP client already
//! proven elsewhere in this workspace (`alpm-rs` fetching real package
//! archives from Arch mirrors, `makepkg-rs` downloading PKGBUILD
//! sources). No real curl-CLI-compatible crate exists to vendor
//! wholesale without pulling in a lot of unrelated scope (the closest
//! candidate bundles BitTorrent and SSH support), so this is our own
//! thin CLI layer over `ureq` — same pattern as `tar_cmd.rs`/
//! `gzip_cmd.rs`, not a from-scratch HTTP client.
//!
//! Scope: GET (default), `-X METHOD`, `-o FILE`/`-O` (write to a file
//! instead of stdout), `-I` (headers only), `-H "Name: value"`
//! (repeatable), `-d DATA`/`--data`/`--data-binary` (request body,
//! implies POST if no `-X` given), `-A` (User-Agent), `-L` (accepted;
//! `ureq` already follows redirects by default, so this is a no-op),
//! `-s`/`--silent` and `-f`/`--fail` (exit non-zero on an HTTP error
//! status instead of printing the error body). Not implemented:
//! `--data-urlencode`, cookies, `.netrc`, HTTP/2, client certificates.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Write};
use std::vec::IntoIter;

struct Args {
    url: Option<String>,
    method: Option<String>,
    output: Option<String>,
    remote_name: bool,
    headers_only: bool,
    headers: Vec<(String, String)>,
    body: Option<String>,
    user_agent: Option<String>,
    fail_on_error: bool,
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut args = Args {
        url: None,
        method: None,
        output: None,
        remote_name: false,
        headers_only: false,
        headers: Vec::new(),
        body: None,
        user_agent: None,
        fail_on_error: false,
    };

    let mut iter = argv.iter().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-X" | "--request" => {
                args.method = Some(iter.next().ok_or("-X requires a method")?.clone());
            }
            "-o" | "--output" => {
                args.output = Some(iter.next().ok_or("-o requires a filename")?.clone());
            }
            "-O" | "--remote-name" => args.remote_name = true,
            "-I" | "--head" => args.headers_only = true,
            "-H" | "--header" => {
                let raw = iter.next().ok_or("-H requires a header")?;
                let (name, value) = raw.split_once(':').ok_or("header must be 'Name: value'")?;
                args.headers
                    .push((name.trim().to_string(), value.trim().to_string()));
            }
            "-d" | "--data" | "--data-binary" | "--data-raw" => {
                args.body = Some(iter.next().ok_or("-d requires data")?.clone());
            }
            "-A" | "--user-agent" => {
                args.user_agent = Some(iter.next().ok_or("-A requires a value")?.clone());
            }
            "-L" | "--location" | "-s" | "--silent" | "-v" | "--verbose" => {} // accepted, see module docs
            "-f" | "--fail" => args.fail_on_error = true,
            _ if arg.starts_with('-') => return Err(format!("unsupported curl flag: {arg}")),
            _ => args.url = Some(arg.clone()),
        }
    }

    Ok(args)
}

fn remote_filename(url: &str) -> String {
    let without_query = url.split(['?', '#']).next().unwrap_or(url);
    without_query
        .rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or("index.html")
        .to_string()
}

pub fn run(args: IntoIter<OsString>) -> i32 {
    let argv: Vec<String> = args.map(|s| s.to_string_lossy().into_owned()).collect();
    let parsed = match parse_args(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("curl: {e}");
            return 2;
        }
    };
    let Some(url) = parsed.url.clone() else {
        eprintln!("curl: no URL given");
        return 2;
    };

    let method = parsed.method.clone().unwrap_or_else(|| {
        if parsed.headers_only {
            "HEAD".into()
        } else if parsed.body.is_some() {
            "POST".into()
        } else {
            "GET".into()
        }
    });

    let mut request = ureq::request(&method, &url);
    for (name, value) in &parsed.headers {
        request = request.set(name, value);
    }
    if let Some(ua) = &parsed.user_agent {
        request = request.set("User-Agent", ua);
    }

    let result = match &parsed.body {
        Some(body) => request.send_string(body),
        None => request.call(),
    };

    let response = match result {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            if parsed.fail_on_error {
                eprintln!("curl: HTTP {code}");
                return 22;
            }
            r
        }
        Err(e) => {
            eprintln!("curl: {e}");
            return 1;
        }
    };

    if parsed.headers_only {
        println!("HTTP/1.1 {} {}", response.status(), response.status_text());
        for name in response.headers_names() {
            if let Some(value) = response.header(&name) {
                println!("{name}: {value}");
            }
        }
        return 0;
    }

    let dest_name = if parsed.remote_name {
        Some(remote_filename(&url))
    } else {
        parsed.output.clone()
    };

    let write_result = match &dest_name {
        Some(path) => File::create(path)
            .and_then(|mut f| io::copy(&mut response.into_reader(), &mut f).map(|_| ())),
        None => {
            let stdout = io::stdout();
            let mut lock = stdout.lock();
            io::copy(&mut response.into_reader(), &mut lock).map(|_| ())
        }
    };

    if let Err(e) = write_result {
        eprintln!("curl: {e}");
        return 1;
    }
    let _ = io::stdout().flush();
    0
}
