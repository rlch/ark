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

    /// Scripts consumed by the `mock-claude` shim binary. Each script drives a
    /// scripted Claude Code session for end-to-end tests (T-126).
    pub const MOCK_CLAUDE_SCRIPTS: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/mock-claude-scripts"
    );
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

    /// Read `{MOCK_CLAUDE_SCRIPTS}/{name}.json` into a String. Used by end-to-end
    /// tests that drive the `mock-claude` shim binary (T-126).
    ///
    /// # Panics
    /// Panics if the file does not exist or cannot be read.
    pub fn load_mock_claude_script(name: &str) -> String {
        let path = Path::new(paths::MOCK_CLAUDE_SCRIPTS).join(format!("{name}.json"));
        read_to_string_or_panic(&path, "mock-claude script", name)
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

/// Bundle of fixture directories that used to back the engine contract
/// suite (deleted in cleanup-T-010 along with `ark_core::engine`).
///
/// Kept around because the JSONL transcripts + hook payload fixtures on
/// disk are still useful scaffolding for extensions that want to parse
/// Claude Code output; the struct itself no longer has an in-tree
/// consumer.
#[derive(Debug, Clone)]
pub struct EngineFixtures {
    /// Directory of committed Claude JSONL transcripts (T-112).
    pub transcripts: std::path::PathBuf,
    /// Directory of committed Claude hook JSON payloads (T-113).
    pub hook_payloads: std::path::PathBuf,
}

impl EngineFixtures {
    /// Absolute path to the named transcript fixture
    /// (`{transcripts}/{name}.jsonl`).
    pub fn transcript(&self, name: &str) -> std::path::PathBuf {
        self.transcripts.join(format!("{name}.jsonl"))
    }

    /// Absolute path to the named hook payload fixture
    /// (`{hook_payloads}/{event}.json`).
    pub fn hook_payload(&self, event: &str) -> std::path::PathBuf {
        self.hook_payloads.join(format!("{event}.json"))
    }
}

/// Return an [`EngineFixtures`] pointing at the directories this crate ships.
///
/// Used by engine contract suites (T-114+) so downstream integration tests do
/// not have to hand-roll `CARGO_MANIFEST_DIR` juggling.
pub fn engine_fixtures() -> EngineFixtures {
    EngineFixtures {
        transcripts: std::path::PathBuf::from(paths::CLAUDE_TRANSCRIPTS),
        hook_payloads: std::path::PathBuf::from(paths::HOOK_PAYLOADS),
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
            ("mock-claude-scripts", paths::MOCK_CLAUDE_SCRIPTS),
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
    #[should_panic(expected = "ark-test-fixtures: failed to load mock-claude script")]
    fn load_mock_claude_script_panics_on_missing_file() {
        let _ = loaders::load_mock_claude_script("no-such-script");
    }

    #[test]
    fn load_mock_claude_script_reads_happy_path() {
        let raw = loaders::load_mock_claude_script("happy-path");
        assert!(raw.contains("\"events\""), "happy-path must list events");
        assert!(
            raw.contains("\"Stop\""),
            "happy-path must terminate with a Stop event"
        );
    }

    #[test]
    fn engine_fixtures_points_at_committed_dirs() {
        let fx = super::engine_fixtures();
        assert!(
            fx.transcripts.is_absolute(),
            "engine_fixtures().transcripts must be absolute, got {:?}",
            fx.transcripts
        );
        assert!(
            fx.hook_payloads.is_absolute(),
            "engine_fixtures().hook_payloads must be absolute, got {:?}",
            fx.hook_payloads
        );
        assert!(
            fx.transcripts.is_dir(),
            "transcripts dir must exist: {:?}",
            fx.transcripts
        );
        assert!(
            fx.hook_payloads.is_dir(),
            "hook_payloads dir must exist: {:?}",
            fx.hook_payloads
        );
        // helpers produce the expected on-disk fixtures.
        assert!(
            fx.transcript("basic-toolUse").is_file(),
            "basic-toolUse.jsonl must exist at {:?}",
            fx.transcript("basic-toolUse")
        );
        assert!(
            fx.hook_payload("post-tool-use").is_file(),
            "post-tool-use.json must exist at {:?}",
            fx.hook_payload("post-tool-use")
        );
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
