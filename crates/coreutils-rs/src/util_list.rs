// Included verbatim by both `main.rs` (for dispatch) and `build.rs` (for
// locating each vendored uu_* crate's `locales/` directory), so the two
// can't drift out of sync with each other.
const UTILS: &[(&str, &str)] = &[
    ("ls", "uu_ls"),
    ("cat", "uu_cat"),
    ("cp", "uu_cp"),
    ("mv", "uu_mv"),
    ("rm", "uu_rm"),
    ("mkdir", "uu_mkdir"),
    ("echo", "uu_echo"),
    ("pwd", "uu_pwd"),
    ("touch", "uu_touch"),
    ("wc", "uu_wc"),
    ("head", "uu_head"),
    ("tail", "uu_tail"),
    ("true", "uu_true"),
    ("false", "uu_false"),
    ("chmod", "uu_chmod"),
    ("chown", "uu_chown"),
    ("chgrp", "uu_chgrp"),
    ("ln", "uu_ln"),
    ("rmdir", "uu_rmdir"),
    ("mkfifo", "uu_mkfifo"),
    ("mknod", "uu_mknod"),
    ("du", "uu_du"),
    ("df", "uu_df"),
    ("sort", "uu_sort"),
    ("uniq", "uu_uniq"),
    ("cut", "uu_cut"),
    ("tr", "uu_tr"),
    ("tee", "uu_tee"),
    ("dd", "uu_dd"),
    ("dirname", "uu_dirname"),
    ("basename", "uu_basename"),
    ("realpath", "uu_realpath"),
    ("readlink", "uu_readlink"),
    ("sync", "uu_sync"),
    ("sleep", "uu_sleep"),
    ("date", "uu_date"),
    ("id", "uu_id"),
    ("whoami", "uu_whoami"),
    ("who", "uu_who"),
    ("uname", "uu_uname"),
    ("env", "uu_env"),
    ("printf", "uu_printf"),
    ("seq", "uu_seq"),
    ("shuf", "uu_shuf"),
    ("split", "uu_split"),
    ("join", "uu_join"),
    ("paste", "uu_paste"),
    ("comm", "uu_comm"),
    ("expand", "uu_expand"),
    ("unexpand", "uu_unexpand"),
    ("fold", "uu_fold"),
    ("fmt", "uu_fmt"),
    ("nl", "uu_nl"),
    ("od", "uu_od"),
    ("base64", "uu_base64"),
    ("base32", "uu_base32"),
    ("md5sum", "uu_md5sum"),
    ("sha1sum", "uu_sha1sum"),
    ("sha256sum", "uu_sha256sum"),
    ("sha512sum", "uu_sha512sum"),
    ("mktemp", "uu_mktemp"),
    ("install", "uu_install"),
    ("stat", "uu_stat"),
    ("test", "uu_test"),
    ("[", "uu_test"),
    ("expr", "uu_expr"),
    ("yes", "uu_yes"),
    ("nice", "uu_nice"),
    ("nohup", "uu_nohup"),
    ("timeout", "uu_timeout"),
    ("kill", "uu_kill"),
    ("factor", "uu_factor"),
    ("numfmt", "uu_numfmt"),
    ("tsort", "uu_tsort"),
    ("csplit", "uu_csplit"),
    ("shred", "uu_shred"),
    ("link", "uu_link"),
    ("unlink", "uu_unlink"),
    ("vdir", "uu_vdir"),
    ("dir", "uu_dir"),
    ("dircolors", "uu_dircolors"),
    ("groups", "uu_groups"),
    ("logname", "uu_logname"),
    ("tty", "uu_tty"),
    ("users", "uu_users"),
    ("stdbuf", "uu_stdbuf"),
    ("hostid", "uu_hostid"),
    ("arch", "uu_arch"),
    ("nproc", "uu_nproc"),
    ("printenv", "uu_printenv"),
    ("pathchk", "uu_pathchk"),
    ("pinky", "uu_pinky"),
    ("sum", "uu_sum"),
    ("cksum", "uu_cksum"),
    // Not coreutils — GNU findutils, a separate upstream project, also
    // with its own official uutils Rust port (the `findutils` crate,
    // vendored the same way as the `uu_*` crates above). No locales dir
    // to bundle for these; findutils doesn't use uucore's Fluent i18n.
    ("find", "findutils"),
    ("xargs", "findutils"),
    ("locate", "findutils"),
    ("updatedb", "findutils"),
    // Not coreutils, not uutils — GNU tar and GNU gzip are separate
    // upstream projects with no Rust port to vendor, so these are our
    // own thin CLIs over the tar/flate2/zstd crates (see tar_cmd.rs and
    // gzip_cmd.rs for what's in and out of scope).
    ("tar", "archrs-native"),
    ("gzip", "archrs-native"),
    ("gunzip", "archrs-native"),
    ("zcat", "archrs-native"),
    // GNU grep is also its own separate upstream project. No `uu_grep`
    // to vendor, but ripgrep's own search-engine libraries are — see
    // grep_cmd.rs for the CLI glue built on top of them.
    ("grep", "archrs-native"),
    // GNU sed is likewise separate, with no Rust port of its scripting
    // language to draw on at all — see sed_cmd.rs for the explicitly
    // scoped-down subset implemented here.
    ("sed", "archrs-native"),
    // less is also its own separate project; vendors the `minus`
    // terminal-pager crate instead — see less_cmd.rs.
    ("less", "archrs-native"),
    // Phase 4 (ROADMAP.md): a POSIX/bash-compatible shell, vendored from
    // `brush-shell` rather than hand-writing one — unlike everything
    // above, this reads real process argv itself instead of taking an
    // args iterator, so main.rs special-cases dispatch for these two.
    ("sh", "brush-shell"),
    ("bash", "brush-shell"),
    // Rest of "build tooling" (ROADMAP.md): small, well-scoped
    // vendor targets for the remaining base-devel-adjacent utilities
    // PKGBUILDs commonly need.
    ("which", "archrs-native"),
    ("patch", "archrs-native"),
    // Not base-devel-specific, just a real gap in everyday userland
    // coverage: AWK is its own full programming language, so this
    // vendors `awk-rs` (a from-scratch AWK lexer/parser/interpreter)
    // rather than hand-rolling one.
    ("awk", "archrs-native"),
];
