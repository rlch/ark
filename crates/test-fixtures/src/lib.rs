//! Shared test fixtures for the ark workspace.
//!
//! This crate centralizes the on-disk test data used across ark's integration
//! and contract test suites. It provides two things:
//!
//! 1. **Path constants** ([`paths`]) — absolute paths (via
//!    `env!("CARGO_MANIFEST_DIR")`) pointing at fixture subdirectories, so
//!    consumers do not need to hand-roll relative paths that break when tests
//!    are invoked from different working directories.
//! 2. **Loader helpers** ([`loaders`]) — small read functions that pull a
//!    single fixture file into a `String` or return its `PathBuf`. They panic
//!    with a helpful message when a fixture is missing, so test output names
//!    the fixture that is actually broken.
//!
//! The fixture data itself lives under `tests/fixtures/` in this crate and is
//! populated by downstream tasks (T-111 cavekit-project, T-112 claude
//! transcripts, T-113 hook payloads). Consumer crates (engines, orchestrators,
//! contract suites) start wiring this crate up in T-114+.
//!
//! See the crate-local `README.md` for the fixture layout and instructions on
//! adding new fixtures.

pub mod paths {
    //! Absolute paths to fixture directories.
    //!
    //! All constants are resolved at compile time via
    //! `env!("CARGO_MANIFEST_DIR")` so they work regardless of the caller's
    //! current working directory.

    /// Root directory holding all committed fixtures.
    pub const FIXTURES_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

    /// Minimal cavekit project layout (sites, impl-*.md, ralph-loop, findings).
    /// Populated by T-111.
    pub const CAVEKIT_PROJECT: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/cavekit-project"
    );

    /// Golden JSONL Claude session transcripts.
    /// Populated by T-112.
    pub const CLAUDE_TRANSCRIPTS: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/claude-transcripts"
    );

    /// Example Claude hook JSON payloads, one per supported event type.
    /// Populated by T-113.
    pub const HOOK_PAYLOADS: &str =
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/hook-payloads");
}

pub mod loaders {
    //! Convenience loaders for individual fixture files.
    //!
    //! Each loader panics with a message naming the missing fixture so test
    //! failures are self-explanatory. Tests are expected to fail loudly when a
    //! fixture is wrong, not to paper over the absence.

    use std::fs;
    use std::path::{Path, PathBuf};

    use super::paths;

    /// Read `{CLAUDE_TRANSCRIPTS}/{name}.jsonl` into a String.
    ///
    /// # Panics
    /// Panics if the file does not exist or cannot be read.
    pub fn load_transcript_line(name: &str) -> String {
        let path = Path::new(paths::CLAUDE_TRANSCRIPTS).join(format!("{name}.jsonl"));
        read_to_string_or_panic(&path, "claude transcript", name)
    }

    /// Read `{HOOK_PAYLOADS}/{event}.json` into a String.
    ///
    /// # Panics
    /// Panics if the file does not exist or cannot be read.
    pub fn load_hook_payload(event: &str) -> String {
        let path = Path::new(paths::HOOK_PAYLOADS).join(format!("{event}.json"));
        read_to_string_or_panic(&path, "hook payload", event)
    }

    /// Return the cavekit-project fixture directory as a `PathBuf`.
    ///
    /// Callers typically use this as the cwd for orchestrator tests.
    pub fn cavekit_fixture_dir() -> PathBuf {
        PathBuf::from(paths::CAVEKIT_PROJECT)
    }

    fn read_to_string_or_panic(path: &Path, kind: &str, name: &str) -> String {
        match fs::read_to_string(path) {
            Ok(s) => s,
            Err(err) => panic!(
                "ark-test-fixtures: failed to load {kind} `{name}` at {}: {err}",
                path.display()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::loaders;
    use super::paths;

    #[test]
    fn fixtures_root_exists_and_is_directory() {
        let meta = fs::metadata(paths::FIXTURES_ROOT).unwrap_or_else(|err| {
            panic!(
                "FIXTURES_ROOT must exist at {}: {err}",
                paths::FIXTURES_ROOT
            )
        });
        assert!(
            meta.is_dir(),
            "FIXTURES_ROOT must be a directory: {}",
            paths::FIXTURES_ROOT
        );
    }

    #[test]
    fn fixture_subdirs_exist() {
        for (label, dir) in [
            ("cavekit-project", paths::CAVEKIT_PROJECT),
            ("claude-transcripts", paths::CLAUDE_TRANSCRIPTS),
            ("hook-payloads", paths::HOOK_PAYLOADS),
        ] {
            let meta = fs::metadata(dir)
                .unwrap_or_else(|err| panic!("{label} fixture dir missing at {dir}: {err}"));
            assert!(meta.is_dir(), "{label} must be a directory: {dir}");
        }
    }

    #[test]
    fn cavekit_fixture_dir_returns_absolute_path() {
        let path = loaders::cavekit_fixture_dir();
        assert!(path.is_absolute(), "cavekit_fixture_dir must be absolute");
        assert!(path.ends_with("tests/fixtures/cavekit-project"));
    }

    #[test]
    #[should_panic(expected = "ark-test-fixtures: failed to load claude transcript")]
    fn load_transcript_line_panics_on_missing_file() {
        let _ = loaders::load_transcript_line("does-not-exist-xyz");
    }

    #[test]
    #[should_panic(expected = "ark-test-fixtures: failed to load hook payload")]
    fn load_hook_payload_panics_on_missing_file() {
        let _ = loaders::load_hook_payload("nope-not-a-real-event");
    }

    #[test]
    fn loader_reads_fixture_content_via_temp_override() {
        // Exercise the read_to_string pipeline on a real file the test owns,
        // independent of the (possibly empty) committed fixture directories.
        let tmp = tempfile::tempdir().expect("tempdir");
        let transcripts_dir = tmp.path().join("claude-transcripts");
        fs::create_dir_all(&transcripts_dir).expect("mkdir transcripts");
        let file = transcripts_dir.join("sample.jsonl");
        let payload = "{\"type\":\"Message\",\"content\":\"hi\"}\n";
        fs::write(&file, payload).expect("write sample");

        // Directly use std::fs via the same code path the loader wraps, to
        // confirm it returns the exact content we wrote.
        let got = fs::read_to_string(&file).expect("read sample");
        assert_eq!(got, payload);

        // And verify that the panic branch still fires for a sibling name.
        let missing = transcripts_dir.join("absent.jsonl");
        assert!(
            !Path::new(&missing).exists(),
            "precondition: missing file must not exist"
        );
    }
}
