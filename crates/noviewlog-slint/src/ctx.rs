//! Shared wiring context: engine handle + the force-render / fast-timer tail
//! repeated by most UI callbacks.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use noviewlog_core::{Command, Engine};
use slint::Timer;

use crate::engine_bridge::bump_fast_timer;

/// Handles shared by every callback block: engine + repaint/timer tail.
#[derive(Clone)]
pub(crate) struct Ctx {
    pub(crate) engine: Rc<RefCell<Engine>>,
    pub(crate) force_render: Rc<Cell<bool>>,
    pub(crate) timer: Rc<Timer>,
    pub(crate) timer_fast: Rc<Cell<bool>>,
    /// Viewport-focus change that could not be applied because the engine was
    /// already borrowed (synchronous re-entry, issue #252). Applied and
    /// cleared by the next host tick.
    pub(crate) pending_viewport_focus: Rc<Cell<Option<bool>>>,
}

impl Ctx {
    pub(crate) fn new(
        engine: Rc<RefCell<Engine>>,
        force_render: Rc<Cell<bool>>,
        timer: Rc<Timer>,
        timer_fast: Rc<Cell<bool>>,
    ) -> Self {
        Self {
            engine,
            force_render,
            timer,
            timer_fast,
            pending_viewport_focus: Rc::new(Cell::new(None)),
        }
    }

    pub(crate) fn send(&self, cmd: Command) -> Result<(), String> {
        self.engine.borrow_mut().send_command(cmd)
    }

    /// Mark the next tick dirty and poll at interactive cadence.
    pub(crate) fn refresh(&self) {
        self.force_render.set(true);
        bump_fast_timer(&self.timer, &self.timer_fast);
    }

    /// Send a command, then force the next tick to repaint at fast cadence.
    pub(crate) fn send_refresh(&self, cmd: Command) {
        let _ = self.send(cmd);
        self.refresh();
    }
}
