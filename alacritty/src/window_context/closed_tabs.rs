//! Closed tabs that stay in the strip for a grace period.
//!
//! Closing a tab does not kill its shell right away. The tab shrinks to a
//! small restore button while its pty keeps running untouched, so a click on
//! the button brings it back with everything it printed meanwhile. When the
//! grace period ends the tab exits for real, through the same path a shell
//! exiting on its own takes.

use std::error::Error;
use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::event_loop::EventLoopProxy;
use winit::window::WindowId;

use alacritty_terminal::event_loop::{EventLoopSender, Notifier};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Term;
use alacritty_terminal::vte::ansi::{Handler, NamedPrivateMode, PrivateMode};

use crate::config::UiConfig;
use crate::config::tabs::TabSwitchStrategy;
use crate::display::SizeInfo;
use crate::event::{Event, EventProxy, EventType, TabAction, TabId};
use crate::scheduler::{Scheduler, TimerId, Topic};

use super::{TerminalTab, WindowContext};

/// Tab id of the blank terminal shown while every tab of a window is closed.
const BLANK_TAB_ID: TabId = TabId(u64::MAX);

/// Pick the tab to show when the tab at `from` goes away. Follows the switch
/// strategy first, then falls back to the nearest open tab on either side.
pub(super) fn pick_live_tab(
    closed: &[bool],
    from: usize,
    strategy: TabSwitchStrategy,
    previous: Option<usize>,
) -> Option<usize> {
    let live = |index: &usize| *index != from && closed.get(*index) == Some(&false);
    let preferred = match strategy {
        TabSwitchStrategy::Previous => previous.filter(live),
        TabSwitchStrategy::Left => (0..from).rev().find(live),
        TabSwitchStrategy::Right => (from + 1..closed.len()).find(live),
        TabSwitchStrategy::Last => (0..closed.len()).rev().find(live),
    };
    preferred.or_else(|| (from + 1..closed.len()).find(live)).or_else(|| (0..from).rev().find(live))
}

impl TerminalTab {
    /// A terminal with no shell behind it. Input written to it goes nowhere.
    pub(super) fn blank(
        window_id: WindowId,
        size_info: SizeInfo,
        config: &UiConfig,
        proxy: &EventLoopProxy<Event>,
    ) -> Result<Self, Box<dyn Error>> {
        let event_proxy = EventProxy::new(proxy.clone(), window_id, BLANK_TAB_ID);
        let mut terminal = Term::new(config.term_options(), &size_info, event_proxy);
        terminal.unset_private_mode(PrivateMode::Named(NamedPrivateMode::ShowCursor));

        Ok(Self {
            id: BLANK_TAB_ID,
            terminal: Arc::new(FairMutex::new(terminal)),
            notifier: Notifier(EventLoopSender::disconnected()?),
            terminal_title: None,
            detected_title: config.window.identity.title.clone(),
            custom_title: None,
            message_buffer: Default::default(),
            cursor_blink_timed_out: false,
            prev_bell_cmd: None,
            inline_search_state: Default::default(),
            search_state: Default::default(),
            #[cfg(not(windows))]
            master_fd: -1,
            #[cfg(not(windows))]
            shell_pid: 0,
            closed_at: None,
        })
    }
}

impl WindowContext {
    pub(super) fn is_closed(&self, index: usize) -> bool {
        self.tabs[index].closed_at.is_some()
    }

    /// Index of the tab whose terminal fills the window. None while the
    /// active slot holds a closed tab, then the blank terminal is shown.
    pub(super) fn shown_index(&self) -> Option<usize> {
        (!self.is_closed(self.active_tab)).then_some(self.active_tab)
    }

    pub(super) fn shown_tab(&self) -> &TerminalTab {
        match self.shown_index() {
            Some(index) => &self.tabs[index],
            None => &self.blank,
        }
    }

    /// Same as `shown_tab` for callers that already borrow other fields.
    pub(super) fn shown_slot<'a>(
        tabs: &'a mut [TerminalTab],
        blank: &'a mut TerminalTab,
        active: usize,
    ) -> &'a mut TerminalTab {
        match tabs.get_mut(active) {
            Some(tab) if tab.closed_at.is_none() => tab,
            _ => blank,
        }
    }

    /// Nearest open tab after or before the active one, wrapping around.
    pub(super) fn live_neighbor(&self, forward: bool) -> Option<usize> {
        let len = self.tabs.len();
        (1..len)
            .map(|offset| {
                if forward {
                    (self.active_tab + offset) % len
                } else {
                    (self.active_tab + len - offset) % len
                }
            })
            .find(|&index| !self.is_closed(index))
    }

    pub(super) fn last_live_tab(&self) -> Option<usize> {
        self.tabs.iter().rposition(|tab| tab.closed_at.is_none())
    }

    fn next_live_tab(&self, from: usize) -> Option<usize> {
        let closed: Vec<bool> = self.tabs.iter().map(|tab| tab.closed_at.is_some()).collect();
        let previous =
            self.last_active_tab_id.and_then(|id| self.tabs.iter().position(|tab| tab.id == id));
        pick_live_tab(&closed, from, self.config.tabs.tab_switch_strategy, previous)
    }

    pub(super) fn close_active_tab(&mut self, scheduler: &mut Scheduler) {
        self.cancel_window_close_confirmation();
        self.display.window.hold = false;
        let index = self.active_tab;
        if self.is_closed(index) {
            return;
        }

        let grace = self.config.tabs.close_grace_period;
        if grace == 0 {
            self.tabs[index].terminal.lock().exit();
            return;
        }

        let closed_at = Instant::now();
        let tab = &mut self.tabs[index];
        tab.closed_at = Some(closed_at);
        let window_id = self.display.window.id();
        let expire = TabAction::Expire { tab_id: tab.id, closed_at };
        scheduler.schedule(
            Event::new(EventType::Tab(expire), window_id),
            Duration::from_secs(grace),
            false,
            TimerId::new(Topic::TabExpiry, window_id),
        );

        match self.next_live_tab(index) {
            Some(next) => self.set_active_tab(next),
            None => {
                self.cancel_tab_title_editor();
                self.refresh_active_tab();
            },
        }
    }

    pub(super) fn restore_tab(&mut self, index: usize) {
        let Some(tab) = self.tabs.get_mut(index) else { return };
        if tab.closed_at.take().is_none() {
            return;
        }

        if self.active_tab != index {
            if !self.is_closed(self.active_tab) {
                self.last_active_tab_id = Some(self.tabs[self.active_tab].id);
            }
            self.active_tab = index;
        }
        self.cancel_tab_title_editor();
        self.refresh_active_tab();
    }

    /// End the grace period of a closed tab. The timer that fires this
    /// carries the close instant, so a timer from an earlier close of a tab
    /// that was restored and closed again does not cut the new period short.
    pub(super) fn expire_tab(&mut self, tab_id: TabId, closed_at: Instant) {
        let Some(tab) = self.tabs.iter().find(|tab| tab.id == tab_id) else { return };
        if tab.closed_at != Some(closed_at) {
            return;
        }

        self.display.window.hold = false;
        tab.terminal.lock().exit();
    }

    /// Drop a tab whose shell has exited. True when no tab is left.
    pub fn handle_tab_exit(&mut self, tab_id: Option<TabId>) -> bool {
        if self.display.window.hold {
            return false;
        }

        self.cancel_window_close_confirmation();

        let Some(index) = self.tab_index(tab_id) else {
            return false;
        };
        let closing_tab_id = self.tabs[index].id;
        let was_active = index == self.active_tab;
        let next_active_id = if was_active {
            self.next_live_tab(index).map(|next| self.tabs[next].id)
        } else {
            None
        };

        self.tabs.remove(index);
        if self.tab_title_editor.as_ref().is_some_and(|editor| editor.tab_id == closing_tab_id) {
            self.tab_title_editor = None;
        }

        if self.tabs.is_empty() {
            return true;
        }

        self.active_tab = if was_active {
            next_active_id
                .and_then(|id| self.tabs.iter().position(|tab| tab.id == id))
                .unwrap_or_else(|| index.min(self.tabs.len() - 1))
        } else {
            self.active_tab - usize::from(index < self.active_tab)
        };
        self.refresh_active_tab();

        false
    }
}

#[cfg(test)]
mod tests {
    use super::pick_live_tab;
    use crate::config::tabs::TabSwitchStrategy;

    #[test]
    fn skips_closed_tabs_and_falls_back_to_the_nearest_open_one() {
        let closed = [false, true, false, true];
        assert_eq!(pick_live_tab(&closed, 2, TabSwitchStrategy::Previous, Some(1)), Some(0));
        assert_eq!(pick_live_tab(&closed, 2, TabSwitchStrategy::Right, None), Some(0));
        assert_eq!(pick_live_tab(&closed, 0, TabSwitchStrategy::Left, None), Some(2));
        assert_eq!(pick_live_tab(&closed, 0, TabSwitchStrategy::Last, None), Some(2));
        assert_eq!(pick_live_tab(&[false, true], 0, TabSwitchStrategy::Previous, None), None);
    }
}
