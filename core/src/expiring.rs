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
    released: Vec<(Weak<Expiring<T>>, u32)>,
    frame: u32,
}

impl<T> Default for ExpiringValues<T> {
    fn default() -> Self {
        Self {
            values: Vec::new(),
            released: Vec::new(),
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

    pub fn release(&mut self, value: &Rc<Expiring<T>>) {
        if value.value.borrow().is_some() {
            self.released.push((Rc::downgrade(value), self.frame));
        }
    }

    pub fn end_frame(&mut self) {
        for (value, frame) in self.released.drain(..) {
            if let Some(value) = value.upgrade()
                && value.last_used.get() != frame
            {
                value.value.take();
            }
        }
        self.frame = self.frame.wrapping_add(1);
        if self.frame.is_multiple_of(Self::SWEEP_INTERVAL) {
            self.retain_used_within(Self::MAX_IDLE_FRAMES);
        }
    }

    pub fn drop_unused_since_last_frame(&mut self) {
        self.retain_used_within(1);
    }

    fn retain_used_within(&mut self, frames: u32) {
        let frame = self.frame;
        self.values.retain(|value| {
            let Some(value) = value.upgrade() else {
                return false;
            };
            if frame.wrapping_sub(value.last_used.get()) <= frames {
                return true;
            }
            value.value.take();
            value.tracked.set(false);
            false
        });
    }
}
