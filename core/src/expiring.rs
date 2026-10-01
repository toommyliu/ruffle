use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

/// A value that's dropped when it hasn't been used for a while, and made again
/// when it's next needed.
#[derive(Debug)]
pub struct Expiring<T> {
    pub value: RefCell<Option<T>>,
    last_used: Cell<u32>,
    tracked: Cell<bool>,
}

impl<T> Default for Expiring<T> {
    fn default() -> Self {
        Self {
            value: RefCell::new(None),
            last_used: Cell::new(0),
            tracked: Cell::new(false),
        }
    }
}

pub struct ExpiringValues<T> {
    values: Vec<Weak<Expiring<T>>>,
    frame: u32,
}

impl<T> Default for ExpiringValues<T> {
    fn default() -> Self {
        Self {
            values: Vec::new(),
            frame: 0,
        }
    }
}

impl<T> ExpiringValues<T> {
    const SWEEP_INTERVAL: u32 = 24;
    const MAX_IDLE_FRAMES: u32 = 72;

    pub fn used(&mut self, value: &Rc<Expiring<T>>) {
        value.last_used.set(self.frame);
        if !value.tracked.replace(true) {
            self.values.push(Rc::downgrade(value));
        }
    }

    pub fn end_frame(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        if !self.frame.is_multiple_of(Self::SWEEP_INTERVAL) {
            return;
        }
        let frame = self.frame;
        self.values.retain(|value| {
            let Some(value) = value.upgrade() else {
                return false;
            };
            if frame.wrapping_sub(value.last_used.get()) <= Self::MAX_IDLE_FRAMES {
                return true;
            }
            value.value.take();
            value.tracked.set(false);
            false
        });
    }
}
