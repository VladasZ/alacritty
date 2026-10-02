//! Closed windows that stay alive for a grace period.
//!
//! Closing a window or quitting the app does not kill the shells right away.
//! The window is hidden with every tab and pty untouched. Reopening the app
//! within the grace period shows it again exactly as it was. When the period
//! ends the window closes for real, through the normal window close path.

use std::time::Duration;

use crate::event::{Event, EventType, TabAction};
use crate::scheduler::{Scheduler, TimerId, Topic};

use super::WindowContext;

impl WindowContext {
    pub fn is_parked(&self) -> bool {
        self.parked
    }

    /// Hide the window with its tabs alive. With the grace period off this is
    /// a plain window close.
    pub fn park_or_close(&mut self, scheduler: &mut Scheduler) {
        if self.parked {
            return;
        }

        let grace = self.config.tabs.close_grace_period;
        if grace == 0 {
            self.request_window_close();
            return;
        }

        self.cancel_window_close_confirmation();
        self.cancel_tab_title_editor();
        self.parked = true;
        self.display.window.set_visible(false);

        let window_id = self.display.window.id();
        scheduler.schedule(
            Event::new(EventType::Tab(TabAction::ParkExpire), window_id),
            Duration::from_secs(grace),
            false,
            TimerId::new(Topic::WindowPark, window_id),
        );
    }

    pub(super) fn unpark(&mut self, scheduler: &mut Scheduler) {
        if !self.parked {
            return;
        }

        self.parked = false;
        scheduler.unschedule(TimerId::new(Topic::WindowPark, self.display.window.id()));
        self.display.window.set_visible(true);
        self.display.window.focus_window();
        self.display.pending_update.dirty = true;
        self.dirty = true;
    }

    pub(super) fn expire_park(&mut self) {
        if self.parked {
            self.confirm_window_close();
        }
    }
}
