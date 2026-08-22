//! Pure classification tests for the logic relocated from the control
//! plane's `scan/discovery.rs`. The expectations here pin the control
//! plane's exact semantics so the move is behavior-preserving.

use super::*;

#[test]
fn default_media_set_covers_common_containers_case_insensitively() {
    assert!(is_supported_media_path(Path::new("movie.mkv")));
    assert!(is_supported_media_path(Path::new("home movie.MOV")));
    assert!(is_supported_media_path(Path::new("clip.webm")));
    assert!(!is_supported_media_path(Path::new("notes.txt")));
    assert!(!is_supported_media_path(Path::new("song.mp3")));
}

#[test]
fn empty_allowlist_falls_back_to_built_in_defaults() {
    // An empty allowlist means "scan the default media set", never "nothing".
    assert!(matches_media_extension(Path::new("a/film.mkv"), &[]));
    assert!(!matches_media_extension(Path::new("a/song.mp3"), &[]));
}

#[test]
fn non_empty_allowlist_restricts_primaries_case_insensitively() {
    let allowlist = vec!["MP3".to_owned(), "MkV".to_owned()];
    assert!(matches_media_extension(Path::new("a/film.mkv"), &allowlist));
    assert!(matches_media_extension(Path::new("a/song.mp3"), &allowlist));
    assert!(!matches_media_extension(
        Path::new("a/clip.avi"),
        &allowlist
    ));
}

#[test]
fn sidecar_classification_maps_extensions_to_roles() {
    assert_eq!(
        classify_sidecar(Path::new("movie.srt")),
        Some(SidecarKind::Subtitle)
    );
    assert_eq!(
        classify_sidecar(Path::new("movie.SRT")),
        Some(SidecarKind::Subtitle)
    );
    assert_eq!(
        classify_sidecar(Path::new("movie.nfo")),
        Some(SidecarKind::Nfo)
    );
    assert_eq!(
        classify_sidecar(Path::new("poster.jpg")),
        Some(SidecarKind::Poster)
    );
    assert_eq!(
        classify_sidecar(Path::new("poster.tbn")),
        Some(SidecarKind::Poster)
    );
    assert_eq!(classify_sidecar(Path::new("movie.mkv")), None);
    assert_eq!(classify_sidecar(Path::new("archive.zip")), None);
}

#[test]
fn trailer_suffix_rule_requires_dash_or_dot_suffix_on_media_extension() {
    assert_eq!(
        classify_sidecar(Path::new("film-trailer.mkv")),
        Some(SidecarKind::Trailer)
    );
    assert_eq!(
        classify_sidecar(Path::new("film.TRAILER.mp4")),
        Some(SidecarKind::Trailer)
    );
    // A bare stem of exactly "trailer" carries neither suffix separator.
    assert_eq!(classify_sidecar(Path::new("trailer.mkv")), None);
    // Media extension without the suffix stays primary.
    assert_eq!(classify_sidecar(Path::new("film.mkv")), None);
    // Trailer suffix on a non-media extension is not a sidecar at all.
    assert_eq!(classify_sidecar(Path::new("film-trailer.txt")), None);
}

#[test]
fn sidecar_roles_use_wire_vocabulary() {
    assert_eq!(SidecarKind::Subtitle.role(), "external_subtitle");
    assert_eq!(SidecarKind::Nfo.role(), "nfo");
    assert_eq!(SidecarKind::Poster.role(), "poster");
    assert_eq!(SidecarKind::Trailer.role(), "trailer");
}

#[test]
fn exact_stem_match_anchors_a_sidecar_to_its_primary() {
    let candidates = vec!["movie/movie.mkv".to_owned()];
    assert_eq!(
        best_sidecar_candidate(&candidates, "movie/movie.srt"),
        Some(0)
    );
}

#[test]
fn stem_prefix_matches_require_separator_and_win_by_longest_stem() {
    let candidates = vec!["root/movie.mkv".to_owned(), "root2/movie2.mkv".to_owned()];
    // "movie2.srt" prefixes only "movie2", and the '.'-separator rule keeps it
    // off plain "movie" — the control-plane regression that motivated
    // longest-stem matching in the first place.
    assert_eq!(
        best_sidecar_candidate(&candidates, "root2/movie2.srt"),
        Some(1)
    );
    // Language-tagged subtitles attach through a '.' separator...
    let tagged = vec!["dir/movie.mkv".to_owned()];
    assert_eq!(best_sidecar_candidate(&tagged, "dir/movie.en.srt"), Some(0));
    // ...and commentary tracks through a '-' separator.
    assert_eq!(
        best_sidecar_candidate(&tagged, "dir/movie-commentary.srt"),
        Some(0)
    );
    // An unrelated stem must not match at all.
    assert_eq!(best_sidecar_candidate(&tagged, "dir/unrelated.srt"), None);
}

#[test]
fn equal_length_matches_tie_break_deterministically() {
    let candidates = vec!["b/movie.mkv".to_owned(), "a/movie.mkv".to_owned()];
    // Both primaries tie on stem length; the lexicographically smaller
    // locator wins so grouping never depends on hash iteration order.
    assert_eq!(best_sidecar_candidate(&candidates, "c/movie.srt"), Some(1));
}

#[test]
fn unmatched_sidecar_yields_none_so_the_walker_can_count_it() {
    let candidates = vec!["dir/movie.mkv".to_owned()];
    assert_eq!(best_sidecar_candidate(&[], "dir/movie.srt"), None);
    assert_eq!(best_sidecar_candidate(&candidates, "dir/orphan.srt"), None);
}
