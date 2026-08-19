//! Lexical classification of a Path/Dir Target value that can never match a
//! real file or directory - decided purely from its own characters, never by
//! touching the filesystem.
//!
//! `serve/examples/stale_anchors.rs`'s read-only census already sorts a
//! NON-RESOLVING anchor into one of seven classes for a human report (`unc`,
//! `posix-absolute`, `windows-absolute`, `env-template`, `route-like`,
//! `not-a-path`, `relative-missing`), decided the identical lexical way -
//! never by normalising, never by touching the filesystem. This module owns
//! the narrower WRITE-TIME question `gate::check_target_binding` asks of
//! every Path/Dir Target value, whether it would ever resolve or not: can
//! this value, by its own shape, ever match a real file at all? `unmatchable`
//! is the one place that decision is made, so a census tool and a write gate
//! can never silently disagree about it - the same "exactly one definition"
//! doctrine `model/tests/single_can_fire_definition.rs` and `model/tests/
//! single_normalization.rs` already enforce elsewhere in this crate for
//! `Kind::can_fire` and `normalize::normalize_target`. `serve/examples/
//! stale_anchors.rs` cannot be edited from here (out of this change's scope
//! - see the task report this module was built for); it still carries its
//! own independent copy of the same env-template/route-like/extension
//! primitives today, and should be pointed at this module instead.
//!
//! Three shapes only - a deliberate SUBSET of the census's seven classes,
//! exactly the ones a write-time refusal needs:
//!   - `EnvTemplate` - an unexpanded `%NAME%`/`$NAME`/`${NAME}` placeholder,
//!     which is never how a real path is stored on disk.
//!   - `Route` - a single leading forward slash with no further separator,
//!     which on this platform is a web route, never a filesystem path.
//!   - `BareWord` - no path separator at all, and no file extension: a bare
//!     word, never a real file or directory.
//!
//! A UNC path, a POSIX-absolute path and a Windows-absolute path are never
//! refused here - see `unmatchable`'s own doc comment for exactly where
//! that line falls, and for the known, deliberate gaps this narrow a rule
//! leaves open.

/// Why a Path/Dir Target value can never match a real file or directory,
/// decided lexically. Returned by `unmatchable`, and named after the three
/// shapes that function's own doc comment describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnmatchableAnchor {
    /// Carries an unexpanded environment-variable template.
    EnvTemplate,
    /// A single leading slash with no further separator - a web route.
    Route,
    /// No path separator at all, and no file extension - a bare word.
    BareWord,
}

/// A name character inside an environment-variable placeholder: ASCII
/// letters, digits and underscore - the character set every real
/// `%NAME%`/`$NAME` environment variable this codebase uses is drawn from
/// (e.g. `%LOCALAPPDATA%`).
fn is_env_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// A `%NAME%` (Windows) style variable placeholder anywhere in `v`.
fn has_percent_template(v: &str) -> bool {
    let b = v.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let mut j = i + 1;
            while j < b.len() && is_env_name_byte(b[j]) {
                j += 1;
            }
            if j > i + 1 && j < b.len() && b[j] == b'%' {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// A `$NAME` / `${NAME}` (POSIX shell) style variable placeholder anywhere
/// in `v`.
fn has_dollar_template(v: &str) -> bool {
    let b = v.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'$' {
            let mut j = i + 1;
            let braced = j < b.len() && b[j] == b'{';
            if braced {
                j += 1;
            }
            let start = j;
            while j < b.len() && is_env_name_byte(b[j]) {
                j += 1;
            }
            let named = j > start;
            if named && (!braced || (j < b.len() && b[j] == b'}')) {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Contains a `%NAME%` or `$NAME`/`${NAME}` style variable placeholder
/// anywhere. Lexical only: this looks for the punctuation and a plausible
/// name shape, never for whether any such variable actually exists.
fn has_env_template(v: &str) -> bool {
    has_percent_template(v) || has_dollar_template(v)
}

/// A single leading slash with no further `/` or `\` after it, e.g.
/// `/admin`. A value with a FURTHER separator after the first slash
/// (`/etc/crontab`, `/admin/orders/:id`) is deliberately NOT this shape -
/// see `unmatchable`'s own doc comment for why that boundary is drawn here
/// and where it can be wrong, in both directions.
fn is_route_like(v: &str) -> bool {
    if is_a_root_directory(v) {
        return false;
    }
    match v.strip_prefix('/') {
        Some(rest) => !rest.contains('/') && !rest.contains('\\'),
        None => false,
    }
}

/// The handful of top-level directories that exist on every unix-shaped
/// machine, Git Bash on Windows included.
///
/// THE DEFECT THIS CLOSES, found 2026-08-19 while repairing a real rule: "do
/// not write intermediate files to /tmp" could not be anchored at `/tmp` at
/// all, because the route heuristic reads a single leading slash with nothing
/// after it as a web route and refuses it as a place. `/tmp` is not a route on
/// any machine this runs on; neither are the others below. A closed list, not
/// a guess: anything outside it keeps the old reading.
fn is_a_root_directory(v: &str) -> bool {
    const ROOTS: &[&str] = &[
        "/tmp", "/var", "/etc", "/usr", "/opt", "/home", "/root", "/srv", "/mnt", "/media", "/dev",
        "/proc", "/bin", "/sbin", "/lib",
    ];
    let trimmed = v.trim_end_matches('/');
    ROOTS.iter().any(|r| trimmed.eq_ignore_ascii_case(r))
}

/// The final path-separator-delimited segment carries a `.` that is neither
/// its first character (a dotfile like `.gitignore` is not "an extension")
/// nor its last (a trailing `.` with nothing after it is not one either).
fn has_extension(v: &str) -> bool {
    let last_segment = v.rsplit(|c| c == '/' || c == '\\').next().unwrap_or(v);
    match last_segment.rfind('.') {
        Some(0) | None => false,
        Some(pos) => pos + 1 < last_segment.len(),
    }
}

/// Whether `v` carries a `/` or `\` anywhere at all.
fn has_path_separator(v: &str) -> bool {
    v.contains('/') || v.contains('\\')
}

/// Whether `value` can ever match a real file or directory, decided purely
/// from its own characters. `None` means "maybe": nothing here proves it
/// CAN match either, only that none of the three shapes below applies -
/// `gate::check_target_binding` is the only caller, and only for a
/// `Path`/`Dir` Target value. Trims `value` itself, so a caller may pass a
/// raw or an already-trimmed value either way.
///
/// Checked in this fixed order, first match wins - the same priority
/// `serve/examples/stale_anchors.rs`'s own `classify` gives the equivalent
/// classes (env-template is checked well before route-like there too, so a
/// value that happens to fit both reads as the more specific reason):
///   1. `EnvTemplate` - see `has_env_template`.
///   2. `Route` - see `is_route_like`.
///   3. `BareWord` - no path separator anywhere AND no file extension.
///
/// NEVER refused by this function (see `gate::check_target_binding`'s own
/// ground 15, where this matters): a UNC path (`\\server\share` - no
/// leading forward slash, so never `Route`, and always carries a `\`
/// separator, so never `BareWord`); a Windows-absolute path (`C:\Users\...`
/// - same reasoning); a POSIX-absolute path WITH MORE THAN ONE SEGMENT
/// (`/etc/crontab`, `/srv/x` - the further separator after the leading
/// slash rules out `Route`, and the separator itself rules out `BareWord`).
///
/// KNOWN, DELIBERATE GAPS, in both directions - see the task report this
/// function was built for, which asked for exactly this trade-off rather
/// than a filesystem-perfect classifier:
///   - a single-segment POSIX-absolute path (`/etc`, `/tmp`, nothing after
///     it) is lexically identical to a single-segment route, so it IS
///     refused as `Route` - the same boundary `stale_anchors.rs` already
///     draws (its own `classify` sorts a bare `/etc` into `route-like` for
///     the identical reason: its `posix-absolute` class requires "at least
///     one further separator").
///   - a multi-segment web route (`/admin/orders/:id`) is lexically
///     identical to a multi-segment POSIX-absolute path, so it is NOT
///     refused at all - the same gap, the other direction.
///   - a bare, directory-unqualified real filename with no extension
///     (`LICENSE`, `Makefile`, `Dockerfile`) IS refused as `BareWord`,
///     indistinguishable here from a genuinely non-path bare word (`gate`,
///     a symbol name mistyped as a path) - the third shape is exactly as
///     literally specified, no narrower.
///   - a slash-joined list that is not a path at all but happens to carry
///     separators (a ticker list like `AAPL/MSFT/GOOG`) is NOT refused: it
///     has a path separator, so it fails the `BareWord` test, and it is
///     the one census "not-a-path" shape this module deliberately does not
///     cover (`stale_anchors.rs`'s own `not-a-path` class also matches this
///     via a second, ticker-specific heuristic this module does not port).
pub fn unmatchable(value: &str) -> Option<UnmatchableAnchor> {
    let v = value.trim();
    if has_env_template(v) {
        return Some(UnmatchableAnchor::EnvTemplate);
    }
    if is_route_like(v) {
        return Some(UnmatchableAnchor::Route);
    }
    if !has_path_separator(v) && !has_extension(v) {
        return Some(UnmatchableAnchor::BareWord);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------- EnvTemplate

    #[test]
    fn a_percent_style_env_template_is_unmatchable() {
        assert_eq!(unmatchable(r"%LOCALAPPDATA%\thor\guard-rulebook.json"), Some(UnmatchableAnchor::EnvTemplate));
    }

    /// A real directory that happens to look like a route. The rule this
    /// comes from - do not write intermediate files to /tmp - could not be
    /// anchored where it belongs until this exception existed.
    #[test]
    fn a_top_level_directory_is_a_place_not_a_route() {
        assert_eq!(unmatchable("/tmp"), None);
        assert_eq!(unmatchable("/var"), None);
        assert_eq!(unmatchable("/etc/"), None);
        // and everything else with that shape is still a route
        assert_eq!(unmatchable("/admin"), Some(UnmatchableAnchor::Route));
        assert_eq!(unmatchable("/preview"), Some(UnmatchableAnchor::Route));
    }

    #[test]
    fn a_dollar_style_env_template_is_unmatchable() {
        assert_eq!(unmatchable("$HOME/.config/thor/settings.json"), Some(UnmatchableAnchor::EnvTemplate));
    }

    #[test]
    fn a_braced_dollar_style_env_template_is_unmatchable() {
        assert_eq!(unmatchable("${HOME}/.config/thor/settings.json"), Some(UnmatchableAnchor::EnvTemplate));
    }

    #[test]
    fn an_env_template_takes_priority_over_a_route_like_shape() {
        // Checked in fixed order, env-template first: a value that could
        // fit both reads as the more specific reason, mirroring
        // stale_anchors.rs's own class order (env-template before
        // route-like).
        assert_eq!(unmatchable("/%VAR%"), Some(UnmatchableAnchor::EnvTemplate));
    }

    // ------------------------------------------------------------- Route

    #[test]
    fn a_single_segment_route_is_unmatchable() {
        assert_eq!(unmatchable("/admin"), Some(UnmatchableAnchor::Route));
    }

    #[test]
    fn a_bare_root_slash_is_unmatchable_as_a_route() {
        assert_eq!(unmatchable("/"), Some(UnmatchableAnchor::Route));
    }

    // --------------------------------------------------------- BareWord

    #[test]
    fn a_bare_word_with_no_extension_and_no_separator_is_unmatchable() {
        assert_eq!(unmatchable("gate"), Some(UnmatchableAnchor::BareWord));
    }

    #[test]
    fn a_bare_word_is_trimmed_before_classification() {
        assert_eq!(unmatchable("  gate  "), Some(UnmatchableAnchor::BareWord));
    }

    // -------------------------------------------- must still be accepted
    //
    // Requirement: a UNC path, a (multi-segment) POSIX-absolute path and a
    // Windows-absolute path are real paths that simply live elsewhere -
    // refusing them would be wrong, so `unmatchable` must return `None` for
    // every one of these.

    #[test]
    fn a_unc_path_is_not_unmatchable() {
        assert_eq!(unmatchable(r"\\fileserver\web\site\index.html"), None);
    }

    #[test]
    fn a_unc_path_to_an_extensionless_share_root_is_not_unmatchable() {
        assert_eq!(unmatchable(r"\\fileserver\web"), None);
    }

    #[test]
    fn a_multi_segment_posix_absolute_path_is_not_unmatchable() {
        assert_eq!(unmatchable("/etc/crontab"), None);
        assert_eq!(unmatchable("/srv/x"), None);
    }

    #[test]
    fn a_windows_absolute_path_is_not_unmatchable() {
        assert_eq!(unmatchable(r"C:\Users\other\dev\thor2\model\src\gate.rs"), None);
    }

    #[test]
    fn a_windows_absolute_directory_with_no_extension_is_not_unmatchable() {
        assert_eq!(unmatchable(r"C:\Users\other\dev\thor2"), None);
    }

    // -------------------------------------------------- ordinary relative

    #[test]
    fn a_relative_path_with_an_extension_is_not_unmatchable() {
        assert_eq!(unmatchable("model/src/gate.rs"), None);
    }

    #[test]
    fn a_relative_directory_with_a_separator_but_no_extension_is_not_unmatchable() {
        assert_eq!(unmatchable("model/src"), None);
    }

    #[test]
    fn a_bare_filename_with_an_extension_and_no_directory_is_not_unmatchable() {
        assert_eq!(unmatchable("gate.rs"), None);
    }

    // ------------------------------------------------------- known gaps
    //
    // Documented, not celebrated: these pin the exact boundary described in
    // `unmatchable`'s own doc comment, so a future change to this function
    // has to notice it is moving that boundary, not drift into it by
    // accident.

    #[test]
    fn a_single_segment_posix_absolute_path_outside_the_known_roots_is_still_a_route() {
        // The gap this used to pin was total: every single-segment absolute
        // path, "/etc" and "/tmp" included, was refused as a route, so a rule
        // about the temp directory could not be anchored at the temp
        // directory. Since 2026-08-19 the handful of directories that exist on
        // every unix-shaped machine are read as places (see
        // is_a_root_directory); everything else keeps the old reading, because
        // "/workspace" really is lexically indistinguishable from a route.
        assert_eq!(unmatchable("/etc"), None);
        assert_eq!(unmatchable("/workspace"), Some(UnmatchableAnchor::Route));
    }

    #[test]
    fn a_multi_segment_web_route_is_a_known_gap_not_caught() {
        // "/admin/orders/:id" is a real web route, but it has a further
        // separator after the leading slash, so it reads exactly like a
        // multi-segment POSIX-absolute path and is NOT refused.
        assert_eq!(unmatchable("/admin/orders/:id"), None);
    }

    #[test]
    fn a_bare_extensionless_real_filename_is_a_known_tradeoff_refused_as_a_bare_word() {
        // LICENSE/Makefile/Dockerfile are real, common, extensionless
        // filenames - indistinguishable here from a genuinely non-path bare
        // word with no directory qualifier. Shape 3 is exactly as
        // literally specified (no path separator AND no extension), so
        // this is refused, not narrowed to exclude known real filenames.
        assert_eq!(unmatchable("LICENSE"), Some(UnmatchableAnchor::BareWord));
        assert_eq!(unmatchable("Makefile"), Some(UnmatchableAnchor::BareWord));
    }

    #[test]
    fn a_ticker_list_with_separators_is_a_known_gap_not_caught() {
        // A slash-joined list of stock tickers has a path separator, so it
        // fails the BareWord test even though it is not a path either -
        // the one stale_anchors.rs "not-a-path" shape this module does not
        // cover (that classifier's own ticker-specific heuristic was not
        // ported here).
        assert_eq!(unmatchable("AAPL/MSFT/GOOG"), None);
    }
}
