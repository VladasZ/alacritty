//! Unix signal listener.

use std::io::{Error as IoError, Read};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use signal_hook::consts::{SIGINT, SIGTERM, SIGUSR1};
use signal_hook::flag;
use signal_hook::low_level::pipe;
use winit::event_loop::EventLoopProxy;

use crate::event::{Event, EventType};

pub struct SignalListener {
    pub pipe: UnixStream,

    event_proxy: EventLoopProxy<Event>,
    shutdown: Arc<AtomicBool>,
    screenshot: Arc<AtomicBool>,
}

impl SignalListener {
    pub fn new(event_proxy: EventLoopProxy<Event>) -> Result<Self, IoError> {
        let (pipe, write) = UnixStream::pair()?;

        // Each signal sets its own flag, then wakes the loop through the pipe.
        // The garbage byte the pipe writes cannot tell signals apart, so the
        // flags do, which also avoids a spurious shutdown on rapid SIGUSR1.
        let shutdown = Arc::new(AtomicBool::new(false));
        let screenshot = Arc::new(AtomicBool::new(false));
        flag::register(SIGINT, Arc::clone(&shutdown))?;
        flag::register(SIGTERM, Arc::clone(&shutdown))?;
        flag::register(SIGUSR1, Arc::clone(&screenshot))?;
        pipe::register(SIGINT, write.try_clone()?)?;
        pipe::register(SIGTERM, write.try_clone()?)?;
        pipe::register(SIGUSR1, write)?;

        Ok(Self { event_proxy, pipe, shutdown, screenshot })
    }

    /// Process the next signal.
    pub fn process_signal(&mut self) -> Result<(), IoError> {
        // Drain one wake byte from the pipe.
        self.pipe.read_exact(&mut [0])?;

        if self.screenshot.swap(false, Ordering::SeqCst) {
            let _ = self.event_proxy.send_event(Event::new(EventType::Screenshot, None));
        }

        if self.shutdown.swap(false, Ordering::SeqCst) {
            let _ = self.event_proxy.send_event(Event::new(EventType::Shutdown, None));
        }

        Ok(())
    }
}
