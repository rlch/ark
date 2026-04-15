//! Shared ratatui application scaffold for `ark pane` subcommands.
//!
//! See `context/kits/cavekit-pane-commands.md` R4:
//! - `NO_COLOR` honored (widgets read the global flag and choose grayscale)
//! - tokio runtime + crossterm backend
//! - Ctrl+C graceful shutdown, terminal restored without corruption
//! - errors surfaced via `tracing` to stderr
//!
//! # Usage
//!
//! Each pane command (diff/git/log) owns its state `S`, a render closure
//! `R`, and an event handler `H`. The event loop multiplexes crossterm
//! key/resize events, a 100ms tick, and custom events injected by the
//! caller (e.g. notify file-watch events for the diff pane).

use std::io;
use std::sync::OnceLock;
use std::time::Duration;

use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent,
        KeyModifiers,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures::StreamExt;
use ratatui::{Frame, Terminal, backend::CrosstermBackend};
use tokio::sync::mpsc;

/// Terminal handle used by pane commands (crossterm backend over stdout).
pub type Tui = Terminal<CrosstermBackend<io::Stdout>>;

/// Global flag set once at `run_pane` startup reflecting whether the
/// `NO_COLOR` env var was set (see <https://no-color.org>). Widgets read
/// this via [`no_color`] to pick grayscale styling instead of colored.
static NO_COLOR_SET: OnceLock<bool> = OnceLock::new();

/// Returns whether `NO_COLOR` was detected at pane startup. When called
/// before `run_pane` has initialized the flag, defaults to `false`.
pub fn no_color() -> bool {
    *NO_COLOR_SET.get().unwrap_or(&false)
}

/// Pure helper: returns `true` when the env getter yields any non-empty
/// value for `NO_COLOR` (per the NO_COLOR convention, any set value
/// disables color).
pub fn no_color_from_env<F>(getter: F) -> bool
where
    F: Fn(&str) -> Option<String>,
{
    matches!(getter("NO_COLOR"), Some(v) if !v.is_empty())
}

/// Pure helper: returns `true` when the key event is `Ctrl+C`.
pub fn is_ctrl_c(ev: &KeyEvent) -> bool {
    matches!(ev.code, KeyCode::Char('c') | KeyCode::Char('C'))
        && ev.modifiers.contains(KeyModifiers::CONTROL)
}

/// Events delivered to a pane's event handler.
pub enum PaneEvent {
    /// Keyboard input from the terminal.
    Key(KeyEvent),
    /// Terminal resized to `(width, height)`. ratatui autosizes on the next
    /// draw; widgets typically only need this to re-layout scroll state.
    Resize(u16, u16),
    /// Periodic tick (~100ms) so widgets can refresh derived state.
    Tick,
    /// Custom event injected by the pane (e.g. file-watch notifications).
    Custom(Box<dyn std::any::Any + Send>),
}

/// Control-flow signal returned by an event handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneFlow {
    /// Keep running the event loop.
    Continue,
    /// Exit the event loop cleanly (terminal will be restored by the guard).
    Quit,
}

/// RAII guard for the terminal's raw mode + alternate screen + mouse capture.
///
/// Created via [`TerminalGuard::enter`]; the drop impl always restores the
/// terminal, even on panic unwinding. Paired with a panic hook in
/// [`run_pane`] so ratatui panics don't leave the terminal corrupted.
pub struct TerminalGuard {
    _nothing: (),
}

impl TerminalGuard {
    /// Enters raw mode, alt-screen, and mouse capture; returns the guard
    /// plus an initialized [`Tui`]. On drop the terminal is restored.
    pub fn enter() -> io::Result<(Self, Tui)> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok((Self { _nothing: () }, terminal))
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
    }
}

/// Install a panic hook that restores the terminal before the previous
/// hook runs (so backtraces still print, but to a sane terminal). Idempotent.
fn install_panic_hook() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
            prev(info);
        }));
    });
}

/// The shared pane event loop.
///
/// Takes an initial state, a render closure, and an event handler. Runs
/// until the handler returns [`PaneFlow::Quit`] or the event stream ends.
/// Ctrl+C is intercepted and mapped to `Quit` before the handler sees it
/// (R4: graceful Ctrl+C shutdown).
///
/// # Events
/// - Crossterm key/resize via async `EventStream` (tokio-native).
/// - 100ms ticks via `tokio::time::interval`.
/// - Custom events injected through the returned channel... see
///   [`PaneEventInjector`] once a future patch wires that plumbing; for
///   T-039 the loop only consumes key/resize/tick.
pub async fn run_pane<S, R, H>(
    mut state: S,
    mut render: R,
    mut handle_event: H,
) -> anyhow::Result<()>
where
    S: Send + 'static,
    R: FnMut(&mut Frame, &S) + Send + 'static,
    H: FnMut(&mut S, PaneEvent) -> PaneFlow + Send + 'static,
{
    // NO_COLOR detection (R4). Widgets read via `no_color()`.
    let _ = NO_COLOR_SET.set(no_color_from_env(|k| std::env::var(k).ok()));

    install_panic_hook();

    let (_guard, mut terminal) =
        TerminalGuard::enter().map_err(|e| anyhow::anyhow!("terminal setup failed: {e}"))?;

    // Initial draw so the pane shows something immediately.
    terminal.draw(|f| render(f, &state))?;

    let mut events = EventStream::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        let pane_event = tokio::select! {
            maybe = events.next() => match maybe {
                Some(Ok(Event::Key(k))) => {
                    if is_ctrl_c(&k) {
                        tracing::debug!("ctrl+c received, exiting pane");
                        return Ok(());
                    }
                    PaneEvent::Key(k)
                }
                Some(Ok(Event::Resize(w, h))) => PaneEvent::Resize(w, h),
                Some(Ok(_other)) => continue, // mouse, focus, paste — ignored for v1
                Some(Err(e)) => {
                    tracing::error!("event stream error: {e}");
                    return Err(anyhow::anyhow!("crossterm event error: {e}"));
                }
                None => {
                    tracing::debug!("event stream ended");
                    return Ok(());
                }
            },
            _ = ticker.tick() => PaneEvent::Tick,
        };

        match handle_event(&mut state, pane_event) {
            PaneFlow::Continue => {}
            PaneFlow::Quit => return Ok(()),
        }

        terminal.draw(|f| render(f, &state))?;
    }
}

/// Re-exported so pane callers can construct custom channels.
///
/// Kept for forward-compat with T-040 (file-watch events → `PaneEvent::Custom`).
pub type CustomEventSender = mpsc::UnboundedSender<Box<dyn std::any::Any + Send>>;

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn pane_flow_variants_match() {
        let cont = PaneFlow::Continue;
        let quit = PaneFlow::Quit;
        assert!(matches!(cont, PaneFlow::Continue));
        assert!(matches!(quit, PaneFlow::Quit));
        assert_ne!(cont, quit);
    }

    #[test]
    fn is_ctrl_c_detects_lower_and_upper() {
        assert!(is_ctrl_c(&key(KeyCode::Char('c'), KeyModifiers::CONTROL)));
        assert!(is_ctrl_c(&key(KeyCode::Char('C'), KeyModifiers::CONTROL)));
    }

    #[test]
    fn is_ctrl_c_rejects_plain_c() {
        assert!(!is_ctrl_c(&key(KeyCode::Char('c'), KeyModifiers::NONE)));
        assert!(!is_ctrl_c(&key(KeyCode::Char('c'), KeyModifiers::SHIFT)));
    }

    #[test]
    fn is_ctrl_c_rejects_other_ctrl_keys() {
        assert!(!is_ctrl_c(&key(KeyCode::Char('d'), KeyModifiers::CONTROL)));
        assert!(!is_ctrl_c(&key(KeyCode::Esc, KeyModifiers::CONTROL)));
    }

    #[test]
    fn no_color_env_set_nonempty_is_true() {
        assert!(no_color_from_env(|k| if k == "NO_COLOR" {
            Some("1".to_string())
        } else {
            None
        }));
    }

    #[test]
    fn no_color_env_unset_is_false() {
        assert!(!no_color_from_env(|_| None));
    }

    #[test]
    fn no_color_env_empty_is_false() {
        // Per NO_COLOR spec only non-empty values disable color.
        assert!(!no_color_from_env(|k| if k == "NO_COLOR" {
            Some(String::new())
        } else {
            None
        }));
    }

    #[test]
    fn no_color_env_any_nonempty_value_disables_color() {
        // NO_COLOR spec: *any* non-empty value (including "0" or "false")
        // disables color. Widgets must not parse the value.
        for val in ["0", "false", "no", "true", " ", "anything"] {
            let v = val.to_string();
            assert!(
                no_color_from_env(|k| if k == "NO_COLOR" {
                    Some(v.clone())
                } else {
                    None
                }),
                "value {val:?} should disable color"
            );
        }
    }

    #[test]
    fn is_ctrl_c_with_additional_modifiers_still_true() {
        // CTRL+SHIFT+c or CTRL+ALT+c must still be recognized — predicate only
        // requires CONTROL to be present, so extra modifiers don't break it.
        let mods = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert!(is_ctrl_c(&key(KeyCode::Char('c'), mods)));
        let mods = KeyModifiers::CONTROL | KeyModifiers::ALT;
        assert!(is_ctrl_c(&key(KeyCode::Char('C'), mods)));
    }

    #[test]
    fn pane_event_custom_wraps_arbitrary_payload() {
        // Ensures Box<dyn Any + Send> round-trips through PaneEvent::Custom —
        // the pattern pane handlers use to dispatch typed custom messages.
        let payload: Box<dyn std::any::Any + Send> = Box::new(42u32);
        let ev = PaneEvent::Custom(payload);
        match ev {
            PaneEvent::Custom(p) => {
                let n = p.downcast::<u32>().expect("downcast to u32");
                assert_eq!(*n, 42);
            }
            _ => panic!("expected Custom variant"),
        }
    }
}
