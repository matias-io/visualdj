//! Picking the output monitor from configuration, with a safe fallback.
use crate::config::MonitorChoice;

/// What we know about a monitor, independent of winit so the choice is unit-testable.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorInfo {
    pub name: String,
    pub is_primary: bool,
    pub size: (u32, u32),
    pub scale: f64,
}

/// Index of the monitor to use and whether we had to fall back to the primary because the
/// requested one is missing (unplugged, renamed, out of range). Never panics; with no
/// monitors at all it returns `(0, true)` and the caller decides.
pub fn choose(available: &[MonitorInfo], choice: &MonitorChoice) -> (usize, bool) {
    let primary = available.iter().position(|m| m.is_primary).unwrap_or(0);
    match choice {
        MonitorChoice::Primary => (primary, available.is_empty()),
        MonitorChoice::Index(i) if *i < available.len() => (*i, false),
        MonitorChoice::NameContains(needle) => {
            let needle = needle.to_lowercase();
            available
                .iter()
                .position(|m| m.name.to_lowercase().contains(&needle))
                .map_or((primary, true), |i| (i, false))
        }
        MonitorChoice::Index(_) => (primary, true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitors() -> Vec<MonitorInfo> {
        vec![
            MonitorInfo {
                name: r"\\.\DISPLAY1".into(),
                is_primary: true,
                size: (2400, 1600),
                scale: 1.5,
            },
            MonitorInfo {
                name: r"\\.\DISPLAY2".into(),
                is_primary: false,
                size: (1920, 1080),
                scale: 1.0,
            },
        ]
    }

    #[test]
    fn primary_by_default() {
        assert_eq!(choose(&monitors(), &MonitorChoice::Primary), (0, false));
    }

    #[test]
    fn index_selects_that_monitor() {
        assert_eq!(choose(&monitors(), &MonitorChoice::Index(1)), (1, false));
    }

    #[test]
    fn monitor_selection_falls_back() {
        // An unplugged or out-of-range monitor must not panic; use the primary and say so.
        assert_eq!(choose(&monitors(), &MonitorChoice::Index(7)), (0, true));
        assert_eq!(
            choose(
                &monitors(),
                &MonitorChoice::NameContains("PROJECTOR".into())
            ),
            (0, true)
        );
    }

    #[test]
    fn name_match_is_case_insensitive() {
        assert_eq!(
            choose(&monitors(), &MonitorChoice::NameContains("display2".into())),
            (1, false)
        );
    }

    #[test]
    fn no_monitors_at_all_yields_zero_and_fallback() {
        assert_eq!(choose(&[], &MonitorChoice::Index(0)), (0, true));
    }
}
